//! Live kernel runtime backing the GUI dashboard.
//!
//! Creates the real scheduler, block registry, watchdog, shared IPC bus and
//! live-update engine; when [`GuiRuntime::start`] is called it additionally
//! launches a scheduling tick thread, a watchdog heartbeat generator and a
//! handful of real OS-thread "kernel" processes that keep the IPC bus busy,
//! so the dashboard shows genuine metrics instead of placeholder data.

use aios_block_mgr::hot_reload::HotReloader;
use aios_block_mgr::loader::BlockLoader;
use aios_block_mgr::marketplace::BlockMarketplace;
use aios_block_mgr::registry::BlockRegistry;
use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};
use aios_ipc::bus::IpcBus;
use aios_live_update::engine::LiveUpdateEngine;
use aios_process_mgr::scheduler::{Scheduler, SuspendFlag, TerminateFlag};
use aios_process_mgr::task::Priority;
use aios_store::installer::BlockInstaller;
use aios_watchdog::heartbeat::Heartbeat;
use aios_watchdog::watchdog::{Watchdog, WatchdogConfig};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const HEARTBEAT_SECRET: &[u8] = b"aios_gui_secret";
const SCHEDULER_TICK_MS: u64 = 25;

/// A minimal self-contained kernel runtime for the GUI.
pub struct GuiRuntime {
    pub scheduler: Arc<Mutex<Scheduler>>,
    pub registry: Arc<Mutex<BlockRegistry>>,
    pub watchdog: Arc<Mutex<Watchdog>>,
    pub ipc_bus: Arc<Mutex<IpcBus>>,
    pub live_update: Arc<Mutex<LiveUpdateEngine>>,
    pub installer: BlockInstaller,
    pub hot_reloader: Arc<Mutex<HotReloader>>,
    pub marketplace: Arc<Mutex<BlockMarketplace>>,
    pub blocks_dir: PathBuf,
    /// Total packets drained off the IPC bus so far (live traffic counter).
    ipc_drained: AtomicU64,
    started: bool,
    scheduler_stop: Arc<AtomicBool>,
    heartbeat_stop: Arc<AtomicBool>,
    scheduler_thread: Option<std::thread::JoinHandle<()>>,
    heartbeat_thread: Option<std::thread::JoinHandle<()>>,
}

