use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::capability::Capability;
use crate::output::CommandOutput;
use crate::parser::{parse_line, split_alias_target, ParseError};
use crate::spec::{CommandGroup, CommandSpec, GROUPS};

macro_rules! spec {
    ($id:literal, $group:expr, $scope:expr, $bare:expr, $usage:literal, $summary:literal, [$($sub:literal),* $(,)?], [$($cap:expr),* $(,)?]) => {
        CommandSpec {
            id: $id,
            group: $group,
            scope: $scope,
            usage: $usage,
            summary: $summary,
            subcommands: &[$($sub),*],
            capabilities: &[$($cap),*],
            allow_bare_args: $bare,
        }
    };
}

use crate::capability::Scope::*;

/// The single canonical command table. `help` is generated from this slice.
pub const SPECS: &[CommandSpec] = &[
    // ── Session ────────────────────────────────────────────────────────────
    spec!("help", CommandGroup::Session, Both, true, "help [group|command]", "list commands from the registry", [], []),
    spec!("version", CommandGroup::Session, Both, false, "version", "print kernel/host version and AI tier", [], []),
    spec!("clear", CommandGroup::Session, Both, false, "clear", "clear visible output and scrollback", [], []),
    spec!("history", CommandGroup::Session, Both, true, "history [n]", "show recent input history", [], []),
    spec!("echo", CommandGroup::Session, Both, true, "echo [text...]", "print text; arguments keep their case", [], []),
    spec!("log", CommandGroup::Session, Both, false, "log show|clear", "console and system log", ["show", "clear"], []),
    spec!("exit", CommandGroup::Session, Both, true, "exit [code]", "close the console", [], []),
    spec!("sleep", CommandGroup::Session, Both, false, "sleep <seconds>", "pause execution", [], []),
    spec!("alias", CommandGroup::Session, Host, true, "alias [name=command]", "define or list session aliases", [], []),
    spec!("unalias", CommandGroup::Session, Host, false, "unalias <name>", "remove a session alias", [], []),

    // ── System ─────────────────────────────────────────────────────────────
    spec!("status", CommandGroup::System, Both, false, "status", "runtime status: uptime, RAM, CPU, tier, watchdog", [], []),
    spec!("sys", CommandGroup::System, Both, false, "sys info|tier", "hardware inventory and AI tier (not runtime status)", ["info", "tier"], []),
    spec!("kernel", CommandGroup::System, Kernel, false, "kernel info|ver", "kernel telemetry: pid, switches, IPC counters", ["info", "ver"], []),
    spec!("metrics", CommandGroup::System, Both, true, "metrics [--json] [--prometheus]", "counter snapshot", [], []),
    spec!("health", CommandGroup::System, Both, false, "health", "health-check blocks and processes", [], []),
    spec!("uptime", CommandGroup::System, Both, false, "uptime", "time since boot", [], []),
    spec!("thermal", CommandGroup::System, Kernel, false, "thermal", "thermal zone readings", [], []),
    spec!("battery", CommandGroup::System, Kernel, false, "battery", "battery and lid state", [], []),
    spec!("fb", CommandGroup::System, Kernel, false, "fb info", "framebuffer/GOP parameters", ["info"], []),

    // ── Process ────────────────────────────────────────────────────────────
    spec!("ps", CommandGroup::Process, Both, true, "ps [--all]", "list processes", [], []),
    spec!("process", CommandGroup::Process, Both, false, "process spawn|kill|renice|suspend|resume|affinity", "process lifecycle (replaces ambiguous `spawn`)", ["spawn", "kill", "renice", "suspend", "resume", "affinity"], []),
    spec!("sched", CommandGroup::Process, Both, false, "sched mode|quantum|aging|rt-deadline|rt-clear", "scheduler policy", ["mode", "quantum", "aging", "rt-deadline", "rt-clear"], [Capability::SchedModify]),
    spec!("faults", CommandGroup::Process, Kernel, true, "faults [stats|history|pending]", "fault recovery statistics", ["stats", "history", "pending"], []),
    spec!("fault", CommandGroup::Process, Both, false, "fault resolve <pid> <action>", "resolve a pending fault", ["resolve"], []),
    spec!("jobs", CommandGroup::Process, Host, false, "jobs", "list background jobs", [], []),
    spec!("fg", CommandGroup::Process, Host, false, "fg <id>", "bring a job to foreground", [], []),
    spec!("bg", CommandGroup::Process, Host, false, "bg <id>", "resume a job in background", [], []),

    // ── Block ──────────────────────────────────────────────────────────────
    spec!("block", CommandGroup::Block, Both, false, "block list|info|load|run|mock|unload|activate|deps|verify|swap|rollback", "block lifecycle (load = real wasm, run = send text to installed block)", ["list", "info", "load", "run", "mock", "unload", "activate", "deps", "verify", "swap", "rollback"], []),
    spec!("update", CommandGroup::Block, Host, false, "update history|pending|recover", "live-update journal", ["history", "pending", "recover"], []),
    spec!("state", CommandGroup::Block, Host, false, "state extract|restore <block>", "block state transfer", ["extract", "restore"], []),

    // ── IPC ────────────────────────────────────────────────────────────────
    spec!("ipc", CommandGroup::Ipc, Both, false, "ipc stats|flush|freeze|unfreeze|reroute|send|peek", "IPC bus control", ["stats", "flush", "freeze", "unfreeze", "reroute", "send", "peek"], []),

    // ── Watchdog ───────────────────────────────────────────────────────────
    spec!("wd", CommandGroup::Watchdog, Both, false, "wd status|start|stop|reset|force-safe-mode|escalate", "watchdog control; reset = controlled orchestrator restart", ["status", "start", "stop", "reset", "force-safe-mode", "escalate"], [Capability::WdControl]),

    // ── Security ───────────────────────────────────────────────────────────
    spec!("caps", CommandGroup::Security, Host, false, "caps list|issue|revoke|check", "capability tokens", ["list", "issue", "revoke", "check"], [Capability::SecurityAdmin]),
    spec!("sandbox", CommandGroup::Security, Host, false, "sandbox start|stop|log|mem", "sandbox control", ["start", "stop", "log", "mem"], [Capability::SecurityAdmin]),
    spec!("hw-protect", CommandGroup::Security, Host, false, "hw-protect status|mpk|tee|iommu|clear", "MPK/TEE/IOMMU state", ["status", "mpk", "tee", "iommu", "clear"], [Capability::SecurityAdmin]),
    spec!("acl", CommandGroup::Security, Host, false, "acl grant|revoke <cap> <path>", "filesystem ACL grants", ["grant", "revoke"], [Capability::SecurityAdmin]),

    // ── Hardware ───────────────────────────────────────────────────────────
    spec!("hw", CommandGroup::Hardware, Both, false, "hw detect|rescan|profile|fingerprints", "hardware detection and mock profiles", ["detect", "rescan", "profile", "fingerprints"], []),
    spec!("driver", CommandGroup::Hardware, Host, false, "driver list|install|remove|rollback|provision|failures", "driver store and self-healing", ["list", "install", "remove", "rollback", "provision", "failures"], [Capability::DriverModify]),
    spec!("hotplug", CommandGroup::Hardware, Host, false, "hotplug start|events|drain", "hotplug monitor", ["start", "events", "drain"], []),

    // ── Filesystem ─────────────────────────────────────────────────────────
    spec!("vfs", CommandGroup::Filesystem, Both, false, "vfs ls|cat|head|readat|write|mkdir|rm|mv|stat|exists|resolve|cp", "virtual filesystem operations", ["ls", "cat", "head", "readat", "write", "mkdir", "rm", "mv", "stat", "exists", "resolve", "cp"], []),
    spec!("fm", CommandGroup::Filesystem, Host, false, "fm jobs|cancel <id>", "file manager background jobs", ["jobs", "cancel"], []),

    // ── Network ────────────────────────────────────────────────────────────
    spec!("net", CommandGroup::Network, Both, false, "net show|set|reset|validate|config|status|lease|apply|recheck", "network config and link state", ["show", "set", "reset", "validate", "config", "status", "lease", "apply", "recheck"], []),
    spec!("wifi", CommandGroup::Network, Both, false, "wifi status|scan|connect|disconnect|sim|real", "Wi-Fi radio and association", ["status", "scan", "connect", "disconnect", "sim", "real"], []),

    // ── Diagnostics ────────────────────────────────────────────────────────
    spec!("ping", CommandGroup::Diagnostics, Host, false, "ping <host>", "ICMP echo through the host network", [], []),
    spec!("udp", CommandGroup::Diagnostics, Kernel, false, "udp <ip> <port> <text>", "send a UDP datagram and print the reply", [], []),
    spec!("http", CommandGroup::Diagnostics, Kernel, false, "http <ip> <port> [host]", "TCP GET / and print the response", [], []),
    spec!("socktest", CommandGroup::Diagnostics, Kernel, false, "socktest", "loopback UDP+TCP self-test", [], []),
    spec!("trace", CommandGroup::Diagnostics, Host, false, "trace list", "trace inspection", ["list"], []),
    spec!("crash", CommandGroup::Diagnostics, Host, false, "crash report <kind> <message>", "submit a crash report", ["report"], []),

    // ── Store ──────────────────────────────────────────────────────────────
    spec!("store", CommandGroup::Store, Host, false, "store list|sources|source|trust|search|install|update|check-updates|uninstall|rollback|verify", "package store (publish/sign are deferred)", ["list", "sources", "source", "trust", "search", "install", "update", "check-updates", "uninstall", "rollback", "verify"], [Capability::StoreInstall]),

    // ── Runtime ────────────────────────────────────────────────────────────
    spec!("wasm", CommandGroup::Runtime, Host, false, "wasm compile|call|mem|grow", "WASM sandbox", ["compile", "call", "mem", "grow"], []),
    spec!("wasi", CommandGroup::Runtime, Host, false, "wasi policy <mode>", "WASI syscall filter", ["policy"], []),
    spec!("exec", CommandGroup::Runtime, Host, true, "exec detect|caps|<program> [args...]", "compatibility inspection or run a text program", ["detect", "caps"], []),
    spec!("deps", CommandGroup::Runtime, Host, false, "deps scan|heal <path>", "dependency healing", ["scan", "heal"], []),
    spec!("compat", CommandGroup::Runtime, Host, false, "compat list|kill", "POSIX/Win32 compatibility processes", ["list", "kill"], []),
    spec!("telemetry", CommandGroup::Runtime, Both, false, "telemetry record|query|latest|avg|peak|clear", "telemetry ring", ["record", "query", "latest", "avg", "peak", "clear"], []),
    spec!("context", CommandGroup::Runtime, Host, false, "context stats|export|compact|persist", "context store", ["stats", "export", "compact", "persist"], []),
    spec!("workflow", CommandGroup::Runtime, Host, false, "workflow list|priority", "workflow profiles", ["list", "priority"], []),
    spec!("stability", CommandGroup::Runtime, Host, false, "stability list|best|worst", "stability scores", ["list", "best", "worst"], []),
    spec!("source", CommandGroup::Runtime, Host, false, "source <file>", "execute a file of console commands", [], []),

    // ── Bridge ─────────────────────────────────────────────────────────────
    spec!("bridge", CommandGroup::Bridge, Host, false, "bridge status|start|stop", "HTTP bridge control", ["status", "start", "stop"], [Capability::BridgeControl]),
    spec!("auth", CommandGroup::Bridge, Host, false, "auth register|login|me", "bridge tokens", ["register", "login", "me"], []),
    spec!("hub", CommandGroup::Bridge, Host, false, "hub status|wifi|scan|layout", "system hub aggregation; distinct from `sys info`", ["status", "wifi", "scan", "layout"], []),
];

