//! Interactive crash dialog (Ring 3).
//!
//! When the fault interceptor freezes a task and reports it over the IPC bus,
//! the GUI surfaces a modal dialog that lets the user (or policy) decide how
//! to recover: **Rollback** (restore the task's pre-run snapshot and resume),
//! **Terminate** (kill the task), or **Resume** (ignore the error and carry
//! on). The chosen action is forwarded to the process manager as an IPC
//! `RecoveryDecision`.
//!
//! The dialog is deliberately a small state machine (no egui types in the
//! core decision logic) so the workflow can be tested without a windowing
//! session; only [`CrashDialogState::show`] binds to egui.

use aios_core::fault::{FaultInfo, RecoveryAction};

/// A recovery decision produced by the dialog for a faulted task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrashOutcome {
    /// Process the dialog was answered for.
    pub pid: u64,
    /// The user's/policy's choice.
    pub action: RecoveryAction,
}

/// State machine backing the crash dialog.
#[derive(Debug, Default)]
pub struct CrashDialogState {
    /// The fault currently shown, if any.
    pub open: Option<FaultInfo>,
    /// The last decision emitted by the dialog, if any.
    pub outcome: Option<CrashOutcome>,
}

impl CrashDialogState {
    /// Create an empty dialog state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a dialog is currently on screen.
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// The fault currently displayed, if any (test-only accessor).
    #[cfg(test)]
    pub fn current(&self) -> Option<&FaultInfo> {
        self.open.as_ref()
    }

    /// Offer a new fault to the dialog: it is opened at once if nothing else
    /// is showing, otherwise queued with the process manager (which keeps it
    /// pending until a decision is emitted).
    pub fn present(&mut self, fault: FaultInfo) {
        if self.open.is_none() {
            self.open = Some(fault);
        }
    }

    /// Record the decision for `pid` and close the dialog (if it was showing
    /// that pid). The caller applies the outcome to the scheduler.
    pub fn choose(&mut self, pid: u64, action: RecoveryAction) {
        if self.open.as_ref().is_some_and(|f| f.pid == pid) {
            self.open = None;
        }
        self.outcome = Some(CrashOutcome { pid, action });
    }

    /// Take the latest decision, if any, and rotate in the next pending fault
    /// on the next [`CrashDialogState::present`] call.
    pub fn take_outcome(&mut self) -> Option<CrashOutcome> {
        self.outcome.take()
    }

    /// Render the modal dialog for the currently open fault, if any.
    pub fn show(&mut self, ctx: &egui::Context) {
        let Some(fault) = self.open.clone() else {
            return;
        };

        let mut chosen: Option<RecoveryAction> = None;
        egui::Window::new("Task Error — recovery required")
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.strong(&fault.name);
                    ui.label(format!("(pid {}) has faulted.", fault.pid));
                });
                ui.add_space(4.0);
                ui.colored_label(
                    egui::Color32::from_rgb(255, 120, 120),
                    format!("{}: {}", fault.kind.name(), fault.message),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .button("Rollback — retry from snapshot")
                        .clicked()
                    {
                        chosen = Some(RecoveryAction::Rollback);
                    }
                    if ui.button("Terminate").clicked() {
                        chosen = Some(RecoveryAction::Terminate);
                    }
                    if ui.button("Resume — ignore error").clicked() {
                        chosen = Some(RecoveryAction::Resume);
                    }
                });
            });

        if let Some(action) = chosen {
            self.choose(fault.pid, action);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_core::fault::FaultKind;

    fn fault(pid: u64) -> FaultInfo {
        FaultInfo::new(
            pid,
            "worker".to_string(),
            FaultKind::Panic,
            "boom".to_string(),
            1,
        )
    }

    #[test]
    fn present_opens_only_when_closed() {
        let mut d = CrashDialogState::new();
        assert!(!d.is_open());
        d.present(fault(1));
        assert!(d.is_open());
        assert_eq!(d.current().unwrap().pid, 1);

        d.present(fault(2));
        assert_eq!(d.current().unwrap().pid, 1, "busy dialog keeps showing");
    }

    #[test]
    fn choose_emits_outcome_and_closes() {
        let mut d = CrashDialogState::new();
        d.present(fault(7));
        d.choose(7, RecoveryAction::Terminate);
        assert!(!d.is_open());
        let outcome = d.take_outcome().unwrap();
        assert_eq!(
            outcome,
            CrashOutcome {
                pid: 7,
                action: RecoveryAction::Terminate
            }
        );
        assert!(d.take_outcome().is_none());
    }

    #[test]
    fn choose_for_other_pid_keeps_dialog() {
        let mut d = CrashDialogState::new();
        d.present(fault(1));
        d.choose(2, RecoveryAction::Resume);
        assert!(d.is_open());
        let outcome = d.take_outcome().unwrap();
        assert_eq!(outcome.pid, 2);
    }

    #[test]
    fn closes_and_opens_next_pending() {
        let mut d = CrashDialogState::new();
        d.present(fault(1));
        d.present(fault(2));
        d.choose(1, RecoveryAction::Rollback);
        assert!(!d.is_open());
        d.present(fault(2));
        assert_eq!(d.current().unwrap().pid, 2);
    }
}
