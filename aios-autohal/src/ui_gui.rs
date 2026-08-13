use crate::engine::{DeviceView, DriverState, Severity, Toast, ToastKind, GENERIC_FALLBACK_ID};
use crate::ui_tui::{caps_summary, HARDWARE_INSPECTOR_TITLE};
use aios_security::capability::Capability;
use egui::{Color32, RichText, Ui};
use std::collections::HashSet;

/// Actions a GUI user triggers from the panel; the host applies them to the
/// engine (provision/rollback/uninstall/security-matrix) and refreshes views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuiAction {
    Update { index: usize },
    Rollback { index: usize },
    Uninstall { index: usize },
    SetCapabilities { index: usize, caps: Vec<Capability> },
    Rescan,
}

/// Map a driver state to the GUI color (same severity mapping as the TUI).
pub fn state_color(state: DriverState) -> Color32 {
    match state.severity() {
        Severity::Good => Color32::from_rgb(50, 200, 80),
        Severity::Busy => Color32::from_rgb(240, 180, 30),
        Severity::Warn => Color32::from_rgb(240, 150, 40),
        Severity::Bad => Color32::from_rgb(230, 60, 60),
    }
}

/// Map a toast kind to the GUI color.
pub fn toast_color(kind: ToastKind) -> Color32 {
    match kind {
        ToastKind::Info => Color32::from_rgb(100, 160, 255),
        ToastKind::Success => Color32::from_rgb(50, 200, 80),
        ToastKind::Warn => Color32::from_rgb(240, 180, 30),
        ToastKind::Error => Color32::from_rgb(230, 60, 60),
    }
}

/// egui panel "Hardware & Drivers". Reads the same [`DeviceView`]/[`Toast`]
/// data as the TUI inspector and returns [`GuiAction`]s for the host.
pub struct HardwarePanel<'a> {
    pub devices: &'a [DeviceView],
    pub toasts: &'a [Toast],
    /// Disables driver-mutating buttons when a provisioning run is in flight.
    pub enabled: bool,
}