/// Where an alias points. Ambiguous and deferred names never reach a backend.
#[derive(Debug)]
pub enum AliasTarget {
    /// Unambiguous replacement by a canonical command line.
    Canonical(&'static str),
    /// Replacement that applies only when the legacy name has no arguments.
    CanonicalBare(&'static str),
    /// Removed because the old name had incompatible meanings.
    Ambiguous(&'static [&'static str]),
    /// Kept as documentation but intentionally not implemented.
    Deferred(&'static str),
}

/// A legacy name mapped to its canonical replacement or rejection.
#[derive(Debug)]
pub struct Alias {
    /// Legacy spelling, lowercase; may contain several words.
    pub from: &'static str,
    /// Resolution target.
    pub target: AliasTarget,
}

macro_rules! alias {
    ($from:literal => $target:expr) => {
        Alias {
            from: $from,
            target: $target,
        }
    };
}

/// Legacy names preserved for compatibility, with conflicts made explicit.
pub const ALIASES: &[Alias] = &[
    // Unambiguous renames.
    alias!("kill" => AliasTarget::Canonical("process kill")),
    alias!("blocks" => AliasTarget::Canonical("block list")),
    alias!("unload" => AliasTarget::Canonical("block unload")),
    alias!("logs" => AliasTarget::Canonical("log show")),
    alias!("log" => AliasTarget::CanonicalBare("log show")),
    alias!("processes" => AliasTarget::Canonical("ps")),
    alias!("ver" => AliasTarget::Canonical("kernel ver")),
    alias!("wifi" => AliasTarget::CanonicalBare("wifi status")),
    alias!("net get" => AliasTarget::Canonical("net show")),
    alias!("q" => AliasTarget::Canonical("exit")),
    alias!("quit" => AliasTarget::Canonical("exit")),
    alias!("cls" => AliasTarget::Canonical("clear")),
    alias!("?" => AliasTarget::Canonical("help")),
    // Name collisions: one spelling, several incompatible operations.
    alias!("spawn" => AliasTarget::Ambiguous(&["process spawn (new process)", "block load (real wasm file)", "cluster spawn (deferred)"])),
    alias!("load" => AliasTarget::Ambiguous(&["block load (real wasm file)", "block mock (synthetic safe-mode block)"])),
    alias!("restart" => AliasTarget::Ambiguous(&["wd reset (controlled restart)", "block rollback (live-update revert)", "hw rescan (hardware refresh)"])),
    alias!("recover" => AliasTarget::Ambiguous(&["wd reset (orchestrator)", "update recover (live-update journal)"])),
    alias!("info" => AliasTarget::Ambiguous(&["status (runtime)", "sys info (hardware)", "kernel info (kernel telemetry)"])),
    alias!("list" => AliasTarget::Ambiguous(&["ps (processes)", "block list (blocks)", "vfs ls (files)"])),
    alias!("ls" => AliasTarget::Ambiguous(&["block list (blocks)", "vfs ls (files)"])),
    // Explicitly deferred surfaces.
    alias!("fetch" => AliasTarget::Deferred("web fetch requires a browser backend, not a text console")),
    alias!("search" => AliasTarget::Deferred("web search is not a local OS operation")),
    alias!("open" => AliasTarget::Deferred("opening URLs requires WebView/GUI")),
    alias!("intent" => AliasTarget::Deferred("natural-language intents require an LLM backend")),
    alias!("gui" => AliasTarget::Deferred("GUI mode is outside the text console")),
    alias!("tui" => AliasTarget::Deferred("framebuffer view switching is outside the text console")),
    alias!("tabs" => AliasTarget::Deferred("tab overview has no meaning in a text-only console")),
    alias!("cluster" => AliasTarget::Deferred("cluster control has no local backend")),
    alias!("store publish" => AliasTarget::Deferred("publishing needs a signing key and remote registry")),
    alias!("store sign" => AliasTarget::Deferred("signing is a developer workflow, not console operation")),
    alias!("perf-tune-video" => AliasTarget::Deferred("intent-only performance tuning")),
    alias!("memory-compaction" => AliasTarget::Deferred("reported compaction is a stub, not a real operation")),
    alias!("theme" => AliasTarget::Deferred("theming is visual; text console stays monochrome")),
];

/// Failure to turn one input line into a resolvable command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// Input contained only whitespace or a comment.
    Empty,
    /// Tokenizer rejected the line.
    Parse(ParseError),
    /// No spec and no alias matched.
    Unknown(String),
    /// Legacy name had several incompatible meanings.
    Ambiguous {
        /// Name the user typed.
        input: String,
        /// Canonical alternatives.
        candidates: &'static [&'static str],
    },
    /// Name is intentionally deferred.
    Deferred {
        /// Name the user typed.
        input: String,
        /// Why it is not available.
        reason: &'static str,
    },
    /// A grouped command needs one of its declared subcommands.
    MissingSubcommand {
        /// Canonical command id.
        command: &'static str,
        /// Usage string to print.
        usage: &'static str,
    },
    /// Unknown subcommand on a grouped command.
    UnknownSubcommand {
        /// Canonical command id.
        command: &'static str,
        /// Subcommand the user typed.
        subcommand: String,
        /// Valid subcommands.
        known: &'static [&'static str],
    },
}

