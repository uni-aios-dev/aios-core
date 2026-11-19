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
use aios_core::block::BlockId;
use aios_net_config::block::NetSettingsBlock;
use aios_net_config::config::NetworkConfig;
use aios_process_mgr::scheduler::Scheduler;
use aios_process_mgr::task::{Priority, ProcessId};
use aios_store::manager::StoreManager;
use aios_vfs::vfs::{AiosVfs, VfsPath, VirtualFileSystem};

use crate::backend::CommandBackend;
use crate::capability::{BackendKind, CapabilitySet};
use crate::output::CommandOutput;
use crate::registry::{help, Invocation};

/// Block id used by the host network settings block.
const NET_BLOCK_ID: u32 = 7;
/// Maximum number of remembered input lines.
const HISTORY_LIMIT: usize = 200;

/// Construction parameters for [`HostBackend`].
#[derive(Debug, Clone)]
pub struct HostBackendConfig {
    /// Version string printed by `version`.
    pub version: String,
    /// Directory holding store block binaries.
    pub blocks_dir: PathBuf,
    /// Root of the sandboxed `AIOS://` filesystem.
    pub vfs_root: PathBuf,
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
    capabilities: CapabilitySet,
    history: Vec<String>,
    log: Vec<String>,
    restarts: u32,
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
        let net = NetSettingsBlock::with_default_store(
            BlockId::new(NET_BLOCK_ID),
            NetworkConfig::default(),
        );
        Self {
            scheduler,
            registry,
            config,
            store,
            net,
            capabilities: CapabilitySet::all(),
            history: Vec::new(),
            log: Vec::new(),
            restarts: 0,
            started: Instant::now(),
        }
    }

    /// Replaces the capability set of this session.
    pub fn set_capabilities(&mut self, capabilities: CapabilitySet) {
        self.capabilities = capabilities;
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
            ("uptime", _) => CommandOutput::text(format!("{}s", self.started.elapsed().as_secs())),
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
            _ => CommandOutput::unavailable(&format!(
                "'{}' has no host backend yet",
                invocation.display_name()
            )),
        }
    }
}

impl HostBackend {
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
