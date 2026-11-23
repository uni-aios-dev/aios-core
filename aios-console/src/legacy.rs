//! Adapters that keep exact legacy shell text while using canonical console data.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::output::{CommandOutput, ExitCode, OutputControl};

const PROCESS_HEADER: &str = "PID  NAME              PRIO  STATE     RAM";
const PROCESS_SEPARATOR: &str = "---- ----------------  ---- --------- -----";
const BLOCK_HEADER: &str = "ID   NAME         VERSION  STATE";
const BLOCK_SEPARATOR: &str = "---- ------------ -------- --------";
const STATUS_HEADER: &str = "status        host console";
const STATUS_UPTIME: &str = "uptime        ";
const STATUS_PROCESSES: &str = "processes     ";
const STATUS_BLOCKS: &str = "blocks        ";
const STATUS_MEMORY: &str = "memory        ";
const STATUS_RESTARTS: &str = "restarts      ";
const STATUS_LOG: &str = "log           ";
const HELP_HEADER: &str = "AIOS text console commands";
const LEGACY_HELP: &str = "Commands:\n  ps                         — list processes\n  blocks                     — list loaded blocks\n  spawn <name> [prio] [ram]  — spawn a process\n  kill <pid>                 — kill a process\n  load <name> [version]      — load a block\n  unload <id>                — unload a block\n  status                     — system status\n  logs                       — view safe mode logs\n  restart                    — restart orchestrator\n  help                       — show this help\n  exit                       — exit safe mode";
/// Exact legacy help line of the `aios` TUI shell.
pub const AIOS_HELP: &str = "Commands: console <command> [args] | ps | blocks | kill <pid> | spawn <wasm> | store list | store search <q> | store install <name> | net get | net set k=v ... | cluster status | cluster spawn <name> | cluster kill <node> <pid> | cluster migrate <node> <pid> [target] | status | logs | restart | clear";

/// Converts canonical `ps` output into the exact legacy safe-mode process text.
///
/// Returns `None` when `output` is not a well-formed canonical process list, so
/// a frontend can keep the legacy dispatcher instead of printing a partial table.
pub fn process_list_text(output: &CommandOutput) -> Option<String> {
    if output.code != ExitCode::Ok {
        return None;
    }

    if let [line] = output.lines.as_slice() {
        return (line == "No processes running").then(|| line.clone());
    }

    let (header, separator, rows, total) = match output.lines.as_slice() {
        [header, separator, rows @ .., total] => (header, separator, rows, total),
        _ => return None,
    };
    if header != PROCESS_HEADER || separator != PROCESS_SEPARATOR {
        return None;
    }
    if rows.is_empty() || !total.starts_with("Total: ") || !total.ends_with(" processes") {
        return None;
    }

    let mut text = String::from(PROCESS_HEADER);
    text.push('\n');
    for row in rows {
        text.push_str(&legacy_process_row(row)?);
        text.push('\n');
    }
    text.push('\n');
    text.push_str(total);
    Some(text)
}

/// Converts canonical `block list` output into the exact legacy safe-mode block text.
///
/// Returns `None` when `output` is not a well-formed canonical block list, so a
/// frontend can keep the legacy dispatcher instead of printing a partial table.
pub fn block_list_text(output: &CommandOutput) -> Option<String> {
    if output.code != ExitCode::Ok {
        return None;
    }

    if let [line] = output.lines.as_slice() {
        return (line == "No blocks loaded").then(|| line.clone());
    }

    let (header, separator, rows, total) = match output.lines.as_slice() {
        [header, separator, rows @ .., total] => (header, separator, rows, total),
        _ => return None,
    };
    if header != BLOCK_HEADER || separator != BLOCK_SEPARATOR {
        return None;
    }
    let count = total
        .strip_prefix("Total: ")
        .and_then(|value| value.strip_suffix(" blocks"))
        .and_then(|value| value.parse::<usize>().ok())?;
    if count != rows.len() {
        return None;
    }
    if rows.is_empty() {
        return Some(String::from("No blocks loaded"));
    }

    let mut text = String::from(BLOCK_HEADER);
    text.push('\n');
    for row in rows {
        text.push_str(&legacy_block_row(row)?);
        text.push('\n');
    }
    text.push('\n');
    text.push_str(total);
    Some(text)
}

/// Converts canonical `status` output into the exact legacy safe-mode status text.
///
/// `crash_log_count` comes from the scheduler because the canonical status does
/// not expose the crash log. Returns `None` for an unexpected canonical shape so
/// a frontend can keep the legacy dispatcher instead of printing partial status.
pub fn status_text(output: &CommandOutput, crash_log_count: usize) -> Option<String> {
    if output.code != ExitCode::Ok {
        return None;
    }

    let [status, uptime, processes, blocks, memory, restarts, log] = output.lines.as_slice() else {
        return None;
    };
    if status != STATUS_HEADER
        || !uptime.starts_with(STATUS_UPTIME)
        || !restarts.starts_with(STATUS_RESTARTS)
        || !log.starts_with(STATUS_LOG)
    {
        return None;
    }

    let processes = processes.strip_prefix(STATUS_PROCESSES)?;
    let blocks = blocks.strip_prefix(STATUS_BLOCKS)?;
    let memory = memory.strip_prefix(STATUS_MEMORY)?.strip_suffix(" MB")?;
    let (used, total) = memory.split_once('/')?;
    let used = used.parse::<u64>().ok()?;
    let total = total.parse::<u64>().ok()?;
    let pressure = used as f64 / total.max(1) as f64 * 100.0;

    Some(format!(
        "System: SAFE MODE\nProcesses: {processes}\nBlocks: {blocks} loaded\nRAM: {used}/{total} MB ({pressure:.1}%)\nCrash log: {crash_log_count} entries"
    ))
}

