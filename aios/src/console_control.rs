//! Real host implementations of the `aios-console` control seams.
//!
//! The watchdog, bridge and auth subsystems sit above `aios-console` in the
//! dependency graph, so the console defines traits and this binary supplies
//! adapters over the live orchestrator handles. Every adapter reports real
//! state or a real library error; none of them fabricates success.

use crate::orchestrator::push_log;
use aios_bridge::auth::AuthHandle;
use aios_bridge::server::{start_server_with_shutdown, BridgeContext};
use aios_console::control::{AuthProvider, BridgeControl, WatchdogControl};
use aios_watchdog::runner::WatchdogRunner;
use aios_watchdog::watchdog::Watchdog;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

/// Console control over the watchdog instance shared with the bridge.
pub struct HostWatchdogControl {
    watchdog: Arc<Mutex<Watchdog>>,
    runner: Mutex<Option<WatchdogRunner>>,
}

impl HostWatchdogControl {
    /// Builds a control surface over an existing shared watchdog handle.
    pub fn new(watchdog: Arc<Mutex<Watchdog>>) -> Self {
        Self {
            watchdog,
            runner: Mutex::new(None),
        }
    }
}

impl WatchdogControl for HostWatchdogControl {
    fn status(&self) -> Vec<String> {
        let monitoring = self
            .runner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some();
        let watchdog = self
            .watchdog
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (received, missed) = watchdog.stats();
        vec![
            format!(
                "monitoring thread: {}",
                if monitoring { "running" } else { "stopped" }
            ),
            format!("state: {:?}", watchdog.state()),
            format!("missed heartbeats: {}", watchdog.missed_count()),
            format!("heartbeats received: {received}"),
            format!("heartbeats missed: {missed}"),
            format!("state events: {}", watchdog.state_log().len()),
        ]
    }

    fn start(&self) -> Result<(), String> {
        let mut runner = self.runner.lock().map_err(|error| error.to_string())?;
        if runner.is_some() {
            return Err("watchdog monitoring already running".into());
        }
        let interval = self
            .watchdog
            .lock()
            .map_err(|error| error.to_string())?
            .config()
            .heartbeat_interval_ms;
        *runner = Some(WatchdogRunner::from_watchdog(
            self.watchdog.clone(),
            interval,
        ));
        Ok(())
    }

    fn stop(&self) -> Result<(), String> {
        let mut runner = self.runner.lock().map_err(|error| error.to_string())?;
        let mut active = runner
            .take()
            .ok_or_else(|| "watchdog monitoring is not running".to_string())?;
        active.stop();
        Ok(())
    }

    fn force_safe_mode(&self) {
        self.watchdog
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .force_safe_mode();
    }

    fn reset(&self) -> Result<(), String> {
        let runner = self.runner.lock().map_err(|error| error.to_string())?;
        if let Some(runner) = runner.as_ref() {
            runner.reset();
        } else {
            self.watchdog
                .lock()
                .map_err(|error| error.to_string())?
                .reset();
        }
        Ok(())
    }

    fn escalate(&self) -> Vec<String> {
        self.watchdog
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .escalate_actions()
            .iter()
            .map(|action| format!("{action:?}"))
            .collect()
    }
}

/// Runtime state of the in-process HTTP bridge, shared with the console.
pub struct BridgeRuntime {
    context: Arc<BridgeContext>,
    running: Arc<AtomicBool>,
    logs: Arc<Mutex<Vec<String>>>,
    port: u16,
    shutdown: Mutex<Option<watch::Sender<()>>>,
}

impl BridgeRuntime {
    /// Creates a bridge runtime whose listener has not been started yet.
    pub fn new(
        context: Arc<BridgeContext>,
        running: Arc<AtomicBool>,
        logs: Arc<Mutex<Vec<String>>>,
        port: u16,
    ) -> Self {
        Self {
            context,
            running,
            logs,
            port,
            shutdown: Mutex::new(None),
        }
    }