impl HardwarePanel<'_> {
    pub fn show(&mut self, ui: &mut Ui) -> Vec<GuiAction> {
        let mut actions = Vec::new();

        // Hot-plug toast strip (same messages as the TUI).
        for toast in self.toasts.iter().rev().take(4) {
            ui.colored_label(toast_color(toast.kind), &toast.message);
        }

        ui.separator();
        ui.horizontal(|ui| {
            ui.heading(HARDWARE_INSPECTOR_TITLE);
            if ui
                .add_enabled(self.enabled, egui::Button::new("\u{21bb} Rescan"))
                .clicked()
            {
                actions.push(GuiAction::Rescan);
            }
        });
        ui.separator();

        if self.devices.is_empty() {
            ui.label("No devices detected. Run a hardware rescan to populate the panel.");
            return actions;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.render_summary_table(ui);
                ui.separator();
                for (index, dev) in self.devices.iter().enumerate() {
                    self.render_device_detail(ui, index, dev, &mut actions);
                }
            });

        actions
    }

    fn render_summary_table(&self, ui: &mut Ui) {
        egui::Grid::new("hw_summary")
            .striped(true)
            .min_col_width(72.0)
            .show(ui, |ui| {
                for header in [
                    "Bus",
                    "Device",
                    "VID:PID",
                    "Driver",
                    "Source",
                    "Status",
                    "Capabilities",
                ] {
                    ui.strong(header);
                }
                ui.end_row();

                for dev in self.devices.iter() {
                    ui.label(dev.fingerprint.bus.label());
                    ui.label(dev.fingerprint.display_name());
                    ui.label(format!(
                        "{:04X}:{:04X}",
                        dev.fingerprint.vendor_id, dev.fingerprint.device_id
                    ));
                    ui.label(&dev.driver_name);
                    ui.label(dev.source.clone().unwrap_or_default());
                    ui.colored_label(state_color(dev.state), dev.state.label());
                    ui.label(caps_summary(&dev.capabilities));
                    ui.end_row();

                    let progress = dev.progress;
                    if matches!(dev.state, DriverState::Downloading | DriverState::Compiling) {
                        ui.add(
                            egui::ProgressBar::new(progress as f32 / 100.0)
                                .text(format!("{progress}% {}", dev.state.label())),
                        );
                        ui.end_row();
                    }
                }
            });
    }

    fn render_device_detail(
        &self,
        ui: &mut Ui,
        index: usize,
        dev: &DeviceView,
        actions: &mut Vec<GuiAction>,
    ) {
        egui::CollapsingHeader::new(
            RichText::new(format!(
                "{:04X}:{:04X}  {}",
                dev.fingerprint.vendor_id, dev.fingerprint.device_id, dev.driver_name
            ))
            .size(14.0),
        )
        .id_salt(("hw_device", index))
        .show(ui, |ui| {
            ui.label(format!("Driver id: {}", dev.driver_id));
            if let Some(err) = &dev.last_error {
                ui.colored_label(Color32::from_rgb(230, 60, 60), format!("Error: {err}"));
            }

            ui.separator();
            ui.strong("Security matrix");
            self.render_capability_matrix(ui, index, dev, actions);

            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(self.enabled, egui::Button::new("Update Driver"))
                    .clicked()
                {
                    actions.push(GuiAction::Update { index });
                }
                if ui
                    .add_enabled(self.enabled, egui::Button::new("Rollback to Generic"))
                    .clicked()
                {
                    actions.push(GuiAction::Rollback { index });
                }
                let can_uninstall = self.enabled && dev.driver_id != GENERIC_FALLBACK_ID;
                if ui
                    .add_enabled(can_uninstall, egui::Button::new("Uninstall"))
                    .clicked()
                {
                    actions.push(GuiAction::Uninstall { index });
                }
            });
        });
    }

    fn render_capability_matrix(
        &self,
        ui: &mut Ui,
        index: usize,
        dev: &DeviceView,
        actions: &mut Vec<GuiAction>,
    ) {
        let mut checked: HashSet<Capability> = dev.capabilities.iter().copied().collect();
        let mut changed = false;

        ui.horizontal_wrapped(|ui| {
            for cap in Capability::all_variants() {
                let mut on = checked.contains(&cap);
                if ui
                    .checkbox(&mut on, cap.name())
                    .on_hover_text(cap.description())
                    .changed()
                {
                    if on {
                        checked.insert(cap);
                    } else {
                        checked.remove(&cap);
                    }
                    changed = true;
                }
            }
        });

        if changed {
            let caps: Vec<Capability> = Capability::all_variants()
                .into_iter()
                .filter(|c| checked.contains(c))
                .collect();
            actions.push(GuiAction::SetCapabilities { index, caps });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::{BusType, HardwareFingerprint};

    fn view() -> DeviceView {
        DeviceView {
            fingerprint: HardwareFingerprint {
                bus: BusType::USB,
                vendor_id: 0x046D,
                device_id: 0x0825,
                class_code: 0,
                serial_or_acpi: None,
            },
            driver_id: "driver.usb.046d.0825".into(),
            driver_name: "Logitech C270 Webcam".into(),
            source: Some("Builtin".into()),
            state: DriverState::Active,
            failures: 0,
            progress: 100,
            capabilities: vec![Capability::HwAccess],
            last_error: None,
        }
    }

    #[test]
    fn test_colors_map_severity() {
        assert_ne!(
            state_color(DriverState::Active),
            state_color(DriverState::Failed)
        );
        assert_ne!(
            toast_color(ToastKind::Success),
            toast_color(ToastKind::Error)
        );
    }

    #[test]
    fn test_panel_runs_and_returns_no_actions_without_input() {
        let ctx = egui::Context::default();
        let devices = vec![view()];
        let toasts = vec![Toast {
            message: "[Hardware] Detected USB 046D:0825 -> looking up driver...".into(),
            kind: ToastKind::Info,
            created_ms: 0,
        }];
        let mut actions = Vec::new();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut panel = HardwarePanel {
                    devices: &devices,
                    toasts: &toasts,
                    enabled: true,
                };
                actions = panel.show(ui);
            });
        });
        assert!(actions.is_empty(), "no input must produce no actions");
    }

    #[test]
    fn test_empty_panel_no_panic() {
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut panel = HardwarePanel {
                    devices: &[],
                    toasts: &[],
                    enabled: true,
                };
                assert!(panel.show(ui).is_empty());
            });
        });
    }
}
