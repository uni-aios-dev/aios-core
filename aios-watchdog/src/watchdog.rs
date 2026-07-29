use crate::heartbeat::Heartbeat;
use aios_core::error::{AIOSException, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WatchdogState {
    Monitoring,
    Warned,
    Suspended,
    Recovering,
    SafeMode,
}

#[derive(Debug, Clone)]
pub struct WatchdogConfig {
    pub heartbeat_interval_ms: u64,
    pub max_missed_heartbeats: u32,
    pub warn_threshold: u32,
    pub recovery_timeout_ms: u64,
    pub secret: Vec<u8>,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval_ms: 1000,
            max_missed_heartbeats: 3,
            warn_threshold: 2,
            recovery_timeout_ms: 10_000,
            secret: b"default_aios_secret".to_vec(),
        }
    }
}

pub struct Watchdog {
    config: WatchdogConfig,
    state: WatchdogState,
    last_heartbeat: Option<Heartbeat>,
    missed_count: u32,
    total_received: u64,
    total_missed: u64,
    state_log: Vec<WatchdogEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchdogEvent {
    pub timestamp_ms: u64,
    pub event_type: WatchdogEventType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WatchdogEventType {
    HeartbeatReceived(u64),
    HeartbeatMissed(u32),
    Warned,
    Suspended,
    Recovering,
    EnteredSafeMode,
    Recovered,
}

impl Watchdog {
    pub fn new(config: WatchdogConfig) -> Self {
        Self {
            config,
            state: WatchdogState::Monitoring,
            last_heartbeat: None,
            missed_count: 0,
            total_received: 0,
            total_missed: 0,
            state_log: Vec::new(),
        }
    }

    pub fn receive_heartbeat(&mut self, heartbeat: &Heartbeat) -> Result<()> {
        if !heartbeat.verify(&self.config.secret) {
            return Err(AIOSException::IntegrityCheckFailed(
                "Heartbeat HMAC verification failed".into(),
            ));
        }

        self.total_received += 1;
        self.missed_count = 0;
        self.last_heartbeat = Some(heartbeat.clone());

        self.log_event(WatchdogEventType::HeartbeatReceived(heartbeat.sequence));

        if self.state == WatchdogState::Recovering
            || self.state == WatchdogState::SafeMode
            || self.state == WatchdogState::Warned
        {
            self.state = WatchdogState::Monitoring;
            self.log_event(WatchdogEventType::Recovered);
        }

        Ok(())
    }

    pub fn check_timeout(&mut self) -> WatchdogAction {
        match self.state {
            WatchdogState::Monitoring | WatchdogState::Warned => {
                let overdue = self
                    .last_heartbeat
                    .as_ref()
                    .is_none_or(|hb| hb.age_ms() > self.config.heartbeat_interval_ms);

                if overdue {
                    self.missed_count += 1;
                    self.total_missed += 1;
                    self.log_event(WatchdogEventType::HeartbeatMissed(self.missed_count));

                    if self.missed_count >= self.config.max_missed_heartbeats {
                        self.state = WatchdogState::Suspended;
                        self.log_event(WatchdogEventType::Suspended);
                        return WatchdogAction::SuspendOrchestrator;
                    }

                    if self.missed_count >= self.config.warn_threshold
                        && self.state == WatchdogState::Monitoring
                    {
                        self.state = WatchdogState::Warned;
                        self.log_event(WatchdogEventType::Warned);
                        return WatchdogAction::WarnOrchestrator;
                    }
                }
                WatchdogAction::None
            }
            WatchdogState::Suspended => {
                self.state = WatchdogState::Recovering;
                self.log_event(WatchdogEventType::Recovering);
                WatchdogAction::KillProcess(0)
            }
            WatchdogState::Recovering => {
                let elapsed = self
                    .last_heartbeat
                    .as_ref()
                    .map_or(u64::MAX, |hb| hb.age_ms());
                if elapsed > self.config.recovery_timeout_ms {
                    self.state = WatchdogState::SafeMode;
                    self.log_event(WatchdogEventType::EnteredSafeMode);
                    return WatchdogAction::SafeModeShell;
                }
                WatchdogAction::WaitForRecovery
            }
            WatchdogState::SafeMode => WatchdogAction::InSafeMode,
        }
    }

    pub fn force_safe_mode(&mut self) {
        self.state = WatchdogState::SafeMode;
        self.log_event(WatchdogEventType::EnteredSafeMode);
    }

    pub fn escalate_actions(&self) -> Vec<WatchdogAction> {
        let mut actions = Vec::new();
        match self.state {
            WatchdogState::SafeMode => {
                actions.push(WatchdogAction::DumpState(format!(
                    "safe_mode_dump_{}",
                    Heartbeat::now_ms()
                )));
                actions.push(WatchdogAction::SafeModeShell);
            }
            WatchdogState::Recovering => {
                actions.push(WatchdogAction::DumpState(format!(
                    "recovery_dump_{}",
                    Heartbeat::now_ms()
                )));
            }
            WatchdogState::Suspended => {
                actions.push(WatchdogAction::KillProcess(0));
                actions.push(WatchdogAction::DumpState(format!(
                    "suspend_dump_{}",
                    Heartbeat::now_ms()
                )));
            }
            WatchdogState::Warned => {
                actions.push(WatchdogAction::DumpState(format!(
                    "warn_dump_{}",
                    Heartbeat::now_ms()
                )));
            }
            WatchdogState::Monitoring => {}
        }
        actions
    }

    pub fn reset(&mut self) {
        self.state = WatchdogState::Monitoring;
        self.last_heartbeat = None;
        self.missed_count = 0;
    }

    pub fn state(&self) -> WatchdogState {
        self.state
    }

    pub fn missed_count(&self) -> u32 {
        self.missed_count
    }

    pub fn stats(&self) -> (u64, u64) {
        (self.total_received, self.total_missed)
    }

    pub fn state_log(&self) -> &[WatchdogEvent] {
        &self.state_log
    }

    pub fn last_heartbeat(&self) -> Option<&Heartbeat> {
        self.last_heartbeat.as_ref()
    }

    pub fn config(&self) -> &WatchdogConfig {
        &self.config
    }

    fn log_event(&mut self, event_type: WatchdogEventType) {
        self.state_log.push(WatchdogEvent {
            timestamp_ms: Heartbeat::now_ms(),
            event_type,
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchdogAction {
    None,
    WarnOrchestrator,
    SuspendOrchestrator,
    AttemptRecovery,
    WaitForRecovery,
    EnterSafeMode,
    InSafeMode,
    KillProcess(u32),
    DumpState(String),
    SafeModeShell,
}

impl WatchdogAction {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            WatchdogAction::EnterSafeMode
                | WatchdogAction::InSafeMode
                | WatchdogAction::KillProcess(_)
                | WatchdogAction::SafeModeShell
        )
    }

    pub fn severity(&self) -> u8 {
        match self {
            WatchdogAction::None => 0,
            WatchdogAction::WarnOrchestrator => 1,
            WatchdogAction::WaitForRecovery => 2,
            WatchdogAction::SuspendOrchestrator => 3,
            WatchdogAction::AttemptRecovery => 4,
            WatchdogAction::KillProcess(_) => 5,
            WatchdogAction::DumpState(_) => 6,
            WatchdogAction::EnterSafeMode => 7,
            WatchdogAction::SafeModeShell => 8,
            WatchdogAction::InSafeMode => 9,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn test_config() -> WatchdogConfig {
        WatchdogConfig {
            heartbeat_interval_ms: 100,
            max_missed_heartbeats: 3,
            warn_threshold: 2,
            recovery_timeout_ms: 500,
            secret: b"test_secret".to_vec(),
        }
    }

    fn good_heartbeat(seq: u64) -> Heartbeat {
        Heartbeat::new(seq, b"test_secret")
    }

    #[test]
    fn test_healthy_heartbeat_flow() {
        let mut wd = Watchdog::new(test_config());
        assert_eq!(wd.state(), WatchdogState::Monitoring);
        wd.receive_heartbeat(&good_heartbeat(1)).unwrap();
        assert_eq!(wd.missed_count(), 0);
        assert_eq!(wd.stats(), (1, 0));
    }

    #[test]
    fn test_missed_heartbeats_trigger_suspend() {
        let mut wd = Watchdog::new(test_config());
        wd.receive_heartbeat(&good_heartbeat(1)).unwrap();

        std::thread::sleep(Duration::from_millis(120));
        let action = wd.check_timeout();
        assert_eq!(action, WatchdogAction::None);

        std::thread::sleep(Duration::from_millis(120));
        let action = wd.check_timeout();
        assert_eq!(action, WatchdogAction::WarnOrchestrator);
        assert_eq!(wd.state(), WatchdogState::Warned);

        std::thread::sleep(Duration::from_millis(120));
        let action = wd.check_timeout();
        assert_eq!(action, WatchdogAction::SuspendOrchestrator);
        assert_eq!(wd.state(), WatchdogState::Suspended);
    }

    #[test]
    fn test_graduated_escalation_warned_to_safe_mode() {
        let mut wd = Watchdog::new(test_config());
        wd.receive_heartbeat(&good_heartbeat(1)).unwrap();

        std::thread::sleep(Duration::from_millis(120));
        let action = wd.check_timeout();
        assert_eq!(action, WatchdogAction::None);

        std::thread::sleep(Duration::from_millis(120));
        let action = wd.check_timeout();
        assert_eq!(action, WatchdogAction::WarnOrchestrator);

        std::thread::sleep(Duration::from_millis(120));
        let action = wd.check_timeout();
        assert_eq!(action, WatchdogAction::SuspendOrchestrator);

        let action = wd.check_timeout();
        assert_eq!(action, WatchdogAction::KillProcess(0));
        assert_eq!(wd.state(), WatchdogState::Recovering);

        std::thread::sleep(Duration::from_millis(600));
        let action = wd.check_timeout();
        assert_eq!(action, WatchdogAction::SafeModeShell);
        assert_eq!(wd.state(), WatchdogState::SafeMode);
    }

    #[test]
    fn test_heartbeat_recovers_from_warned() {
        let mut wd = Watchdog::new(test_config());
        wd.receive_heartbeat(&good_heartbeat(1)).unwrap();

        std::thread::sleep(Duration::from_millis(120));
        wd.check_timeout();
        assert_eq!(wd.state(), WatchdogState::Monitoring);

        std::thread::sleep(Duration::from_millis(120));
        wd.check_timeout();
        assert_eq!(wd.state(), WatchdogState::Warned);

        wd.receive_heartbeat(&good_heartbeat(2)).unwrap();
        assert_eq!(wd.state(), WatchdogState::Monitoring);
        assert_eq!(wd.missed_count(), 0);
    }

    #[test]
    fn test_escalate_in_warned() {
        let mut wd = Watchdog::new(test_config());
        wd.receive_heartbeat(&good_heartbeat(1)).unwrap();
        std::thread::sleep(Duration::from_millis(120));
        wd.check_timeout();
        std::thread::sleep(Duration::from_millis(120));
        wd.check_timeout();
        assert_eq!(wd.state(), WatchdogState::Warned);

        let actions = wd.escalate_actions();
        assert_eq!(actions.len(), 1);
        assert!(actions
            .iter()
            .any(|a| matches!(a, WatchdogAction::DumpState(_))));
    }

    #[test]
    fn test_recovery_cycle() {
        let mut wd = Watchdog::new(test_config());
        wd.receive_heartbeat(&good_heartbeat(1)).unwrap();

        std::thread::sleep(Duration::from_millis(120));
        wd.check_timeout();
        std::thread::sleep(Duration::from_millis(120));
        wd.check_timeout();
        assert_eq!(wd.state(), WatchdogState::Warned);

        std::thread::sleep(Duration::from_millis(120));
        let action = wd.check_timeout();
        assert_eq!(action, WatchdogAction::SuspendOrchestrator);
        assert_eq!(wd.state(), WatchdogState::Suspended);

        let action = wd.check_timeout();
        assert_eq!(action, WatchdogAction::KillProcess(0));
        assert_eq!(wd.state(), WatchdogState::Recovering);

        wd.receive_heartbeat(&good_heartbeat(2)).unwrap();
        assert_eq!(wd.state(), WatchdogState::Monitoring);
    }

    #[test]
    fn test_recovery_timeout_enters_safe_mode() {
        let mut wd = Watchdog::new(test_config());
        wd.receive_heartbeat(&good_heartbeat(1)).unwrap();

        std::thread::sleep(Duration::from_millis(120));
        wd.check_timeout();
        std::thread::sleep(Duration::from_millis(120));
        wd.check_timeout();
        std::thread::sleep(Duration::from_millis(120));
        wd.check_timeout();
        wd.check_timeout();
        assert_eq!(wd.state(), WatchdogState::Recovering);

        std::thread::sleep(Duration::from_millis(600));
        let action = wd.check_timeout();
        assert_eq!(action, WatchdogAction::SafeModeShell);
        assert_eq!(wd.state(), WatchdogState::SafeMode);
    }

    #[test]
    fn test_invalid_heartbeat_rejected() {
        let mut wd = Watchdog::new(test_config());
        let bad = Heartbeat::new(1, b"wrong_secret");
        assert!(wd.receive_heartbeat(&bad).is_err());
    }

    #[test]
    fn test_force_safe_mode() {
        let mut wd = Watchdog::new(test_config());
        wd.force_safe_mode();
        assert_eq!(wd.state(), WatchdogState::SafeMode);
    }

    #[test]
    fn test_reset() {
        let mut wd = Watchdog::new(test_config());
        wd.force_safe_mode();
        wd.reset();
        assert_eq!(wd.state(), WatchdogState::Monitoring);
    }

    #[test]
    fn test_state_log() {
        let mut wd = Watchdog::new(test_config());
        wd.receive_heartbeat(&good_heartbeat(1)).unwrap();
        std::thread::sleep(Duration::from_millis(120));
        wd.check_timeout();
        std::thread::sleep(Duration::from_millis(120));
        wd.check_timeout();
        std::thread::sleep(Duration::from_millis(120));
        wd.check_timeout();
        assert!(wd.state_log().len() >= 5);
    }

    #[test]
    fn test_escalate_in_safemode() {
        let mut wd = Watchdog::new(test_config());
        wd.force_safe_mode();
        let actions = wd.escalate_actions();
        assert_eq!(actions.len(), 2);
        assert!(actions
            .iter()
            .any(|a| matches!(a, WatchdogAction::SafeModeShell)));
        assert!(actions
            .iter()
            .any(|a| matches!(a, WatchdogAction::DumpState(_))));
    }

    #[test]
    fn test_escalate_in_suspended() {
        let mut wd = Watchdog::new(test_config());
        wd.receive_heartbeat(&good_heartbeat(1)).unwrap();
        std::thread::sleep(Duration::from_millis(120));
        wd.check_timeout();
        std::thread::sleep(Duration::from_millis(120));
        wd.check_timeout();
        std::thread::sleep(Duration::from_millis(120));
        wd.check_timeout();
        assert_eq!(wd.state(), WatchdogState::Suspended);

        let actions = wd.escalate_actions();
        assert!(actions
            .iter()
            .any(|a| matches!(a, WatchdogAction::KillProcess(_))));
        assert!(actions
            .iter()
            .any(|a| matches!(a, WatchdogAction::DumpState(_))));
    }

    #[test]
    fn test_escalate_in_monitoring_empty() {
        let wd = Watchdog::new(test_config());
        let actions = wd.escalate_actions();
        assert!(actions.is_empty());
    }

    #[test]
    fn test_action_severity() {
        assert_eq!(WatchdogAction::None.severity(), 0);
        assert_eq!(WatchdogAction::InSafeMode.severity(), 9);
    }

    #[test]
    fn test_action_is_terminal() {
        assert!(!WatchdogAction::None.is_terminal());
        assert!(!WatchdogAction::AttemptRecovery.is_terminal());
        assert!(WatchdogAction::SafeModeShell.is_terminal());
    }
}