    /// Spawns the listener with a fresh graceful-shutdown channel.
    pub fn spawn_server(&self) -> Result<(), String> {
        if self.running.load(Ordering::SeqCst) {
            return Err("bridge listener already running".into());
        }
        let (sender, mut receiver) = watch::channel::<()>(());
        let context = self.context.clone();
        let running = self.running.clone();
        let logs = self.logs.clone();
        let addr = format!("0.0.0.0:{}", self.port);
        self.running.store(true, Ordering::SeqCst);
        *self.shutdown.lock().map_err(|error| error.to_string())? = Some(sender);
        tokio::spawn(async move {
            push_log(&logs, format!("AIOS: Bridge listening on {addr}"));
            let result = start_server_with_shutdown(context, &addr, async move {
                let _ = receiver.changed().await;
            })
            .await;
            running.store(false, Ordering::SeqCst);
            match result {
                Ok(()) => push_log(&logs, "AIOS: Bridge listener stopped".into()),
                Err(error) => {
                    push_log(&logs, format!("AIOS ERROR: Bridge server failed: {error}"));
                }
            }
        });
        Ok(())
    }
}

impl BridgeControl for BridgeRuntime {
    fn status(&self) -> Vec<String> {
        vec![
            format!(
                "running: {}",
                if self.running.load(Ordering::SeqCst) {
                    "yes"
                } else {
                    "no"
                }
            ),
            format!("port: {}", self.port),
            format!("uptime_secs: {}", self.context.uptime_secs()),
            format!(
                "requests: {}",
                self.context.request_counter.load(Ordering::SeqCst)
            ),
        ]
    }

    fn start(&self) -> Result<(), String> {
        self.spawn_server()
    }

    fn stop(&self) -> Result<(), String> {
        if !self.running.load(Ordering::SeqCst) {
            return Err("bridge listener is not running".into());
        }
        let sender = self
            .shutdown
            .lock()
            .map_err(|error| error.to_string())?
            .take()
            .ok_or_else(|| "bridge listener is not running".to_string())?;
        let _ = sender.send(());
        Ok(())
    }
}

/// Console authentication provider over the bridge's local user store.
pub struct HostAuthProvider {
    auth: AuthHandle,
}

impl HostAuthProvider {
    /// Builds an authentication provider over an existing auth handle.
    pub fn new(auth: AuthHandle) -> Self {
        Self { auth }
    }
}

impl AuthProvider for HostAuthProvider {
    fn register(&self, username: &str, password: &str) -> Result<String, String> {
        self.auth
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .register(username, password)
            .map_err(|error| error.to_string())
    }

    fn login(&self, username: &str, password: &str) -> Result<String, String> {
        self.auth
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .login(username, password)
            .map_err(|error| error.to_string())
    }