impl GuiRuntime {
    /// Create the runtime objects without starting any threads. The blocks
    /// directory defaults to `AIOS_BLOCKS_DIR` (or `./blocks`); tests use a
    /// throwaway temp dir so they never touch the repository.
    pub fn new(ram_total_mb: u64) -> Self {
        let blocks_dir = if cfg!(test) {
            std::env::temp_dir().join(format!(
                "aios_gui_test_blocks_{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ))
        } else {
            PathBuf::from(std::env::var("AIOS_BLOCKS_DIR").unwrap_or_else(|_| "blocks".to_string()))
        };
        std::fs::create_dir_all(&blocks_dir).ok();

        let mut registry = BlockRegistry::new();
        let _ = BlockLoader::load_from_binary(
            &mut registry,
            "hal",
            "1.0.0",
            b"hal-native-module".to_vec(),
        );
        let _ =
            BlockLoader::load_from_binary(&mut registry, "ipc_bus", "1.0.0", b"ipc_bus".to_vec());
        let _ = BlockLoader::load_from_binary(
            &mut registry,
            "scheduler",
            "1.0.0",
            b"scheduler".to_vec(),
        );
        let _ = BlockLoader::load_from_binary(
            &mut registry,
            "browser",
            "0.1.0",
            b"browser-native".to_vec(),
        );
        registry.set_block_dependencies("ipc_bus", vec!["hal".into()]);
        registry.set_block_dependencies("scheduler", vec!["ipc_bus".into()]);
        let results = registry.boot_discover(&blocks_dir);
        log::info!(
            "GUI runtime: {}/{} disk blocks loaded from {:?}",
            results.iter().filter(|r| r.is_ok()).count(),
            results.len(),
            blocks_dir
        );

        let scheduler = Scheduler::new(ram_total_mb)
            .with_aging_threshold(5000)
            .with_time_slice(100);
        let watchdog = Watchdog::new(WatchdogConfig {
            secret: HEARTBEAT_SECRET.to_vec(),
            ..Default::default()
        });
        let installer = BlockInstaller::new(blocks_dir.as_path());
        let hot_reloader = HotReloader::with_watch_dir(blocks_dir.as_path());

        let mut marketplace = BlockMarketplace::new();
        marketplace.add_repository("official");
        let offers = [
            (
                "hal",
                "1.0.0",
                "Hardware abstraction layer (native module)",
                "AIOS Core",
            ),
            ("ipc_bus", "1.0.0", "Kernel IPC transport bus", "AIOS Core"),
            (
                "scheduler",
                "1.0.0",
                "Round-robin priority scheduler",
                "AIOS Core",
            ),
            (
                "compression",
                "1.0.0",
                "Streaming compression utilities",
                "AIOS Core",
            ),
        ];
        for (name, version, description, author) in offers {
            let meta = BlockMarketplace::create_metadata(
                name,
                version,
                description,
                author,
                "",
                0,
                vec!["internal".into()],
                "1.0.0",
            );
            let _ = marketplace.publish_block("official", meta);
        }
        log::info!("GUI runtime: seeded {} marketplace offers", offers.len());

        Self {
            scheduler: Arc::new(Mutex::new(scheduler)),
            registry: Arc::new(Mutex::new(registry)),
            watchdog: Arc::new(Mutex::new(watchdog)),
            ipc_bus: Arc::new(Mutex::new(IpcBus::new(4096))),
            live_update: Arc::new(Mutex::new(LiveUpdateEngine::new(10_000))),
            installer,
            hot_reloader: Arc::new(Mutex::new(hot_reloader)),
            marketplace: Arc::new(Mutex::new(marketplace)),
            blocks_dir,
            ipc_drained: AtomicU64::new(0),
            started: false,
            scheduler_stop: Arc::new(AtomicBool::new(false)),
            heartbeat_stop: Arc::new(AtomicBool::new(false)),
            scheduler_thread: None,
            heartbeat_thread: None,
        }
    }

    pub fn is_started(&self) -> bool {
        self.started
    }

    /// Spawn the background threads (scheduler tick, watchdog heartbeats,
    /// real IPC-generating kernel processes). Idempotent.
    pub fn start(&mut self) {
        if self.started {
            return;
        }
        self.started = true;

        {
            let mut scheduler = self.scheduler.lock().unwrap();
            for (name, priority, ram, target, interval) in [
                ("ai_orchestrator", Priority::High, 512u64, 42u32, 25u64),
                ("io_handler", Priority::Normal, 128, 42, 60),
                ("health_monitor", Priority::Low, 64, 42, 120),
                ("telemetry_agg", Priority::Normal, 96, 42, 40),
            ] {
                let bus = self.ipc_bus.clone();
                let _ = scheduler.spawn_real_process(name, priority, ram, move |term, susp| {
                    ipc_generator(bus, target, interval, term, susp);
                });
            }
        }

        let tick = self.scheduler.clone();
        let scheduler_stop = self.scheduler_stop.clone();
        self.scheduler_thread = Some(std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(SCHEDULER_TICK_MS));
            if scheduler_stop.load(Ordering::Relaxed) {
                break;
            }
            let Ok(mut scheduler) = tick.lock() else {
                break;
            };
            scheduler.tick();
        }));

        let watchdog = self.watchdog.clone();
        let heartbeat_stop = self.heartbeat_stop.clone();
        self.heartbeat_thread = Some(std::thread::spawn(move || {
            let mut seq: u64 = 1;
            loop {
                std::thread::sleep(Duration::from_millis(1000));
                if heartbeat_stop.load(Ordering::Relaxed) {
                    break;
                }
                let mut wd = match watchdog.lock() {
                    Ok(wd) => wd,
                    Err(_) => break,
                };
                let heartbeat = Heartbeat::new(seq, HEARTBEAT_SECRET);
                if heartbeat.verify(HEARTBEAT_SECRET) {
                    let _ = wd.receive_heartbeat(&heartbeat);
                }
                seq += 1;
                let _ = wd.check_timeout();
            }
        }));
    }

    /// Drain the IPC bus and return the number of packets received since the
    /// last call. Idempotent and cheap even when no generators are running.
    pub fn drain_ipc(&self) -> u64 {
        let mut bus = match self.ipc_bus.lock() {
            Ok(bus) => bus,
            Err(_) => return 0,
        };
        let mut drained = 0u64;
        while bus.receive().is_some() {
            drained += 1;
        }
        if drained > 0 {
            self.ipc_drained.fetch_add(drained, Ordering::Relaxed);
        }
        drained
    }

    /// Total packets consumed since the runtime was created.
    pub fn ipc_drained_total(&self) -> u64 {
        self.ipc_drained.load(Ordering::Relaxed)
    }

    /// Re-scan the blocks directory and hot-reload any changed binaries, so
    /// editing a `<name>_<version>.wasm` on disk takes effect live.
    pub fn poll_hot_reload(&mut self) {
        let mut reloader = match self.hot_reloader.lock() {
            Ok(r) => r,
            Err(_) => return,
        };
        let mut registry = match self.registry.lock() {
            Ok(r) => r,
            Err(_) => return,
        };
        let events = reloader.scan_and_reload(&mut registry);
        for event in events {
            log::info!("GUI hot-reload: {event:?}");
        }
    }
}

