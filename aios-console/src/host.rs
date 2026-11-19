//! Host implementation of [`CommandBackend`].
//!
//! The backend delegates to the same library crates the current host TUI uses,
//! so a command has one canonical name and one real implementation. An
//! unimplemented subcommand is reported as unavailable instead of being silently
//! accepted.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use aios_block_mgr::loader::BlockLoader;
use aios_block_mgr::registry::BlockRegistry;
use aios_context::telemetry::{TelemetryEntry, TelemetryStore};
use aios_core::block::{BlockId, BlockState};
use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};
use aios_hal::ai_tier::AiTier;
use aios_hal::hardware::HardwareProfile;
use aios_ipc::bus::IpcBus;
use aios_net_config::block::NetSettingsBlock;
use aios_net_config::config::NetworkConfig;
use aios_process_mgr::scheduler::Scheduler;
use aios_process_mgr::task::{Priority, ProcessId, ProcessState};
use aios_store::manager::StoreManager;
use aios_vfs::vfs::{AiosVfs, VfsPath, VirtualFileSystem};

use crate::backend::{dispatch, CommandBackend};
use crate::capability::{BackendKind, CapabilitySet};
use crate::output::{CommandOutput, ExitCode, OutputControl};
use crate::registry::{help, Invocation};

/// Block id used by the host network settings block.
const NET_BLOCK_ID: u32 = 7;
/// Maximum number of remembered input lines.
const HISTORY_LIMIT: usize = 200;
/// Maximum nesting depth for `source` scripts, preventing runaway recursion.
const MAX_SOURCE_DEPTH: usize = 8;

type MockProfileFactory = fn() -> HardwareProfile;

/// Point-in-time view of counters used by `metrics` and `health`.
struct MetricsSnapshot {
    uptime_secs: u64,
    process_total: usize,
    process_running: usize,
    process_ready: usize,
    process_suspended: usize,
    process_crashed: usize,
    process_terminated: usize,
    crash_events: usize,
    ram_used_mb: u64,
    ram_total_mb: u64,
    ram_used_percent: u64,
    block_total: usize,
    block_loaded: usize,
    block_active: usize,
    block_frozen: usize,
    block_error: usize,
    block_unloaded: usize,
    restarts: u32,
    max_restarts: u32,
    log_entries: usize,
    unhealthy: Vec<String>,
    degraded: Vec<String>,
}

/// Construction parameters for [`HostBackend`].
#[derive(Debug, Clone)]
pub struct HostBackendConfig {
    /// Version string printed by `version`.
    pub version: String,
    /// Directory holding store block binaries.
    pub blocks_dir: PathBuf,
    /// Root of the sandboxed `AIOS://` filesystem.
    pub vfs_root: PathBuf,
    /// File used to persist `net set` / `net reset` changes.
    pub net_config_path: PathBuf,
    /// RAM quota handed to a newly created scheduler.
    pub total_ram_mb: u64,
    /// Maximum number of controlled watchdog restarts.
    pub max_restarts: u32,
}

impl Default for HostBackendConfig {
    fn default() -> Self {
        let base = std::env::temp_dir().join("aios-console");
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            blocks_dir: base.join("blocks"),
            vfs_root: base.join("vfs"),
            net_config_path: base.join("network.json"),
            total_ram_mb: 1024,
            max_restarts: 3,
        }
    }
}

/// Text console backend for a host process.
pub struct HostBackend {
    /// Shared scheduler so the host orchestrator and console see the same processes.
    pub scheduler: Arc<Mutex<Scheduler>>,
    /// Shared block registry so the host orchestrator and console see the same blocks.
    pub registry: Arc<Mutex<BlockRegistry>>,
    config: HostBackendConfig,
    store: StoreManager,
    net: NetSettingsBlock,
    telemetry: TelemetryStore,
    ipc_bus: Arc<Mutex<IpcBus>>,
    frozen_packets: Vec<IpcPacket>,
    hardware_profile: Option<(String, HardwareProfile)>,
    capabilities: CapabilitySet,
    history: Vec<String>,
    log: Vec<String>,
    restarts: u32,
    source_depth: usize,
    started: Instant,
}

impl HostBackend {
    /// Creates a backend with its own scheduler and registry.
    pub fn new() -> Self {
        Self::with_config(HostBackendConfig::default())
    }

    /// Creates a backend with its own scheduler and registry from explicit config.
    pub fn with_config(config: HostBackendConfig) -> Self {
        let scheduler = Arc::new(Mutex::new(Scheduler::new(config.total_ram_mb)));
        let registry = Arc::new(Mutex::new(BlockRegistry::new()));
        Self::from_handles(scheduler, registry, config)
    }

    /// Creates a backend sharing the scheduler and registry of the host orchestrator.
    pub fn from_handles(
        scheduler: Arc<Mutex<Scheduler>>,
        registry: Arc<Mutex<BlockRegistry>>,
        config: HostBackendConfig,
    ) -> Self {
        let store = StoreManager::new(config.blocks_dir.clone());
        let net = NetSettingsBlock::new(
            BlockId::new(NET_BLOCK_ID),
            NetworkConfig::default(),
            config.net_config_path.clone(),
        );
        Self {
            scheduler,
            registry,
            config,
            store,
            net,
            telemetry: TelemetryStore::new(),
            ipc_bus: Arc::new(Mutex::new(IpcBus::new(256))),
            frozen_packets: Vec::new(),
            hardware_profile: None,
            capabilities: CapabilitySet::all(),
            history: Vec::new(),
            log: Vec::new(),
            restarts: 0,
            source_depth: 0,
            started: Instant::now(),
        }
    }

    /// Replaces the capability set of this session.
    pub fn set_capabilities(&mut self, capabilities: CapabilitySet) {
        self.capabilities = capabilities;
    }

    /// Shares the host IPC bus so `ipc` commands observe real bus traffic.
    pub fn set_ipc_bus(&mut self, bus: Arc<Mutex<IpcBus>>) {
        self.ipc_bus = bus;
    }

    /// Overrides the hardware inventory used by `sys` and `hw` commands.
    pub fn set_hardware_profile(&mut self, profile: HardwareProfile) {
        self.hardware_profile = Some(("injected".to_string(), profile));
    }

    /// Returns the controlled restart counter.
    pub fn restarts(&self) -> u32 {
        self.restarts
    }

    /// Returns the recorded console log entries.
    pub fn log_entries(&self) -> &[String] {
        &self.log
    }

    fn note(&mut self, message: impl Into<String>) {
        self.log.push(message.into());
    }

    fn open_vfs(&self) -> Result<AiosVfs, CommandOutput> {
        AiosVfs::new(self.config.vfs_root.clone())
            .map_err(|error| CommandOutput::error(&format!("vfs init: {error}")))
    }

    fn path_arg(invocation: &Invocation, index: usize) -> Result<VfsPath, CommandOutput> {
        let raw = invocation
            .arg(index)
            .ok_or_else(|| CommandOutput::usage(invocation.spec.usage))?;
        VfsPath::parse(raw)
            .map_err(|error| CommandOutput::error(&format!("invalid path '{raw}': {error}")))
    }