    fn me(&self, token: &str) -> Result<String, String> {
        let header = if token.trim_start().starts_with("Bearer ") {
            token.trim().to_string()
        } else {
            format!("Bearer {token}")
        };
        self.auth
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .bearer_user(&header)
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_block_mgr::registry::BlockRegistry;
    use aios_bridge::auth::open_auth;
    use aios_process_mgr::scheduler::Scheduler;
    use aios_security::access_control::AccessControlLayer;
    use aios_watchdog::watchdog::WatchdogConfig;
    use std::time::Duration;

    fn watchdog_handle() -> Arc<Mutex<Watchdog>> {
        Arc::new(Mutex::new(Watchdog::new(WatchdogConfig {
            heartbeat_interval_ms: 200,
            max_missed_heartbeats: 3,
            warn_threshold: 2,
            recovery_timeout_ms: 1_000,
            secret: b"console_control_test".to_vec(),
        })))
    }

    fn bridge_runtime(port: u16) -> BridgeRuntime {
        let scheduler = Arc::new(Mutex::new(Scheduler::new(64)));
        let registry = BlockRegistry::new();
        let context = Arc::new(BridgeContext::new(
            scheduler,
            registry,
            AccessControlLayer::new(b"console_bridge_test".to_vec(), 86_400_000),
            Watchdog::new(WatchdogConfig::default()),
            42,
        ));
        BridgeRuntime::new(
            context,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Vec::new())),
            port,
        )
    }

    #[test]
    fn watchdog_control_drives_the_shared_instance() {
        let handle = watchdog_handle();
        let control = HostWatchdogControl::new(handle.clone());

        let status = control.status();
        assert!(status
            .iter()
            .any(|line| line == "monitoring thread: stopped"));
        assert!(status.iter().any(|line| line == "state: Monitoring"));

        control.start().expect("start watchdog");
        assert!(control.start().is_err(), "second start must fail");
        let status = control.status();
        assert!(status
            .iter()
            .any(|line| line == "monitoring thread: running"));

        control.force_safe_mode();
        assert_eq!(
            handle
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .state(),
            aios_watchdog::watchdog::WatchdogState::SafeMode
        );
        assert!(control
            .status()
            .iter()
            .any(|line| line == "state: SafeMode"));
        let actions = control.escalate();
        assert!(actions
            .iter()
            .any(|action| action.contains("SafeModeShell")));
        assert!(actions.iter().any(|action| action.contains("DumpState")));

        control.reset().expect("reset watchdog");
        assert_eq!(
            handle
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .state(),
            aios_watchdog::watchdog::WatchdogState::Monitoring
        );
        assert_eq!(
            handle
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .missed_count(),
            0
        );

        control.stop().expect("stop watchdog");
        assert!(control.stop().is_err(), "second stop must fail");
        assert!(control
            .status()
            .iter()
            .any(|line| line == "monitoring thread: stopped"));
    }

    #[test]
    fn watchdog_control_resets_without_a_monitoring_thread() {
        let handle = watchdog_handle();
        let control = HostWatchdogControl::new(handle.clone());

        control.force_safe_mode();
        control.reset().expect("reset watchdog without runner");

        assert_eq!(
            handle
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .state(),
            aios_watchdog::watchdog::WatchdogState::Monitoring
        );
    }

    #[test]
    fn auth_provider_uses_the_real_bridge_user_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = HostAuthProvider::new(open_auth(dir.path()).expect("auth handle"));

        let token = provider
            .register("alice", "secret-password-123")
            .expect("register alice");
        assert!(!token.is_empty());
        assert!(provider.register("alice", "secret-password-123").is_err());

        let login = provider
            .login("alice", "secret-password-123")
            .expect("login alice");
        assert!(!login.is_empty());
        assert!(provider.login("alice", "wrong-password-123").is_err());

        assert_eq!(provider.me(&token).expect("me by token"), "alice");
        assert_eq!(
            provider
                .me(&format!("Bearer {login}"))
                .expect("me by bearer header"),
            "alice"
        );
        assert!(provider.me("not.a.token").is_err());
    }

    #[tokio::test]
    async fn bridge_runtime_stops_and_restarts_the_real_listener() {
        let runtime = bridge_runtime(0);
        let status = runtime.status();
        assert!(status.iter().any(|line| line == "running: no"));
        assert!(status.iter().any(|line| line == "port: 0"));
        assert!(runtime.stop().is_err(), "stop before start must fail");

        runtime.start().expect("start bridge");
        assert!(runtime.start().is_err(), "second start must fail");
        assert!(runtime.status().iter().any(|line| line == "running: yes"));

        runtime.stop().expect("request graceful shutdown");
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while runtime.status().iter().any(|line| line == "running: yes") {
            assert!(std::time::Instant::now() < deadline, "bridge did not stop");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        runtime.start().expect("restart bridge");
        assert!(runtime.status().iter().any(|line| line == "running: yes"));
        runtime.stop().expect("stop restarted bridge");
    }
}