/// Converts canonical `exit` output into the exact legacy safe-mode exit text.
///
/// Returns `None` unless the output is a clean `OutputControl::Exit` with no
/// lines, so a frontend can keep the legacy dispatcher instead of inventing a
/// shutdown message for an unexpected canonical result.
pub fn exit_text(output: &CommandOutput) -> Option<String> {
    if output.code != ExitCode::Ok || !output.lines.is_empty() {
        return None;
    }
    match output.control {
        OutputControl::Exit(_) => Some(String::from("Exiting safe mode shell")),
        _ => None,
    }
}

/// Converts canonical `help` output into the exact legacy safe-mode help text.
///
/// Returns `None` when the output is not the generated all-commands help, so a
/// frontend can keep the legacy dispatcher instead of replacing a help topic
/// with the shell-wide legacy listing.
pub fn help_text(output: &CommandOutput) -> Option<String> {
    if output.code != ExitCode::Ok || output.lines.first().map(String::as_str) != Some(HELP_HEADER)
    {
        return None;
    }
    Some(String::from(LEGACY_HELP))
}

/// Formats safe-mode event entries with the exact legacy numbering and empty text.
pub fn log_text(entries: &[String]) -> String {
    if entries.is_empty() {
        return String::from("No safe mode events logged");
    }
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| format!("{}: {}", index + 1, entry))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Exact output and safe-mode event produced by a legacy mutation command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyMutation {
    pub success: bool,
    pub output: String,
    pub event: Option<String>,
}

impl LegacyMutation {
    fn ok(output: String, event: String) -> Self {
        Self {
            success: true,
            output,
            event: Some(event),
        }
    }

    fn err(output: String) -> Self {
        Self {
            success: false,
            output,
            event: None,
        }
    }
}

/// Formats the exact legacy output of a failed mutation (`Failed: <error>`).
pub fn mutation_failed(error: &str) -> LegacyMutation {
    LegacyMutation::err(format!("Failed: {error}"))
}

/// Formats the exact legacy output and event of `restart` / `recover`.
pub fn restart_success(attempt: u32, max: u32) -> LegacyMutation {
    LegacyMutation::ok(
        format!("Orchestrator restart initiated ({attempt}/{max})"),
        format!("Orchestrator restart attempt {attempt}/{max}"),
    )
}

/// Formats the exact legacy restart-limit failure (no `Failed:` prefix).
pub fn restart_limit_reached(max: u32) -> LegacyMutation {
    LegacyMutation::err(format!(
        "Max restarts ({max}) reached. Manual intervention required."
    ))
}

/// Formats the exact legacy `SafeModeShell` response for an unknown command.
///
/// The original input is preserved verbatim, including case and surrounding
/// whitespace, so a frontend can replace the final legacy fallback without
/// changing its output.
pub fn unknown_text(cmd: &str) -> String {
    format!("Unknown command: '{cmd}'. Type 'help' for available commands.")
}

/// Formats the exact legacy `aios` TUI shell line for an unknown command.
///
/// The legacy dispatcher lowercases the first whitespace-delimited token and
/// ignores any remaining arguments, so the adapter takes that token directly.
pub fn aios_unknown_text(command: &str) -> String {
    format!("  unknown command '{command}' — type help")
}

/// Returns the exact legacy `aios` TUI shell usage line for `command`.
///
/// `None` means the command has no fixed usage line, so its legacy path must
/// stay in place (for example `cluster`, whose usage depends on the subcommand).
pub fn aios_usage_text(command: &str) -> Option<String> {
    match command {
        "kill" => Some(String::from("Usage: kill <pid>")),
        "spawn" => Some(String::from("Usage: spawn <wasm-path-or-file>")),
        "store" => Some(String::from(
            "Usage: store list | store search <q> | store install <name>",
        )),
        "store search" => Some(String::from("Usage: store search <query>")),
        "store install" => Some(String::from("Usage: store install <name>")),
        "net" => Some(String::from("Usage: net get | net set key=value ...")),
        _ => None,
    }
}

/// Formats the exact legacy `aios` TUI shell output of a successful `kill`.
pub fn aios_kill_success(pid: u64, name: &str) -> String {
    format!("  killed '{name}' ({pid})")
}

/// Formats the exact legacy `aios` TUI shell output of a failed `kill`.
pub fn aios_kill_failed(error: &str) -> String {
    format!("  kill failed: {error}")
}

/// Formats the exact legacy `aios` TUI shell `logs` output from shared entries.
pub fn aios_log_text(entries: &[String]) -> Vec<String> {
    entries
        .iter()
        .rev()
        .take(20)
        .rev()
        .map(|line| format!("  {line}"))
        .collect()
}