impl ResolveError {
    /// Formats the error as a single diagnostic line.
    pub fn message(&self) -> String {
        match self {
            Self::Empty => "empty input".to_string(),
            Self::Parse(error) => {
                alloc::format!("cannot parse line: {} at {}", error.message, error.offset)
            }
            Self::Unknown(name) => alloc::format!("unknown command '{name}'; type help"),
            Self::Ambiguous { input, candidates } => alloc::format!(
                "ambiguous command '{input}'; use one of: {}",
                candidates.join(", ")
            ),
            Self::Deferred { input, reason } => {
                alloc::format!("command '{input}' is deferred: {reason}")
            }
            Self::MissingSubcommand { command, usage } => {
                alloc::format!("missing subcommand for '{command}'; usage: {usage}")
            }
            Self::UnknownSubcommand {
                command,
                subcommand,
                known,
            } => alloc::format!(
                "unknown subcommand '{subcommand}' for '{command}'; use one of: {}",
                known.join(", ")
            ),
        }
    }

    /// Converts the error into the console output contract.
    pub fn into_output(self) -> CommandOutput {
        match self {
            Self::Empty => CommandOutput::default(),
            Self::Deferred { .. } => CommandOutput::unavailable(&self.message()),
            _ => CommandOutput::usage(&self.message()),
        }
    }
}

