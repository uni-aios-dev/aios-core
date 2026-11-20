//! Host implementation of [`CommandBackend`].
//!
//! The backend delegates to the same library crates the current host TUI uses,
//! so a command has one canonical name and one real implementation. An
//! unimplemented subcommand is reported as unavailable instead of being silently
//! accepted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use aios_autohal::engine::{AutohalEngine, EngineConfig};
use aios_autohal::fingerprint::extract_fingerprints;
use aios_autohal::hotplug::{HotplugConfig, HotplugMonitor};
use aios_block_mgr::loader::BlockLoader;
use aios_block_mgr::registry::BlockRegistry;
use aios_context::persistence::PersistentStore;
use aios_context::store::EmbeddedContextStore;
use aios_context::telemetry::{TelemetryEntry, TelemetryStore};
use aios_core::block::{BlockId, BlockState};
use aios_core::fault::RecoveryAction;
use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};
use aios_debug::crash_reporter::{CrashKind, CrashReporter};
use aios_exec_compat::dependency_healer::DependencyHealer;
use aios_exec_compat::format::ExecutableType;
use aios_exec_compat::sandbox_compat::CompatSandboxManager;
use aios_hal::ai_tier::AiTier;
use aios_hal::hardware::HardwareProfile;
use aios_iommu::iommu::IommuManager;
use aios_iommu::IommuInfo;
use aios_ipc::bus::IpcBus;
use aios_live_update::cow_live_update::PersistedLiveUpdateEngine;
use aios_mpk::mpk::MpkManager;
use aios_net_config::block::NetSettingsBlock;
use aios_net_config::config::NetworkConfig;
use aios_process_mgr::scheduler::{Scheduler, SchedulingMode};
use aios_process_mgr::task::{Priority, ProcessId, ProcessState};
use aios_security::access_control::AccessControlLayer;
use aios_security::capability::Capability as SecCapability;
use aios_security::hardware_bridge::HardwareSecurityBridge;
use aios_security::sandbox::Sandbox;
use aios_store::manager::StoreManager;
use aios_store::manifest::{sign_manifest, ManifestInfo};
use aios_store::ManifestValidator;
use aios_sys_control::net_manager::{NetManager, SimulatedWifi, WifiBackend};
use aios_sys_control::SysControlHub;
use aios_tee::TeeInfo;
use aios_telemetry::trace::{SpanStatus, TraceContext};
use aios_vfs::vfs::{AiosVfs, VfsPath, VirtualFileSystem};
use aios_wasm::executor::BlockExecutor;
use aios_wasm::{IsolationConfig, SandboxConfig, WasiFilter, WasmBlock, WasmSandbox};
use base64::Engine as _;
use ed25519_dalek::SigningKey;
use sha2::{Digest, Sha256};

