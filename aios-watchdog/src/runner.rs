use crate::heartbeat::Heartbeat;
use crate::watchdog::{Watchdog, WatchdogAction, WatchdogConfig, WatchdogState};
use aios_core::error::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct WatchdogRunner {
    watchdog: Arc<Mutex<Watchdog>>,
    handle: Option<std::thread::JoinHandle<()>>,
    stop_flag: Arc<AtomicBool>,
    actions_received: Arc<Mutex<Vec<WatchdogAction>>>,
}

impl WatchdogRunner {
    pub fn start(config: WatchdogConfig) -> Self {
        let watchdog = Arc::new(Mutex::new(Watchdog::new(config)));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let actions_received = Arc::new(Mutex::new(Vec::new()));

        let wd_clone = watchdog.clone();
        let stop_clone = stop_flag.clone();
        let actions_clone = actions_received.clone();
        let interval = watchdog.lock().unwrap().config().heartbeat_interval_ms;

        let handle = std::thread::Builder::new()
            .name("aios-watchdog".into())
            .spawn(move || loop {
                if stop_clone.load(Ordering::Relaxed) {
                    break;
                }

                let action = {
                    let mut wd = wd_clone.lock().unwrap();
                    wd.check_timeout()
                };

                if action != WatchdogAction::None {
                    actions_clone.lock().unwrap().push(action);
                }

                std::thread::sleep(std::time::Duration::from_millis(interval / 2));
            })
            .expect("Failed to spawn watchdog thread");

        log::info!("WatchdogRunner: Started background monitoring thread");

        Self {
            watchdog,
            handle: Some(handle),
            stop_flag,
            actions_received,
        }
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        log::info!("WatchdogRunner: Stopped");
    }

    pub fn receive_heartbeat(&self, heartbeat: &Heartbeat) -> Result<()> {
        let mut wd = self.watchdog.lock().unwrap();
        wd.receive_heartbeat(heartbeat)
    }

    pub fn state(&self) -> WatchdogState {
        self.watchdog.lock().unwrap().state()
    }

    pub fn missed_count(&self) -> u32 {
        self.watchdog.lock().unwrap().missed_count()
    }

    pub fn stats(&self) -> (u64, u64) {
        self.watchdog.lock().unwrap().stats()
    }

    pub fn pop_actions(&self) -> Vec<WatchdogAction> {
        let mut actions = self.actions_received.lock().unwrap();
        actions.drain(..).collect()
    }

    pub fn escalate(&self) -> Vec<WatchdogAction> {
        let wd = self.watchdog.lock().unwrap();
        let actions = wd.escalate_actions();
        drop(wd);
        if !actions.is_empty() {
            let mut queue = self.actions_received.lock().unwrap();
            for action in &actions {
                queue.push(action.clone());
            }
        }
        actions
    }

    pub fn force_safe_mode(&self) {
        self.watchdog.lock().unwrap().force_safe_mode();
    }

    pub fn reset(&self) {
        self.watchdog.lock().unwrap().reset();
    }

    pub fn watchdog(&self) -> &Arc<Mutex<Watchdog>> {
        &self.watchdog
    }
}

