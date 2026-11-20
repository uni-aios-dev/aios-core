use aios_block_mgr::registry::BlockRegistry;
use aios_process_mgr::scheduler::Scheduler;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ShellCommand {
    ListProcesses,
    ListBlocks,
    KillProcess {
        pid: u64,
    },
    SpawnProcess {
        name: String,
        priority: u8,
        ram_mb: u64,
    },
    LoadBlock {
        name: String,
        version: String,
    },
    UnloadBlock {
        block_id: u32,
    },
    SystemStatus,
    ViewLogs,
    RestartOrchestrator,
    Help,
    Exit,
    Unknown(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellResponse {
    pub success: bool,
    pub output: String,
}

pub struct SafeModeShell {
    log: Vec<String>,
    event_log: Arc<Mutex<Vec<String>>>,
    orchestrator_restarts: u32,
    max_restarts: u32,
}

impl SafeModeShell {
    pub fn new(max_restarts: u32) -> Self {
        Self::with_event_log(max_restarts, Arc::new(Mutex::new(Vec::new())))
    }

    /// Creates a shell whose events are also visible through a shared handle.
    ///
    /// The handle starts as the source of truth, so a shell created after earlier
    /// events still reports the same log as the backend reading that handle.
    pub fn with_event_log(max_restarts: u32, event_log: Arc<Mutex<Vec<String>>>) -> Self {
        let log = event_log
            .lock()
            .map(|entries| entries.clone())
            .unwrap_or_default();
        Self {
            log,
            event_log,
            orchestrator_restarts: 0,
            max_restarts,
        }
    }

    fn record(&mut self, entry: String) {
        self.log.push(entry.clone());
        if let Ok(mut shared) = self.event_log.lock() {
            shared.push(entry);
        }
    }

    pub fn parse_command(input: &str) -> ShellCommand {
        let lower = input.trim().to_lowercase();
        let parts: Vec<&str> = lower.split_whitespace().collect();
        match parts.first().copied() {
            Some("ps") | Some("processes") | Some("list") => ShellCommand::ListProcesses,
            Some("blocks") | Some("ls") => ShellCommand::ListBlocks,
            Some("kill") => {
                let pid = parts
                    .get(1)
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                ShellCommand::KillProcess { pid }
            }
            Some("spawn") => {
                let name = parts.get(1).unwrap_or(&"unnamed").to_string();
                let priority = parts.get(2).and_then(|s| s.parse::<u8>().ok()).unwrap_or(2);
                let ram_mb = parts
                    .get(3)
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(64);
                ShellCommand::SpawnProcess {
                    name,
                    priority,
                    ram_mb,
                }
            }
            Some("load") => {
                let name = parts.get(1).unwrap_or(&"unknown").to_string();
                let version = parts.get(2).unwrap_or(&"0.1.0").to_string();
                ShellCommand::LoadBlock { name, version }
            }
            Some("unload") => {
                let bid = parts
                    .get(1)
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);
                ShellCommand::UnloadBlock { block_id: bid }
            }
            Some("status") | Some("info") => ShellCommand::SystemStatus,
            Some("logs") | Some("log") => ShellCommand::ViewLogs,
            Some("restart") | Some("recover") => ShellCommand::RestartOrchestrator,
            Some("help") | Some("?") => ShellCommand::Help,
            Some("exit") | Some("quit") | Some("q") => ShellCommand::Exit,
            _ => ShellCommand::Unknown(input.to_string()),
        }
    }

    pub fn execute(
        &mut self,
        command: ShellCommand,
        scheduler: &mut Scheduler,
        registry: &mut BlockRegistry,
    ) -> ShellResponse {
        match command {
            ShellCommand::ListProcesses => {
                let procs = scheduler.all_processes();
                if procs.is_empty() {
                    return ShellResponse { success: true, output: "No processes running".into() };
                }
                let mut out = String::from("PID  NAME              PRIO  STATE     RAM\n");
                for p in &procs {
                    out.push_str(&format!(
                        "{:<4} {:<17} {:<5} {:<9} {}MB\n",
                        p.pid.0, p.name, p.priority, p.state, p.ram_quota_mb,
                    ));
                }
                out.push_str(&format!("\nTotal: {} processes", procs.len()));
                ShellResponse { success: true, output: out }
            }
            ShellCommand::ListBlocks => {
                let blocks = registry.topology_with_state();
                if blocks.is_empty() {
                    return ShellResponse { success: true, output: "No blocks loaded".into() };
                }
                let mut out = String::from("ID   NAME         VERSION  STATE\n");
                for (m, state) in &blocks {
                    out.push_str(&format!("{:<5} {:<13} {:<9} {:?}\n", m.id, m.name, m.version, state));
                }
                out.push_str(&format!("\nTotal: {} blocks", blocks.len()));
                ShellResponse { success: true, output: out }
            }
            ShellCommand::KillProcess { pid } => {
                let pid = aios_process_mgr::task::ProcessId(pid);
                match scheduler.kill_process(pid) {
                    Ok(p) => {
                        self.record(format!("Killed process {} ({})", pid.0, p.name));
                        ShellResponse { success: true, output: format!("Process {} ({}) terminated", pid.0, p.name) }
                    }
                    Err(e) => ShellResponse { success: false, output: format!("Failed: {e}") },
                }
            }
            ShellCommand::SpawnProcess { name, priority, ram_mb } => {
                let prio = aios_process_mgr::task::Priority::from_u8(priority);
                match scheduler.spawn_process(&name, prio, ram_mb) {
                    Ok(pid) => {
                        self.record(format!("Spawned {} as PID {}", name, pid.0));
                        ShellResponse { success: true, output: format!("Spawned '{}' as PID {} (priority={}, RAM={}MB)", name, pid.0, prio, ram_mb) }
                    }
                    Err(e) => ShellResponse { success: false, output: format!("Failed: {e}") },
                }
            }
            ShellCommand::LoadBlock { name, version } => {
                let binary = format!("block_{name}_{version}").into_bytes();
                match aios_block_mgr::loader::BlockLoader::load_from_binary(registry, &name, &version, binary) {
                    Ok(manifest) => {
                        self.record(format!("Loaded block '{}' v{} as ID {}", name, version, manifest.id));
                        ShellResponse { success: true, output: format!("Block '{}' v{} loaded as ID {}", name, version, manifest.id) }
                    }
                    Err(e) => ShellResponse { success: false, output: format!("Failed: {e}") },
                }
            }
            ShellCommand::UnloadBlock { block_id } => {
                match registry.unload_block(aios_core::block::BlockId(block_id)) {
                    Ok(_) => {
                        self.record(format!("Unloaded block {block_id}"));
                        ShellResponse { success: true, output: format!("Block {block_id} unloaded") }
                    }
                    Err(e) => ShellResponse { success: false, output: format!("Failed: {e}") },
                }
            }
            ShellCommand::SystemStatus => {
                let (used, total) = scheduler.ram_usage();
                let pressure = format!("{:.1}%", used as f64 / total.max(1) as f64 * 100.0);
                ShellResponse {
                    success: true,
                    output: format!(
                        "System: SAFE MODE\nProcesses: {} ({} running, {} ready)\nBlocks: {} loaded\nRAM: {}/{} MB ({})\nCrash log: {} entries",
                        scheduler.process_count(),
                        scheduler.running_count(),
                        scheduler.ready_count(),
                        registry.count(),
                        used, total, pressure,
                        scheduler.crash_log().len(),
                    ),
                }
            }
            ShellCommand::ViewLogs => {
                let output = if self.log.is_empty() {
                    "No safe mode events logged".into()
                } else {
                    self.log.iter().enumerate()
                        .map(|(i, e)| format!("{}: {}", i + 1, e))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                ShellResponse { success: true, output }
            }
            ShellCommand::RestartOrchestrator => {
                if self.orchestrator_restarts >= self.max_restarts {
                    return ShellResponse {
                        success: false,
                        output: format!(
                            "Max restarts ({}) reached. Manual intervention required.",
                            self.max_restarts
                        ),
                    };
                }
                self.orchestrator_restarts += 1;
                self.record(format!(
                    "Orchestrator restart attempt {}/{}",
                    self.orchestrator_restarts, self.max_restarts
                ));
                ShellResponse {
                    success: true,
                    output: format!(
                        "Orchestrator restart initiated ({}/{})",
                        self.orchestrator_restarts, self.max_restarts
                    ),
                }
            }
            ShellCommand::Help => ShellResponse {
                success: true,
                output: "Commands:\n  ps                         — list processes\n  blocks                     — list loaded blocks\n  spawn <name> [prio] [ram]  — spawn a process\n  kill <pid>                 — kill a process\n  load <name> [version]      — load a block\n  unload <id>                — unload a block\n  status                     — system status\n  logs                       — view safe mode logs\n  restart                    — restart orchestrator\n  help                       — show this help\n  exit                       — exit safe mode".into(),
            },
            ShellCommand::Exit => ShellResponse {
                success: true,
                output: "Exiting safe mode shell".into(),
            },
            ShellCommand::Unknown(cmd) => ShellResponse {
                success: false,
                output: format!("Unknown command: '{cmd}'. Type 'help' for available commands."),
            },
        }
    }

    pub fn orchestrator_restarts(&self) -> u32 {
        self.orchestrator_restarts
    }

    pub fn log_entries(&self) -> &[String] {
        &self.log
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_block_mgr::registry::BlockRegistry;
    use aios_process_mgr::scheduler::Scheduler;

    fn make_system() -> (Scheduler, BlockRegistry) {
        let mut sched = Scheduler::new(8192);
        let mut reg = BlockRegistry::new();
        let _ = sched.spawn_process(
            "ai_orchestrator",
            aios_process_mgr::task::Priority::High,
            512,
        );
        let _ = sched.spawn_process("io_handler", aios_process_mgr::task::Priority::Normal, 128);
        let _ = aios_block_mgr::loader::BlockLoader::load_from_binary(
            &mut reg,
            "hal",
            "0.1.0",
            b"hal-data".to_vec(),
        );
        (sched, reg)
    }

    fn empty_system() -> (Scheduler, BlockRegistry) {
        (Scheduler::new(8192), BlockRegistry::new())
    }

    #[test]
    fn legacy_empty_list_outputs_are_exact() {
        let (mut sched, mut reg) = empty_system();
        let mut shell = SafeModeShell::new(3);
        assert_eq!(
            shell.execute(ShellCommand::ListProcesses, &mut sched, &mut reg),
            ShellResponse {
                success: true,
                output: "No processes running".into()
            }
        );
        assert_eq!(
            shell.execute(ShellCommand::ListBlocks, &mut sched, &mut reg),
            ShellResponse {
                success: true,
                output: "No blocks loaded".into()
            }
        );
    }

    #[test]
    fn legacy_process_list_output_is_exact() {
        let (mut sched, mut reg) = empty_system();
        let process = sched
            .spawn_process(
                "ai_orchestrator",
                aios_process_mgr::task::Priority::High,
                512,
            )
            .unwrap();
        let expected = format!(
            "PID  NAME              PRIO  STATE     RAM\n{:<4} {:<17} {:<5} {:<9} {}MB\n\nTotal: 1 processes",
            process.0,
            "ai_orchestrator",
            aios_process_mgr::task::Priority::High,
            aios_process_mgr::task::ProcessState::Ready,
            512
        );
        let mut shell = SafeModeShell::new(3);
        let resp = shell.execute(ShellCommand::ListProcesses, &mut sched, &mut reg);
        assert!(resp.success);
        assert_eq!(resp.output, expected);
    }

    #[test]
    fn legacy_block_list_output_is_exact() {
        let (mut sched, mut reg) = empty_system();
        let manifest = aios_block_mgr::loader::BlockLoader::load_from_binary(
            &mut reg,
            "hal",
            "0.1.0",
            b"hal-data".to_vec(),
        )
        .unwrap();
        let expected = format!(
            "ID   NAME         VERSION  STATE\n{:<5} {:<13} {:<9} {:?}\n\nTotal: 1 blocks",
            manifest.id,
            "hal",
            "0.1.0",
            aios_core::block::BlockState::Active
        );
        let mut shell = SafeModeShell::new(3);
        let resp = shell.execute(ShellCommand::ListBlocks, &mut sched, &mut reg);
        assert!(resp.success);
        assert_eq!(resp.output, expected);
    }

    #[test]
    fn legacy_kill_and_spawn_outputs_are_exact() {
        let (mut sched, mut reg) = empty_system();
        let pid = sched
            .spawn_process("worker", aios_process_mgr::task::Priority::Normal, 64)
            .unwrap();
        let mut shell = SafeModeShell::new(3);
        assert_eq!(
            shell.execute(
                ShellCommand::KillProcess { pid: pid.0 },
                &mut sched,
                &mut reg
            ),
            ShellResponse {
                success: true,
                output: format!("Process {} (worker) terminated", pid.0)
            }
        );
        assert_eq!(
            shell.log_entries(),
            &[format!("Killed process {} (worker)", pid.0)]
        );
        assert_eq!(
            shell.execute(
                ShellCommand::KillProcess { pid: 9999 },
                &mut sched,
                &mut reg
            ),
            ShellResponse {
                success: false,
                output: "Failed: Process not found: 9999".into()
            }
        );

        let (mut spawn_sched, mut spawn_reg) = empty_system();
        let mut spawn_shell = SafeModeShell::new(3);
        assert_eq!(
            spawn_shell.execute(
                ShellCommand::SpawnProcess {
                    name: "new_worker".into(),
                    priority: 3,
                    ram_mb: 128,
                },
                &mut spawn_sched,
                &mut spawn_reg,
            ),
            ShellResponse {
                success: true,
                output: "Spawned 'new_worker' as PID 1 (priority=High, RAM=128MB)".into()
            }
        );
        assert_eq!(
            spawn_shell.log_entries(),
            &["Spawned new_worker as PID 1".to_string()]
        );

        let (mut tiny_sched, mut tiny_reg) = (Scheduler::new(1), BlockRegistry::new());
        let mut fail_shell = SafeModeShell::new(3);
        assert_eq!(
            fail_shell.execute(
                ShellCommand::SpawnProcess {
                    name: "huge".into(),
                    priority: 2,
                    ram_mb: 64,
                },
                &mut tiny_sched,
                &mut tiny_reg,
            ),
            ShellResponse {
                success: false,
                output: "Failed: Scheduler error: Insufficient RAM: requested 64MB, available 1MB"
                    .into()
            }
        );
    }

    #[test]
    fn legacy_block_load_unload_outputs_are_exact() {
        let (mut sched, mut reg) = empty_system();
        let mut shell = SafeModeShell::new(3);
        assert_eq!(
            shell.execute(
                ShellCommand::LoadBlock {
                    name: "test_block".into(),
                    version: "1.0.0".into(),
                },
                &mut sched,
                &mut reg,
            ),
            ShellResponse {
                success: true,
                output: "Block 'test_block' v1.0.0 loaded as ID block_1".into()
            }
        );
        assert_eq!(
            shell.log_entries(),
            &["Loaded block 'test_block' v1.0.0 as ID block_1".to_string()]
        );
        assert_eq!(
            shell.execute(
                ShellCommand::UnloadBlock { block_id: 1 },
                &mut sched,
                &mut reg
            ),
            ShellResponse {
                success: true,
                output: "Block 1 unloaded".into()
            }
        );
        assert_eq!(shell.log_entries()[1], "Unloaded block 1".to_string());
        assert_eq!(
            shell.execute(
                ShellCommand::UnloadBlock { block_id: 99 },
                &mut sched,
                &mut reg
            ),
            ShellResponse {
                success: false,
                output: "Failed: Block not found: block_99".into()
            }
        );
    }

    #[test]
    fn legacy_status_output_is_exact() {
        let (mut sched, mut reg) = empty_system();
        let mut shell = SafeModeShell::new(3);
        assert_eq!(
            shell.execute(ShellCommand::SystemStatus, &mut sched, &mut reg),
            ShellResponse {
                success: true,
                output: "System: SAFE MODE\nProcesses: 0 (0 running, 0 ready)\nBlocks: 0 loaded\nRAM: 0/8192 MB (0.0%)\nCrash log: 0 entries".into()
            }
        );
    }

    #[test]
    fn legacy_logs_output_is_exact() {
        let (mut sched, mut reg) = empty_system();
        let mut shell = SafeModeShell::new(3);
        assert_eq!(
            shell.execute(ShellCommand::ViewLogs, &mut sched, &mut reg),
            ShellResponse {
                success: true,
                output: "No safe mode events logged".into()
            }
        );
        let spawned = shell.execute(
            ShellCommand::SpawnProcess {
                name: "worker".into(),
                priority: 2,
                ram_mb: 64,
            },
            &mut sched,
            &mut reg,
        );
        assert!(spawned.success);
        let pid = sched.all_processes()[0].pid.0;
        let killed = shell.execute(ShellCommand::KillProcess { pid }, &mut sched, &mut reg);
        assert!(killed.success);
        assert_eq!(
            shell.execute(ShellCommand::ViewLogs, &mut sched, &mut reg),
            ShellResponse {
                success: true,
                output: format!("1: Spawned worker as PID {pid}\n2: Killed process {pid} (worker)")
            }
        );
    }

    #[test]
    fn legacy_restart_outputs_are_exact() {
        let (mut sched, mut reg) = empty_system();
        let mut shell = SafeModeShell::new(2);
        assert_eq!(
            shell.execute(ShellCommand::RestartOrchestrator, &mut sched, &mut reg),
            ShellResponse {
                success: true,
                output: "Orchestrator restart initiated (1/2)".into()
            }
        );
        assert_eq!(
            shell.execute(ShellCommand::RestartOrchestrator, &mut sched, &mut reg),
            ShellResponse {
                success: true,
                output: "Orchestrator restart initiated (2/2)".into()
            }
        );
        assert_eq!(
            shell.execute(ShellCommand::RestartOrchestrator, &mut sched, &mut reg),
            ShellResponse {
                success: false,
                output: "Max restarts (2) reached. Manual intervention required.".into()
            }
        );
        assert_eq!(
            shell.log_entries(),
            &[
                "Orchestrator restart attempt 1/2".to_string(),
                "Orchestrator restart attempt 2/2".to_string()
            ]
        );
    }

    #[test]
    fn legacy_help_exit_and_unknown_outputs_are_exact() {
        let (mut sched, mut reg) = empty_system();
        let mut shell = SafeModeShell::new(3);
        assert_eq!(
            shell.execute(ShellCommand::Help, &mut sched, &mut reg),
            ShellResponse {
                success: true,
                output: "Commands:\n  ps                         — list processes\n  blocks                     — list loaded blocks\n  spawn <name> [prio] [ram]  — spawn a process\n  kill <pid>                 — kill a process\n  load <name> [version]      — load a block\n  unload <id>                — unload a block\n  status                     — system status\n  logs                       — view safe mode logs\n  restart                    — restart orchestrator\n  help                       — show this help\n  exit                       — exit safe mode".into()
            }
        );
        assert_eq!(
            shell.execute(ShellCommand::Exit, &mut sched, &mut reg),
            ShellResponse {
                success: true,
                output: "Exiting safe mode shell".into()
            }
        );
        let command = SafeModeShell::parse_command("  WhAt Ever  ");
        assert!(matches!(command, ShellCommand::Unknown(ref value) if value == "  WhAt Ever  "));
        assert_eq!(
            shell.execute(command, &mut sched, &mut reg),
            ShellResponse {
                success: false,
                output: "Unknown command: '  WhAt Ever  '. Type 'help' for available commands."
                    .into()
            }
        );
    }

    #[test]
    fn test_parse_help() {
        assert_eq!(SafeModeShell::parse_command("help"), ShellCommand::Help);
        assert_eq!(SafeModeShell::parse_command("?"), ShellCommand::Help);
    }

    #[test]
    fn test_parse_kill() {
        assert_eq!(
            SafeModeShell::parse_command("kill 42"),
            ShellCommand::KillProcess { pid: 42 }
        );
    }

    #[test]
    fn test_parse_spawn() {
        let cmd = SafeModeShell::parse_command("spawn worker 3 256");
        assert_eq!(
            cmd,
            ShellCommand::SpawnProcess {
                name: "worker".into(),
                priority: 3,
                ram_mb: 256
            }
        );
    }

    #[test]
    fn test_parse_load() {
        let cmd = SafeModeShell::parse_command("load my_block 2.0.0");
        assert_eq!(
            cmd,
            ShellCommand::LoadBlock {
                name: "my_block".into(),
                version: "2.0.0".into()
            }
        );
    }

    #[test]
    fn test_parse_unknown() {
        assert!(matches!(
            SafeModeShell::parse_command("asdfghjkl"),
            ShellCommand::Unknown(_)
        ));
    }

    #[test]
    fn test_execute_list_processes() {
        let (mut sched, mut reg) = make_system();
        let mut shell = SafeModeShell::new(3);
        let resp = shell.execute(ShellCommand::ListProcesses, &mut sched, &mut reg);
        assert!(resp.success);
        assert!(resp.output.contains("ai_orchestrator"));
        assert!(resp.output.contains("2 processes"));
    }

    #[test]
    fn test_execute_list_blocks() {
        let (mut sched, mut reg) = make_system();
        let mut shell = SafeModeShell::new(3);
        let resp = shell.execute(ShellCommand::ListBlocks, &mut sched, &mut reg);
        assert!(resp.success);
        assert!(resp.output.contains("hal"));
    }

    #[test]
    fn test_execute_kill_process() {
        let (mut sched, mut reg) = make_system();
        let mut shell = SafeModeShell::new(3);
        let resp = shell.execute(ShellCommand::KillProcess { pid: 2 }, &mut sched, &mut reg);
        assert!(resp.success);
        assert!(resp.output.contains("terminated"));
        assert_eq!(shell.log_entries().len(), 1);
    }

    #[test]
    fn test_execute_spawn_process() {
        let (mut sched, mut reg) = make_system();
        let mut shell = SafeModeShell::new(3);
        let resp = shell.execute(
            ShellCommand::SpawnProcess {
                name: "new_worker".into(),
                priority: 2,
                ram_mb: 64,
            },
            &mut sched,
            &mut reg,
        );
        assert!(resp.success);
        assert!(sched.process_count() >= 3);
    }

    #[test]
    fn test_execute_load_block() {
        let (mut sched, mut reg) = make_system();
        let mut shell = SafeModeShell::new(3);
        let resp = shell.execute(
            ShellCommand::LoadBlock {
                name: "test_block".into(),
                version: "1.0.0".into(),
            },
            &mut sched,
            &mut reg,
        );
        assert!(resp.success);
        assert!(reg.count() >= 2);
    }

    #[test]
    fn test_execute_system_status() {
        let (mut sched, mut reg) = make_system();
        let mut shell = SafeModeShell::new(3);
        let resp = shell.execute(ShellCommand::SystemStatus, &mut sched, &mut reg);
        assert!(resp.success);
        assert!(resp.output.contains("SAFE MODE"));
        assert!(resp.output.contains("Processes:"));
    }

    #[test]
    fn test_execute_restart_limit() {
        let (mut sched, mut reg) = make_system();
        let mut shell = SafeModeShell::new(2);
        shell.execute(ShellCommand::RestartOrchestrator, &mut sched, &mut reg);
        shell.execute(ShellCommand::RestartOrchestrator, &mut sched, &mut reg);
        let resp = shell.execute(ShellCommand::RestartOrchestrator, &mut sched, &mut reg);
        assert!(!resp.success);
        assert!(resp.output.contains("Max restarts"));
    }

    #[test]
    fn test_help_output() {
        let (mut sched, mut reg) = make_system();
        let mut shell = SafeModeShell::new(3);
        let resp = shell.execute(ShellCommand::Help, &mut sched, &mut reg);
        assert!(resp.output.contains("spawn"));
        assert!(resp.output.contains("unload"));
    }

    #[test]
    fn test_view_logs_numbered() {
        let (mut sched, mut reg) = make_system();
        let mut shell = SafeModeShell::new(3);
        shell.execute(ShellCommand::KillProcess { pid: 2 }, &mut sched, &mut reg);
        shell.execute(ShellCommand::KillProcess { pid: 1 }, &mut sched, &mut reg);
        let resp = shell.execute(ShellCommand::ViewLogs, &mut sched, &mut reg);
        assert!(resp.output.contains("1:"));
        assert!(resp.output.contains("2:"));
    }

    #[test]
    fn shared_event_log_sees_shell_events() {
        let (mut sched, mut reg) = empty_system();
        let shared = Arc::new(Mutex::new(Vec::new()));
        let mut shell = SafeModeShell::with_event_log(3, shared.clone());
        shell.execute(ShellCommand::RestartOrchestrator, &mut sched, &mut reg);
        let expected = vec!["Orchestrator restart attempt 1/3".to_string()];
        assert_eq!(shared.lock().unwrap().as_slice(), expected.as_slice());
        let late_shell = SafeModeShell::with_event_log(3, shared);
        assert_eq!(late_shell.log_entries(), expected.as_slice());
    }

    #[test]
    fn legacy_parser_vocabulary_matches_console_counterparts() {
        use aios_console::registry::{resolve, ResolveError};

        let canonical: &[(&str, ShellCommand, &str, Option<&str>)] = &[
            ("ps", ShellCommand::ListProcesses, "ps", None),
            ("processes", ShellCommand::ListProcesses, "ps", None),
            ("blocks", ShellCommand::ListBlocks, "block", Some("list")),
            (
                "kill 42",
                ShellCommand::KillProcess { pid: 42 },
                "process",
                Some("kill"),
            ),
            (
                "unload 7",
                ShellCommand::UnloadBlock { block_id: 7 },
                "block",
                Some("unload"),
            ),
            ("status", ShellCommand::SystemStatus, "status", None),
            ("logs", ShellCommand::ViewLogs, "log", Some("show")),
            ("log", ShellCommand::ViewLogs, "log", Some("show")),
            ("help", ShellCommand::Help, "help", None),
            ("?", ShellCommand::Help, "help", None),
            ("exit", ShellCommand::Exit, "exit", None),
            ("quit", ShellCommand::Exit, "exit", None),
            ("q", ShellCommand::Exit, "exit", None),
        ];
        for (input, legacy, id, sub) in canonical {
            assert_eq!(
                SafeModeShell::parse_command(input),
                legacy.clone(),
                "input: {input}"
            );
            let invocation =
                resolve(input).unwrap_or_else(|error| panic!("{input} did not resolve: {error:?}"));
            assert_eq!(invocation.spec.id, *id, "input: {input}");
            assert_eq!(invocation.subcommand, *sub, "input: {input}");
        }

        let ambiguous: &[(&str, ShellCommand)] = &[
            ("list", ShellCommand::ListProcesses),
            ("ls", ShellCommand::ListBlocks),
            (
                "spawn worker 3 256",
                ShellCommand::SpawnProcess {
                    name: "worker".into(),
                    priority: 3,
                    ram_mb: 256,
                },
            ),
            (
                "load my_block 2.0.0",
                ShellCommand::LoadBlock {
                    name: "my_block".into(),
                    version: "2.0.0".into(),
                },
            ),
            ("info", ShellCommand::SystemStatus),
            ("restart", ShellCommand::RestartOrchestrator),
            ("recover", ShellCommand::RestartOrchestrator),
        ];
        for (input, legacy) in ambiguous {
            assert_eq!(
                SafeModeShell::parse_command(input),
                legacy.clone(),
                "input: {input}"
            );
            assert!(
                matches!(resolve(input), Err(ResolveError::Ambiguous { .. })),
                "{input} must stay ambiguous"
            );
        }
    }
}