/// A resolved command ready for backend dispatch.
#[derive(Debug, Clone)]
pub struct Invocation {
    /// Canonical spec selected by the registry.
    pub spec: &'static CommandSpec,
    /// Lowercase declared subcommand, if the command is grouped.
    pub subcommand: Option<&'static str>,
    /// Case-preserving positional arguments.
    pub args: Vec<String>,
    /// Parsed flags.
    pub flags: BTreeMap<String, Option<String>>,
    /// Original input line.
    pub raw: String,
}

impl Invocation {
    /// Returns positional argument `index`.
    pub fn arg(&self, index: usize) -> Option<&str> {
        self.args.get(index).map(String::as_str)
    }

    /// Returns the first positional argument.
    pub fn first_arg(&self) -> Option<&str> {
        self.arg(0)
    }

    /// Returns a flag value, or `None` when the flag is absent.
    pub fn flag(&self, key: &str) -> Option<Option<&str>> {
        self.flags.get(key).map(|value| value.as_deref())
    }

    /// Returns whether a flag was supplied.
    pub fn has_flag(&self, key: &str) -> bool {
        self.flags.contains_key(key)
    }

    /// Returns the canonical display name, e.g. `block load`.
    pub fn display_name(&self) -> String {
        match self.subcommand {
            Some(sub) => alloc::format!("{} {}", self.spec.id, sub),
            None => self.spec.id.to_string(),
        }
    }
}