/// Exact legacy kernel shell help lines.
pub const KERNEL_HELP: &[&str] = &[
    "commands:",
    "  console <command> [args]  unified text console (help, net, wifi, ...)",
    "  help       this list",
    "  tabs       tab overview",
    "  info       kernel telemetry",
    "  ver        version banner",
    "  clear      clear log",
    "  echo TEXT  echo text",
    "  gui        enter windowed GUI mode",
    "  tui        leave GUI, return to this console",
    "  faults     fault history + recovery statistics",
    "  wifi       radio mode + Wi-Fi card status",
    "  wifi sim   enable the test radio (3 sim APs)",
    "  wifi real  real radio only (card, no sim)",
    "  udp A.B.C.D PORT TEXT  send a UDP datagram, show the reply",
    "  http A.B.C.D PORT [HOST]  TCP GET / and print the response",
    "  socktest  loopback UDP+TCP self-test over the bound IP",
];

/// Exact legacy kernel shell unknown-command line.
pub const KERNEL_UNKNOWN: &str = "unknown command, type help";

/// Formats the exact legacy kernel shell `ver` output.
pub fn kernel_version_text(version: &str) -> Vec<String> {
    vec![version.to_string()]
}

/// Formats the exact legacy kernel shell `info` output.
pub fn kernel_info_text(pid: u32, switches: u64, sent: u64, recv: u64) -> Vec<String> {
    vec![
        "ticks/switches/ipc are live on the System, Sched and IPC tabs".to_string(),
        format!("pid={pid} switches={switches} ipc-sent={sent} ipc-recv={recv}"),
    ]
}

/// One fault record projected into the exact legacy kernel shell text.
pub struct KernelFaultLine {
    /// Faulted task slot.
    pub pid: u32,
    /// Stable fault code name.
    pub kind: &'static str,
    /// Fault counter for this task.
    pub count: u8,
    /// Instruction pointer at the fault.
    pub rip: u64,
    /// Snapshot state hash.
    pub state_hash: u64,
}

/// Counters behind the exact legacy kernel shell `faults` summary.
pub struct KernelFaultStats {
    /// Total number of faults observed by the kernel.
    pub total: u64,
    /// Number of faults resolved by resuming the task.
    pub resume: u64,
    /// Number of faults resolved by terminating the task.
    pub terminate: u64,
    /// Number of faults resolved by rolling the task back.
    pub rollback: u64,
    /// Number of fault records awaiting a decision.
    pub queued: usize,
    /// Number of queued fault events.
    pub events: usize,
    /// Whether the recovery dialog is currently pending.
    pub dialog_pending: bool,
}

/// Formats the exact legacy kernel shell `faults` output.
pub fn kernel_faults_text(
    stats: &KernelFaultStats,
    history: &[KernelFaultLine],
    pending: &[KernelFaultLine],
) -> Vec<String> {
    let mut lines = vec![format!(
        "faults total={} resume={} terminate={} rollback={} queued={} events={}",
        stats.total, stats.resume, stats.terminate, stats.rollback, stats.queued, stats.events
    )];
    for record in history {
        lines.push(format!(
            "  pid {} {} (crash #{}) rip={:#x} hash={:#x}",
            record.pid, record.kind, record.count, record.rip, record.state_hash
        ));
    }
    for record in pending {
        lines.push(format!(
            "  queued: pid {} {} (crash #{}) rip={:#x}",
            record.pid, record.kind, record.count, record.rip
        ));
    }
    if stats.dialog_pending {
        lines.push("  (dialog pending: a task awaits a recovery decision)".to_string());
    }
    lines
}

/// Formats the exact legacy kernel shell `wifi` status output.
pub fn kernel_wifi_text(sim: bool, card: &str) -> Vec<String> {
    vec![
        format!("mode: {}", if sim { "test radio (sim)" } else { "real" }),
        format!("card: {card}"),
    ]
}

/// Exact legacy kernel shell `wifi sim` output line.
pub fn kernel_wifi_sim_text() -> &'static str {
    "test radio enabled (sim, 3 APs)"
}

/// Exact legacy kernel shell `wifi real` output line.
pub fn kernel_wifi_real_text() -> &'static str {
    "real radio mode (test radio off)"
}

/// Text returned after the kernel's real reachability re-probe is requested.
pub fn kernel_net_recheck_text() -> &'static str {
    "network recheck requested"
}