impl Drop for WatchdogRunner {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
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
            max_missed_heartbeats: 2,
            warn_threshold: 1,
            recovery_timeout_ms: 300,
            secret: b"runner_test_secret".to_vec(),
        }
    }

    #[test]
    fn test_runner_start_stop() {
        let mut runner = WatchdogRunner::start(test_config());
        assert_eq!(runner.state(), WatchdogState::Monitoring);
        std::thread::sleep(Duration::from_millis(50));
        runner.stop();
        assert!(runner.handle.is_none());
    }

    #[test]
    fn test_runner_receives_heartbeat() {
        let runner = WatchdogRunner::start(test_config());
        let hb = Heartbeat::new(1, b"runner_test_secret");
        runner.receive_heartbeat(&hb).unwrap();
        assert_eq!(runner.stats(), (1, 0));
        std::thread::sleep(Duration::from_millis(10));
    }

    #[test]
    fn test_runner_detects_missed_heartbeats() {
        let runner = WatchdogRunner::start(test_config());
        let hb = Heartbeat::new(1, b"runner_test_secret");
        runner.receive_heartbeat(&hb).unwrap();

        std::thread::sleep(Duration::from_millis(350));
        let actions = runner.pop_actions();
        assert!(
            actions.contains(&WatchdogAction::SuspendOrchestrator)
                || runner.state() == WatchdogState::Suspended
        );
    }

    #[test]
    fn test_runner_force_safe_mode() {
        let runner = WatchdogRunner::start(test_config());
        runner.force_safe_mode();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(runner.state(), WatchdogState::SafeMode);
    }

    #[test]
    fn test_runner_reset() {
        let runner = WatchdogRunner::start(test_config());
        runner.force_safe_mode();
        std::thread::sleep(Duration::from_millis(20));
        runner.reset();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(runner.state(), WatchdogState::Monitoring);
    }

    #[test]
    fn test_runner_pop_actions() {
        let runner = WatchdogRunner::start(test_config());
        runner.force_safe_mode();
        std::thread::sleep(Duration::from_millis(50));
        let actions = runner.pop_actions();
        assert!(!actions.is_empty());
        let actions2 = runner.pop_actions();
        for action in &actions2 {
            assert!(action == &WatchdogAction::InSafeMode);
        }
    }

    #[test]
    fn test_runner_drop_stops_thread() {
        let runner = WatchdogRunner::start(test_config());
        std::thread::sleep(Duration::from_millis(50));
        drop(runner);
    }

    #[test]
    fn test_runner_recovery_after_heartbeat() {
        let config = WatchdogConfig {
            heartbeat_interval_ms: 200,
            max_missed_heartbeats: 2,
            warn_threshold: 1,
            recovery_timeout_ms: 5000,
            secret: b"runner_test_secret".to_vec(),
        };
        let runner = WatchdogRunner::start(config);
        let hb1 = Heartbeat::new(1, b"runner_test_secret");
        runner.receive_heartbeat(&hb1).unwrap();

        std::thread::sleep(Duration::from_millis(500));
        assert!(
            runner.state() == WatchdogState::Suspended
                || runner.state() == WatchdogState::Recovering
                || runner.state() == WatchdogState::SafeMode
        );

        let hb2 = Heartbeat::new(2, b"runner_test_secret");
        runner.receive_heartbeat(&hb2).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(runner.state(), WatchdogState::Monitoring);
    }

    #[test]
    fn test_runner_escalate_in_safemode() {
        let runner = WatchdogRunner::start(test_config());
        runner.force_safe_mode();
        std::thread::sleep(Duration::from_millis(20));

        let actions = runner.escalate();
        assert!(!actions.is_empty());
        assert!(actions
            .iter()
            .any(|a| matches!(a, WatchdogAction::SafeModeShell)));
        assert!(actions
            .iter()
            .any(|a| matches!(a, WatchdogAction::DumpState(_))));
    }

    #[test]
    fn test_runner_escalate_in_suspended() {
        let config = WatchdogConfig {
            heartbeat_interval_ms: 50,
            max_missed_heartbeats: 1,
            warn_threshold: 1,
            recovery_timeout_ms: 5000,
            secret: b"runner_test_secret".to_vec(),
        };
        let runner = WatchdogRunner::start(config);
        let hb = Heartbeat::new(1, b"runner_test_secret");
        runner.receive_heartbeat(&hb).unwrap();

        std::thread::sleep(Duration::from_millis(200));
        assert!(
            runner.state() == WatchdogState::Suspended
                || runner.state() == WatchdogState::Recovering
                || runner.state() == WatchdogState::SafeMode
        );

        let actions = runner.escalate();
        if runner.state() == WatchdogState::Suspended {
            assert!(actions
                .iter()
                .any(|a| matches!(a, WatchdogAction::KillProcess(_))));
        }
    }

    #[test]
    fn test_escalate_in_monitoring_returns_empty() {
        let runner = WatchdogRunner::start(test_config());
        let hb = Heartbeat::new(1, b"runner_test_secret");
        runner.receive_heartbeat(&hb).unwrap();
        let actions = runner.escalate();
        assert!(actions.is_empty());
    }

    #[test]
    fn test_action_severity_ordering() {
        assert!(WatchdogAction::None.severity() < WatchdogAction::SuspendOrchestrator.severity());
        assert!(
            WatchdogAction::SuspendOrchestrator.severity()
                < WatchdogAction::KillProcess(0).severity()
        );
        assert!(
            WatchdogAction::KillProcess(0).severity() < WatchdogAction::EnterSafeMode.severity()
        );
        assert!(
            WatchdogAction::EnterSafeMode.severity() < WatchdogAction::SafeModeShell.severity()
        );
    }

    #[test]
    fn test_action_is_terminal() {
        assert!(!WatchdogAction::None.is_terminal());
        assert!(!WatchdogAction::SuspendOrchestrator.is_terminal());
        assert!(WatchdogAction::EnterSafeMode.is_terminal());
        assert!(WatchdogAction::KillProcess(1).is_terminal());
        assert!(WatchdogAction::SafeModeShell.is_terminal());
    }
}