/// Looks up a canonical spec by identifier.
pub fn find_spec(id: &str) -> Option<&'static CommandSpec> {
    SPECS.iter().find(|spec| spec.id == id)
}

fn prepare(
    head: String,
    mut positionals: Vec<String>,
    flags: BTreeMap<String, Option<String>>,
    raw: String,
) -> Result<Invocation, ResolveError> {
    let spec = find_spec(&head).ok_or_else(|| ResolveError::Unknown(head.clone()))?;
    if spec.subcommands.is_empty() {
        return Ok(Invocation {
            spec,
            subcommand: None,
            args: positionals,
            flags,
            raw,
        });
    }

    let Some(first) = positionals.first() else {
        if spec.allow_bare_args {
            return Ok(Invocation {
                spec,
                subcommand: None,
                args: positionals,
                flags,
                raw,
            });
        }
        return Err(ResolveError::MissingSubcommand {
            command: spec.id,
            usage: spec.usage,
        });
    };
    let lower = first.to_lowercase();
    if let Some(sub) = spec
        .subcommands
        .iter()
        .find(|candidate| **candidate == lower)
    {
        let sub = *sub;
        positionals.remove(0);
        return Ok(Invocation {
            spec,
            subcommand: Some(sub),
            args: positionals,
            flags,
            raw,
        });
    }
    if spec.allow_bare_args {
        return Ok(Invocation {
            spec,
            subcommand: None,
            args: positionals,
            flags,
            raw,
        });
    }
    Err(ResolveError::UnknownSubcommand {
        command: spec.id,
        subcommand: first.clone(),
        known: spec.subcommands,
    })
}