/// Exact legacy `aios-shell` help lines, including ANSI colour codes.
pub const SHELL_HELP: &[&str] = &[
    "\x1b[1;36mтХФтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХЧ\x1b[0m",
    "\x1b[1;36mтХС                    AIOS Shell v2.2                        тХС\x1b[0m",
    "\x1b[1;36mтХЪтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХРтХЭ\x1b[0m",
    "\n\x1b[1;33m[BUILTIN]\x1b[0m exit,quit,clear,pwd,cd,env,export,unset,history,alias,unalias,source,type,echo,exec,jobs,fg,bg",
    "\x1b[1;33m[FILES]\x1b[0m ls,cat,head,tail,less,grep,find,wc,cp,mv,rm,mkdir,touch,chmod,chown,ln,stat,file",
    "\x1b[1;33m[SYSTEM]\x1b[0m ps,top,htop,free,df,du,uname,uptime,whoami,id,date,dmesg,kernel,lsmod,modinfo",
    "\x1b[1;33m[NET]\x1b[0m ping,curl,wget,nc,ss,netstat,ip,ifconfig,route,arp,nslookup,dig,traceroute",
    "\x1b[1;33m[DEBUG]\x1b[0m strace,lsof,lscpu,lsblk,lsusb,lspci,dmidecode,hdparm,iostat,vmstat",
    "\x1b[1;33m[DEV]\x1b[0m git,make,cmake,gcc,g++,rustc,cargo,python,python3,node,npm,pip",
    "\x1b[1;33m[ARCHIVE]\x1b[0m tar,gzip,gunzip,bzip2,xz,zip,unzip",
    "\x1b[1;33m[AIOS]\x1b[0m gui,tui,aios,logs,hw,services,aios-shell-reload",
    "\x1b[1;33m[CONSOLE]\x1b[0m console <command> (console help lists unified commands)",
    "\x1b[1;33m[EDITORS]\x1b[0m vi,vim,nano,emacs,micro",
    "\n\x1b[2mRun any program directly. Use & for background. TAB for completion.\x1b[0m",
];

/// Formats the exact legacy `aios-shell` echo output.
pub fn shell_echo_text(args: &[String]) -> Vec<String> {
    vec![args.join(" ")]
}

/// Formats the exact legacy `aios-shell` `pwd` output.
pub fn shell_pwd_text(path: &str) -> Vec<String> {
    vec![path.to_string()]
}

/// Formats the exact legacy `aios-shell` alias listing.
pub fn shell_alias_text(entries: &[(&str, &str)]) -> Vec<String> {
    let mut entries = entries.to_vec();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    entries
        .into_iter()
        .map(|(name, target)| format!("alias {name}='{target}'"))
        .collect()
}

/// Formats the exact legacy `aios-shell` alias definition line.
pub fn shell_alias_set_text(name: &str, value: &str) -> Vec<String> {
    vec![format!("alias {name}='{value}'")]
}

/// Formats the exact legacy `aios-shell` history output.
pub fn shell_history_text(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| !line.is_empty())
        .map(|(index, line)| format!("{:5}: {}", index + 1, line))
        .collect()
}

/// Formats the exact legacy `aios-shell` `cd` error line.
pub fn shell_cd_error_text(path: &str, error: &str) -> Vec<String> {
    vec![format!("cd: {path}: {error}")]
}

/// Formats the exact legacy `aios-shell` environment listing.
pub fn shell_env_text(entries: &[(String, String)]) -> Vec<String> {
    entries
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect()
}

/// Formats the exact legacy `aios-shell` `export KEY=VALUE` line.
pub fn shell_export_set_text(key: &str, value: &str) -> Vec<String> {
    vec![format!("export {key}={value}")]
}

/// Formats the exact legacy `aios-shell` `export KEY` lookup line.
pub fn shell_env_value_text(key: &str, value: &str) -> Vec<String> {
    vec![format!("{key}={value}")]
}

/// Formats the exact legacy `aios-shell` `source` recursion error.
pub fn shell_source_recursion_text(path: &str) -> Vec<String> {
    vec![format!("source: {path}: recursion detected")]
}

/// Formats the exact legacy `aios-shell` `source` missing-file error.
pub fn shell_source_missing_text(path: &str) -> Vec<String> {
    vec![format!("source: {path}: No such file")]
}

/// Formats the exact legacy `aios-shell` background job list.
pub fn shell_jobs_text(jobs: &[(usize, &str, u32)]) -> Vec<String> {
    if jobs.is_empty() {
        return vec!["No jobs".to_string()];
    }
    jobs.iter()
        .map(|(id, cmd, pid)| format!("[{id}]+ Running {cmd} (pid {pid})"))
        .collect()
}

/// Formats the exact legacy `aios-shell` `fg` success line.
pub fn shell_fg_text(id: usize, cmd: &str) -> Vec<String> {
    vec![format!("[{id}]+ Running        {cmd}")]
}

/// Formats the exact legacy `aios-shell` `bg` success line.
pub fn shell_bg_text(id: usize, cmd: &str) -> Vec<String> {
    vec![format!("[{id}]+ Running        &{cmd}")]
}

/// Formats an exact legacy `aios-shell` job-control error line.
pub fn shell_job_error_text(command: &str, message: &str) -> Vec<String> {
    vec![format!("{command}: {message}")]
}

/// Outcome of the legacy `aios` shell net IPC dispatch.
pub enum AiosNetResult {
    /// Text returned by the net settings block, or a local validation message.
    Text(String),
    /// The block acknowledged without a text payload.
    Acknowledged,
    /// The block did not answer.
    NoResponse,
    /// IPC dispatch failed.
    Error(String),
}

/// Formats the exact legacy `aios` TUI shell `net get` output.
pub fn aios_net_get_text(result: AiosNetResult) -> String {
    match result {
        AiosNetResult::Text(text) => text,
        AiosNetResult::Acknowledged => String::from("net settings block: OK"),
        AiosNetResult::NoResponse => String::from("net: no response"),
        AiosNetResult::Error(error) => format!("net ERROR: {error}"),
    }
}