    fn command_status(&mut self, message: impl Into<String>) -> CommandOutput {
        let message = message.into();
        self.note(message.clone());
        CommandOutput::text(message)
    }
}

impl Default for HostBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandBackend for HostBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Host
    }

    fn capabilities(&self) -> &CapabilitySet {
        &self.capabilities
    }

    fn name(&self) -> &'static str {
        "host"
    }

    fn execute(&mut self, invocation: &Invocation) -> CommandOutput {
        if !invocation.raw.trim().is_empty() {
            self.history.push(invocation.raw.clone());
            if self.history.len() > HISTORY_LIMIT {
                self.history.remove(0);
            }
        }

        match (invocation.spec.id, invocation.subcommand) {
            ("help", _) => CommandOutput::ok(help(invocation.first_arg())),
            ("version", _) => {
                CommandOutput::text(format!("aios-console host {}", self.config.version))
            }
            ("clear", _) => CommandOutput::clear(),
            ("exit", _) => {
                let code = invocation
                    .first_arg()
                    .and_then(|value| value.parse::<u8>().ok())
                    .unwrap_or(0);
                CommandOutput::exit(code)
            }
            ("echo", _) => CommandOutput::text(invocation.args.join(" ")),
            ("history", _) => {
                let count = invocation
                    .first_arg()
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(self.history.len());
                let start = self.history.len().saturating_sub(count);
                CommandOutput::ok(
                    self.history[start..]
                        .iter()
                        .enumerate()
                        .map(|(index, line)| format!("{:>4}  {}", start + index + 1, line))
                        .collect(),
                )
            }
            ("log", Some("show")) => {
                if self.log.is_empty() {
                    CommandOutput::text("no log entries")
                } else {
                    CommandOutput::ok(self.log.clone())
                }
            }
            ("log", Some("clear")) => {
                self.log.clear();
                CommandOutput::text("log cleared")
            }
            ("status", _) => self.status(),
            ("sys", Some("info")) => self.sys_info(),
            ("sys", Some("tier")) => self.sys_tier(),
            ("uptime", _) => CommandOutput::text(format!("{}s", self.started.elapsed().as_secs())),
            ("metrics", _) => self.metrics(invocation),
            ("health", _) => self.health(),
            ("ps", _) => self.process_list(),
            ("process", Some("spawn")) => self.process_spawn(invocation),
            ("process", Some("kill")) => self.process_kill(invocation),
            ("process", Some("renice")) => self.process_renice(invocation),
            ("process", Some("suspend")) => self.process_signal(invocation, true),
            ("process", Some("resume")) => self.process_signal(invocation, false),
            ("process", Some("affinity")) => self.process_affinity(invocation),
            ("block", Some("list")) => self.block_list(),
            ("block", Some("info")) => self.block_info(invocation),
            ("block", Some("load")) => self.block_load(invocation),
            ("block", Some("mock")) => self.block_mock(invocation),
            ("block", Some("unload")) => self.block_unload(invocation),
            ("block", Some("activate")) => self.block_activate(invocation),
            ("block", Some("deps")) => self.block_deps(),
            ("block", Some("verify")) => self.block_verify(invocation),
            ("wd", Some("status")) => CommandOutput::ok(vec![
                format!(
                    "safe mode restarts: {}/{}",
                    self.restarts, self.config.max_restarts
                ),
                format!("log entries: {}", self.log.len()),
            ]),
            ("wd", Some("reset")) => {
                if self.restarts >= self.config.max_restarts {
                    return CommandOutput::error(&format!(
                        "max restarts ({}) reached; manual intervention required",
                        self.config.max_restarts
                    ));
                }
                self.restarts += 1;
                self.command_status(format!(
                    "orchestrator restart {} of {} initiated",
                    self.restarts, self.config.max_restarts
                ))
            }
            ("net", Some("show")) => CommandOutput::text(self.net.config().to_json()),
            ("net", Some("set")) => self.net_set(invocation),
            ("net", Some("reset")) => match self.net.reset() {
                Ok(()) => self.command_status("network configuration reset to defaults"),
                Err(error) => CommandOutput::error(&format!("net reset failed: {error}")),
            },
            ("store", Some(subcommand)) => self.store_command(invocation, subcommand),
            ("vfs", Some(subcommand)) => self.vfs_command(invocation, subcommand),
            ("ipc", Some(subcommand)) => self.ipc_command(invocation, subcommand),
            ("hw", Some("detect")) => self.hw_detect(),
            ("hw", Some("rescan")) => self.hw_rescan(),
            ("hw", Some("profile")) => self.hw_profile(invocation),
            ("telemetry", Some(subcommand)) => self.telemetry_command(invocation, subcommand),
            ("source", _) => self.source(invocation),
            _ => CommandOutput::unavailable(&format!(
                "'{}' has no host backend yet",
                invocation.display_name()
            )),
        }
    }
}

impl HostBackend {
    fn source(&mut self, invocation: &Invocation) -> CommandOutput {
        let Some(path) = invocation.first_arg() else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        if self.source_depth >= MAX_SOURCE_DEPTH {
            return CommandOutput::error(&format!(
                "source nesting limit ({MAX_SOURCE_DEPTH}) reached at '{path}'"
            ));
        }
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => return CommandOutput::error(&format!("read '{path}': {error}")),
        };