fn lower_words(name: &str, positionals: &[String]) -> Vec<String> {
    let mut words = Vec::with_capacity(positionals.len() + 1);
    words.push(name.to_string());
    words.extend(positionals.iter().map(|word| word.to_lowercase()));
    words
}

fn matches_prefix(words: &[&str], lower: &[String]) -> bool {
    words.len() <= lower.len()
        && words
            .iter()
            .enumerate()
            .all(|(index, word)| lower[index] == *word)
}

/// Parses and resolves one input line into a canonical invocation.
pub fn resolve(input: &str) -> Result<Invocation, ResolveError> {
    let parsed = parse_line(input).map_err(ResolveError::Parse)?;
    if parsed.is_empty() {
        return Err(ResolveError::Empty);
    }

    let lower = lower_words(&parsed.name, &parsed.positionals);

    // Multi-word aliases first so `store publish` wins over a bare `store`.
    for entry in ALIASES.iter().filter(|entry| entry.from.contains(' ')) {
        let words: Vec<&str> = entry.from.split_whitespace().collect();
        if !matches_prefix(&words, &lower) {
            continue;
        }
        return apply_alias(entry, &words, &parsed);
    }
    for entry in ALIASES.iter().filter(|entry| !entry.from.contains(' ')) {
        if entry.from == parsed.name {
            let words = [entry.from];
            return apply_alias(entry, &words, &parsed);
        }
    }

    prepare(parsed.name, parsed.positionals, parsed.flags, parsed.raw)
}

fn apply_alias(
    entry: &Alias,
    matched: &[&str],
    parsed: &crate::parser::ParsedLine,
) -> Result<Invocation, ResolveError> {
    match entry.target {
        AliasTarget::Ambiguous(candidates) => Err(ResolveError::Ambiguous {
            input: entry.from.to_string(),
            candidates,
        }),
        AliasTarget::Deferred(reason) => Err(ResolveError::Deferred {
            input: entry.from.to_string(),
            reason,
        }),
        AliasTarget::Canonical(target) => canonical_target(target, matched, parsed),
        AliasTarget::CanonicalBare(target) if parsed.positionals.is_empty() => {
            canonical_target(target, matched, parsed)
        }
        _ => prepare(
            parsed.name.clone(),
            parsed.positionals.clone(),
            parsed.flags.clone(),
            parsed.raw.clone(),
        ),
    }
}

fn canonical_target(
    target: &str,
    matched: &[&str],
    parsed: &crate::parser::ParsedLine,
) -> Result<Invocation, ResolveError> {
    let (head, leading) = split_alias_target(target).map_err(ResolveError::Parse)?;
    let skip = matched.len().saturating_sub(1);
    let mut positionals = leading;
    positionals.extend(parsed.positionals.iter().skip(skip).cloned());
    prepare(head, positionals, parsed.flags.clone(), parsed.raw.clone())
}

/// Generates help lines for all specs or for one group/command.
pub fn help(topic: Option<&str>) -> Vec<String> {
    let Some(topic) = topic else {
        let mut lines = alloc::vec![
            "AIOS text console commands".to_string(),
            "scope: [B] both backends, [H] host only, [K] kernel only".to_string(),
            String::new(),
        ];
        for group in GROUPS {
            lines.push(alloc::format!("[{}]", group.as_str()));
            lines.extend(
                SPECS
                    .iter()
                    .filter(|spec| spec.group == *group)
                    .map(CommandSpec::help_line),
            );
            lines.push(String::new());
        }
        return lines;
    };

    let lower = topic.to_lowercase();
    if let Some(group) = GROUPS.iter().find(|group| group.as_str() == lower) {
        let mut lines = alloc::vec![alloc::format!("[{}]", group.as_str())];
        lines.extend(
            SPECS
                .iter()
                .filter(|spec| spec.group == *group)
                .map(CommandSpec::help_line),
        );
        return lines;
    }
    if let Some(spec) = find_spec(&lower) {
        return alloc::vec![
            alloc::format!("  {}  {}", spec.scope.tag(), spec.usage),
            alloc::format!("      {}", spec.summary),
        ];
    }
    alloc::vec![alloc::format!("ERR usage: no help topic '{topic}'")]
}

/// Returns all canonical identifiers; used by parity tests and completion.
pub fn command_ids() -> Vec<&'static str> {
    SPECS.iter().map(|spec| spec.id).collect()
}