/// Formats the exact legacy `aios` TUI shell `net set` output.
pub fn aios_net_set_text(result: AiosNetResult) -> String {
    match result {
        AiosNetResult::Text(text) => text,
        AiosNetResult::Acknowledged => String::from("net settings block: OK"),
        AiosNetResult::NoResponse => String::from("AIOS: net: no response"),
        AiosNetResult::Error(error) => format!("AIOS: net ERROR: {error}"),
    }
}

/// Formats the exact legacy `aios` shell `store search` start line.
pub fn aios_store_search_start(query: &str) -> String {
    format!("  searching store for '{query}'...")
}

/// Formats the exact legacy `aios` shell `store install` start line.
pub fn aios_store_install_start(name: &str) -> String {
    format!("  installing '{name}'...")
}

/// Formats the exact legacy `aios` TUI shell `restart` output line.
pub fn aios_restart_text() -> &'static str {
    "  hardware re-probed (hardware engine rescanned)"
}

/// Formats the exact legacy `aios` TUI shell `restart` log entry.
pub fn aios_restart_log() -> &'static str {
    "AIOS: restart requested via shell"
}

/// Formats the exact legacy `aios` TUI shell `status` output from system values.
pub fn aios_status_text(
    uptime_secs: u64,
    bridge_online: bool,
    tier: &str,
    used_gb: f64,
    total_gb: f64,
    blocks: usize,
) -> Vec<String> {
    vec![
        format!(
            "  uptime={uptime_secs}s bridge={} tier={tier}",
            if bridge_online { "online" } else { "starting" }
        ),
        format!("  RAM {used_gb:.1}GB used/{total_gb:.1}GB total"),
        format!("  blocks={blocks}"),
    ]
}

/// Formats the exact legacy output and event of `kill <pid>`.
pub fn kill_success(pid: u64, name: &str) -> LegacyMutation {
    LegacyMutation::ok(
        format!("Process {pid} ({name}) terminated"),
        format!("Killed process {pid} ({name})"),
    )
}

/// Formats the exact legacy output and event of `spawn <name> [prio] [ram]`.
pub fn spawn_success(name: &str, pid: u64, priority: &str, ram_mb: u64) -> LegacyMutation {
    LegacyMutation::ok(
        format!("Spawned '{name}' as PID {pid} (priority={priority}, RAM={ram_mb}MB)"),
        format!("Spawned {name} as PID {pid}"),
    )
}

/// Formats the exact legacy output and event of `load <name> [version]`.
///
/// `id_display` is the `BlockId` rendering (`block_<n>`), matching legacy text.
pub fn load_success(name: &str, version: &str, id_display: &str) -> LegacyMutation {
    LegacyMutation::ok(
        format!("Block '{name}' v{version} loaded as ID {id_display}"),
        format!("Loaded block '{name}' v{version} as ID {id_display}"),
    )
}

/// Formats the exact legacy output and event of `unload <id>`.
pub fn unload_success(id: u32) -> LegacyMutation {
    LegacyMutation::ok(
        format!("Block {id} unloaded"),
        format!("Unloaded block {id}"),
    )
}

fn legacy_process_row(row: &str) -> Option<String> {
    let fields = row.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 5 {
        return None;
    }

    let pid = fields[0];
    let name = fields[1..fields.len() - 3].join(" ");
    let priority = fields[fields.len() - 3];
    let state = fields[fields.len() - 2];
    let ram = fields[fields.len() - 1].strip_suffix("MB")?;
    if name.is_empty() || ram.is_empty() {
        return None;
    }

    Some(format!(
        "{:<4} {:<17} {} {} {}MB",
        pid, name, priority, state, ram
    ))
}

fn legacy_block_row(row: &str) -> Option<String> {
    let fields = row.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 4 {
        return None;
    }

    let id = fields[0].parse::<u64>().ok()?;
    let version = fields[fields.len() - 2];
    let state = fields[fields.len() - 1];
    let name = fields[1..fields.len() - 2].join(" ");
    if name.is_empty() || version.is_empty() || state.is_empty() {
        return None;
    }

    let id = format!("block_{id}");
    Some(format!("{:<5} {:<13} {:<9} {}", id, name, version, state))
}

#[cfg(test)]
mod tests {
    use super::{
        aios_kill_failed, aios_kill_success, aios_log_text, aios_net_get_text, aios_net_set_text,
        aios_restart_log, aios_restart_text, aios_status_text, aios_store_install_start,
        aios_store_search_start, aios_unknown_text, aios_usage_text, block_list_text, exit_text,
        help_text, kill_success, load_success, log_text, mutation_failed, process_list_text,
        restart_limit_reached, restart_success, spawn_success, status_text, unknown_text,
        unload_success, AiosNetResult, AIOS_HELP, BLOCK_HEADER, BLOCK_SEPARATOR, HELP_HEADER,
        LEGACY_HELP, PROCESS_HEADER, PROCESS_SEPARATOR,
    };
    use crate::output::CommandOutput;