use crate::backend::{dispatch, CommandBackend};
use crate::capability::{BackendKind, CapabilitySet};
use crate::control::{AuthProvider, BridgeControl, WatchdogControl};
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
    /// Database file used by `context persist`.
    pub context_db_path: PathBuf,
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
            context_db_path: base.join("context.redb"),
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
    /// WASM executor that preserves state between `block run` invocations.
    pub executor: Arc<Mutex<BlockExecutor>>,
    config: HostBackendConfig,
    publish_url: Option<String>,
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
    aliases: BTreeMap<String, String>,
    trace_context: TraceContext,
    crash_reporter: CrashReporter,
    context_store: EmbeddedContextStore,
    wasi_filter: WasiFilter,
    wasm_source: Option<Vec<u8>>,
    runtime_blocks: BTreeMap<String, BlockId>,
    compat_manager: CompatSandboxManager,
    access_control: AccessControlLayer,
    cap_token_ids: Vec<u32>,
    sandboxes: BTreeMap<u32, Sandbox>,
    hw_bridge: HardwareSecurityBridge,
    mpk: Option<MpkManager>,
    iommu: IommuManager,
    tee: TeeInfo,
    wifi: NetManager,
    sys_hub: SysControlHub,
    autohal: Option<AutohalEngine>,
    hotplug: Option<HotplugMonitor>,
    live_update: Option<PersistedLiveUpdateEngine>,
    watchdog_control: Option<Arc<dyn WatchdogControl>>,
    bridge_control: Option<Arc<dyn BridgeControl>>,
    auth_provider: Option<Arc<dyn AuthProvider>>,
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
        let version = config.version.clone();
        let live_update = PersistedLiveUpdateEngine::new(30_000, config.blocks_dir.clone()).ok();
        let executor = Arc::new(Mutex::new(
            BlockExecutor::with_default_config()
                .unwrap_or_else(|error| panic!("aios WASM sandbox: {error}")),
        ));
        Self {
            scheduler,
            registry,
            executor,
            config,
            publish_url: None,
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
            aliases: BTreeMap::new(),
            trace_context: TraceContext::new(),
            crash_reporter: CrashReporter::new("aios-console", &version),
            context_store: EmbeddedContextStore::new(1000),
            wasi_filter: WasiFilter::default(),
            wasm_source: None,
            runtime_blocks: BTreeMap::new(),
            compat_manager: CompatSandboxManager::new(),
            access_control: AccessControlLayer::new(b"aios-console-issuer-v1".to_vec(), 3_600_000),
            cap_token_ids: Vec::new(),
            sandboxes: BTreeMap::new(),
            hw_bridge: HardwareSecurityBridge::new(),
            mpk: MpkManager::detect().ok(),
            iommu: IommuManager::new(16),
            tee: TeeInfo::detect(),
            wifi: NetManager::default(),
            sys_hub: SysControlHub::defaults(),
            autohal: None,
            hotplug: None,
            live_update,
            watchdog_control: None,
            bridge_control: None,
            auth_provider: None,
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

    /// Shares the host WASM executor so `block run` reuses the same live
    /// instances (and therefore the same block state) as the orchestrator.
    pub fn set_executor(&mut self, executor: Arc<Mutex<BlockExecutor>>) {
        self.executor = executor;
    }

    /// Overrides the bridge publish endpoint used by `store publish`.
    ///
    /// Without an override the command posts to
    /// `http://localhost:$AIOS_BRIDGE_PORT/api/v1/store/publish` (port 8080 by
    /// default). Tests use this seam to avoid depending on a running bridge.
    pub fn set_publish_url(&mut self, url: impl Into<String>) {
        self.publish_url = Some(url.into());
    }

    /// Injects the host-owned watchdog control surface for `wd` commands.
    pub fn set_watchdog_control(&mut self, control: Arc<dyn WatchdogControl>) {
        self.watchdog_control = Some(control);
    }

    /// Injects the host-owned bridge control surface for `bridge` commands.
    pub fn set_bridge_control(&mut self, control: Arc<dyn BridgeControl>) {
        self.bridge_control = Some(control);
    }

    /// Injects the host-owned authentication surface for `auth` commands.
    pub fn set_auth_provider(&mut self, provider: Arc<dyn AuthProvider>) {
        self.auth_provider = Some(provider);
    }

    /// Overrides the hardware inventory used by `sys` and `hw` commands.
    pub fn set_hardware_profile(&mut self, profile: HardwareProfile) {
        self.hardware_profile = Some(("injected".to_string(), profile));
    }

    /// Executes one input line with session alias expansion and trace recording.
    pub fn execute_line(&mut self, line: &str) -> CommandOutput {
        let expanded = self.expand_aliases(line);
        let _span = self.trace_context.begin_span("console.command");
        self.trace_context.add_tag("input", &expanded);
        let output = dispatch(self, &expanded);
        let status = match output.code {
            ExitCode::Ok => SpanStatus::Ok,
            _ => SpanStatus::Error(format!("{:?}", output.code)),
        };
        self.trace_context.end_span(status);
        output
    }

    fn expand_aliases(&self, line: &str) -> String {
        let trimmed = line.trim();
        let (name, rest) = match trimmed.find(char::is_whitespace) {
            Some(index) => (&trimmed[..index], &trimmed[index..]),
            None => (trimmed, ""),
        };
        match self.aliases.get(&name.to_ascii_lowercase()) {
            Some(target) => format!("{target}{rest}"),
            None => trimmed.to_string(),
        }
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
            ("sleep", _) => self.sleep(invocation),
            ("alias", _) => self.alias_command(invocation),
            ("unalias", _) => self.unalias_command(invocation),
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
            ("fault", Some("resolve")) => self.fault_resolve(invocation),
            ("sched", Some(subcommand)) => self.sched_command(invocation, subcommand),
            ("block", Some("list")) => self.block_list(),
            ("block", Some("run")) => self.block_run(invocation),
            ("block", Some("info")) => self.block_info(invocation),
            ("block", Some("load")) => self.block_load(invocation),
            ("block", Some("mock")) => self.block_mock(invocation),
            ("block", Some("unload")) => self.block_unload(invocation),
            ("block", Some("activate")) => self.block_activate(invocation),
            ("block", Some("deps")) => self.block_deps(),
            ("block", Some("verify")) => self.block_verify(invocation),
            ("block", Some("swap")) => self.block_swap(invocation),
            ("block", Some("rollback")) => self.block_rollback(invocation),
            ("update", Some("history")) => self.update_history(),
            ("update", Some("pending")) => self.update_pending(),
            ("update", Some("recover")) => self.update_recover(),
            ("wd", Some("status")) => self.watchdog_status(),
            ("wd", Some("start")) => self.watchdog_start(),
            ("wd", Some("stop")) => self.watchdog_stop(),
            ("wd", Some("force-safe-mode")) => self.watchdog_force_safe_mode(),
            ("wd", Some("escalate")) => self.watchdog_escalate(),
            ("wd", Some("reset")) => self.watchdog_reset(),
            ("net", Some("show")) => CommandOutput::text(self.net.config().to_json()),
            ("net", Some("set")) => self.net_set(invocation),
            ("net", Some("reset")) => match self.net.reset() {
                Ok(()) => self.command_status("network configuration reset to defaults"),
                Err(error) => CommandOutput::error(&format!("net reset failed: {error}")),
            },
            ("net", Some("validate")) => self.net_validate(invocation),
            ("net", Some("config")) => self.net_config(),
            ("net", Some("status")) => self.net_status(),
            ("net", Some("apply")) => self.net_set(invocation),
            ("net", Some("lease")) => self.net_lease(),
            ("store", Some(subcommand)) => self.store_command(invocation, subcommand),
            ("vfs", Some(subcommand)) => self.vfs_command(invocation, subcommand),
            ("ipc", Some(subcommand)) => self.ipc_command(invocation, subcommand),
            ("hw", Some("detect")) => self.hw_detect(),
            ("hw", Some("rescan")) => self.hw_rescan(),
            ("hw", Some("profile")) => self.hw_profile(invocation),
            ("hw", Some("fingerprints")) => self.hw_fingerprints(),
            ("driver", Some(subcommand)) => self.driver_command(invocation, subcommand),
            ("hotplug", Some(subcommand)) => self.hotplug_command(invocation, subcommand),
            ("telemetry", Some(subcommand)) => self.telemetry_command(invocation, subcommand),
            ("caps", Some(subcommand)) => self.caps_command(invocation, subcommand),
            ("sandbox", Some(subcommand)) => self.sandbox_command(invocation, subcommand),
            ("hw-protect", Some(subcommand)) => self.hw_protect_command(invocation, subcommand),
            ("wasm", Some(subcommand)) => self.wasm_command(invocation, subcommand),
            ("wasi", Some("policy")) => self.wasi_policy(invocation),
            ("exec", _) => self.exec_command(invocation),
            ("deps", Some(subcommand)) => self.deps_command(invocation, subcommand),
            ("compat", Some(subcommand)) => self.compat_command(invocation, subcommand),
            ("context", Some(subcommand)) => self.context_command(invocation, subcommand),
            ("workflow", Some(subcommand)) => self.workflow_command(invocation, subcommand),
            ("stability", Some(subcommand)) => self.stability_command(invocation, subcommand),
            ("trace", Some("list")) => self.trace_list(),
            ("crash", Some("report")) => self.crash_report(invocation),
            ("wifi", Some(subcommand)) => self.wifi_command(invocation, subcommand),
            ("hub", Some(subcommand)) => self.hub_command(invocation, subcommand),
            ("bridge", Some(subcommand)) => self.bridge_command(invocation, subcommand),
            ("auth", Some(subcommand)) => self.auth_command(invocation, subcommand),
            ("source", _) => self.source(invocation),
            _ => {
                let name = invocation.display_name();
                match crate::unavailable::host_unavailable_reason(
                    invocation.spec.id,
                    invocation.subcommand,
                ) {
                    Some(reason) => {
                        CommandOutput::unavailable(&format!("'{name}' is unavailable: {reason}"))
                    }
                    None => {
                        CommandOutput::unavailable(&format!("'{name}' has no host backend yet"))
                    }
                }
            }
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
            let output = self.execute_line(line);
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

    fn hw_fingerprints(&mut self) -> CommandOutput {
        let (source, profile) = self.hardware();
        let fingerprints = extract_fingerprints(&profile);
        let mut lines = vec![
            "hw             hardware fingerprints".to_string(),
            format!("source         {source}"),
            format!("count          {} fingerprint(s)", fingerprints.len()),
        ];
        for (index, fingerprint) in fingerprints.iter().enumerate() {
            let label = format!("#{index}");
            let actionable = if fingerprint.is_actionable() {
                "yes"
            } else {
                "no"
            };
            lines.push(format!(
                "{label:<14}{}  {}  driver={}  class=0x{:06X}  actionable={actionable}",
                fingerprint.key(),
                fingerprint.display_name(),
                fingerprint.driver_id(),
                fingerprint.class_code,
            ));
        }
        if fingerprints.is_empty() {
            lines.push("none           no hardware fingerprints".to_string());
        }
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

    /// Renders the exact legacy `ps` text from the canonical host process list.
    ///
    /// Returns `None` if the canonical shape changes, so a frontend can fall
    /// back to the legacy dispatcher instead of printing a partial table.
    pub fn process_list_legacy_text(&self) -> Option<String> {
        crate::legacy::process_list_text(&self.process_list())
    }

    /// Renders the exact legacy `blocks` text from the canonical host block list.
    ///
    /// Returns `None` if the canonical shape changes, so a frontend can fall
    /// back to the legacy dispatcher instead of printing a partial table.
    pub fn block_list_legacy_text(&self) -> Option<String> {
        crate::legacy::block_list_text(&self.block_list())
    }

    /// Renders the exact legacy `status` text from the canonical host status.
    ///
    /// The scheduler crash log count is supplied separately because the canonical
    /// status does not expose it. Returns `None` if the canonical shape changes.
    pub fn status_legacy_text(&self) -> Option<String> {
        let crash_log_count = guard(&self.scheduler).crash_log().len();
        crate::legacy::status_text(&self.status(), crash_log_count)
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

    /// Run a text command in an installed WASM block, keeping the instance
    /// alive between calls so stateful blocks (calculator, minesweeper, …)
    /// continue where the previous invocation stopped.
    fn block_run(&mut self, invocation: &Invocation) -> CommandOutput {
        let Some(name) = invocation.first_arg() else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        let text = invocation.args.get(1..).unwrap_or_default().join(" ");
        let installed = match self.store.find_installed(name) {
            Some(installed) => installed,
            None => {
                return CommandOutput::error(&format!(
                    "block '{name}' is not installed; run 'store install {name}' first"
                ))
            }
        };
        let binary = match std::fs::read(&installed.path) {
            Ok(binary) => binary,
            Err(error) => {
                return CommandOutput::error(&format!(
                    "read '{}': {error}",
                    installed.path.display()
                ))
            }
        };

        let id = if let Some(id) = self.runtime_blocks.get(name).copied() {
            id
        } else {
            let mut registry = guard(&self.registry);
            match BlockLoader::load_from_binary(
                &mut registry,
                &installed.manifest.name,
                &installed.manifest.version,
                binary,
            ) {
                Ok(manifest) => manifest.id,
                Err(error) => return CommandOutput::error(&format!("block load failed: {error}")),
            }
        };
        self.runtime_blocks.insert(name.to_string(), id);

        if !guard(&self.executor).is_executed(id) {
            let registry = guard(&self.registry);
            if let Err(error) =
                guard(&self.executor).execute_block(&registry, id, IsolationConfig::default())
            {
                return CommandOutput::error(&format!("block execute failed: {error}"));
            }
        }

        let packet = IpcPacket::new(
            0,
            1,
            CommandId::Custom,
            Payload::Custom(name.to_string(), text.into_bytes()),
        );
        match guard(&self.executor).dispatch_wasm_packet(id, &packet) {
            Ok(Some(response)) => {
                let payload = response.payload;
                let reply = match payload {
                    Payload::Text(text) => text,
                    Payload::Custom(_, data) | Payload::Binary(data) => {
                        String::from_utf8_lossy(&data).into_owned()
                    }
                    Payload::Empty => String::new(),
                    other => format!("unexpected block reply: {other:?}"),
                };
                CommandOutput::ok(reply.lines().map(str::to_string).collect())
            }
            Ok(None) => CommandOutput::ok(vec!["no response from block".into()]),
            Err(error) => CommandOutput::error(&format!("block run failed: {error}")),
        }
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

    fn block_swap(&mut self, invocation: &Invocation) -> CommandOutput {
        let Some(old_target) = invocation.arg(0) else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        let Some(new_target) = invocation.arg(1) else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        if self.live_update.is_none() {
            return CommandOutput::error("live-update engine unavailable: storage init failed");
        }
        let plan = {
            let registry = guard(&self.registry);
            let Some(old_id) = resolve_block(&registry, old_target) else {
                return CommandOutput::error(&format!("block '{old_target}' not found"));
            };
            let Some(new_id) = resolve_block(&registry, new_target) else {
                return CommandOutput::error(&format!("block '{new_target}' not found"));
            };
            let Ok(old_entry) = registry.get(old_id) else {
                return CommandOutput::error(&format!("block '{old_target}' not found"));
            };
            let Ok(new_entry) = registry.get(new_id) else {
                return CommandOutput::error(&format!("block '{new_target}' not found"));
            };
            (
                old_id.0,
                old_entry.binary.clone(),
                old_entry.manifest.version.clone(),
                new_entry.binary.clone(),
                new_entry.manifest.version.clone(),
                new_entry.manifest.sha256,
            )
        };
        let (old_id, old_binary, old_version, new_binary, new_version, new_sha256) = plan;
        let mut bus = guard(&self.ipc_bus);
        let engine = self
            .live_update
            .as_mut()
            .expect("live-update engine checked above");
        match engine.perform_swap(
            old_id,
            old_binary,
            old_version.clone(),
            Vec::new(),
            new_binary,
            new_version.clone(),
            new_sha256,
            &mut bus,
            None,
        ) {
            Ok(()) => {
                drop(bus);
                self.command_status(format!(
                    "block_{old_id} swap {old_version} -> {new_version} recorded; rollback entry stored"
                ))
            }
            Err(error) => CommandOutput::error(&format!("block swap failed: {error}")),
        }
    }

    fn block_rollback(&mut self, invocation: &Invocation) -> CommandOutput {
        if self.live_update.is_none() {
            return CommandOutput::error("live-update engine unavailable: storage init failed");
        }
        let ids: Vec<u32> = match invocation.first_arg() {
            Some(target) => {
                let registry = guard(&self.registry);
                let Some(id) = resolve_block(&registry, target) else {
                    return CommandOutput::error(&format!("block '{target}' not found"));
                };
                vec![id.0]
            }
            None => {
                let engine = self
                    .live_update
                    .as_ref()
                    .expect("live-update engine checked above");
                engine
                    .engine()
                    .pending_rollbacks()
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect()
            }
        };
        if ids.is_empty() {
            return CommandOutput::text("no pending rollbacks");
        }
        let engine = self
            .live_update
            .as_mut()
            .expect("live-update engine checked above");
        let mut bus = guard(&self.ipc_bus);
        let mut lines = Vec::new();
        let mut failed = false;
        for id in ids {
            match engine.rollback(id, &mut bus) {
                Ok(_) => lines.push(format!("block_{id} rolled back")),
                Err(error) => {
                    failed = true;
                    lines.push(format!("block_{id} rollback failed: {error}"));
                }
            }
        }
        drop(bus);
        for line in &lines {
            self.note(line.clone());
        }
        if failed {
            CommandOutput::error(&lines.join("\n"))
        } else {
            CommandOutput::ok(lines)
        }
    }

    fn update_history(&self) -> CommandOutput {
        let Some(engine) = self.live_update.as_ref() else {
            return CommandOutput::error("live-update engine unavailable: storage init failed");
        };
        let history = engine.swap_history();
        if history.is_empty() {
            return CommandOutput::text("no swaps recorded in this session");
        }
        let mut lines = vec![
            "BLOCK    OLD VERSION   NEW VERSION   SUCCESS  ROLLED_BACK  TIMESTAMP".to_string(),
        ];
        for record in history {
            lines.push(format!(
                "{:<8} {:<12} {:<13} {:<8} {:<12} {}",
                record.block_id,
                record.old_version,
                record.new_version,
                record.success,
                record.rolled_back,
                record.timestamp
            ));
        }
        CommandOutput::ok(lines)
    }

    fn update_pending(&self) -> CommandOutput {
        let Some(engine) = self.live_update.as_ref() else {
            return CommandOutput::error("live-update engine unavailable: storage init failed");
        };
        let pending = engine.engine().pending_rollbacks();
        if pending.is_empty() {
            return CommandOutput::text("no pending rollbacks");
        }
        let mut lines = vec!["BLOCK    PREVIOUS VERSION".to_string()];
        for (id, version) in pending {
            lines.push(format!("{:<8} {version}", format!("block_{id}")));
        }
        CommandOutput::ok(lines)
    }

    fn update_recover(&mut self) -> CommandOutput {
        let Some(engine) = self.live_update.as_mut() else {
            return CommandOutput::error("live-update engine unavailable: storage init failed");
        };
        match engine.recover_from_crash() {
            Ok(ids) if ids.is_empty() => CommandOutput::text("no interrupted hot-swaps to recover"),
            Ok(ids) => {
                let lines: Vec<String> = ids
                    .iter()
                    .map(|id| format!("recovered interrupted hot-swap for block_{id}"))
                    .collect();
                for line in &lines {
                    self.note(line.clone());
                }
                CommandOutput::ok(lines)
            }
            Err(error) => CommandOutput::error(&format!("update recover failed: {error}")),
        }
    }

    fn watchdog_status(&self) -> CommandOutput {
        let mut lines = Vec::new();
        if let Some(control) = self.watchdog_control.as_ref() {
            lines.extend(control.status());
        } else {
            lines.push("watchdog state: no host control injected".to_string());
        }
        lines.push(format!(
            "safe mode restarts: {}/{}",
            self.restarts, self.config.max_restarts
        ));
        lines.push(format!("log entries: {}", self.log.len()));
        CommandOutput::ok(lines)
    }

    fn watchdog_start(&mut self) -> CommandOutput {
        let Some(control) = self.watchdog_control.clone() else {
            return CommandOutput::unavailable("wd start needs a host watchdog control seam");
        };
        match control.start() {
            Ok(()) => self.command_status("watchdog monitoring started"),
            Err(error) => CommandOutput::error(&format!("wd start failed: {error}")),
        }
    }

    fn watchdog_stop(&mut self) -> CommandOutput {
        let Some(control) = self.watchdog_control.clone() else {
            return CommandOutput::unavailable("wd stop needs a host watchdog control seam");
        };
        match control.stop() {
            Ok(()) => self.command_status("watchdog monitoring stopped"),
            Err(error) => CommandOutput::error(&format!("wd stop failed: {error}")),
        }
    }

    fn watchdog_force_safe_mode(&mut self) -> CommandOutput {
        let Some(control) = self.watchdog_control.clone() else {
            return CommandOutput::unavailable(
                "wd force-safe-mode needs a host watchdog control seam",
            );
        };
        control.force_safe_mode();
        self.command_status("watchdog forced into safe mode")
    }

    fn watchdog_escalate(&self) -> CommandOutput {
        let Some(control) = self.watchdog_control.as_ref() else {
            return CommandOutput::unavailable("wd escalate needs a host watchdog control seam");
        };
        CommandOutput::ok(control.escalate())
    }

    fn watchdog_reset(&mut self) -> CommandOutput {
        if self.restarts >= self.config.max_restarts {
            return CommandOutput::error(&format!(
                "max restarts ({}) reached; manual intervention required",
                self.config.max_restarts
            ));
        }
        let Some(control) = self.watchdog_control.clone() else {
            return CommandOutput::unavailable("wd reset needs a host watchdog control seam");
        };
        if let Err(error) = control.reset() {
            return CommandOutput::error(&format!("wd reset failed: {error}"));
        }
        self.restarts += 1;
        self.command_status(format!(
            "orchestrator restart {} of {} initiated",
            self.restarts, self.config.max_restarts
        ))
    }

    fn bridge_command(&mut self, invocation: &Invocation, subcommand: &str) -> CommandOutput {
        let Some(control) = self.bridge_control.clone() else {
            return CommandOutput::unavailable("bridge control needs a host bridge control seam");
        };
        match subcommand {
            "status" => CommandOutput::ok(control.status()),
            "start" => match control.start() {
                Ok(()) => self.command_status("bridge listener start requested"),
                Err(error) => CommandOutput::error(&format!("bridge start failed: {error}")),
            },
            "stop" => match control.stop() {
                Ok(()) => self.command_status("bridge graceful shutdown requested"),
                Err(error) => CommandOutput::error(&format!("bridge stop failed: {error}")),
            },
            _ => CommandOutput::usage(invocation.spec.usage),
        }
    }

    fn auth_command(&self, invocation: &Invocation, subcommand: &str) -> CommandOutput {
        let Some(provider) = self.auth_provider.as_ref() else {
            return CommandOutput::unavailable("auth needs a host authentication seam");
        };
        match subcommand {
            "register" | "login" => {
                let Some(username) = invocation.arg(0) else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let Some(password) = invocation.arg(1) else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let result = if subcommand == "register" {
                    provider.register(username, password)
                } else {
                    provider.login(username, password)
                };
                match result {
                    Ok(token) => CommandOutput::ok(vec![format!("{subcommand} OK"), token]),
                    Err(error) => {
                        CommandOutput::error(&format!("auth {subcommand} failed: {error}"))
                    }
                }
            }
            "me" => {
                let Some(token) = invocation.first_arg() else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                match provider.me(token) {
                    Ok(user) => CommandOutput::text(user),
                    Err(error) => CommandOutput::error(&format!("auth me failed: {error}")),
                }
            }
            _ => CommandOutput::usage(invocation.spec.usage),
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

    fn signing_key(invocation: &Invocation) -> Result<Option<SigningKey>, String> {
        let key_hex = match invocation.flag("key") {
            Some(Some(value)) => Some(value.to_string()),
            Some(None) => return Err("--key requires a 32-byte hex value".into()),
            None => std::env::var("AIOS_STORE_SIGNING_KEY").ok(),
        };
        let Some(key_hex) = key_hex else {
            return Ok(None);
        };
        let bytes =
            hex::decode(key_hex.trim()).map_err(|error| format!("invalid key hex: {error}"))?;
        let array: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| "signing key must be 32 bytes (64 hex chars)".to_string())?;
        Ok(Some(SigningKey::from_bytes(&array)))
    }

    fn store_target(invocation: &Invocation) -> Result<(PathBuf, String, String), String> {
        let file = invocation.first_arg().ok_or_else(|| {
            "Usage: store <sign|publish> <file.wasm> [name] [version] [--key <secret_hex>]"
                .to_string()
        })?;
        let path = PathBuf::from(file);
        let default_name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "block".to_string());
        let name = invocation
            .arg(1)
            .map(str::to_string)
            .unwrap_or(default_name);
        let version = invocation
            .arg(2)
            .map(str::to_string)
            .unwrap_or_else(|| "1.0.0".to_string());
        Ok((path, name, version))
    }

    fn signed_manifest(
        binary: &[u8],
        name: &str,
        version: &str,
        key: &SigningKey,
    ) -> Result<ManifestInfo, String> {
        let mut manifest = ManifestInfo {
            name: name.to_string(),
            version: version.to_string(),
            description: "Managed from AIOS console".into(),
            author: "local-user".into(),
            capabilities: std::collections::HashSet::new(),
            wasm_size_bytes: binary.len() as u64,
            wasm_sha256: hex::encode(Sha256::digest(binary)),
            signature: None,
            store_url: None,
        };
        manifest.signature = Some(sign_manifest(&manifest, key));
        Ok(manifest)
    }

    fn safe_file_token(value: &str) -> String {
        let last = value
            .split(['/', '\\'])
            .rfind(|component| !component.is_empty())
            .unwrap_or("block");
        let token: String = last
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                    character
                } else {
                    '_'
                }
            })
            .collect();
        let trimmed = token.trim_matches('.');
        if trimmed.is_empty() {
            "block".to_string()
        } else {
            trimmed.to_string()
        }
    }

    fn store_sign(&self, invocation: &Invocation) -> CommandOutput {
        let (path, name, version) = match Self::store_target(invocation) {
            Ok(target) => target,
            Err(error) => return CommandOutput::text(&error),
        };
        let key = match Self::signing_key(invocation) {
            Ok(Some(key)) => key,
            Ok(None) => {
                return CommandOutput::text(
                    "No signing key: pass --key <secret_hex> or set AIOS_STORE_SIGNING_KEY",
                )
            }
            Err(error) => return CommandOutput::error(&error),
        };
        let binary = match std::fs::read(&path) {
            Ok(binary) => binary,
            Err(error) => {
                return CommandOutput::error(&format!("read '{}': {error}", path.display()))
            }
        };
        let manifest = match Self::signed_manifest(&binary, &name, &version, &key) {
            Ok(manifest) => manifest,
            Err(error) => return CommandOutput::error(&error),
        };
        let sidecar = path.with_file_name(format!(
            "{}_{}.json",
            Self::safe_file_token(&name),
            Self::safe_file_token(&version)
        ));
        let text = match serde_json::to_string_pretty(&manifest) {
            Ok(text) => text,
            Err(error) => return CommandOutput::error(&format!("serialize manifest: {error}")),
        };
        if let Err(error) = std::fs::write(&sidecar, text) {
            return CommandOutput::error(&format!("write '{}': {error}", sidecar.display()));
        }
        CommandOutput::ok(vec![
            format!("signed '{name}' v{version} -> {}", sidecar.display()),
            format!(
                "public key: {}",
                hex::encode(key.verifying_key().to_bytes())
            ),
        ])
    }

    fn store_publish(&self, invocation: &Invocation) -> CommandOutput {
        let (path, name, version) = match Self::store_target(invocation) {
            Ok(target) => target,
            Err(error) => return CommandOutput::text(&error),
        };
        let key = match Self::signing_key(invocation) {
            Ok(key) => key,
            Err(error) => return CommandOutput::error(&error),
        };
        let binary = match std::fs::read(&path) {
            Ok(binary) => binary,
            Err(error) => {
                return CommandOutput::error(&format!("read '{}': {error}", path.display()))
            }
        };
        let mut body = serde_json::json!({
            "name": name,
            "version": version,
            "description": "Published from AIOS console",
            "author": "local-user",
            "capabilities": [],
            "checksum_sha256": hex::encode(Sha256::digest(&binary)),
            "wasm_base64": base64::engine::general_purpose::STANDARD.encode(&binary),
            "signature": serde_json::Value::Null,
        });
        if let Some(key) = key {
            let manifest = match Self::signed_manifest(&binary, &name, &version, &key) {
                Ok(manifest) => manifest,
                Err(error) => return CommandOutput::error(&error),
            };
            match serde_json::to_value(manifest.signature) {
                Ok(signature) => body["signature"] = signature,
                Err(error) => {
                    return CommandOutput::error(&format!("serialize signature: {error}"))
                }
            }
        }
        let url = self.publish_url.clone().unwrap_or_else(|| {
            let port = std::env::var("AIOS_BRIDGE_PORT").unwrap_or_else(|_| "8080".to_string());
            format!("http://localhost:{port}/api/v1/store/publish")
        });
        let client = match reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
        {
            Ok(client) => client,
            Err(error) => return CommandOutput::error(&format!("publish client: {error}")),
        };
        let response = match client.post(&url).json(&body).send() {
            Ok(response) => response,
            Err(error) => return CommandOutput::error(&format!("publish failed: {error}")),
        };
        let status = response.status();
        let reply: serde_json::Value = match response.json() {
            Ok(reply) => reply,
            Err(error) => return CommandOutput::error(&format!("publish reply: {error}")),
        };
        if reply.get("success").and_then(serde_json::Value::as_bool) == Some(true) {
            let published = reply
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(name.as_str());
            let published_version = reply
                .get("version")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(version.as_str());
            CommandOutput::ok(vec![format!(
                "published '{published}' v{published_version}"
            )])
        } else {
            let error = reply
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown error");
            CommandOutput::error(&format!("publish failed: {error} (HTTP {status})"))
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
            "trust" => {
                let Some(source) = invocation.first_arg() else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let keys: Vec<String> = invocation.args[1..].to_vec();
                if keys.is_empty() {
                    return CommandOutput::usage(invocation.spec.usage);
                }
                match self.store.trust_source(source, &keys) {
                    Ok(()) => self.command_status(format!(
                        "trusted source '{source}' with {} key(s)",
                        keys.len()
                    )),
                    Err(error) => CommandOutput::error(&format!("store trust failed: {error}")),
                }
            }
            "verify" => {
                let name = invocation.first_arg().map(str::to_string);
                let blocks: Vec<_> = match name.as_deref() {
                    Some(name) => self.store.find_installed(name).into_iter().collect(),
                    None => self.store.list_installed(),
                };
                if blocks.is_empty() {
                    return CommandOutput::text("no installed blocks to verify");
                }
                let mut lines = vec!["BLOCK            SHA256  SIGNATURE".to_string()];
                let mut failures = 0usize;
                for block in blocks {
                    let sha = std::fs::read(&block.path)
                        .ok()
                        .and_then(|bytes| {
                            ManifestValidator::validate_sha256(&block.manifest, &bytes).ok()
                        })
                        .unwrap_or(false);
                    let signature = ManifestValidator::verify_signature(&block.manifest)
                        .ok()
                        .unwrap_or(false);
                    if !sha || !signature {
                        failures += 1;
                    }
                    lines.push(format!(
                        "{:<16} {:<7} {}",
                        block.manifest.name,
                        if sha { "OK" } else { "FAIL" },
                        if signature { "OK" } else { "NONE" }
                    ));
                }
                if failures > 0 {
                    lines.push(format!("{failures} block(s) failed verification"));
                }
                CommandOutput::ok(lines)
            }
            "sign" => self.store_sign(invocation),
            "publish" => self.store_publish(invocation),
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
            "readat" => {
                let path = match Self::path_arg(invocation, 0) {
                    Ok(path) => path,
                    Err(output) => return output,
                };
                let Some(offset) = invocation.arg(1).and_then(|raw| raw.parse::<u64>().ok()) else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let Some(max) = invocation.arg(2).and_then(|raw| raw.parse::<usize>().ok()) else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                match StoreManager::block_on(aios_vfs::operations::read_at(
                    &vfs, &path, offset, max,
                )) {
                    Ok(bytes) => {
                        let mut lines =
                            vec![format!("offset        {offset} ({} byte(s))", bytes.len())];
                        lines.extend(String::from_utf8_lossy(&bytes).lines().map(str::to_string));
                        CommandOutput::ok(lines)
                    }
                    Err(error) => CommandOutput::error(&format!("readat failed: {error}")),
                }
            }
            _ => CommandOutput::unavailable(&format!("vfs {subcommand} has no host backend yet")),
        }
    }

    fn sleep(&self, invocation: &Invocation) -> CommandOutput {
        let Some(seconds) = invocation
            .first_arg()
            .and_then(|value| value.parse::<u64>().ok())
        else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        std::thread::sleep(Duration::from_secs(seconds));
        CommandOutput::text(format!("slept {seconds}s"))
    }

    fn alias_command(&mut self, invocation: &Invocation) -> CommandOutput {
        let Some(definition) = invocation.args.first().map(|_| invocation.args.join(" ")) else {
            let mut lines = vec!["NAME        COMMAND".to_string()];
            for (name, target) in &self.aliases {
                lines.push(format!("{name:<12}{target}"));
            }
            lines.push(format!("Total: {} session alias(es)", self.aliases.len()));
            return CommandOutput::ok(lines);
        };
        let Some((name, target)) = definition.split_once('=') else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        if name.is_empty() || target.is_empty() || name.chars().any(char::is_whitespace) {
            return CommandOutput::usage(invocation.spec.usage);
        }
        let name = name.to_ascii_lowercase();
        self.aliases.insert(name.clone(), target.to_string());
        self.command_status(format!("alias '{name}' -> '{target}'"))
    }

    fn unalias_command(&mut self, invocation: &Invocation) -> CommandOutput {
        let Some(name) = invocation.first_arg() else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        match self.aliases.remove(&name.to_ascii_lowercase()) {
            Some(target) => self.command_status(format!("removed alias '{name}' -> '{target}'")),
            None => CommandOutput::error(&format!("no session alias '{name}'")),
        }
    }

    fn fault_resolve(&mut self, invocation: &Invocation) -> CommandOutput {
        let Some(pid) = parse_pid(invocation) else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        let Some(raw) = invocation.arg(1) else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        let action = match raw.to_ascii_lowercase().as_str() {
            "rollback" => RecoveryAction::Rollback,
            "terminate" => RecoveryAction::Terminate,
            "resume" => RecoveryAction::Resume,
            _ => return CommandOutput::usage(invocation.spec.usage),
        };
        let mut scheduler = guard(&self.scheduler);
        match scheduler.resolve_fault(pid, action) {
            Ok(()) => {
                drop(scheduler);
                self.command_status(format!(
                    "fault for PID {} resolved with {}",
                    pid.0,
                    action.name()
                ))
            }
            Err(error) => CommandOutput::error(&format!("fault resolve failed: {error}")),
        }
    }

    fn sched_command(&mut self, invocation: &Invocation, subcommand: &str) -> CommandOutput {
        match subcommand {
            "mode" => {
                let Some(raw) = invocation.first_arg() else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let mode = match raw.to_ascii_lowercase().as_str() {
                    "normal" => SchedulingMode::Normal,
                    "realtime" | "real-time" | "rt" => SchedulingMode::RealTime,
                    _ => return CommandOutput::usage(invocation.spec.usage),
                };
                let mut scheduler = guard(&self.scheduler);
                scheduler.set_scheduling_mode(mode);
                drop(scheduler);
                self.command_status(format!("scheduler mode set to {mode:?}"))
            }
            "quantum" | "aging" => {
                let Some(value) = invocation
                    .first_arg()
                    .and_then(|raw| raw.parse::<u64>().ok())
                else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let mut scheduler = guard(&self.scheduler);
                let replaced =
                    std::mem::replace(&mut *scheduler, Scheduler::new(self.config.total_ram_mb));
                *scheduler = if subcommand == "quantum" {
                    replaced.with_time_slice(value)
                } else {
                    replaced.with_aging_threshold(value)
                };
                drop(scheduler);
                self.command_status(format!("scheduler {subcommand} set to {value}ms"))
            }
            "rt-deadline" => {
                let Some(pid) = parse_pid(invocation) else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let Some(deadline) = invocation.arg(1).and_then(|raw| raw.parse::<u64>().ok())
                else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let mut scheduler = guard(&self.scheduler);
                scheduler.set_rt_deadline(pid, deadline);
                drop(scheduler);
                self.command_status(format!(
                    "real-time deadline {deadline}ms set for PID {}",
                    pid.0
                ))
            }
            "rt-clear" => {
                let mut scheduler = guard(&self.scheduler);
                let targets: Vec<ProcessId> = match parse_pid(invocation) {
                    Some(pid) => vec![pid],
                    None => scheduler.rt_deadlines().keys().copied().collect(),
                };
                for pid in &targets {
                    scheduler.clear_rt_deadline(*pid);
                }
                drop(scheduler);
                self.command_status(format!("cleared {} real-time deadline(s)", targets.len()))
            }
            _ => CommandOutput::unavailable(&format!("sched {subcommand} has no host backend yet")),
        }
    }

    fn net_validate(&self, invocation: &Invocation) -> CommandOutput {
        let Some(raw) = invocation.first_arg() else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        let kind = invocation.arg(1).map(|value| value.to_ascii_lowercase());
        let valid = match kind.as_deref() {
            Some("ip") => Some(("ip address", aios_net_config::validation::validate_ip(raw))),
            Some("port") => raw.parse::<u16>().ok().and_then(|port| {
                aios_net_config::validation::validate_port(port)
                    .ok()
                    .map(|_| ("port", true))
            }),
            Some("url") => aios_net_config::validation::validate_url(raw)
                .ok()
                .map(|_| ("url", true)),
            Some(other) => {
                return CommandOutput::usage(&format!("net validate: unknown kind '{other}'"))
            }
            None => {
                if aios_net_config::validation::validate_ip(raw) {
                    Some(("ip address", true))
                } else if aios_net_config::validation::validate_url(raw).is_ok() {
                    Some(("url", true))
                } else {
                    None
                }
            }
        };
        match valid {
            Some((label, true)) => CommandOutput::text(format!("'{raw}' is a valid {label}")),
            _ => CommandOutput::error(&format!("'{raw}' is not a valid ip/port/url")),
        }
    }

    fn net_config(&self) -> CommandOutput {
        let store =
            aios_net_config::store::NetworkConfigStore::new(self.config.net_config_path.clone());
        match store.load() {
            Ok(Some(config)) => CommandOutput::text(config.to_json()),
            Ok(None) => CommandOutput::text("no persisted network configuration"),
            Err(error) => CommandOutput::error(&format!("net config failed: {error}")),
        }
    }

    fn net_status(&self) -> CommandOutput {
        let link = self.wifi.status();
        CommandOutput::ok(vec![
            format!("state         {:?}", link.state),
            format!(
                "ssid          {}",
                link.ssid.unwrap_or_else(|| "-".to_string())
            ),
            format!(
                "ipv4          {}",
                link.ipv4
                    .map(|ip| ip.to_string())
                    .unwrap_or_else(|| "-".to_string())
            ),
            format!(
                "gateway       {}",
                link.gateway
                    .map(|ip| ip.to_string())
                    .unwrap_or_else(|| "-".to_string())
            ),
            format!(
                "dns           {}",
                link.dns
                    .map(|ip| ip.to_string())
                    .unwrap_or_else(|| "-".to_string())
            ),
            format!("rssi          {} dBm", link.rssi_dbm),
        ])
    }

    fn net_lease(&self) -> CommandOutput {
        CommandOutput::text("no DHCP lease acquired in this session")
    }

    fn caps_command(&mut self, invocation: &Invocation, subcommand: &str) -> CommandOutput {
        match subcommand {
            "list" => {
                if self.cap_token_ids.is_empty() {
                    return CommandOutput::text("no capability tokens issued");
                }
                let mut lines = vec!["BLOCK  EXPIRES_IN_MS  CAPABILITIES".to_string()];
                for id in &self.cap_token_ids {
                    if let Some(token) = self.access_control.get_token(*id) {
                        let caps = token
                            .capabilities
                            .iter()
                            .map(|cap| cap.name())
                            .collect::<Vec<_>>()
                            .join(", ");
                        lines.push(format!("{:<6} {:<14} {}", id, token.remaining_ms(), caps));
                    }
                }
                CommandOutput::ok(lines)
            }
            "issue" => {
                let Some(id) = invocation
                    .first_arg()
                    .and_then(|raw| raw.parse::<u32>().ok())
                else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let caps: Vec<SecCapability> = invocation.args[1..]
                    .iter()
                    .filter_map(|raw| parse_sec_capability(raw))
                    .collect();
                if caps.is_empty() {
                    return CommandOutput::usage(invocation.spec.usage);
                }
                match self.access_control.issue_token(id, caps.clone()) {
                    Ok(_) => {
                        if !self.cap_token_ids.contains(&id) {
                            self.cap_token_ids.push(id);
                        }
                        self.command_status(format!(
                            "issued {} capability token(s) to block {id}",
                            caps.len()
                        ))
                    }
                    Err(error) => CommandOutput::error(&format!("caps issue failed: {error}")),
                }
            }
            "revoke" => {
                let Some(id) = invocation
                    .first_arg()
                    .and_then(|raw| raw.parse::<u32>().ok())
                else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                if self.access_control.revoke_token(id) {
                    self.cap_token_ids.retain(|value| *value != id);
                    self.command_status(format!("revoked capability token for block {id}"))
                } else {
                    CommandOutput::error(&format!("no capability token for block {id}"))
                }
            }
            "check" => {
                let Some(id) = invocation
                    .first_arg()
                    .and_then(|raw| raw.parse::<u32>().ok())
                else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let Some(cap) = invocation.arg(1).and_then(parse_sec_capability) else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                match self.access_control.check_permission(id, &cap) {
                    Ok(()) => CommandOutput::text(format!("block {id} has {}", cap.name())),
                    Err(error) => {
                        CommandOutput::error(&format!("block {id} denied {}: {error}", cap.name()))
                    }
                }
            }
            _ => CommandOutput::unavailable(&format!("caps {subcommand} has no host backend yet")),
        }
    }

    fn sandbox_command(&mut self, invocation: &Invocation, subcommand: &str) -> CommandOutput {
        let Some(id) = invocation
            .first_arg()
            .and_then(|raw| raw.parse::<u32>().ok())
        else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        match subcommand {
            "start" => {
                let memory_mb = invocation
                    .arg(1)
                    .and_then(|raw| raw.parse::<u64>().ok())
                    .unwrap_or(64);
                let max_syscalls = invocation
                    .arg(2)
                    .and_then(|raw| raw.parse::<u64>().ok())
                    .unwrap_or(1000);
                let caps: Vec<SecCapability> = invocation.args[3..]
                    .iter()
                    .filter_map(|raw| parse_sec_capability(raw))
                    .collect();
                let mut sandbox = Sandbox::new(id, caps, memory_mb * 1024 * 1024, max_syscalls);
                sandbox.start();
                self.sandboxes.insert(id, sandbox);
                self.command_status(format!(
                    "sandbox for block {id} started ({memory_mb} MB, {max_syscalls} syscalls)"
                ))
            }
            "stop" => match self.sandboxes.get_mut(&id) {
                Some(sandbox) => {
                    sandbox.terminate();
                    self.command_status(format!("sandbox for block {id} terminated"))
                }
                None => CommandOutput::error(&format!("no sandbox for block {id}")),
            },
            "log" => match self.sandboxes.get(&id) {
                Some(sandbox) => {
                    let log = sandbox.syscall_log();
                    if log.is_empty() {
                        return CommandOutput::text(format!("no syscalls logged for block {id}"));
                    }
                    let mut lines = vec!["TIME_MS          SYSCALL            ALLOWED".to_string()];
                    for entry in log {
                        lines.push(format!(
                            "{:<16}  {:<18} {}",
                            entry.timestamp_ms, entry.name, entry.allowed
                        ));
                    }
                    CommandOutput::ok(lines)
                }
                None => CommandOutput::error(&format!("no sandbox for block {id}")),
            },
            "mem" => match self.sandboxes.get(&id) {
                Some(sandbox) => {
                    let (used, limit) = sandbox.memory_usage();
                    CommandOutput::ok(vec![
                        format!("used       {used} bytes"),
                        format!("limit      {limit} bytes"),
                        format!("state      {:?}", sandbox.state()),
                    ])
                }
                None => CommandOutput::error(&format!("no sandbox for block {id}")),
            },
            _ => {
                CommandOutput::unavailable(&format!("sandbox {subcommand} has no host backend yet"))
            }
        }
    }

    fn hw_protect_command(&mut self, invocation: &Invocation, subcommand: &str) -> CommandOutput {
        match subcommand {
            "status" => {
                let mut lines = vec!["hw-protect    protection status".to_string()];
                let protections = self.hw_bridge.active_protections();
                lines.push(format!("assignments   {}", protections.len()));
                for protection in protections {
                    lines.push(format!("  {protection:?}"));
                }
                lines.push(format!(
                    "mpk           available={} keys={}",
                    self.mpk.as_ref().is_some_and(|mpk| mpk.supported()),
                    self.mpk.as_ref().map_or(0, |mpk| mpk.active_keys_count())
                ));
                lines.push(format!(
                    "iommu         available={} status={:?}",
                    IommuInfo::detect().is_ok_and(|info| info.available),
                    self.iommu.status()
                ));
                lines.push(format!(
                    "tee           supported={} platform={:?}",
                    self.tee.supported, self.tee.platform
                ));
                CommandOutput::ok(lines)
            }
            "mpk" | "tee" | "iommu" => {
                if invocation.args.len() >= 2 {
                    let Some(id) = invocation
                        .first_arg()
                        .and_then(|raw| raw.parse::<u32>().ok())
                    else {
                        return CommandOutput::usage(invocation.spec.usage);
                    };
                    let value = invocation.arg(1).unwrap_or("");
                    let result = match subcommand {
                        "mpk" => value
                            .parse::<u16>()
                            .ok()
                            .and_then(|key| self.hw_bridge.assign_mpk_protection(id, key).ok())
                            .is_some(),
                        "tee" => self
                            .hw_bridge
                            .assign_tee_protection(id, value.to_string())
                            .is_ok(),
                        _ => value
                            .parse::<u32>()
                            .ok()
                            .and_then(|domain| {
                                self.hw_bridge.assign_iommu_protection(id, domain).ok()
                            })
                            .is_some(),
                    };
                    if result {
                        self.command_status(format!(
                            "assigned {subcommand} protection to block {id}"
                        ))
                    } else {
                        CommandOutput::error(&format!("invalid {subcommand} assignment arguments"))
                    }
                } else {
                    CommandOutput::unavailable(&format!(
                        "hw-protect {subcommand} requires <block> <id>; see 'hw-protect status'"
                    ))
                }
            }
            "clear" => {
                let count = self.hw_bridge.protection_count();
                match invocation.first_arg() {
                    Some(raw) if raw.eq_ignore_ascii_case("all") => self.hw_bridge.clear(),
                    Some(raw) => match raw.parse::<u32>() {
                        Ok(id) => {
                            self.hw_bridge.remove_protection(id);
                        }
                        Err(_) => return CommandOutput::usage(invocation.spec.usage),
                    },
                    None => self.hw_bridge.clear(),
                }
                self.command_status(format!("cleared hardware protections (had {count})"))
            }
            _ => CommandOutput::unavailable(&format!(
                "hw-protect {subcommand} has no host backend yet"
            )),
        }
    }

    fn wasm_command(&mut self, invocation: &Invocation, subcommand: &str) -> CommandOutput {
        match subcommand {
            "compile" => {
                let Some(path) = invocation.first_arg() else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let bytes = match std::fs::read(path) {
                    Ok(bytes) => bytes,
                    Err(error) => return CommandOutput::error(&format!("read '{path}': {error}")),
                };
                let sandbox = match WasmSandbox::new(SandboxConfig::default()) {
                    Ok(sandbox) => sandbox,
                    Err(error) => return CommandOutput::error(&format!("wasm sandbox: {error}")),
                };
                let text = String::from_utf8(bytes.clone()).ok();
                let compiled = match text
                    .as_deref()
                    .filter(|text| text.trim_start().starts_with('('))
                {
                    Some(wat) => sandbox.compile_wat(wat).map(|_| ()),
                    None => sandbox.compile_any(&bytes).map(|_| ()),
                };
                match compiled {
                    Ok(()) => {
                        self.wasm_source = Some(bytes.clone());
                        self.command_status(format!(
                            "compiled WASM module from '{path}' ({} bytes)",
                            bytes.len()
                        ))
                    }
                    Err(error) => CommandOutput::error(&format!("wasm compile failed: {error}")),
                }
            }
            "call" | "mem" | "grow" => {
                let Some(function) = invocation.first_arg() else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let source = match self.wasm_source.as_ref() {
                    Some(source) => source.clone(),
                    None => {
                        return CommandOutput::error(
                            "no compiled module; run 'wasm compile <file>'",
                        )
                    }
                };
                let mut block = match WasmBlock::new(
                    "console".to_string(),
                    "1.0.0".to_string(),
                    &source,
                    SandboxConfig::default(),
                    IsolationConfig::default(),
                ) {
                    Ok(block) => block,
                    Err(error) => return CommandOutput::error(&format!("wasm block: {error}")),
                };
                let mut store = match block.create_store() {
                    Ok(store) => store,
                    Err(error) => return CommandOutput::error(&format!("wasm store: {error}")),
                };
                if let Err(error) = block.instantiate(&mut store) {
                    return CommandOutput::error(&format!("wasm instantiate: {error}"));
                }
                match subcommand {
                    "call" => match block.call_func(&mut store, function, &[]) {
                        Ok(values) => CommandOutput::ok(
                            values
                                .iter()
                                .enumerate()
                                .map(|(index, value)| format!("result[{index}]  {value:?}"))
                                .collect(),
                        ),
                        Err(error) => {
                            CommandOutput::error(&format!("wasm call '{function}': {error}"))
                        }
                    },
                    "mem" => {
                        let stats = block.memory_stats();
                        CommandOutput::ok(vec![
                            format!("memory        {} bytes", block.memory_size(&mut store)),
                            format!("limit         {} pages", stats.limit_pages),
                            format!("fuel limit    {}", stats.fuel_limit),
                            format!("instantiated  {}", stats.is_instantiated),
                        ])
                    }
                    _ => {
                        let pages = match function.parse::<u64>() {
                            Ok(pages) => pages,
                            Err(_) => return CommandOutput::usage(invocation.spec.usage),
                        };
                        match block.grow_memory(&mut store, pages) {
                            Ok(()) => {
                                self.command_status(format!("grew WASM memory by {pages} pages"))
                            }
                            Err(error) => {
                                CommandOutput::error(&format!("wasm grow failed: {error}"))
                            }
                        }
                    }
                }
            }
            _ => CommandOutput::unavailable(&format!("wasm {subcommand} has no host backend yet")),
        }
    }

    fn wasi_policy(&mut self, invocation: &Invocation) -> CommandOutput {
        let Some(mode) = invocation.first_arg() else {
            return CommandOutput::ok(vec![
                format!("default      {:?}", self.wasi_filter.default_policy),
                format!("allowed      {}", self.wasi_filter.total_allowed()),
                format!("denied       {}", self.wasi_filter.total_denied()),
                format!("logged       {}", self.wasi_filter.total_logged()),
            ]);
        };
        self.wasi_filter = match mode.to_ascii_lowercase().as_str() {
            "permissive" => WasiFilter::permissive(),
            "restrictive" => WasiFilter::restrictive(),
            "no-network" | "nonetwork" => WasiFilter::no_network(),
            "allow" => WasiFilter::new(aios_wasm::SyscallPolicy::Allow),
            "deny" => WasiFilter::new(aios_wasm::SyscallPolicy::Deny),
            "log" => WasiFilter::new(aios_wasm::SyscallPolicy::Log),
            _ => return CommandOutput::usage(invocation.spec.usage),
        };
        self.command_status(format!("WASI policy set to '{mode}'"))
    }

    fn exec_command(&mut self, invocation: &Invocation) -> CommandOutput {
        match invocation.subcommand {
            Some("detect") | Some("caps") => {
                let Some(target) = invocation.first_arg() else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let kind = match std::fs::read(target) {
                    Ok(bytes) => ExecutableType::from_bytes(&bytes),
                    Err(_) => ExecutableType::from_extension(target),
                };
                if invocation.subcommand == Some("detect") {
                    CommandOutput::ok(vec![
                        format!("file           {target}"),
                        format!("type           {kind:?}"),
                        format!("subsystem      {}", kind.subsystem_name()),
                    ])
                } else {
                    let caps = kind.required_capabilities();
                    let mut lines = vec!["CAPABILITY".to_string()];
                    for cap in caps {
                        lines.push(format!("{cap:?}"));
                    }
                    CommandOutput::ok(lines)
                }
            }
            None => {
                let Some(program) = invocation.first_arg() else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                match std::process::Command::new(program)
                    .args(&invocation.args[1..])
                    .output()
                {
                    Ok(output) => {
                        let mut lines = vec![format!("status         {}", output.status)];
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        lines.extend(stdout.lines().map(str::to_string));
                        lines.extend(stderr.lines().map(|line| format!("stderr: {line}")));
                        CommandOutput::ok(lines)
                    }
                    Err(error) => {
                        CommandOutput::error(&format!("exec '{program}' failed: {error}"))
                    }
                }
            }
            _ => CommandOutput::unavailable(&format!(
                "exec {} has no host backend yet",
                invocation.display_name()
            )),
        }
    }

    fn deps_command(&mut self, invocation: &Invocation, subcommand: &str) -> CommandOutput {
        let Some(target) = invocation.first_arg() else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        let name = Path::new(target)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| target.to_string());
        let kind = ExecutableType::from_extension(target);
        let symbols = invocation.args[1..].to_vec();
        let mut healer = DependencyHealer::new(0);
        match subcommand {
            "scan" => {
                let result = healer.scan_dependencies(&name, kind, &symbols);
                let mut lines = vec![
                    format!("executable     {target}"),
                    format!("type           {kind:?}"),
                    format!("satisfied      {}", result.already_satisfied.len()),
                    format!("missing        {}", result.missing.len()),
                ];
                for dependency in result.missing {
                    lines.push(format!(
                        "  {} ({:?}) required by {}",
                        dependency.name, dependency.dep_type, dependency.required_by
                    ));
                }
                CommandOutput::ok(lines)
            }
            "heal" => match healer.heal_dependencies(&name, kind, &symbols, false) {
                Ok(resolutions) => {
                    let mut lines = vec![format!(
                        "resolved       {} dependency(ies)",
                        resolutions.len()
                    )];
                    for resolution in resolutions {
                        lines.push(format!(
                            "  {} -> {}",
                            resolution.original_name, resolution.resolved_path
                        ));
                    }
                    self.note(format!("healed dependencies for '{target}'"));
                    CommandOutput::ok(lines)
                }
                Err(error) => CommandOutput::error(&format!("deps heal failed: {error}")),
            },
            _ => CommandOutput::unavailable(&format!("deps {subcommand} has no host backend yet")),
        }
    }

    fn compat_command(&mut self, invocation: &Invocation, subcommand: &str) -> CommandOutput {
        match subcommand {
            "list" => {
                let processes = self.compat_manager.active_processes();
                if processes.is_empty() {
                    return CommandOutput::text("no compatibility processes");
                }
                let mut lines = vec!["PID  NAME              TYPE         MEM_MB".to_string()];
                for process in processes {
                    lines.push(format!(
                        "{:<4} {:<17} {:<12} {}",
                        process.pid,
                        process.name,
                        format!("{:?}", process.exe_type),
                        process.memory_used_mb
                    ));
                }
                CommandOutput::ok(lines)
            }
            "kill" => {
                let Some(pid) = invocation
                    .first_arg()
                    .and_then(|raw| raw.parse::<u64>().ok())
                else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                match self.compat_manager.terminate_process(pid) {
                    Ok(()) => {
                        self.command_status(format!("compatibility process {pid} terminated"))
                    }
                    Err(error) => CommandOutput::error(&format!("compat kill failed: {error}")),
                }
            }
            _ => {
                CommandOutput::unavailable(&format!("compat {subcommand} has no host backend yet"))
            }
        }
    }

    fn context_command(&mut self, _invocation: &Invocation, subcommand: &str) -> CommandOutput {
        match subcommand {
            "stats" => CommandOutput::ok(vec![
                format!("total        {}", self.context_store.total_entries()),
                format!("telemetry    {}", self.context_store.telemetry().count()),
                format!("workflows    {}", self.context_store.workflows().count()),
                format!("stability    {}", self.context_store.stability().count()),
                format!("compact      {}", self.context_store.should_compact()),
            ]),
            "export" => {
                let export = self.context_store.export_all();
                CommandOutput::ok(vec![
                    format!("telemetry    {}", export.telemetry_entries),
                    format!("workflows    {}", export.workflow_entries),
                    format!("stability    {}", export.stability_entries),
                ])
            }
            "compact" => {
                let report = self.context_store.compact();
                self.command_status(format!(
                    "compacted context: {} telemetry, {} workflow, {} stability entr(ies)",
                    report.telemetry_pruned, report.workflows_pruned, report.stability_pruned
                ))
            }
            "persist" => {
                let store = PersistentStore::new(&self.config.context_db_path);
                match store.save_all(
                    self.context_store.telemetry(),
                    self.context_store.workflows(),
                    self.context_store.stability(),
                ) {
                    Ok(summary) => self.command_status(format!(
                        "persisted {} telemetry, {} workflow, {} stability entr(ies) to {}",
                        summary.telemetry_entries,
                        summary.workflow_entries,
                        summary.stability_entries,
                        self.config.context_db_path.display()
                    )),
                    Err(error) => CommandOutput::error(&format!("context persist failed: {error}")),
                }
            }
            _ => {
                CommandOutput::unavailable(&format!("context {subcommand} has no host backend yet"))
            }
        }
    }

    fn workflow_command(&mut self, invocation: &Invocation, subcommand: &str) -> CommandOutput {
        match subcommand {
            "list" => {
                let workflows = self.context_store.workflows();
                let mut names: Vec<_> = workflows.profiles.keys().collect();
                names.sort();
                if names.is_empty() {
                    return CommandOutput::text("no workflow profiles");
                }
                let mut lines = vec!["NAME                 USES  TRIGGERS".to_string()];
                for name in names {
                    let profile = &workflows.profiles[name];
                    lines.push(format!(
                        "{:<20} {:<5} {}",
                        profile.name,
                        profile.usage_count,
                        profile.trigger_blocks.join(", ")
                    ));
                }
                CommandOutput::ok(lines)
            }
            "priority" => {
                let Some(profile_name) = invocation.first_arg() else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let Some(process) = invocation.arg(1) else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let workflows = self.context_store.workflows_mut();
                let Some(profile) = workflows.get_mut(profile_name) else {
                    return CommandOutput::error(&format!("no workflow profile '{profile_name}'"));
                };
                match invocation.arg(2) {
                    Some(value) => match value.parse::<u8>().ok() {
                        Some(priority) => {
                            profile.set_priority(process, priority);
                            self.command_status(format!(
                                "priority of '{process}' in '{profile_name}' set to {priority}"
                            ))
                        }
                        None => CommandOutput::usage(invocation.spec.usage),
                    },
                    None => match profile.get_priority(process) {
                        Some(priority) => {
                            CommandOutput::text(format!("{profile_name}/{process} = {priority}"))
                        }
                        None => CommandOutput::text(format!(
                            "no priority for '{process}' in '{profile_name}'"
                        )),
                    },
                }
            }
            _ => CommandOutput::unavailable(&format!(
                "workflow {subcommand} has no host backend yet"
            )),
        }
    }

    fn stability_command(&mut self, invocation: &Invocation, subcommand: &str) -> CommandOutput {
        let stability = self.context_store.stability();
        match subcommand {
            "list" => {
                if stability.scores.is_empty() {
                    return CommandOutput::text("no stability scores");
                }
                let mut lines = vec!["BLOCK            VERSION    SCORE   CRASHES".to_string()];
                for score in &stability.scores {
                    lines.push(format!(
                        "{:<16} {:<10} {:<7.2} {}",
                        score.block_name, score.binary_version, score.score, score.crash_count
                    ));
                }
                CommandOutput::ok(lines)
            }
            "best" => {
                let Some(name) = invocation.first_arg() else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                match stability.best_version(name) {
                    Some(score) => CommandOutput::ok(vec![
                        format!("block          {}", score.block_name),
                        format!("version        {}", score.binary_version),
                        format!("score          {:.2}", score.score),
                    ]),
                    None => CommandOutput::text(format!("no stability score for '{name}'")),
                }
            }
            "worst" => match stability.worst_score() {
                Some(score) => CommandOutput::ok(vec![
                    format!("block          {}", score.block_name),
                    format!("version        {}", score.binary_version),
                    format!("score          {:.2}", score.score),
                    format!("crashes         {}", score.crash_count),
                ]),
                None => CommandOutput::text("no stability scores"),
            },
            _ => CommandOutput::unavailable(&format!(
                "stability {subcommand} has no host backend yet"
            )),
        }
    }

    fn trace_list(&self) -> CommandOutput {
        let spans = self.trace_context.spans();
        if spans.is_empty() {
            return CommandOutput::text("no trace spans");
        }
        let mut lines = vec!["TRACE_ID         OPERATION         DURATION_MS  STATUS".to_string()];
        for span in spans {
            let status = match &span.status {
                SpanStatus::Ok => "Ok".to_string(),
                SpanStatus::Timeout => "Timeout".to_string(),
                SpanStatus::Error(error) => format!("Error({error})"),
            };
            lines.push(format!(
                "{:<16}  {:<17} {:<12} {}",
                span.trace_id,
                span.operation,
                span.end_time_ms
                    .map(|end| end.saturating_sub(span.start_time_ms))
                    .unwrap_or(0),
                status
            ));
        }
        CommandOutput::ok(lines)
    }

    fn crash_report(&mut self, invocation: &Invocation) -> CommandOutput {
        let Some(kind) = invocation.first_arg() else {
            return CommandOutput::usage(invocation.spec.usage);
        };
        let message = invocation.args[1..].join(" ");
        if message.is_empty() {
            return CommandOutput::usage(invocation.spec.usage);
        }
        let kind = match kind.to_ascii_lowercase().as_str() {
            "panic" => CrashKind::Panic,
            "watchdog" | "watchdog_timeout" => CrashKind::WatchdogTimeout,
            "oom" => CrashKind::OOM,
            "block" | "block_crash" => CrashKind::BlockCrash,
            _ => CrashKind::Unknown,
        };
        let report = self
            .crash_reporter
            .generate_report(kind, "console", &message, "", "", false);
        self.command_status(format!("crash report {} generated", report.id))
    }

    fn ensure_autohal(&mut self) -> Result<&mut AutohalEngine, CommandOutput> {
        if self.autohal.is_none() {
            let config = EngineConfig {
                store_root: self.config.blocks_dir.join("drivers"),
                ..EngineConfig::default()
            };
            self.autohal =
                Some(AutohalEngine::new(config).map_err(|error| {
                    CommandOutput::error(&format!("driver engine init: {error}"))
                })?);
        }
        Ok(self.autohal.as_mut().expect("driver engine initialized"))
    }

    fn driver_command(&mut self, invocation: &Invocation, subcommand: &str) -> CommandOutput {
        let (_, profile) = self.hardware();
        let fingerprints = extract_fingerprints(&profile);
        match subcommand {
            "list" => {
                let engine = match self.ensure_autohal() {
                    Ok(engine) => engine,
                    Err(output) => return output,
                };
                let views = engine.device_views();
                if views.is_empty() {
                    return CommandOutput::text("no driver devices registered");
                }
                let mut lines = vec![
                    "DRIVER           NAME                 STATE         FAILURES".to_string(),
                ];
                for view in views {
                    lines.push(format!(
                        "{:<16} {:<20} {:<13} {}",
                        view.driver_id,
                        view.driver_name,
                        format!("{:?}", view.state),
                        view.failures
                    ));
                }
                CommandOutput::ok(lines)
            }
            "failures" => {
                let engine = match self.ensure_autohal() {
                    Ok(engine) => engine,
                    Err(output) => return output,
                };
                let index = engine.store().index();
                match invocation.first_arg() {
                    Some(id) => {
                        CommandOutput::text(format!("{id}: {} failure(s)", index.failures_for(id)))
                    }
                    None => {
                        let mut failures: Vec<_> = index
                            .failures
                            .iter()
                            .filter(|(_, count)| **count > 0)
                            .collect();
                        failures.sort_by(|left, right| left.0.cmp(right.0));
                        if failures.is_empty() {
                            return CommandOutput::text("no driver failures recorded");
                        }
                        let mut lines = vec!["DRIVER           FAILURES".to_string()];
                        for (id, count) in failures {
                            lines.push(format!("{id:<16} {count}"));
                        }
                        CommandOutput::ok(lines)
                    }
                }
            }
            "remove" => {
                let Some(id) = invocation.first_arg() else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let engine = match self.ensure_autohal() {
                    Ok(engine) => engine,
                    Err(output) => return output,
                };
                if engine.uninstall_driver(id) {
                    self.command_status(format!("driver '{id}' removed"))
                } else {
                    CommandOutput::error(&format!("driver '{id}' not found"))
                }
            }
            "rollback" | "install" | "provision" => {
                let engine = match self.ensure_autohal() {
                    Ok(engine) => engine,
                    Err(output) => return output,
                };
                if let Some(target) = invocation.first_arg() {
                    let fingerprint = fingerprints
                        .iter()
                        .find(|candidate| {
                            candidate.key() == target || candidate.driver_id() == target
                        })
                        .cloned();
                    let Some(fingerprint) = fingerprint else {
                        return CommandOutput::error(&format!(
                            "no hardware fingerprint matching '{target}'"
                        ));
                    };
                    if subcommand == "rollback" {
                        engine.rollback_to_generic(&fingerprint);
                        self.command_status(format!(
                            "rolled back '{}' to the generic driver",
                            fingerprint.display_name()
                        ))
                    } else {
                        engine.provision_blocking(fingerprint);
                        self.command_status(format!(
                            "provisioning attempted for '{}'; check 'driver list'",
                            target
                        ))
                    }
                } else {
                    engine.rescan(&profile);
                    self.command_status(format!(
                        "hardware rescan complete ({} fingerprint(s))",
                        fingerprints.len()
                    ))
                }
            }
            _ => {
                CommandOutput::unavailable(&format!("driver {subcommand} has no host backend yet"))
            }
        }
    }

    fn hotplug_command(&mut self, _invocation: &Invocation, subcommand: &str) -> CommandOutput {
        match subcommand {
            "start" => {
                self.hotplug = Some(HotplugMonitor::start(HotplugConfig::default()));
                self.command_status("hotplug monitor started")
            }
            "events" => match self.hotplug.as_ref() {
                Some(monitor) => match monitor.try_recv() {
                    Some(event) => {
                        let action = if event.is_added() { "added" } else { "removed" };
                        CommandOutput::text(format!(
                            "{} {}",
                            action,
                            event.fingerprint().display_name()
                        ))
                    }
                    None => CommandOutput::text("no hotplug events"),
                },
                None => CommandOutput::error("hotplug monitor is not running"),
            },
            "drain" => match self.hotplug.as_ref() {
                Some(monitor) => self.command_status(format!(
                    "drained {} hotplug event(s)",
                    monitor.drain().len()
                )),
                None => CommandOutput::error("hotplug monitor is not running"),
            },
            _ => {
                CommandOutput::unavailable(&format!("hotplug {subcommand} has no host backend yet"))
            }
        }
    }

    fn wifi_command(&mut self, invocation: &Invocation, subcommand: &str) -> CommandOutput {
        match subcommand {
            "status" => self.net_status(),
            "scan" => match StoreManager::block_on(self.wifi.scan()) {
                Ok(networks) => {
                    if networks.is_empty() {
                        return CommandOutput::text("no Wi-Fi networks found");
                    }
                    let mut lines = vec!["SSID                 RSSI  SECURITY  BAND".to_string()];
                    for network in networks {
                        lines.push(format!(
                            "{:<20} {:<5} {:<9} {:?}",
                            network.ssid,
                            network.rssi_dbm,
                            network.security.label(),
                            network.band
                        ));
                    }
                    CommandOutput::ok(lines)
                }
                Err(error) => CommandOutput::error(&format!("wifi scan failed: {error}")),
            },
            "connect" => {
                let Some(ssid) = invocation.first_arg() else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let password = invocation.arg(1);
                match StoreManager::block_on(self.wifi.connect(ssid, password)) {
                    Ok(link) => self.command_status(format!(
                        "connected to '{ssid}' ({:?}, RSSI {} dBm)",
                        link.state, link.rssi_dbm
                    )),
                    Err(error) => CommandOutput::error(&format!("wifi connect failed: {error}")),
                }
            }
            "disconnect" => match StoreManager::block_on(self.wifi.disconnect()) {
                Ok(()) => self.command_status("Wi-Fi disconnected"),
                Err(error) => CommandOutput::error(&format!("wifi disconnect failed: {error}")),
            },
            "sim" => {
                self.wifi = NetManager::new(WifiBackend::Simulated(SimulatedWifi::default()), None);
                self.command_status("Wi-Fi backend set to simulated")
            }
            "real" => {
                self.wifi = NetManager::new(WifiBackend::Host, None);
                self.command_status("Wi-Fi backend set to host")
            }
            _ => CommandOutput::unavailable(&format!("wifi {subcommand} has no host backend yet")),
        }
    }

    fn hub_command(&mut self, invocation: &Invocation, subcommand: &str) -> CommandOutput {
        match subcommand {
            "status" => {
                let snapshot = StoreManager::block_on(self.sys_hub.refresh_status());
                let mut lines = vec![snapshot.status_line()];
                lines.push(format!("layout       {}", snapshot.layout));
                lines.push(format!("battery      {:?}", snapshot.battery_percent));
                lines.push(format!("ac online    {:?}", snapshot.ac_online));
                lines.push(format!("max temp     {:.1} C", snapshot.max_temp_c));
                lines.push(format!("throttled    {}", snapshot.throttled));
                CommandOutput::ok(lines)
            }
            "scan" => match StoreManager::block_on(self.sys_hub.scan_networks()) {
                Ok(networks) => {
                    if networks.is_empty() {
                        return CommandOutput::text("no Wi-Fi networks found");
                    }
                    let mut lines = vec!["SSID                 RSSI  SECURITY".to_string()];
                    for network in networks {
                        lines.push(format!(
                            "{:<20} {:<5} {}",
                            network.ssid,
                            network.rssi_dbm,
                            network.security.label()
                        ));
                    }
                    CommandOutput::ok(lines)
                }
                Err(error) => CommandOutput::error(&format!("hub scan failed: {error}")),
            },
            "wifi" => {
                let Some(ssid) = invocation.first_arg() else {
                    return CommandOutput::usage(invocation.spec.usage);
                };
                let password = invocation.arg(1);
                match StoreManager::block_on(self.sys_hub.connect_wifi(ssid, password)) {
                    Ok(link) => {
                        self.command_status(format!("hub connected to '{ssid}' ({:?})", link.state))
                    }
                    Err(error) => CommandOutput::error(&format!("hub wifi failed: {error}")),
                }
            }
            "layout" => {
                let indicator = StoreManager::block_on(self.sys_hub.layout_indicator());
                CommandOutput::text(indicator)
            }
            _ => CommandOutput::unavailable(&format!("hub {subcommand} has no host backend yet")),
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

fn parse_sec_capability(value: &str) -> Option<SecCapability> {
    let upper = value.trim().to_ascii_uppercase();
    let upper = upper.strip_prefix("CAP_").unwrap_or(&upper);
    SecCapability::all_variants()
        .into_iter()
        .find(|capability| capability.name().trim_start_matches("CAP_") == upper)
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