impl Drop for GuiRuntime {
    fn drop(&mut self) {
        self.scheduler_stop.store(true, Ordering::Relaxed);
        self.heartbeat_stop.store(true, Ordering::Relaxed);

        if let Ok(mut scheduler) = self.scheduler.lock() {
            let kids: Vec<_> = scheduler.all_processes().iter().map(|p| p.pid).collect();
            for pid in kids {
                let _ = scheduler.kill_process(pid);
            }
        }

        while let Some(handle) = self.scheduler_thread.take() {
            let _ = handle.join();
        }
        while let Some(handle) = self.heartbeat_thread.take() {
            let _ = handle.join();
        }
    }
}

/// Body of a real OS-thread "kernel" process: pushes a telemetry packet onto
/// the shared IPC bus on every tick until suspended or terminated.
fn ipc_generator(
    bus: Arc<Mutex<IpcBus>>,
    target: u32,
    interval_ms: u64,
    term: TerminateFlag,
    susp: SuspendFlag,
) {
    loop {
        if term.should_stop() {
            break;
        }
        if susp.is_suspended() {
            std::thread::park();
            continue;
        }
        let packet = IpcPacket::new(
            0,
            target,
            CommandId::Custom,
            Payload::Text("telemetry".into()),
        );
        if let Ok(mut b) = bus.lock() {
            let _ = b.send(packet);
        }
        std::thread::sleep(Duration::from_millis(interval_ms));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_creation_does_not_start_threads() {
        let rt = GuiRuntime::new(4096);
        assert!(!rt.is_started());
        assert!(rt.scheduler_thread.is_none());
        assert!(rt.heartbeat_thread.is_none());
    }

    #[test]
    fn test_runtime_start_is_idempotent() {
        let mut rt = GuiRuntime::new(4096);
        rt.start();
        rt.start();
        assert!(rt.is_started());
        let (ram_used, ram_total) = rt.scheduler.lock().unwrap().ram_usage();
        assert_eq!(ram_total, 4096);
        assert!(ram_used > 0, "seeded kernel processes must consume RAM");
        assert!(rt.scheduler.lock().unwrap().process_count() >= 4);
    }

    #[test]
    fn test_runtime_start_and_kill_process() {
        let mut rt = GuiRuntime::new(4096);
        rt.start();
        std::thread::sleep(Duration::from_millis(150));
        let pid = {
            let s = rt.scheduler.lock().unwrap();
            s.all_processes()[0].pid
        };
        let killed = rt.scheduler.lock().unwrap().kill_process(pid);
        assert!(
            killed.is_ok(),
            "killing a real kernel process must join its thread"
        );
    }

    #[test]
    fn test_drain_ipc_accumulates() {
        let mut rt = GuiRuntime::new(4096);
        rt.start();
        std::thread::sleep(Duration::from_millis(200));
        let drained = rt.drain_ipc();
        assert!(drained > 0, "live kernel processes should populate the bus");
        assert_eq!(rt.ipc_drained_total(), drained);
    }

    #[test]
    fn test_hot_reload_noop_on_empty_dir() {
        let mut rt = GuiRuntime::new(4096);
        rt.poll_hot_reload();
        assert!(!rt.registry.lock().unwrap().all_ids().is_empty());
    }
}