    #[test]
    fn maps_empty_process_list() {
        let output = CommandOutput::text("No processes running");
        assert_eq!(
            process_list_text(&output).as_deref(),
            Some("No processes running")
        );
    }

    #[test]
    fn maps_canonical_process_rows_to_legacy_widths() {
        let row = alloc::format!(
            "{:<4} {:<17} {} {} {}MB",
            1,
            "ai_orchestrator",
            "High",
            "Ready",
            512
        );
        let output = CommandOutput::ok(alloc::vec![
            PROCESS_HEADER.into(),
            PROCESS_SEPARATOR.into(),
            row,
            "Total: 1 processes".into(),
        ]);
        let expected = alloc::format!(
            "PID  NAME              PRIO  STATE     RAM\n{:<4} {:<17} {} {} {}MB\n\nTotal: 1 processes",
            1,
            "ai_orchestrator",
            "High",
            "Ready",
            512
        );
        assert_eq!(
            process_list_text(&output).as_deref(),
            Some(expected.as_str())
        );
    }

    #[test]
    fn maps_empty_block_list() {
        let output = CommandOutput::ok(alloc::vec![
            BLOCK_HEADER.into(),
            BLOCK_SEPARATOR.into(),
            "Total: 0 blocks".into(),
        ]);
        assert_eq!(
            block_list_text(&output).as_deref(),
            Some("No blocks loaded")
        );
    }