        self.source_depth += 1;
        let mut lines = Vec::new();
        let mut code = ExitCode::Ok;
        let mut control = OutputControl::None;
        let mut count = 0usize;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let output = dispatch(self, line);
            count += 1;
            lines.extend(output.lines);
            if code == ExitCode::Ok {
                code = output.code;
            }
            if output.control == OutputControl::Clear && control == OutputControl::None {
                control = OutputControl::Clear;
            }
            if let OutputControl::Exit(exit_code) = output.control {
                control = OutputControl::Exit(exit_code);
                break;
            }
        }
        self.source_depth -= 1;

        let status = format!("sourced '{path}' ({count} command(s))");
        self.note(status.clone());
        lines.push(status);
        CommandOutput {
            lines,
            code,
            duration_ms: 0,
            control,
        }
    }

    fn status(&self) -> CommandOutput {
        let scheduler = guard(&self.scheduler);
        let registry = guard(&self.registry);
        let (used, total) = scheduler.ram_usage();
        CommandOutput::ok(vec![
            "status        host console".to_string(),
            format!("uptime        {}s", self.started.elapsed().as_secs()),
            format!(
                "processes     {} ({} running, {} ready)",
                scheduler.process_count(),
                scheduler.running_count(),
                scheduler.ready_count()
            ),
            format!("blocks        {}", registry.count()),
            format!("memory        {}/{} MB", used, total),
            format!(
                "restarts      {}/{}",
                self.restarts, self.config.max_restarts
            ),
            format!("log           {} entries", self.log.len()),
        ])
    }

    fn hardware(&mut self) -> (String, HardwareProfile) {
        if self.hardware_profile.is_none() {
            self.hardware_profile = Some(("detected".to_string(), HardwareProfile::detect()));
        }
        self.hardware_profile
            .clone()
            .expect("hardware profile initialized")
    }

    fn hardware_lines(header: &str, source: &str, profile: &HardwareProfile) -> Vec<String> {
        let cpu = &profile.cpu;
        let mut lines = vec![
            header.to_string(),
            format!("source         {source}"),
            format!("cpu            {} ({:?})", cpu.model, cpu.vendor),
            format!(
                "topology       {} core(s), {} thread(s)",
                cpu.cores, cpu.threads
            ),
            format!("clock          {} MHz", cpu.base_freq_mhz),
            format!(
                "simd           avx512={} avx2={} sse42={} neon={}",
                cpu.has_avx512, cpu.has_avx2, cpu.has_sse42, cpu.has_neon
            ),
            format!(
                "memory         {} MB total, {} MB available",
                profile.memory.total_mb, profile.memory.available_mb
            ),
            format!(
                "dimm           {} x {} MHz",
                profile.memory.dimm_count, profile.memory.speed_mhz
            ),
        ];
        match &profile.gpu {
            Some(gpu) => {
                let capability = if gpu.compute_capability.is_empty() {
                    String::new()
                } else {
                    format!(", {}", gpu.compute_capability)
                };
                lines.push(format!(
                    "gpu            {} ({}, {} MB VRAM{})",
                    gpu.name, gpu.vendor, gpu.vram_mb, capability
                ));
            }
            None => lines.push("gpu            none".to_string()),
        }
        match &profile.npu {
            Some(npu) => {
                let frameworks = if npu.supported_frameworks.is_empty() {
                    "none".to_string()
                } else {
                    npu.supported_frameworks.join(", ")
                };
                lines.push(format!(
                    "npu            {} ({} TOPS, frameworks: {})",
                    npu.name, npu.tops, frameworks
                ));
            }
            None => lines.push("npu            none".to_string()),
        }
        lines.push(format!(
            "pci            {} device(s)",
            profile.pci_devices.len()
        ));
        lines.push(format!(
            "storage        {} device(s)",
            profile.storage_devices.len()
        ));
        lines.push(format!(
            "usb            {} device(s)",
            profile.usb_devices.len()
        ));
        lines.push(format!(
            "thunderbolt    {} device(s)",
            profile.thunderbolt_devices.len()
        ));
        lines.push(format!("tier           {}", AiTier::from_profile(profile)));
        lines
    }

    fn sys_info(&mut self) -> CommandOutput {
        let (source, profile) = self.hardware();
        CommandOutput::ok(Self::hardware_lines(
            "sys            hardware inventory",
            &source,
            &profile,
        ))
    }

    fn sys_tier(&mut self) -> CommandOutput {
        let (source, profile) = self.hardware();
        let tier = AiTier::from_profile(&profile);
        CommandOutput::ok(vec![
            format!("tier           {tier}"),
            format!("description    {}", tier.description()),
            format!("max model      {:.1} GB", tier.max_model_size_gb()),
            format!("batch          {}", tier.recommended_batch_size()),
            format!("source         {source}"),
        ])
    }

    fn hw_detect(&mut self) -> CommandOutput {
        let (source, profile) = self.hardware();
        CommandOutput::ok(Self::hardware_lines(
            "hw             hardware detection",
            &source,
            &profile,
        ))
    }

    fn hw_rescan(&mut self) -> CommandOutput {
        self.hardware_profile = None;
        let (source, profile) = self.hardware();
        let mut lines = Self::hardware_lines("hw             hardware rescan", &source, &profile);
        self.note("hardware rescan completed");
        lines.push("rescan         complete".to_string());
        CommandOutput::ok(lines)
    }

    fn hw_profile(&mut self, invocation: &Invocation) -> CommandOutput {
        const PROFILES: [(&str, MockProfileFactory); 6] = [
            ("modern", HardwareProfile::mock_modern),
            ("legacy", HardwareProfile::mock_legacy),
            ("legacy-2012", HardwareProfile::mock_legacy_2012),
            ("nvidia", HardwareProfile::mock_nvidia),
            ("intel-meteor-lake", HardwareProfile::mock_intel_meteor_lake),
            ("qualcomm-x-elite", HardwareProfile::mock_qualcomm_x_elite),
        ];
        let Some(requested) = invocation.arg(0) else {
            let (source, profile) = self.hardware();
            return CommandOutput::ok(Self::hardware_lines(
                "hw             hardware profile",
                &source,
                &profile,
            ));
        };
        let name = requested.to_ascii_lowercase();
        let Some(factory) = PROFILES
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, factory)| *factory)
        else {
            let names = PROFILES
                .iter()
                .map(|(candidate, _)| *candidate)
                .collect::<Vec<_>>()
                .join("|");
            return CommandOutput::usage(&format!("hw profile [{names}]"));
        };
        let profile = factory();
        self.hardware_profile = Some((name.clone(), profile.clone()));
        let mut lines = Self::hardware_lines("hw             hardware profile", &name, &profile);
        self.note(format!("hardware profile set to {name}"));
        lines.push(format!("profile        {name}"));
        CommandOutput::ok(lines)
    }

    fn snapshot(&self) -> MetricsSnapshot {
        let scheduler = guard(&self.scheduler);
        let registry = guard(&self.registry);
        let (ram_used_mb, ram_total_mb) = scheduler.ram_usage();
        let ram_used_percent = ram_used_mb
            .saturating_mul(100)
            .checked_div(ram_total_mb)
            .unwrap_or(0);

        let mut process_running = 0;
        let mut process_ready = 0;
        let mut process_suspended = 0;
        let mut process_crashed = 0;
        let mut process_terminated = 0;
        let mut unhealthy = Vec::new();
        let mut degraded = Vec::new();
        for process in scheduler.all_processes() {
            match process.state {
                ProcessState::Running => process_running += 1,
                ProcessState::Ready => process_ready += 1,
                ProcessState::Suspended => {
                    process_suspended += 1;
                    degraded.push(format!(
                        "process '{}' (PID {}) is Suspended",
                        process.name, process.pid.0
                    ));
                }
                ProcessState::Crashed => {
                    process_crashed += 1;
                    unhealthy.push(format!(
                        "process '{}' (PID {}) is Crashed",
                        process.name, process.pid.0
                    ));
                }
                ProcessState::Terminated => process_terminated += 1,
            }
        }
        let crash_events = scheduler.crash_log().len();
        if crash_events > 0 {
            degraded.push(format!("{crash_events} crash event(s) recorded"));
        }

        let mut block_loaded = 0;
        let mut block_active = 0;
        let mut block_frozen = 0;
        let mut block_error = 0;
        let mut block_unloaded = 0;
        for (manifest, state) in registry.topology_with_state() {
            match state {
                BlockState::Loaded => block_loaded += 1,
                BlockState::Active => block_active += 1,
                BlockState::Frozen => {
                    block_frozen += 1;
                    degraded.push(format!("block '{}' is Frozen", manifest.name));
                }
                BlockState::Error => {
                    block_error += 1;
                    unhealthy.push(format!("block '{}' is in Error state", manifest.name));
                }
                BlockState::Unloaded => block_unloaded += 1,
            }
        }

        if ram_used_percent > 85 {
            degraded.push(format!("RAM usage {ram_used_percent}% exceeds 85%"));
        }
        if self.restarts > 0 && self.restarts >= self.config.max_restarts {
            unhealthy.push(format!(
                "restart limit ({}) reached",
                self.config.max_restarts
            ));
        } else if self.restarts > 0 {
            degraded.push(format!("{} watchdog restart(s)", self.restarts));
        }

        MetricsSnapshot {
            uptime_secs: self.started.elapsed().as_secs(),
            process_total: scheduler.process_count(),
            process_running,
            process_ready,
            process_suspended,
            process_crashed,
            process_terminated,
            crash_events,
            ram_used_mb,
            ram_total_mb,
            ram_used_percent,
            block_total: registry.count(),
            block_loaded,
            block_active,
            block_frozen,
            block_error,
            block_unloaded,
            restarts: self.restarts,
            max_restarts: self.config.max_restarts,
            log_entries: self.log.len(),
            unhealthy,
            degraded,
        }
    }

    fn metrics(&self, invocation: &Invocation) -> CommandOutput {
        let snapshot = self.snapshot();
        if invocation.has_flag("json") {
            Self::metrics_json(&snapshot)
        } else if invocation.has_flag("prometheus") {
            Self::metrics_prometheus(&snapshot)
        } else {
            CommandOutput::ok(Self::metrics_lines(&snapshot))
        }
    }

    fn metrics_lines(s: &MetricsSnapshot) -> Vec<String> {
        vec![
            "metrics        host console".to_string(),
            format!("uptime          {}s", s.uptime_secs),
            format!(
                "processes       {} ({} running, {} ready, {} suspended, {} crashed, {} terminated)",
                s.process_total,
                s.process_running,
                s.process_ready,
                s.process_suspended,
                s.process_crashed,
                s.process_terminated
            ),
            format!("crash events    {}", s.crash_events),
            format!(
                "ram             {}/{} MB ({}%)",
                s.ram_used_mb, s.ram_total_mb, s.ram_used_percent
            ),
            format!(
                "blocks          {} ({} loaded, {} active, {} frozen, {} error, {} unloaded)",
                s.block_total,
                s.block_loaded,
                s.block_active,
                s.block_frozen,
                s.block_error,
                s.block_unloaded
            ),
            format!("restarts        {}/{}", s.restarts, s.max_restarts),
            format!("log             {} entries", s.log_entries),
        ]
    }

    fn metrics_json(s: &MetricsSnapshot) -> CommandOutput {
        let value = serde_json::json!({
            "uptime_secs": s.uptime_secs,
            "processes": {
                "total": s.process_total,
                "running": s.process_running,
                "ready": s.process_ready,
                "suspended": s.process_suspended,
                "crashed": s.process_crashed,
                "terminated": s.process_terminated,
            },
            "crash_events": s.crash_events,
            "ram": {
                "used_mb": s.ram_used_mb,
                "total_mb": s.ram_total_mb,
                "used_percent": s.ram_used_percent,
            },
            "blocks": {
                "total": s.block_total,
                "loaded": s.block_loaded,
                "active": s.block_active,
                "frozen": s.block_frozen,
                "error": s.block_error,
                "unloaded": s.block_unloaded,
            },
            "restarts": s.restarts,
            "max_restarts": s.max_restarts,
            "log_entries": s.log_entries,
        });
        match serde_json::to_string_pretty(&value) {
            Ok(json) => CommandOutput::text(json),
            Err(error) => CommandOutput::error(&format!("metrics json: {error}")),
        }
    }

    fn metrics_prometheus(s: &MetricsSnapshot) -> CommandOutput {
        CommandOutput::ok(vec![
            "# HELP aios_uptime_seconds Host console uptime in seconds".to_string(),
            "# TYPE aios_uptime_seconds gauge".to_string(),
            format!("aios_uptime_seconds {}", s.uptime_secs),
            "# HELP aios_processes Total number of processes".to_string(),
            "# TYPE aios_processes gauge".to_string(),
            format!("aios_processes_total {}", s.process_total),
            format!("aios_processes_running {}", s.process_running),
            format!("aios_processes_ready {}", s.process_ready),
            format!("aios_processes_suspended {}", s.process_suspended),
            format!("aios_processes_crashed {}", s.process_crashed),
            format!("aios_processes_terminated {}", s.process_terminated),
            "# HELP aios_crash_events_total Recorded crash events".to_string(),
            "# TYPE aios_crash_events_total counter".to_string(),
            format!("aios_crash_events_total {}", s.crash_events),
            "# HELP aios_ram_used_mb RAM used in megabytes".to_string(),
            "# TYPE aios_ram_used_mb gauge".to_string(),
            format!("aios_ram_used_mb {}", s.ram_used_mb),
            format!("aios_ram_total_mb {}", s.ram_total_mb),
            "# HELP aios_blocks_total Registered blocks".to_string(),
            "# TYPE aios_blocks_total gauge".to_string(),
            format!("aios_blocks_total {}", s.block_total),
            format!("aios_blocks_active {}", s.block_active),
            format!("aios_blocks_frozen {}", s.block_frozen),
            format!("aios_blocks_error {}", s.block_error),
            "# HELP aios_restarts_total Controlled watchdog restarts".to_string(),
            "# TYPE aios_restarts_total counter".to_string(),
            format!("aios_restarts_total {}", s.restarts),
            format!("aios_max_restarts {}", s.max_restarts),
        ])
    }

    fn health(&self) -> CommandOutput {
        let snapshot = self.snapshot();
        let status = if !snapshot.unhealthy.is_empty() {
            "UNHEALTHY"
        } else if !snapshot.degraded.is_empty() {
            "DEGRADED"
        } else {
            "HEALTHY"
        };
        let mut lines = vec![
            format!("health          {status}"),
            format!("uptime          {}s", snapshot.uptime_secs),
            format!(
                "processes       {} ({} running, {} ready)",
                snapshot.process_total, snapshot.process_running, snapshot.process_ready
            ),
            format!(
                "blocks          {} ({} active)",
                snapshot.block_total, snapshot.block_active
            ),
            format!(
                "ram             {}/{} MB ({}%)",
                snapshot.ram_used_mb, snapshot.ram_total_mb, snapshot.ram_used_percent
            ),
        ];
        for finding in snapshot.unhealthy.iter().chain(snapshot.degraded.iter()) {
            lines.push(format!("  - {finding}"));
        }
        if lines.len() == 5 {
            lines.push("  - no problems detected".to_string());
        }
        CommandOutput::ok(lines)
    }

    fn telemetry_command(&mut self, invocation: &Invocation, subcommand: &str) -> CommandOutput {
        match subcommand {
            "record" => {
                let Some(name) = invocation.first_arg() else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let Some(value) = invocation
                    .arg(1)
                    .and_then(|raw| raw.parse::<f64>().ok())
                    .filter(|value| value.is_finite())
                else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let ram = invocation
                    .flag("ram")
                    .and_then(|value| value)
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or_else(|| guard(&self.scheduler).ram_usage().0);
                let mut entry = TelemetryEntry::new(name, value, ram);
                if let Some(block) = invocation.flag("block").and_then(|value| value) {
                    match block.parse::<u32>() {
                        Ok(id) => entry = entry.with_block(id),
                        Err(_) => return CommandOutput::usage(invocation.spec.usage),
                    }
                }
                if let Some(process) = invocation.flag("process").and_then(|value| value) {
                    entry = entry.with_process(process);
                }
                self.telemetry.record(entry);
                self.command_status(format!("recorded '{name}'={value} (ram={ram}MB)"))
            }
            "query" => {
                let entries: Vec<_> = match invocation.first_arg() {
                    Some(name) => self.telemetry.query_metric(name),
                    None => self.telemetry.entries.iter().collect(),
                };
                if entries.is_empty() {
                    return CommandOutput::text("no telemetry entries");
                }
                let mut lines = vec![Self::telemetry_header()];
                lines.extend(
                    entries
                        .iter()
                        .map(|entry| Self::format_telemetry_entry(entry)),
                );
                CommandOutput::ok(lines)
            }
            "latest" => match self.telemetry.latest() {
                Some(entry) => CommandOutput::ok(vec![
                    Self::telemetry_header(),
                    Self::format_telemetry_entry(entry),
                ]),
                None => CommandOutput::text("no telemetry entries"),
            },
            "avg" => match invocation.first_arg() {
                Some(name) => match self.telemetry.average_value(name) {
                    Some(average) => CommandOutput::text(format!("{name} avg: {average:.2}")),
                    None => CommandOutput::text(format!("no samples for '{name}'")),
                },
                None => {
                    let entries = &self.telemetry.entries;
                    if entries.is_empty() {
                        CommandOutput::text("no telemetry entries")
                    } else {
                        let average = entries.iter().map(|entry| entry.value).sum::<f64>()
                            / entries.len() as f64;
                        CommandOutput::text(format!(
                            "overall avg: {average:.2} over {} sample(s)",
                            entries.len()
                        ))
                    }
                }
            },
            "peak" => {
                let peak_ram = self.telemetry.peak_ram();
                let peak_value = self
                    .telemetry
                    .entries
                    .iter()
                    .max_by(|left, right| {
                        left.value
                            .partial_cmp(&right.value)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|entry| (entry.metric_name.clone(), entry.value));
                let mut lines = vec![format!("peak ram    {peak_ram} MB")];
                match peak_value {
                    Some((name, value)) => {
                        lines.push(format!("peak value  {name} = {value:.2}"));
                    }
                    None => lines.push("peak value  no samples".to_string()),
                }
                CommandOutput::ok(lines)
            }
            "clear" => {
                let count = self.telemetry.count();
                self.telemetry.clear();
                self.command_status(format!("cleared {count} telemetry entries"))
            }
            _ => CommandOutput::unavailable(&format!(
                "telemetry {subcommand} has no host backend yet"
            )),
        }
    }

    fn telemetry_header() -> String {
        "TIME_MS           METRIC           VALUE  RAM_MB  BLOCK  PROCESS".to_string()
    }

    fn format_telemetry_entry(entry: &TelemetryEntry) -> String {
        format!(
            "{:<16}  {:<16} {:>8.2} {:>7}  {:<5}  {}",
            entry.timestamp_ms,
            entry.metric_name,
            entry.value,
            entry.ram_used_mb,
            entry
                .block_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "-".to_string()),
            entry.process_name.as_deref().unwrap_or("-"),
        )
    }

    fn ipc_command(&mut self, invocation: &Invocation, subcommand: &str) -> CommandOutput {
        match subcommand {
            "stats" => {
                let bus = guard(&self.ipc_bus);
                let metrics = bus.metrics();
                CommandOutput::ok(vec![
                    "ipc            host bus".to_string(),
                    format!("queued         {} packet(s)", bus.len()),
                    format!("peak depth     {}", metrics.peak_queue_depth),
                    format!("sent           {}", metrics.total_sent),
                    format!("received       {}", metrics.total_received),
                    format!("dropped        {}", metrics.total_dropped),
                    format!("deduplicated   {}", metrics.total_deduplicated),
                    format!("avg latency    {:.2} us", metrics.avg_send_latency_us()),
                ])
            }
            "flush" => {
                let mut bus = guard(&self.ipc_bus);
                let count = bus.len();
                bus.flush();
                drop(bus);
                self.command_status(format!("IPC queue flushed ({count} packet(s) discarded)"))
            }
            "freeze" => {
                let packets = {
                    let mut bus = guard(&self.ipc_bus);
                    bus.freeze()
                };
                let count = packets.len();
                self.frozen_packets = packets;
                self.command_status(format!("IPC bus frozen ({count} packet(s) held)"))
            }
            "unfreeze" => {
                let packets = std::mem::take(&mut self.frozen_packets);
                let count = packets.len();
                let mut bus = guard(&self.ipc_bus);
                bus.unfreeze(packets);
                drop(bus);
                self.command_status(format!("IPC bus unfrozen ({count} packet(s) restored)"))
            }
            "reroute" => {
                let Some(old) = invocation
                    .arg(0)
                    .and_then(|value| value.parse::<u32>().ok())
                else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let Some(new) = invocation
                    .arg(1)
                    .and_then(|value| value.parse::<u32>().ok())
                else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let mut bus = guard(&self.ipc_bus);
                let count = bus.reroute(old, new);
                drop(bus);
                self.command_status(format!(
                    "rerouted {count} packet(s) from block {old} to {new}"
                ))
            }
            "send" => {
                let Some(source) = invocation
                    .arg(0)
                    .and_then(|value| value.parse::<u32>().ok())
                else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let Some(target) = invocation
                    .arg(1)
                    .and_then(|value| value.parse::<u32>().ok())
                else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let text = invocation.arg(2).unwrap_or("");
                let packet = IpcPacket::new(
                    source,
                    target,
                    CommandId::Custom,
                    Payload::Text(text.to_string()),
                );
                let packet = match invocation
                    .flag("priority")
                    .and_then(|value| value)
                    .and_then(|value| value.parse::<u8>().ok())
                {
                    Some(priority) => packet.with_priority(priority),
                    None => packet,
                };
                let packet_id = packet.header.packet_id;
                let mut bus = guard(&self.ipc_bus);
                match bus.send(packet) {
                    Ok(()) => {
                        drop(bus);
                        self.command_status(format!(
                            "IPC packet {packet_id} queued: {source}->{target}"
                        ))
                    }
                    Err(error) => CommandOutput::error(&format!("ipc send failed: {error}")),
                }
            }
            "peek" => {
                let bus = guard(&self.ipc_bus);
                match bus.peek() {
                    Some(packet) => CommandOutput::ok(vec![
                        format!("packet       {}", packet.header.packet_id),
                        format!(
                            "route        {} -> {}",
                            packet.header.source_block, packet.header.target_block
                        ),
                        format!("command      0x{:04X}", packet.header.command_id),
                        format!("priority     {}", packet.header.priority),
                        format!("payload      {}", Self::payload_kind(&packet.payload)),
                    ]),
                    None => CommandOutput::text("IPC queue empty"),
                }
            }
            _ => CommandOutput::unavailable(&format!("ipc {subcommand} has no host backend yet")),
        }
    }

    fn payload_kind(payload: &Payload) -> String {
        match payload {
            Payload::Text(text) => format!("Text({text})"),
            Payload::Binary(bytes) => format!("Binary({} bytes)", bytes.len()),
            Payload::Empty => "Empty".to_string(),
            other => {
                let debug = format!("{other:?}");
                debug
                    .split(['(', '{'])
                    .next()
                    .unwrap_or("Payload")
                    .to_string()
            }
        }
    }

    fn process_list(&self) -> CommandOutput {
        let scheduler = guard(&self.scheduler);
        let processes = scheduler.all_processes();
        if processes.is_empty() {
            return CommandOutput::text("No processes running");
        }
        let mut lines = vec![
            "PID  NAME              PRIO  STATE     RAM".to_string(),
            "---- ----------------  ---- --------- -----".to_string(),
        ];
        for process in &processes {
            lines.push(format!(
                "{:<4} {:<17} {:<4} {:<9} {}MB",
                process.pid.0, process.name, process.priority, process.state, process.ram_quota_mb
            ));
        }
        lines.push(format!("Total: {} processes", processes.len()));
        CommandOutput::ok(lines)
    }

    fn process_spawn(&mut self, invocation: &Invocation) -> CommandOutput {
        let Some(name) = invocation.first_arg() else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        let priority = match invocation.flag("priority").and_then(|value| value) {
            Some(value) => match parse_priority(value) {
                Some(priority) => priority,
                None => return CommandOutput::usage(invocation.spec.usage),
            },
            None => Priority::Normal,
        };
        let ram = invocation
            .flag("ram")
            .and_then(|value| value)
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(64);
        let mut scheduler = guard(&self.scheduler);
        match scheduler.spawn_process(name, priority, ram) {
            Ok(pid) => {
                drop(scheduler);
                self.command_status(format!(
                    "spawned '{name}' as PID {} (priority={priority}, RAM={ram}MB)",
                    pid.0
                ))
            }
            Err(error) => CommandOutput::error(&format!("spawn failed: {error}")),
        }
    }

    fn process_kill(&mut self, invocation: &Invocation) -> CommandOutput {
        let Some(pid) = parse_pid(invocation) else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        let mut scheduler = guard(&self.scheduler);
        match scheduler.kill_process(pid) {
            Ok(process) => {
                drop(scheduler);
                self.command_status(format!("process {} ({}) terminated", pid.0, process.name))
            }
            Err(error) => CommandOutput::error(&format!("kill failed: {error}")),
        }
    }

    fn process_renice(&mut self, invocation: &Invocation) -> CommandOutput {
        let Some(pid) = parse_pid(invocation) else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        let Some(value) = invocation.arg(1).and_then(parse_priority) else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        let mut scheduler = guard(&self.scheduler);
        match scheduler.set_priority(pid, value) {
            Ok(()) => {
                drop(scheduler);
                self.command_status(format!("process {} priority set to {value}", pid.0))
            }
            Err(error) => CommandOutput::error(&format!("renice failed: {error}")),
        }
    }

    fn process_signal(&mut self, invocation: &Invocation, suspend: bool) -> CommandOutput {
        let Some(pid) = parse_pid(invocation) else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        let mut scheduler = guard(&self.scheduler);
        let result = if suspend {
            scheduler.suspend_process(pid)
        } else {
            scheduler.resume_process(pid)
        };
        match result {
            Ok(()) => {
                drop(scheduler);
                let action = if suspend { "suspended" } else { "resumed" };
                self.command_status(format!("process {} {action}", pid.0))
            }
            Err(error) => CommandOutput::error(&format!("signal failed: {error}")),
        }
    }

    fn process_affinity(&mut self, invocation: &Invocation) -> CommandOutput {
        let Some(pid) = parse_pid(invocation) else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        let Some(cores) = invocation.arg(1) else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        let cores: Vec<usize> = cores
            .split(',')
            .filter_map(|value| value.trim().parse::<usize>().ok())
            .collect();
        if cores.is_empty() {
            return CommandOutput::usage(invocation.spec.usage);
        }
        let mut scheduler = guard(&self.scheduler);
        match scheduler.set_cpu_affinity(pid, &cores) {
            Ok(()) => {
                drop(scheduler);
                self.command_status(format!("process {} affinity set to {cores:?}", pid.0))
            }
            Err(error) => CommandOutput::error(&format!("affinity failed: {error}")),
        }
    }

    fn block_list(&self) -> CommandOutput {
        let registry = guard(&self.registry);
        let blocks = registry.topology_with_state();
        let mut lines = vec![
            "ID   NAME         VERSION  STATE".to_string(),
            "---- ------------ -------- --------".to_string(),
        ];
        for (manifest, state) in blocks {
            lines.push(format!(
                "{:<4} {:<12} {:<8} {:?}",
                manifest.id.0, manifest.name, manifest.version, state
            ));
        }
        lines.push(format!("Total: {} blocks", registry.count()));
        CommandOutput::ok(lines)
    }

    fn block_info(&self, invocation: &Invocation) -> CommandOutput {
        let Some(target) = invocation.first_arg() else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        let registry = guard(&self.registry);
        let Some(id) = resolve_block(&registry, target) else {
            return CommandOutput::error(&format!("block '{target}' not found"));
        };
        let Ok(entry) = registry.get(id) else {
            return CommandOutput::error(&format!("block '{target}' not found"));
        };
        CommandOutput::ok(vec![
            format!("id:           {}", entry.manifest.id.0),
            format!("name:         {}", entry.manifest.name),
            format!("version:      {}", entry.manifest.version),
            format!("state:        {:?}", entry.state),
            format!("sha256:       {:02x?}", entry.manifest.sha256),
            format!("binary bytes: {}", entry.binary.len()),
        ])
    }

    fn block_load(&mut self, invocation: &Invocation) -> CommandOutput {
        let Some(path) = invocation.first_arg() else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        let data = match std::fs::read(path) {
            Ok(data) => data,
            Err(error) => return CommandOutput::error(&format!("read '{path}': {error}")),
        };
        let name = invocation
            .flag("name")
            .and_then(|value| value)
            .map(str::to_string)
            .unwrap_or_else(|| {
                Path::new(path)
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "block".to_string())
            });
        let version = invocation
            .flag("version")
            .and_then(|value| value)
            .unwrap_or("1.0.0")
            .to_string();
        let mut registry = guard(&self.registry);
        match BlockLoader::load_from_binary(&mut registry, &name, &version, data) {
            Ok(manifest) => {
                drop(registry);
                self.command_status(format!(
                    "block '{}' v{} loaded as ID {}",
                    manifest.name, manifest.version, manifest.id.0
                ))
            }
            Err(error) => CommandOutput::error(&format!("block load failed: {error}")),
        }
    }

    fn block_mock(&mut self, invocation: &Invocation) -> CommandOutput {
        let Some(name) = invocation.first_arg() else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        let version = invocation
            .flag("version")
            .and_then(|value| value)
            .or_else(|| invocation.arg(1))
            .unwrap_or("0.1.0")
            .to_string();
        let binary = format!("block_{name}_{version}").into_bytes();
        let mut registry = guard(&self.registry);
        match BlockLoader::load_from_binary(&mut registry, name, &version, binary) {
            Ok(manifest) => {
                drop(registry);
                self.command_status(format!(
                    "synthetic block '{name}' v{version} loaded as ID {}",
                    manifest.id.0
                ))
            }
            Err(error) => CommandOutput::error(&format!("block mock failed: {error}")),
        }
    }

    fn block_unload(&mut self, invocation: &Invocation) -> CommandOutput {
        let Some(target) = invocation.first_arg() else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        let mut registry = guard(&self.registry);
        let Some(id) = resolve_block(&registry, target) else {
            return CommandOutput::error(&format!("block '{target}' not found"));
        };
        match registry.unload_block(id) {
            Ok(_) => {
                drop(registry);
                self.command_status(format!("block {target} unloaded"))
            }
            Err(error) => CommandOutput::error(&format!("unload failed: {error}")),
        }
    }

    fn block_activate(&mut self, invocation: &Invocation) -> CommandOutput {
        let Some(target) = invocation.first_arg() else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        let mut registry = guard(&self.registry);
        let Some(id) = resolve_block(&registry, target) else {
            return CommandOutput::error(&format!("block '{target}' not found"));
        };
        match registry.activate_block(id) {
            Ok(()) => {
                drop(registry);
                self.command_status(format!("block {target} activated"))
            }
            Err(error) => CommandOutput::error(&format!("activate failed: {error}")),
        }
    }

    fn block_deps(&self) -> CommandOutput {
        let registry = guard(&self.registry);
        let graph = registry.dependency_graph();
        let mut lines = vec!["BLOCK            DEPENDS ON".to_string()];
        for block in graph.blocks() {
            lines.push(format!(
                "{:<16} {}",
                block,
                graph.dependencies_of(block).join(", ")
            ));
        }
        match graph.load_order() {
            Ok(order) => lines.push(format!("load order: {}", order.join(" -> "))),
            Err(error) => lines.push(format!("load order error: {error:?}")),
        }
        CommandOutput::ok(lines)
    }

    fn block_verify(&self, invocation: &Invocation) -> CommandOutput {
        let Some(target) = invocation.first_arg() else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        let registry = guard(&self.registry);
        let Some(id) = resolve_block(&registry, target) else {
            return CommandOutput::error(&format!("block '{target}' not found"));
        };
        match registry.verify_signature(id) {
            Ok(true) => CommandOutput::text("signature OK"),
            Ok(false) => CommandOutput::text("no valid signature"),
            Err(error) => CommandOutput::error(&format!("verify failed: {error}")),
        }
    }

    fn net_set(&mut self, invocation: &Invocation) -> CommandOutput {
        let mut updates = serde_json::Map::new();
        for argument in &invocation.args {
            let Some((key, value)) = argument.split_once('=') else {
                return CommandOutput::usage(invocation.spec.usage);
            };
            if key.is_empty() {
                return CommandOutput::usage(invocation.spec.usage);
            }
            updates.insert(key.to_string(), json_value(value));
        }
        for (key, value) in &invocation.flags {
            if let Some(value) = value {
                updates.insert(key.clone(), json_value(value));
            }
        }
        if updates.is_empty() {
            return CommandOutput::usage(invocation.spec.usage);
        }
        match self.net.apply(&serde_json::Value::Object(updates)) {
            Ok(()) => self.command_status("network configuration applied"),
            Err(error) => CommandOutput::error(&format!("net set failed: {error}")),
        }
    }

    fn store_command(&mut self, invocation: &Invocation, subcommand: &str) -> CommandOutput {
        match subcommand {
            "list" => {
                let installed = self.store.list_installed();
                let mut lines = vec!["NAME                 VERSION    PATH".to_string()];
                for block in installed {
                    lines.push(format!(
                        "{:<20} {:<10} {}",
                        block.manifest.name,
                        block.manifest.version,
                        block.path.display()
                    ));
                }
                lines.push(format!("Total: {} blocks", lines.len().saturating_sub(1)));
                CommandOutput::ok(lines)
            }
            "sources" => {
                let mut lines = vec!["NAME                 KIND       BASE".to_string()];
                for source in self.store.sources() {
                    lines.push(format!(
                        "{:<20} {:<10} {}",
                        source.name,
                        format!("{:?}", source.kind),
                        source.base
                    ));
                }
                CommandOutput::ok(lines)
            }
            "source" => {
                let Some(spec) = invocation.first_arg() else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                match StoreManager::parse_source_spec(spec) {
                    Ok(source) => match self.store.add_source(source) {
                        Ok(()) => self.command_status(format!("source '{spec}' added")),
                        Err(error) => CommandOutput::error(&format!("source add failed: {error}")),
                    },
                    Err(error) => CommandOutput::error(&format!("invalid source: {error}")),
                }
            }
            "search" => {
                let Some(query) = invocation.first_arg() else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let source = invocation
                    .flag("source")
                    .and_then(|value| value)
                    .map(str::to_string);
                let result = StoreManager::block_on(self.store.search(query, source.as_deref()));
                match result {
                    Ok(items) => {
                        let mut lines = vec!["NAME                 VERSION".to_string()];
                        for item in items {
                            lines.push(format!("{:<20} {}", item.name, item.version));
                        }
                        CommandOutput::ok(lines)
                    }
                    Err(error) => CommandOutput::error(&format!("search failed: {error}")),
                }
            }
            "install" => {
                let Some(name) = invocation.first_arg() else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let source = invocation
                    .flag("source")
                    .and_then(|value| value)
                    .map(str::to_string);
                let version = invocation
                    .flag("version")
                    .and_then(|value| value)
                    .map(str::to_string);
                let result = StoreManager::block_on(self.store.install(
                    source.as_deref(),
                    name,
                    version.as_deref(),
                ));
                match result {
                    Ok(block) => self.command_status(format!(
                        "installed '{}' v{}",
                        block.manifest.name, block.manifest.version
                    )),
                    Err(error) => CommandOutput::error(&format!("install failed: {error}")),
                }
            }
            "update" | "check-updates" => {
                let name = invocation.first_arg().map(str::to_string);
                let result = if subcommand == "update" {
                    StoreManager::block_on(self.store.update(None, name.as_deref()))
                        .map(|items| items.len())
                } else {
                    StoreManager::block_on(self.store.check_updates(None)).map(|items| items.len())
                };
                match result {
                    Ok(count) => self.command_status(format!("{count} update(s) available")),
                    Err(error) => CommandOutput::error(&format!("update check failed: {error}")),
                }
            }
            "uninstall" => {
                let Some(name) = invocation.first_arg() else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                match self.store.uninstall(name) {
                    Ok(items) => {
                        self.command_status(format!("uninstalled '{name}' ({} left)", items.len()))
                    }
                    Err(error) => CommandOutput::error(&format!("uninstall failed: {error}")),
                }
            }
            "rollback" => {
                let Some(name) = invocation.first_arg() else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                match self.store.rollback(name) {
                    Ok(block) => self.command_status(format!(
                        "rolled back '{}' to v{}",
                        block.manifest.name, block.manifest.version
                    )),
                    Err(error) => CommandOutput::error(&format!("rollback failed: {error}")),
                }
            }
            _ => CommandOutput::unavailable(&format!("store {subcommand} has no host backend yet")),
        }
    }

    fn vfs_command(&mut self, invocation: &Invocation, subcommand: &str) -> CommandOutput {
        let vfs = match self.open_vfs() {
            Ok(vfs) => vfs,
            Err(output) => return output,
        };
        match subcommand {
            "ls" => {
                let path = match Self::path_arg(invocation, 0) {
                    Ok(path) => path,
                    Err(output) => return output,
                };
                let result = StoreManager::block_on(vfs.list_dir(&path));
                match result {
                    Ok(entries) => {
                        let mut lines = vec!["MODE  SIZE       NAME".to_string()];
                        for entry in entries {
                            let kind = if entry.is_dir { "dir" } else { "file" };
                            lines.push(format!(
                                "{:<4} {:<10} {}{}",
                                kind,
                                entry.size,
                                entry.name,
                                if entry.is_dir { "/" } else { "" }
                            ));
                        }
                        CommandOutput::ok(lines)
                    }
                    Err(error) => CommandOutput::error(&format!("ls failed: {error}")),
                }
            }
            "cat" | "head" => {
                let path = match Self::path_arg(invocation, 0) {
                    Ok(path) => path,
                    Err(output) => return output,
                };
                let result = StoreManager::block_on(vfs.read_file(&path));
                match result {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        let lines: Vec<String> = if subcommand == "head" {
                            text.lines().take(20).map(str::to_string).collect()
                        } else {
                            text.lines().map(str::to_string).collect()
                        };
                        CommandOutput::ok(lines)
                    }
                    Err(error) => CommandOutput::error(&format!("read failed: {error}")),
                }
            }
            "write" => {
                let path = match Self::path_arg(invocation, 0) {
                    Ok(path) => path,
                    Err(output) => return output,
                };
                if invocation.args.len() < 2 {
                    return CommandOutput::usage(invocation.spec.usage);
                }
                let data = invocation.args[1..].join(" ");
                let result = StoreManager::block_on(vfs.write_file(&path, data.as_bytes()));
                match result {
                    Ok(()) => self.command_status(format!("wrote {}", path.to_uri())),
                    Err(error) => CommandOutput::error(&format!("write failed: {error}")),
                }
            }
            "mkdir" => {
                let path = match Self::path_arg(invocation, 0) {
                    Ok(path) => path,
                    Err(output) => return output,
                };
                match StoreManager::block_on(vfs.create_dir(&path)) {
                    Ok(()) => self.command_status(format!("created {}", path.to_uri())),
                    Err(error) => CommandOutput::error(&format!("mkdir failed: {error}")),
                }
            }
            "rm" => {
                let path = match Self::path_arg(invocation, 0) {
                    Ok(path) => path,
                    Err(output) => return output,
                };
                match StoreManager::block_on(vfs.delete_item(&path)) {
                    Ok(()) => self.command_status(format!("removed {}", path.to_uri())),
                    Err(error) => CommandOutput::error(&format!("rm failed: {error}")),
                }
            }
            "mv" | "cp" => {
                let from = match Self::path_arg(invocation, 0) {
                    Ok(path) => path,
                    Err(output) => return output,
                };
                let to = match Self::path_arg(invocation, 1) {
                    Ok(path) => path,
                    Err(output) => return output,
                };
                let result = if subcommand == "mv" {
                    StoreManager::block_on(vfs.rename(&from, &to))
                } else {
                    match StoreManager::block_on(vfs.read_file(&from)) {
                        Ok(bytes) => StoreManager::block_on(vfs.write_file(&to, &bytes)),
                        Err(error) => {
                            return CommandOutput::error(&format!("cp read failed: {error}"))
                        }
                    }
                };
                match result {
                    Ok(()) => self.command_status(format!(
                        "{subcommand} {} -> {}",
                        from.to_uri(),
                        to.to_uri()
                    )),
                    Err(error) => CommandOutput::error(&format!("{subcommand} failed: {error}")),
                }
            }
            "stat" | "exists" => {
                let path = match Self::path_arg(invocation, 0) {
                    Ok(path) => path,
                    Err(output) => return output,
                };
                if subcommand == "exists" {
                    return match StoreManager::block_on(vfs.exists(&path)) {
                        Ok(true) => CommandOutput::text("true"),
                        Ok(false) => CommandOutput::text("false"),
                        Err(error) => CommandOutput::error(&format!("exists failed: {error}")),
                    };
                }
                match StoreManager::block_on(vfs.metadata(&path)) {
                    Ok(metadata) => CommandOutput::ok(vec![
                        format!("path:       {}", path.to_uri()),
                        format!("directory:  {}", metadata.is_dir),
                        format!("file:       {}", metadata.is_file),
                        format!("symlink:    {}", metadata.is_symlink),
                        format!("size:       {}", metadata.size),
                        format!("readonly:   {}", metadata.readonly),
                        format!("permissions: {}", metadata.permissions),
                    ]),
                    Err(error) => CommandOutput::error(&format!("stat failed: {error}")),
                }
            }
            "resolve" => {
                let path = match Self::path_arg(invocation, 0) {
                    Ok(path) => path,
                    Err(output) => return output,
                };
                match StoreManager::block_on(vfs.resolve(&path)) {
                    Ok(resolved) => CommandOutput::text(resolved.display().to_string()),
                    Err(error) => CommandOutput::error(&format!("resolve failed: {error}")),
                }
            }
            _ => CommandOutput::unavailable(&format!("vfs {subcommand} has no host backend yet")),
        }
    }
}

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn parse_pid(invocation: &Invocation) -> Option<ProcessId> {
    invocation.first_arg()?.parse::<u64>().ok().map(ProcessId)
}

fn parse_priority(value: &str) -> Option<Priority> {
    match value.to_lowercase().as_str() {
        "0" | "background" => Some(Priority::Background),
        "1" | "low" => Some(Priority::Low),
        "2" | "normal" => Some(Priority::Normal),
        "3" | "high" => Some(Priority::High),
        "4" | "critical" => Some(Priority::Critical),
        _ => None,
    }
}

fn resolve_block(registry: &BlockRegistry, target: &str) -> Option<BlockId> {
    if let Ok(number) = target.parse::<u32>() {
        let id = BlockId::new(number);
        if registry.get(id).is_ok() {
            return Some(id);
        }
    }
    registry.find_by_name(target).map(|entry| entry.manifest.id)
}

fn json_value(raw: &str) -> serde_json::Value {
    if let Ok(number) = raw.parse::<u64>() {
        serde_json::Value::Number(number.into())
    } else if let Ok(boolean) = raw.parse::<bool>() {
        serde_json::Value::Bool(boolean)
    } else {
        serde_json::Value::String(raw.to_string())
    }
}