    #[test]
    fn maps_canonical_block_rows_to_legacy_layout() {
        let row = alloc::format!("{:<4} {:<12} {:<8} {}", 1, "hal", "0.1.0", "Active");
        let output = CommandOutput::ok(alloc::vec![
            BLOCK_HEADER.into(),
            BLOCK_SEPARATOR.into(),
            row,
            "Total: 1 blocks".into(),
        ]);
        let expected = alloc::format!(
            "ID   NAME         VERSION  STATE\n{:<5} {:<13} {:<9} {}\n\nTotal: 1 blocks",
            "block_1",
            "hal",
            "0.1.0",
            "Active"
        );
        assert_eq!(block_list_text(&output).as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn maps_canonical_status_to_legacy_layout() {
        let output = CommandOutput::ok(alloc::vec![
            "status        host console".into(),
            "uptime        3s".into(),
            "processes     1 (0 running, 1 ready)".into(),
            "blocks        2".into(),
            "memory        64/65536 MB".into(),
            "restarts      0/3".into(),
            "log           0 entries".into(),
        ]);
        let expected = "System: SAFE MODE\nProcesses: 1 (0 running, 1 ready)\nBlocks: 2 loaded\nRAM: 64/65536 MB (0.1%)\nCrash log: 2 entries";
        assert_eq!(status_text(&output, 2).as_deref(), Some(expected));
    }

    #[test]
    fn maps_canonical_exit_to_legacy_text() {
        assert_eq!(
            exit_text(&CommandOutput::exit(0)).as_deref(),
            Some("Exiting safe mode shell")
        );
        assert_eq!(
            exit_text(&CommandOutput::exit(7)).as_deref(),
            Some("Exiting safe mode shell")
        );
    }

    #[test]
    fn maps_canonical_help_to_legacy_layout() {
        let output = CommandOutput::ok(alloc::vec![
            HELP_HEADER.into(),
            "scope: [B] both backends, [H] host only, [K] kernel only".into(),
            String::new(),
        ]);
        assert_eq!(help_text(&output).as_deref(), Some(LEGACY_HELP));
    }

    #[test]
    fn maps_event_entries_to_legacy_numbering() {
        assert_eq!(log_text(&[]), "No safe mode events logged");
        assert_eq!(
            log_text(&[
                "Spawned worker as PID 1".into(),
                "Killed process 1 (worker)".into(),
            ]),
            "1: Spawned worker as PID 1\n2: Killed process 1 (worker)"
        );
    }

    #[test]
    fn maps_mutation_success_to_legacy_output_and_event() {
        let kill = kill_success(7, "worker");
        assert_eq!(kill.output, "Process 7 (worker) terminated");
        assert_eq!(kill.event.as_deref(), Some("Killed process 7 (worker)"));

        let spawn = spawn_success("new_worker", 1, "High", 128);
        assert_eq!(
            spawn.output,
            "Spawned 'new_worker' as PID 1 (priority=High, RAM=128MB)"
        );
        assert_eq!(spawn.event.as_deref(), Some("Spawned new_worker as PID 1"));

        let load = load_success("test_block", "1.0.0", "block_1");
        assert_eq!(
            load.output,
            "Block 'test_block' v1.0.0 loaded as ID block_1"
        );
        assert_eq!(
            load.event.as_deref(),
            Some("Loaded block 'test_block' v1.0.0 as ID block_1")
        );

        let unload = unload_success(4);
        assert_eq!(unload.output, "Block 4 unloaded");
        assert_eq!(unload.event.as_deref(), Some("Unloaded block 4"));
    }

    #[test]
    fn maps_mutation_failure_to_legacy_output() {
        let failure = mutation_failed("Scheduler error: Insufficient RAM");
        assert!(!failure.success);
        assert_eq!(failure.output, "Failed: Scheduler error: Insufficient RAM");
        assert_eq!(failure.event, None);
    }

    #[test]
    fn maps_restart_success_and_limit_to_legacy_output() {
        let restart = restart_success(2, 3);
        assert!(restart.success);
        assert_eq!(restart.output, "Orchestrator restart initiated (2/3)");
        assert_eq!(
            restart.event.as_deref(),
            Some("Orchestrator restart attempt 2/3")
        );

        let limit = restart_limit_reached(3);
        assert!(!limit.success);
        assert_eq!(
            limit.output,
            "Max restarts (3) reached. Manual intervention required."
        );
        assert_eq!(limit.event, None);
    }

    #[test]
    fn maps_unknown_command_to_legacy_output() {
        assert_eq!(
            unknown_text("  WhAt Ever  "),
            "Unknown command: '  WhAt Ever  '. Type 'help' for available commands."
        );
    }

    #[test]
    fn maps_aios_legacy_text_exactly() {
        assert_eq!(
            AIOS_HELP,
            "Commands: console <command> [args] | ps | blocks | kill <pid> | spawn <wasm> | store list | store search <q> | store install <name> | net get | net set k=v ... | cluster status | cluster spawn <name> | cluster kill <node> <pid> | cluster migrate <node> <pid> [target] | status | logs | restart | clear"
        );
        assert_eq!(
            aios_unknown_text("frobnicate"),
            "  unknown command 'frobnicate' — type help"
        );
        assert_eq!(
            aios_usage_text("kill").as_deref(),
            Some("Usage: kill <pid>")
        );
        assert_eq!(
            aios_usage_text("spawn").as_deref(),
            Some("Usage: spawn <wasm-path-or-file>")
        );
        assert_eq!(
            aios_usage_text("store").as_deref(),
            Some("Usage: store list | store search <q> | store install <name>")
        );
        assert_eq!(
            aios_usage_text("net").as_deref(),
            Some("Usage: net get | net set key=value ...")
        );
        assert_eq!(aios_usage_text("cluster"), None);
        assert_eq!(
            aios_usage_text("store search").as_deref(),
            Some("Usage: store search <query>")
        );
        assert_eq!(
            aios_usage_text("store install").as_deref(),
            Some("Usage: store install <name>")
        );
        assert_eq!(
            aios_restart_text(),
            "  hardware re-probed (hardware engine rescanned)"
        );
        assert_eq!(aios_restart_log(), "AIOS: restart requested via shell");
    }

    #[test]
    fn maps_aios_net_results_exactly() {
        assert_eq!(aios_net_get_text(AiosNetResult::Text("cfg".into())), "cfg");
        assert_eq!(
            aios_net_get_text(AiosNetResult::Acknowledged),
            "net settings block: OK"
        );
        assert_eq!(
            aios_net_get_text(AiosNetResult::NoResponse),
            "net: no response"
        );
        assert_eq!(
            aios_net_get_text(AiosNetResult::Error("boom".into())),
            "net ERROR: boom"
        );
        assert_eq!(
            aios_net_set_text(AiosNetResult::Text("AIOS: net: usage".into())),
            "AIOS: net: usage"
        );
        assert_eq!(
            aios_net_set_text(AiosNetResult::NoResponse),
            "AIOS: net: no response"
        );
        assert_eq!(
            aios_net_set_text(AiosNetResult::Error("boom".into())),
            "AIOS: net ERROR: boom"
        );
    }

    #[test]
    fn maps_kernel_shell_text_exactly() {
        assert_eq!(super::KERNEL_HELP[0], "commands:");
        assert_eq!(super::KERNEL_UNKNOWN, "unknown command, type help");
        assert_eq!(
            super::kernel_version_text("AIOS kernel v1"),
            ["AIOS kernel v1"]
        );
        assert_eq!(
            super::kernel_info_text(3, 7, 11, 13),
            [
                "ticks/switches/ipc are live on the System, Sched and IPC tabs",
                "pid=3 switches=7 ipc-sent=11 ipc-recv=13"
            ]
        );
        let history = [super::KernelFaultLine {
            pid: 4,
            kind: "GeneralProtectionFault",
            count: 2,
            rip: 0x1234,
            state_hash: 0xabcd,
        }];
        let stats = super::KernelFaultStats {
            total: 5,
            resume: 1,
            terminate: 2,
            rollback: 3,
            queued: 4,
            events: 6,
            dialog_pending: true,
        };
        let faults = super::kernel_faults_text(&stats, &history, &history);
        assert_eq!(
            faults,
            [
                "faults total=5 resume=1 terminate=2 rollback=3 queued=4 events=6",
                "  pid 4 GeneralProtectionFault (crash #2) rip=0x1234 hash=0xabcd",
                "  queued: pid 4 GeneralProtectionFault (crash #2) rip=0x1234",
                "  (dialog pending: a task awaits a recovery decision)"
            ]
        );
        assert_eq!(
            super::kernel_wifi_text(true, "sim card"),
            ["mode: test radio (sim)", "card: sim card"]
        );
        assert_eq!(
            super::kernel_wifi_sim_text(),
            "test radio enabled (sim, 3 APs)"
        );
        assert_eq!(
            super::kernel_wifi_real_text(),
            "real radio mode (test radio off)"
        );
        assert_eq!(
            super::kernel_net_recheck_text(),
            "network recheck requested"
        );
    }

    #[test]
    fn maps_aios_shell_text_exactly() {
        assert_eq!(super::SHELL_HELP.len(), 14);
        assert!(super::SHELL_HELP[0].starts_with("\x1b[1;36m"));
        assert_eq!(
            super::SHELL_HELP[13],
            "\n\x1b[2mRun any program directly. Use & for background. TAB for completion.\x1b[0m"
        );
        assert_eq!(
            super::shell_echo_text(&["MiXeD".to_string(), "Case".to_string()]),
            ["MiXeD Case"]
        );
        assert_eq!(super::shell_pwd_text("/tmp/aios"), ["/tmp/aios"]);
        assert_eq!(
            super::shell_alias_text(&[("ll", "ls -la"), ("gs", "git status")]),
            ["alias gs='git status'", "alias ll='ls -la'"]
        );
        assert_eq!(
            super::shell_alias_set_text("ll", "ls -la"),
            ["alias ll='ls -la'"]
        );
        let history = vec!["first".to_string(), String::new(), "third".to_string()];
        assert_eq!(
            super::shell_history_text(&history),
            ["    1: first", "    3: third"]
        );
    }

    #[test]
    fn maps_aios_shell_state_text_exactly() {
        assert_eq!(
            super::shell_cd_error_text("/nope", "No such file or directory"),
            ["cd: /nope: No such file or directory"]
        );
        let env = vec![
            ("HOME".to_string(), "/home/user".to_string()),
            ("PATH".to_string(), "/usr/bin".to_string()),
        ];
        assert_eq!(
            super::shell_env_text(&env),
            ["HOME=/home/user", "PATH=/usr/bin"]
        );
        assert_eq!(
            super::shell_export_set_text("KEY", "value"),
            ["export KEY=value"]
        );
        assert_eq!(super::shell_env_value_text("KEY", "value"), ["KEY=value"]);
        assert_eq!(
            super::shell_source_recursion_text("rc"),
            ["source: rc: recursion detected"]
        );
        assert_eq!(
            super::shell_source_missing_text("rc"),
            ["source: rc: No such file"]
        );
        assert_eq!(super::shell_jobs_text(&[]), ["No jobs"]);
        assert_eq!(
            super::shell_jobs_text(&[(7, "sleep 1", 4242)]),
            ["[7]+ Running sleep 1 (pid 4242)"]
        );
        assert_eq!(
            super::shell_fg_text(7, "sleep 1"),
            ["[7]+ Running        sleep 1"]
        );
        assert_eq!(
            super::shell_bg_text(7, "sleep 1"),
            ["[7]+ Running        &sleep 1"]
        );
        assert_eq!(
            super::shell_job_error_text("fg", "no current job"),
            ["fg: no current job"]
        );
    }

    #[test]
    fn maps_aios_store_start_lines_exactly() {
        assert_eq!(
            aios_store_search_start("worker"),
            "  searching store for 'worker'..."
        );
        assert_eq!(
            aios_store_install_start("worker"),
            "  installing 'worker'..."
        );
    }

    #[test]
    fn maps_aios_command_output_exactly() {
        assert_eq!(aios_kill_success(7, "worker"), "  killed 'worker' (7)");
        assert_eq!(
            aios_kill_failed("no such process"),
            "  kill failed: no such process"
        );
        assert!(aios_log_text(&[]).is_empty());
        assert_eq!(aios_log_text(&["a".into(), "b".into()]), ["  a", "  b"]);
        assert_eq!(
            aios_status_text(12, true, "basic", 1.2, 4.0, 3),
            [
                "  uptime=12s bridge=online tier=basic",
                "  RAM 1.2GB used/4.0GB total",
                "  blocks=3",
            ]
        );
        assert_eq!(
            aios_status_text(0, false, "basic", 0.0, 1.0, 0)[0],
            "  uptime=0s bridge=starting tier=basic"
        );
    }

    #[test]
    fn rejects_unexpected_outputs() {
        assert_eq!(process_list_text(&CommandOutput::usage("ps")), None);
        assert_eq!(
            process_list_text(&CommandOutput::ok(alloc::vec!["other".into()])),
            None
        );
        assert_eq!(block_list_text(&CommandOutput::usage("block list")), None);
        assert_eq!(
            block_list_text(&CommandOutput::ok(alloc::vec![
                BLOCK_HEADER.into(),
                BLOCK_SEPARATOR.into(),
                "Total: 1 blocks".into(),
            ])),
            None
        );
        assert_eq!(status_text(&CommandOutput::usage("status"), 0), None);
        assert_eq!(
            status_text(&CommandOutput::ok(alloc::vec!["other".into()]), 0),
            None
        );
        assert_eq!(exit_text(&CommandOutput::ok(Vec::new())), None);
        assert_eq!(
            exit_text(&CommandOutput::text("Exiting safe mode shell")),
            None
        );
        assert_eq!(help_text(&CommandOutput::usage("help")), None);
        assert_eq!(help_text(&CommandOutput::text("  status  status")), None);
    }
}
