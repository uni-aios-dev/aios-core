# AIOS Architecture

## System Overview

AIOS (AI-Native Operating System) is a modular microkernel-style OS designed for AI-native workloads. It consists of 33 Rust crates forming a layered architecture: foundation types at the bottom, hardware abstraction and process management in the middle, safety/security/context systems, and user-facing interfaces (the native kernel
framebuffer GUI, host TUI, integrated binary) at the top. Since v2.81.0 the
`aios-kernel` desktop is the single user-facing GUI; host egui/eframe code is
legacy reference only.

All inter-crate communication flows through a binary IPC protocol. Blocks (kernel modules) are hot-swappable with automatic rollback. An AI Orchestrator translates natural language intents into system operations. The `aios` crate provides a unified system binary with both interactive TUI mode and headless daemon mode, replacing the separate `aios-tui` and `aios-daemon` entry points.

> **See also:** `docs/SCHEME.md` — call-level program scheme with the per-crate function map; `docs/AUDIT.md` — full project audit report.

```
┌──────────────────────────────────────────────────────┐
│              Interface Layer (User-Facing)            │
│  Kernel GUI (fb) │  Host TUI   │  Unified `aios` bin │
├──────────────────────────────────────────────────────┤
│              Safety & Security Layer                  │
│  watchdog (heartbeat/safe-mode)                       │
│  security (capabilities/sandboxing)                   │
│  context (telemetry/workflows/stability)              │
├──────────────────────────────────────────────────────┤
│                Management Layer                       │
│  block-mgr (registry/loader/router)                  │
│  process-mgr (scheduler/crash resilience)             │
│  live-update (hot-swap/rollback)                      │
├──────────────────────────────────────────────────────┤
│              Abstraction Layer                        │
│  HAL (hardware detect / tier classification)          │
│  IPC (bus + channel transports)                       │
├──────────────────────────────────────────────────────┤
│              Foundation Layer                         │
│  core (types / protocol / crypto / errors)            │
└──────────────────────────────────────────────────────┘
```

---

## Unified Text Console (`aios-console`)

The three existing command front-ends (standalone `aios-tui`, the `aios`
orchestrator TUI and the ring-0 `aios-kernel` shell) used five independent
parsers with conflicting names and semantics. `aios-console` is the single
`no_std`-compatible command core that replaces those parsers:

```
   text line ──> parser ──> registry.resolve ──> scope/capability gate
                                                      │
                                        CommandBackend::execute
                                                      │
                                          CommandOutput (text lines)
```

- `parser.rs` tokenizes quotes, escapes and `#` comments. Command and
  subcommand names are lowercased; arguments keep their original case.
- `spec.rs` / `registry.rs` hold the canonical `CommandSpec` table, the alias
  table and generated `help`. Ambiguous legacy names are rejected with the list
  of canonical alternatives; deferred names report `unavailable`. Unambiguous
  spellings (`processes`, `ver`, `cls`) map directly, while argument-free
  aliases (`log`, `wifi`) select a default subcommand without capturing
  `log clear` or `wifi scan`.
- `capability.rs` declares `Scope` (`[H]`, `[K]`, `[B]`) and `CapabilitySet`;
  `backend.rs` gates every invocation before calling a backend.
- `output.rs` defines `CommandOutput`, exit codes (`0` OK, `1` usage,
  `2` denied, `3` unavailable, `4` backend error) and `OutputControl`
  (`Clear`, `Exit`) so the kernel `clear` bug cannot recur in the new core.
- `deferred.rs` keeps the commented-out GUI/WebView/LLM/cluster/pipe variants.

`aios-console/src/host.rs` implements `HostBackend` behind the `std` feature. It
delegates to the same library crates the current host TUI uses: `Scheduler` for
`ps`/`process`, `BlockRegistry`/`BlockLoader` for `block`, `NetSettingsBlock`
for `net`, `StoreManager` for `store`, and `AiosVfs` for `vfs`. `source <file>`
runs a script by feeding each line back through the same `dispatch()` entry
point, with an 8-level recursion guard. `metrics` and `health` share one
`MetricsSnapshot` collected from the scheduler (process states, crash log, RAM),
the block registry (block states) and session state (restarts, log); `metrics`
renders it as text, JSON or Prometheus exposition, while `health` derives a
status plus per-issue findings. `telemetry` owns a `TelemetryStore` from
`aios-context`, so samples recorded through the console use the same ring
representation as the rest of the system. `ipc` drives an `Arc<Mutex<IpcBus>>`
shared with `BridgeContext`; `freeze` holds drained packets in the backend until
`unfreeze`. `sys` and `hw` read `aios-hal` `HardwareProfile` snapshots: the first
command detects hardware lazily, `hw rescan` forces re-detection and `hw profile`
selects a mock profile; `hw fingerprints` runs
`aios_autohal::extract_fingerprints` on that profile, and
`set_hardware_profile` injects a snapshot for tests. Session state (history,
log, restart counter, source depth) lives in the backend.
`HostBackend::from_handles`
accepts shared `Arc<Mutex<Scheduler>>` / `Arc<Mutex<BlockRegistry>>` handles so
the console and the orchestrator can observe the same runtime; `with_config`
creates private handles for tests. Subcommands without a real implementation
return `unavailable` rather than a fake success. `TuiApp` owns one `HostBackend`
built from the orchestrator's handles, and the shell tab routes `console <...>`
lines through it. Bare `clear`/`cls` now go through the same `console_execute`
path: both emit no lines and only `OutputControl::Clear`, matching the legacy
buffer clear; they are also recorded in `HostBackend` history. The first batch of
bare `aios` adapters also covers `help`/`?`, `ps`, `blocks`, valid `kill`, `logs`,
`status`, `restart`, `store list`, the fixed usage lines for
`kill`/`spawn`/`store`/`net` (including missing `store search`/`store install`
arguments) and the unknown-command fallback. `execute_aios_adapter` returns a
flag, and `shell_execute` falls back to `shell_execute_legacy` for everything
else. Pure formatters live in `aios-console/src/legacy.rs` (`AIOS_HELP`,
`aios_unknown_text`, `aios_usage_text`, `aios_kill_success`, `aios_kill_failed`,
`aios_log_text`, `aios_status_text`, `aios_restart_text`, `aios_restart_log`) and
the data adapters in `HostBackend` (`aios_process_list_text`,
`aios_block_list_text`, `aios_kill`, `aios_store_list_text`). The `kill` adapter
keeps the distinct `aios` wording (`  killed '<name>' (<pid>)` /
`  kill failed: <error>`) instead of the safe-mode `LegacyMutation` text and
records no event, because the `aios` dispatcher does not log kills. Bare
`restart` keeps its hardware re-probe in the frontend and its fixed output/log
lines in `aios-console`; it is a different operation from canonical `wd reset`
and safe-mode `recover`. Bare `cluster` commands use the host-only
`aios-console/src/legacy_host.rs::aios_cluster_text` adapter over the same
`DistributedScheduler` handle; the formatter and all mutating subcommands live in
`aios-console`, while `aios-console` keeps its `no_std` kernel build because the
new module is gated behind the `std` feature. Bare `spawn <path>` now calls
`HostBackend::aios_spawn_path`, which uses the shared block registry and returns
the exact legacy log lines; the adapter records them in the TUI log sink and emits
no shell lines, as the old dispatcher did. Valid `store search`/`store install`
use `aios_store_search_start`/`aios_store_install_start` from `legacy.rs` over the
same asynchronous `store_spawn` seam, while `net get`/`net set` use
`AiosNetResult` plus `aios_net_get_text`/`aios_net_set_text`; both keep the
frontend-owned router and tokio handles, but no command is handled by
`shell_execute_legacy` in the normal shell path. That dispatcher remains only as
the byte-for-byte parity reference in tests.
`aios-shell` likewise owns one `HostBackend` created in `main` and recognises a
case-insensitive `console <...>` prefix before alias expansion, so the registry
never sees the prefix itself. `console` alone prints generated help;
`OutputControl::Clear` clears the terminal and `Exit(code)` exits the process.
Bare POSIX builtins (`help`/`?`, `clear`/`cls`, `pwd`, `echo`, `history`,
`alias`, `unalias`, `cd`, `env`/`export`/`unset`, `source`/`.`,
`jobs`/`fg`/`bg`) are handled by `execute_shell_adapter`, which returns a
`ShellOutcome` (`Lines`, `Stderr` or `Clear`) and uses exact formatters from
`aios-console/src/legacy.rs` (`SHELL_HELP`, `shell_echo_text`, `shell_pwd_text`,
`shell_alias_text`, `shell_alias_set_text`, `shell_history_text`,
`shell_cd_error_text`, `shell_env_text`, `shell_export_set_text`,
`shell_env_value_text`, `shell_source_recursion_text`,
`shell_source_missing_text`, `shell_jobs_text`, `shell_fg_text`,
`shell_bg_text`, `shell_job_error_text`). The frontend still owns real side
effects: history/alias files, `$VAR` expansion, environment mutation,
`OLDPWD`/current directory, sourced-file execution, PID pruning, job listing and
`SIGCONT` delivery, as well as screen I/O. Parity tests compare text byte-for-byte,
including the original 60-column ANSI help banner; stateful tests verify the side
effects. The recursion guard in `source` releases its `RefCell` borrow before
running sourced lines, so a recursive `source` reports an error instead of
panicking as the legacy dispatcher did. Canonical `console jobs`/`fg`/`bg` use the
same `JobTable`: `main` injects it into `HostBackend` as a `JobControl` seam, so
the console lists the shell's live background jobs and `fg <id>`/`bg <id>` deliver
a real `SIGCONT` through the owning frontend. A bare `HostBackend` without that
seam still reports `unavailable`. Only inherently frontend-side lines remain in
`run_command_legacy`: external programs, GUI/TUI launchers, `logs`/`hw`/`services`,
`type`/`exec`/`which`, `aios-shell-reload` and `exit`.
The standalone `aios-tui` binary likewise owns one `HostBackend` created in
`main`. Its Shell tab recognises the same case-insensitive `console <...>` prefix
before the TUI extras and the `SafeModeShell` fallback. `execute_shell_cmd`
returns a flag so `console exit` can break the event loop through the normal
shutdown path, and `console clear` clears the shell buffer. Its `Scheduler` and
`BlockRegistry` are now `Arc<Mutex<...>>` shared with `HostBackend::from_handles`,
so `console ps` and `console block list` observe the same processes and blocks as
the safe-mode commands and the dashboard. Bare `ps`, `processes`, `list`,
`blocks`, `ls`, `status`, `info`, `exit`, `quit`, `q`, `help`, `?`, `logs`, `log`,
`kill`, `spawn`, `load`, `unload`, `restart` and `recover` now use `HostBackend`
legacy adapters, which map runtime data and real mutations back to the exact
legacy text, so their output is unchanged. The read-only adapters are
`process_list_legacy_text`, `block_list_legacy_text`, `status_legacy_text`,
`exit_legacy_text`, `legacy_help_text` and `legacy_log_text`. The five mutation
adapters (`legacy_kill`, `legacy_spawn`, `legacy_load`, `legacy_unload`,
`legacy_restart`) parse the whole command exactly like `SafeModeShell`, run the
real scheduler/registry operation or use the bounded restart counter through
the shared backend, and record the legacy event on success. Pure output and
event formatting lives in `aios-console/src/legacy.rs::LegacyMutation`. The
log adapter reads an event-log handle injected with `HostBackend::set_event_log`;
`SafeModeShell::with_event_log` appends safe-mode events to the same handle
without adding a console→watchdog dependency. Failure lines keep the exact
legacy `Failed: <error>` wording, printed by the TUI as `Error: Failed: <error>`;
the restart-limit failure is the one exception with its own legacy wording
`Max restarts (<max>) reached. Manual intervention required.` (no `Failed:`
prefix). `legacy_restart` deliberately uses `HostBackend::restarts` /
`config.max_restarts` instead of `WatchdogControl::reset`, so bare `restart` and
`recover` keep the legacy counter and event semantics; only canonical
`wd reset` calls the injected watchdog seam.
Bare `exit`/`quit`/`q` print the legacy line but keep the legacy shutdown flag:
only `console exit` breaks the event loop. The help adapter preserves the
separate TUI extras block. The unknown-command fallback also uses a console
adapter (`legacy::unknown_text`), so it no longer executes `SafeModeShell`.
Canonical `console info` remains ambiguous; only the legacy safe-mode alias is
adapter-backed. Other lines keep the existing TUI behaviour.
All four frontends use `aios_console::frontend::console_prefix`, a `no_std`
helper in the core crate: it matches the prefix case-insensitively, preserves
argument case and treats bare or whitespace-only `console` as a help request.
`BridgeContext.registry` is now `Arc<Mutex<BlockRegistry>>` so the bridge, TUI
and console observe the same blocks; `BridgeContext.ipc_bus` is likewise
`Arc<Mutex<IpcBus>>`, shared with the console. The legacy dispatchers are kept
until full parity is verified. Parity is checked by
`aios-console/tests/legacy_parity.rs` across all five command surfaces and by
unit tests against the pure seams `SafeModeShell::parse_command`,
`IntentEngine::translate` and the `aios-shell` alias expander. Hardware-bound
handlers stay out of these tests; the table asserts resolution results only.
`legacy_parity.rs` therefore proves resolution but not output equality:
`aios/src/tui/mod.rs` pins the exact legacy lines for `help`/`?`, unknown
commands, `kill`/`store`/`net` usage strings and empty `ps`, and sixteen binary
parity tests compare the adapter path with `shell_execute_legacy` byte-for-byte
(read-only commands share one `TuiApp` instance so the `HashMap` iteration order
in `Scheduler::all_processes` is identical for both paths), including `cluster`
with clustering disabled and with a live scheduler; one more test checks
that bare `restart` still appends the legacy log entry, while
`aios-console/tests/host_backend.rs` pins the canonical host wording for
`help`, `clear` and usage errors. `aios-watchdog/src/safe_mode.rs` now also pins
the exact legacy `ShellResponse.success` and `output` for every branch — empty
and populated lists, process/block lifecycle success and failure, `status`,
empty and numbered `logs`, restart success and limit, `help`, `exit` and the
mixed-case unknown-command fallback. These are the output-level targets any
future `SafeModeShell` migration must match; resolver-only tables are not enough.

`HostBackendConfig.context_db_path` selects the redb file used by `context
persist`. `HostBackend::execute_line()` is the alias- and trace-aware entry
point: session aliases created by `alias`/`unalias` are expanded before dispatch
and every invocation is recorded as a span in `aios-telemetry::TraceContext`
(`trace list`). A second batch of adapters, all backed by real library calls,
covers `sleep`, `alias|unalias`, `fault resolve`, `sched mode|quantum|aging|
rt-deadline|rt-clear`, `net validate|config|status|apply|lease`,
`store trust|verify`, `vfs readat`, `caps list|issue|revoke|check`,
`sandbox start|stop|log|mem`, `hw-protect status|mpk|tee|iommu|clear`,
`wasm compile|call|mem|grow`, `wasi policy`, `exec detect|caps|<program>`,
`deps scan|heal`, `compat list|kill`, `context stats|export|compact|persist`,
`workflow list|priority`, `stability list|best|worst`, `trace list`,
`crash report`, `driver list|failures|remove|rollback|install|provision`,
`hotplug start|events|drain`, `wifi status|scan|connect|disconnect|sim|real`
and `hub status|scan|wifi|layout`. They use the optional std dependencies
`aios-debug`, `aios-exec-compat`, `aios-iommu`, `aios-mpk`, `aios-security`,
`aios-sys-control`, `aios-tee`, `aios-telemetry` and `aios-wasm`; the kernel
`no_std` build is unaffected. `aios-live-update` is now an optional std
dependency: `block swap|rollback` and `update history|pending|recover` use the
real `PersistedLiveUpdateEngine` journal, rollback entries and crash recovery.
`block swap` reports that the swap was recorded and a rollback entry stored; it
does not claim live block code was replaced because the registry does not expose
live block instances.

Dependency cycles for watchdog/bridge/auth/file-manager commands are avoided
with the injection seams in `aios-console/src/control.rs` (`WatchdogControl`,
`BridgeControl`, `AuthProvider`, `FileManagerControl`). The `aios` binary
implements the first three in `aios/src/console_control.rs` over its own handles:
`WatchdogRunner::from_watchdog` attaches a monitoring thread to the same
`Watchdog` the bridge status endpoint reads, `start_server_with_shutdown` lets
`bridge stop` gracefully close the listener and `bridge start` spawn it again,
and `auth` calls the real `AuthStore`. `WatchdogControl::reset` calls
`Watchdog::reset` on the shared instance (keeping an attached runner alive) and
`wd reset` requires that seam before incrementing the bounded restart counter.
The `FileManagerControl` seam is implemented in `aios-fm/src/console_control.rs`
over a live `FileManager`: `jobs()` maps the engine's job snapshot into console
text and `cancel()` calls the real `Progress::cancel()` token. `aios-tui`
injects this control after starting its `FileManager`, so `console fm jobs` and
`console fm cancel <id>` observe and cancel actual background transfers. The
`JobControl` seam is implemented by `aios-shell`'s `JobTable`: it shares the
process table with the bare builtins, `jobs()` returns a live snapshot, and
`fg`/`bg` find the requested id and deliver `SIGCONT` before updating or keeping
the table. `aios-shell` injects it into `HostBackend` in `main`, so canonical
`console jobs`/`fg <id>`/`bg <id>` are real. Without an injected adapter these
commands still return `unavailable`.

`aios-console/src/unavailable.rs` holds the precise reason for the registered
commands that still have no real host API, so `ERR unavailable` names the missing
capability instead of only saying "no host backend yet": `jobs|fg|bg` need a
host-owned job table with OS process groups, signals and TTY foreground that
`aios-process-mgr` does not model (the `JobControl` seam supplies it in
`aios-shell`). `acl grant|revoke` and `ping` are now real: `aios-vfs` provides a
path-scoped `PathAcl` that `AiosVfs` enforces after canonicalization, while
`aios-net::icmp::ping` sends actual ICMPv4 echo requests (`IcmpSendEcho` on
Windows, an ICMP datagram/raw socket on Unix) and correlates replies by
identifier, sequence and checksum. `HostBackend` shares one `PathAcl` with
`open_vfs`, so console grants affect the same policy that `vfs` enforces, and
uses a default `IcmpPingControl` that tests replace with a mock seam. Host
`net recheck`, `net apply` and `net adapters` are now real: `NetManager` queries
the OS (`netsh` on Windows, `ip addr` on Linux), refreshes the cached link/lease
state and reports actual adapter rows; `net apply` persists the config before
triggering that recheck. Commands whose seam is still absent stay
`unavailable` until that adapter is injected.
`fm jobs|cancel` is now real wherever a live `FileManagerControl` is injected
(`aios-tui`), and `jobs|fg|bg` are real wherever a live `JobControl` is injected
(`aios-shell`); a bare `HostBackend` without those seams still reports
`unavailable`. `state extract|restore` is now real: `BlockExecutor::extract_state`
and `restore_state` snapshot the live linear memory of a WASM block that has been
executed (the same migration primitive `WasmLiveUpdateEngine` uses), and
`HostBackend` keeps the snapshot in the session so `state restore <block>` writes
it back into the live instance. `KernelBackend` uses the same `net` command
names but reads and mutates the kernel's own settings through
`aios-console::net_text`; its `net recheck` calls the real
`crate::net::recheck()` instead of reporting a cached verdict.

`aios-kernel/src/console_backend.rs` implements `KernelBackend` with
`aios-console` as a `no_std` dependency (`default-features = false`). It adapts
the existing kernel entry points: generated `help`, `echo`, `version`,
`kernel info|ver`, `faults stats|history|pending`, `fault resolve`,
`wifi status|sim|real|scan|connect|disconnect`,
`net show|config|status|lease|set|apply|validate|reset|adapters|recheck`,
`udp`, `http` and `socktest`.
Everything else returns `unavailable`. The kernel `clear` bug is fixed here:
the backend clears `SHELL.history` and emits an explicit `OutputControl::Clear`.
The kernel shell exposes the backend as `console <...>` through the shared
case-insensitive prefix helper, so `CONSOLE VER` and `CoNsOlE echo MiXeD Case`
reach it; the legacy dispatcher remains for all other commands and keeps its
case-sensitive bare spellings. This is a separate implementation
because `aios-kernel` is `x86_64-unknown-none` and cannot link `std`, `ratatui`,
`tokio` or `reqwest`.

The kernel TUI's bare shell commands are now adapter-backed as well.
`aios-kernel/src/tui.rs::execute_kernel_adapter` calls `no_std` formatters in
`aios-console/src/legacy.rs` for `help`, `ver`, `info`, `faults`, `wifi` (`sim`/
`real`), `echo`, `clear` and the unknown-command fallback. `KERNEL_HELP`,
`KERNEL_UNKNOWN`, `KernelFaultLine` and `KernelFaultStats` pin the exact legacy
text; `KernelFaultStats` groups the fault counters so the formatter stays below
the clippy argument limit without suppressing the lint. `run_command_legacy` is
kept only as a parity reference, and parity tests compare both paths
byte-for-byte. Bare `clear` now clears the shell scrollback like `console clear`.
`tabs`, `gui`, `tui`, `udp`, `http` and `socktest` stay on the legacy path until
real adapters exist; canonical `console info` remains ambiguous.

---

## Layer 1: Foundation (`aios-core`)

### Error Handling (`error.rs`)

All AIOS operations return `aios_core::error::Result<T>`, where `T = std::result::Result<T, AIOSException>`.

`AIOSException` has 19 variants covering every failure mode in the system:

| Variant | Use Case |
|---------|----------|
| `BlockNotFound(String)` | Block ID not in registry |
| `BlockAlreadyRegistered(String)` | Duplicate block name |
| `InvalidSignature { expected, actual }` | SHA-256 mismatch on binary |
| `IntegrityCheckFailed(String)` | General integrity failure |
| `StateExtractionFailed(String)` | Cannot serialize block state |
| `StateRestoreFailed(String)` | Cannot deserialize block state |
| `HotSwapFailed(String)` | Atomic hot-swap failed mid-operation |
| `RollbackFailed(String)` | Cannot restore previous block version |
| `IPCError(String)` | IPC transport failure |
| `SchedulerError(String)` | Process scheduling failure |
| `ProcessNotFound(u64)` | PID not in scheduler |
| `ProcessAlreadyExists(u64)` | Duplicate PID |
| `PermissionDenied(String)` | Unauthorized operation |
| `HardwareNotDetected(String)` | Missing hardware component |
| `InvalidPayload(String)` | Malformed IPC payload |
| `Timeout(String)` | Operation timed out |
| `ConfigurationError(String)` | Invalid configuration |
| `SerializationError(String)` | Bincode failure |
| `Generic(String)` | Catch-all |

### Block Types (`block.rs`)

**`BlockId`** — Unique 32-bit identifier for every block. Implements `Display` as `"block_{id}"`.

**`BlockManifest`** — Metadata for a registered block:
- `id: BlockId`, `name: String`, `version: String`, `sha256: [u8; 32]`

**`BlockState`** — Lifecycle states:
```
Unloaded → Loaded → Active ↔ Frozen → Unloaded
                  ↓
                Error
```

**`StatefulBlock` trait** — Interface every block must implement:
```rust
pub trait StatefulBlock: Send {
    fn id(&self) -> BlockId;
    fn name(&self) -> &str;
    fn version(&self) -> &str;
    fn state(&self) -> BlockState;
    fn handle_message(&mut self, packet: &IpcPacket) -> Result<Option<IpcPacket>>;
    fn extract_state(&self) -> Result<Vec<u8>>;      // default: empty
    fn restore_state(&mut self, state: &[u8]) -> Result<()>;  // default: no-op
    fn health_check(&self) -> bool;                   // default: true
}
```

### SHA-256 Crypto (`crypto.rs`)

Four functions for binary integrity:
- `compute_sha256(data) -> String` — hex-encoded hash
- `compute_sha256_bytes(data) -> [u8; 32]` — raw 32-byte hash
- `verify_sha256(data, expected_hex) -> bool` — string comparison
- `verify_sha256_bytes(data, expected_bytes) -> bool` — byte comparison

### IPC Protocol (`ipc_protocol.rs`)

Binary protocol using `bincode` serialization. Every packet is self-describing and integrity-checked.

**`Header`** (fixed-size, `#[repr(C)]`):
| Field | Type | Description |
|-------|------|-------------|
| `packet_id` | `u64` | Auto-incrementing unique ID (AtomicU64) |
| `source_block` | `u32` | Sender block ID |
| `target_block` | `u32` | Receiver block ID |
| `command_id` | `u16` | Operation code from `CommandId` enum |
| `priority` | `u8` | 0-255, higher = more urgent |
| `payload_len` | `u32` | Byte length of serialized payload |
| `checksum` | `[u8; 32]` | SHA-256 of payload bytes |

**`Payload` enum** (15 variants):
- `Empty`, `Binary(Vec<u8>)`, `Text(String)`
- Block operations: `RegisterBlock`, `UnloadBlock`, `GetTopology`
- Process operations: `SpawnProcess`, `KillProcess`, `AdjustPriority`
- System operations: `HealthCheck`, `ExtractState`, `RestoreState`
- Update operations: `HotSwap`, `Rollback`
- AI operations: `IntentCommand`
- Extensible: `Custom(String, Vec<u8>)`

**`CommandId` enum** (13 commands, u16 repr):
- Block domain: `0x0001`-`0x0003`
- Process domain: `0x0010`-`0x0012`
- System domain: `0x0020`-`0x0031`
- Update domain: `0x0040`-`0x0041`
- AI domain: `0x0050`
- Extensible: `0x00FF`

**`Response` enum**: `Success(Payload)`, `Failure { code, message }`, `Timeout`

**`IpcPacket`** methods:
- `new()` — auto-generates packet_id and SHA-256 checksum
- `serialize()` / `deserialize()` — bincode binary encoding
- `verify_checksum()` — integrity check
- `response_ok()` / `response_err()` — response factories

**Performance**:
- Debug: < 50us per packet serialize+deserialize
- Release: < 1us per packet serialize+deserialize

---

## Layer 2: Abstraction

### IPC Transport (`aios-ipc`)

#### Bus (`bus.rs`)

`IpcBus` — VecDeque-backed message bus for block-to-block communication:
- **Priority-based ordering** — `send_priority()` dequeues highest-priority packets first
- FIFO ordering within same priority level
- **Backpressure policies**: `Reject` (returns error) or `DropOldest` (evicts front of queue)
- **Message deduplication** — `with_dedup()` enables packet_id-based dedup via `HashSet<u64>`
- **Bus metrics** — `BusMetrics` tracks `total_sent`, `total_received`, `total_dropped`, `total_deduplicated`, `peak_queue_depth`, `avg_send_latency_us`
- **Freeze/Unfreeze** for atomic state transfer during hot-swap
- **Frozen bus rejects** new messages (returns `SchedulerError`)

`SharedIpcBus` — `Arc<Mutex<IpcBus>>` wrapper for thread-safe multi-producer/single-consumer access. Implements `Clone`.

**Freeze protocol** (used during hot-swap):
```
1. bus.freeze() → drains queue, returns Vec<IpcPacket>, sets frozen=true
2. Perform swap operations (no messages lost)
3. bus.unfreeze(saved_packets) → restores packets in order, sets frozen=false
```

#### Channel (`channel.rs`)

`IpcSender` / `IpcReceiver` — `mpsc`-based typed channel:
- `IpcSender::send()` — non-blocking, returns `Result<()>`
- `IpcReceiver::receive()` — blocking
- `IpcReceiver::try_receive()` — non-blocking, returns `Option<IpcPacket>`
- `IpcSender` implements `Clone` for multi-producer

### Hardware Abstraction Layer (`aios-hal`)

#### Hardware Detection (`hardware.rs`)

`HardwareProfile` — Complete system hardware description:

```rust
pub struct HardwareProfile {
    pub cpu: CpuInfo,
    pub gpu: Option<GpuInfo>,
    pub npu: Option<NpuInfo>,
    pub memory: MemoryInfo,
    pub pci_devices: Vec<PciDevice>,
}
```

**Detection methods**:
- `HardwareProfile::detect()` — real hardware via OS APIs:
  - Windows: `wmic` commands
  - Linux: `/proc/cpuinfo`, `/proc/meminfo`
  - x86: CPUID intrinsics for feature detection

**CpuInfo fields**: cores, threads, model, has_avx512, has_avx2, has_sse42, has_neon, base_freq_mhz, vendor (Intel/AMD/ARM/Apple/Unknown)

**GpuInfo fields**: name, vram_mb, compute_shaders, vendor, driver_version, cuda_cores, compute_capability

**GPU detection methods**:
- `detect_gpu_nvidia()` — Windows: runs `nvidia-smi --query-gpu=name,memory.total,driver_version,compute_cap`
- `estimate_cuda_cores(gpu_name)` — maps GPU model names to CUDA core counts (RTX 4090→16384, A100→6912, H100→16896)
- `detect_gpu_wmic()` — legacy Windows WMI fallback
- `detect_gpu_amd()` — Linux: parses `rocm-smi --showproductname --showmeminfo vram` output

**Storage detection**:
- `StorageDevice` struct: `name`, `interface`, `capacity_gb`, `model`
- `StorageInterface` enum: `NVMe`, `SATA`, `USB`, `Unknown`
- `detect_storage()` — Windows: `wmic diskdrive` / Linux: `/sys/block` enumeration
- `HardwareProfile::storage_devices: Vec<StorageDevice>` — present on all profiles

**Mock profiles** (for testing without real hardware):
- `mock_legacy()` — Intel i5-3570, 8GB, no GPU/NPU → Tier 2
- `mock_modern()` — AMD Ryzen 9 7950X, 64GB, RTX 4090 (full GPU info), XDNA2 NPU → Tier 1
- `mock_legacy_2012()` — Intel i3-3220, 4GB, Intel HD 2500 → Tier 3
- `mock_nvidia()` — AMD Ryzen 9 7950X3D, 128GB, RTX 4090 (16384 CUDA cores, 8.9 compute capability) → Tier 1

**`HalBlock`** — Hardware abstraction as a `StatefulBlock`:
- Responds to `HealthCheck` and `Custom("get_hardware_profile")` IPC messages
- Serializes/deserializes entire `HardwareProfile` for state extraction
- Health check: true if CPU cores > 0

#### AI Tier Classification (`ai_tier.rs`)

`AiTier` — Classifies hardware capability for AI workloads:

| Tier | Requirements | Max Model | Batch Size | Use Case |
|------|-------------|-----------|------------|----------|
| **Tier 1** | NPU + GPU + AVX-512 + ≥16GB RAM | 70 GB | 64 | Local LLM inference |
| **Tier 2** | AVX2/NEON + ≥4GB RAM | 7 GB | 8 | Edge inference |
| **Tier 3** | Everything else | 0.5 GB | 1 | Lightweight tasks |

Classification is deterministic based on hardware flags. Any single missing requirement drops to the next lower tier.

---

## Layer 3: Management

### Block Manager (`aios-block-mgr`)

#### Hybrid block model (overview)

AIOS uses a hybrid model: a small **compiled-in base set** of native blocks plus **dynamic disk blocks**.

- **Base blocks (compiled-in)**: core HAL/browser/Net-settings blocks are compiled into the `aios` binary and registered at startup. The compiled-in set is selected via Cargo features: `base-hal`, `base-browser`, `base-net-settings`, `base-all` (default `["webview","base-all"]`). Base factories/imports are `cfg(feature = "...")`-gated, so builds can trim the base set with `--no-default-features --features ...` (build scripts for live/initramfs add `--features base-all` when `--no-default-features` is used). On top of the build-time choice the **runtime trims the set to the detected profile** (`base_factories_for(total_ram_mb)`): the compiled-in set is the upper bound, and a block with a hard hardware requirement is left out when the machine does not meet it (v2.62.0 — below `BROWSER_MIN_RAM_MB` = 4096 MiB the `browser` block is skipped and logged).
- **Disk blocks (dynamic)**: discovered under `AIOS_BLOCKS_DIR` as `.wasm` or `.bin` bundles with sidecar metadata (`block.toml` / `manifest.json`). `.wasm` blocks are executed via `aios-wasm` (Wasmtime), wired to the `MessageRouter` and can implement IPC (`handle_message` ABI). `.bin`/non-WASM payloads are **registered only** (never executed). `DiskBlockReport` exposes `registered_only_count()` to surface them.

#### Registry (`registry.rs`)

`BlockRegistry` — Central block catalog:
- `register_block(name, version, binary)` → `BlockId` — assigns ID, computes SHA-256, stores as `Loaded`
- `activate_block(id)` → state transition to `Active`
- `unload_block(id)` → removes entry, returns `BlockEntry`
- `topology()` → `Vec<BlockManifest>` — all registered blocks
- `verify_signature(id)` → recomputes SHA-256 vs stored hash
- `find_by_name(name)` → name-based lookup
- `load_from_path(dir)` → scans directory for `.wasm` and `.bin` files, loads all discovered blocks
- `load_from_path_str(dir_str)` → string-path convenience wrapper
- `boot_discover(root)` → recursive walk of all subdirectories, discovers and registers `.wasm`/`.bin` files, creates directory if missing

`BlockEntry` stores: `manifest: BlockManifest`, `state: BlockState`, `binary: Vec<u8>`, `capabilities: Option<CapabilityToken>`

#### Loader (`loader.rs`)

`BlockLoader` — High-level load pipeline:
1. `validate_binary(binary, expected_sha256)` — SHA-256 comparison
2. `load_from_binary(registry, name, version, binary)` — register + validate + activate in one call
3. `load_from_binary_with_capabilities(registry, name, version, binary, token)` — same, but assigns an optional `CapabilityToken`
4. `load_from_directory(registry, dir)` — scans directory for `.wasm`/`.bin` files, looks for sidecar `.json` manifests (name, version, capabilities, TTL overrides), loads each
5. `unload_block(registry, id)` — warns if unloading active block

`BlockManifestJson` — sidecar manifest struct parsed from `.json` files:
- `name: Option<String>` — override block name (instead of filename)
- `version: Option<String>` — override version
- `capabilities: Option<Vec<String>>` — capability names to assign (e.g., `CAP_NET_BIND`)
- `ttl_ms: Option<u64>` — capability token TTL (default: 3600000ms)

#### Factory (`factory.rs`)

`BlockFactory` — constructs a compiled-in (native) block, the "base set" half of
the hybrid loader:
- `name()` / `version()` — registry key and manifest version
- `create(id)` → `Box<dyn StatefulBlock>` — builds the block bound to its `BlockId`

`BlockFactoryRegistry` — ordered catalog of factories:
- `register(factory)` — rejects duplicate names with `BlockAlreadyRegistered`
- `get(name)` / `contains(name)` / `names()` / `len()` / `is_empty()`
- `instantiate(name, id)` → `Box<dyn StatefulBlock>` — `BlockNotFound` when missing

Concrete factories live in the consumer (`aios/src/blocks.rs`), keeping this crate free of block-specific dependencies.

#### Router (`router.rs`)

`MessageRouter` — Dispatches IPC packets to block handlers:
- `register_handler(block_id, handler)` — attach `Box<dyn FnMut>` handler
- `add_route(from, to)` — redirect mapping (block A's messages go to block B)
- `dispatch(packet)` — resolves route, then invokes handler
- `route_target(target)` — returns redirect target or original

Handler signature: `FnMut(&IpcPacket) -> Result<Option<IpcPacket>>`

#### Dependency Graph (`dependency.rs`)

`DependencyGraph` — manages block load/unload ordering:
- `add_block(name)` — register a block without dependencies
- `add_dependency(block, depends_on)` — declare dependency with cycle detection (DFS)
- `load_order()` — topological sort (Kahn's algorithm) for correct initialization sequence
- `unload_order()` — reverse topological for safe teardown
- `dependencies_of(block)` / `dependents_of(block)` — graph queries
- `remove_block(name)` — removes node and all references from other dependencies

**Cycle detection**: `add_dependency()` checks for cycles before adding an edge. Returns `DependencyError::CircularDependency` with the cycle chain.

**Topological sort**: Dependencies are loaded before dependents. Independent nodes may appear in any order (HashMap iteration order is non-deterministic).

#### Semantic Versioning (`version.rs`)

`SemanticVersion` — block version management:
- `parse("1.2.3")` / `parse("v2.0.1")` — supports optional `v` prefix
- `Ord` implementation: compares major → minor → patch
- `is_compatible_with(base)` — same major, current minor >= base minor
- `is_newer_than(other)` — self > other
- `bump_major/minor/patch()` — version incrementing
- `Display` — `"1.2.3"` format

#### Hot-Reload (`hot_reload.rs`)

`HotReloader` — watches a directory for new/updated/removed block files:
- `HotReloadConfig`: `watch_dir`, `poll_interval_ms`, `auto_activate`
- `scan_and_reload(registry)` — scans for `.bin`/`.aib` files, detects changes via SHA-256
- `TrackedFile`: `path`, `modified`, `sha256`, `loaded_id` — tracks each watched file
- `ReloadEvent` enum: `NewBlock`, `UpdatedBlock`, `RemovedBlock`, `Error`, `NoChange`
- Auto-creates watch directory if missing; event log accumulates for audit trail
- On file change: unloads old block, loads new binary via `BlockLoader::load_from_binary()`

### Process Manager (`aios-process-mgr`)

#### Task Types (`task.rs`)

**`Priority`** (5 levels, Ord ordering):
```
Background(0) < Low(1) < Normal(2) < High(3) < Critical(4)
```

**`ProcessState`**:
```
Ready → Running → Terminated
  ↑        ↓
  └── Suspended
            ↓
          Crashed → (restart → Ready)
```

**`Process`** struct:
- `pid: ProcessId`, `name`, `priority`, `state`
- `ram_quota_mb: u64` — reserved RAM
- `cpu_time_ms: u64` — accumulated CPU time
- `crash_count: u32`, `max_restarts: u32` (default: 3)
- `parent_pid: Option<ProcessId>` — for child processes
- `group_id: Option<u64>` — process group membership

**`ProcessGroup`** struct:
- `id: u64`, `name: String`, `priority: Priority`
- `member_pids: Vec<ProcessId>` — processes in this group
- `created_at_ms: u64`, `session_id: Option<u64>`

**`ProcessTimer`** — time-slice tracking:
- `quota_ms` — maximum allowed runtime per slice
- `quota_exceeded()` — checks if time slice is exhausted
- `remaining_ms()` — time left in current slice

#### Scheduler (`scheduler.rs`)

`Scheduler` — Priority-based preemptive scheduler:

**Data structures**:
- `processes: HashMap<ProcessId, Process>` — all processes
- `priority_queues: BTreeMap<Priority, Vec<ProcessId>>` — ready queues by priority
- `current: Option<ProcessId>` — currently running process
- `timer: Option<ProcessTimer>` — current time slice

**Process lifecycle**:
1. `spawn_process(name, priority, ram_mb)` → `ProcessId` — enforces RAM limit
2. `schedule_next()` → highest-priority ready process with aging boost
3. `tick()` → checks timer expiry, reschedules if needed
4. `kill_process(pid)` → terminates, frees RAM
5. Optional: `suspend_process()`, `resume_process()`

**Process aging** (starvation prevention):
- `aging_threshold_ms` (default: 500ms) — wait time before priority boost; `with_aging_threshold` clamps zero to 1 ms and `schedule_next()` guards the divisor
- `schedule_next()` computes effective priority = base + (wait / threshold), capped at +4 levels
- Low-priority processes waiting 4x threshold are boosted to Critical level
- All processes evaluated globally (no early-break by queue level)
- `force_preempt()` — forcibly expires current time slice (for testing and manual reschedule)

**Weighted round-robin** (proportional time slices):
- `priority_weight()` maps: Background=1, Low=2, Normal=3, High=4, Critical=5
- Time slice = `default_time_slice_ms.saturating_mul(priority_weight)` (Critical gets 5x Background's slice; zero `with_time_slice` is clamped to 1 ms)
- `round_robin_positions: HashMap<Priority, usize>` tracks position within each priority queue for fairness

**Memory pressure detection**:
- `memory_pressure_threshold` (default: 0.8) — usage ratio triggering Critical
- Warning at `threshold * 0.75`, Critical at `threshold`
- `MemoryPressure` enum: `Normal(usage)`, `Warning(usage)`, `Critical(usage)`
- `MemoryPressureEvent` struct: level, usage ratio, used/total MB, callback names
- `register_memory_pressure_callback(name)` — register notification targets
- `check_memory_pressure()` → `Option<MemoryPressureEvent>` (None if Normal)

**Process groups**:
- `create_group(name, priority)` → `u64` group ID
- `create_session(name, priority)` → `u64` session ID (group with session_id)

**CPU affinity** (`cpu_affinity.rs`):
- `set_cpu_affinity(pid, cores)` — stores a per-thread affinity mask (the OS call targets the calling thread, so it must run on the target thread itself)
- `get_cpu_affinity(pid)` — queries current affinity for a thread
- `available_cpu_cores()` — returns number of available cores
- `validate_cores(cores)` — pre-validates a mask before storing it
- Platform: `SetThreadAffinityMask` (Windows), `sched_setaffinity` (Linux), no-op fallback
- Application model: the spawned process thread reads its stored mask (`Arc<Mutex<Vec<usize>>>`) and applies it before running the payload, so the scheduler thread is never re-pinned
- `add_to_group(pid, group_id)` / `remove_from_group(pid)` — membership management
- `kill_group(group_id)` — terminate all members and remove group
- `suspend_group(group_id)` / `resume_group(group_id)` — bulk state changes
- `set_group_priority(group_id, priority)` — change priority for all members
- `group_members(group_id)`, `all_groups()`, `group_count()`, `get_group()`

**Real-time scheduling mode**:
- `SchedulingMode` enum: `Normal` (weighted round-robin) and `RealTime` (deadline-based)
- `set_scheduling_mode(mode)` / `scheduling_mode()` — switch between modes
- `set_rt_deadline(pid, deadline_ms)` — assign absolute deadline to a process
- `clear_rt_deadline(pid)` — remove deadline from a process
- RT scheduling: picks process with earliest deadline (smallest remaining time)
- `JitterEntry` struct: `pid`, `expected_ms`, `actual_ms`, `timestamp` — tracks scheduling jitter
- `jitter_log()` / `clear_jitter_log()` — jitter audit trail
- Max jitter entries: 1000 (FIFO eviction)

**Crash resilience**:
- `report_crash(pid)` → increments `crash_count`, logs `CrashEvent` with timestamp
- `should_restart(pid)` → true if `crash_count < max_restarts`
- `crash_log: Vec<CrashEvent>` — audit trail of all crashes

**RAM management**:
- Total RAM configurable at construction (default from system)
- Each process reserves `ram_quota_mb`
- `spawn_process` fails with `SchedulerError` if RAM exhausted
- `kill_process` releases reserved RAM

#### IPC Process Control (`process_control.rs`)

`handle_process_command(scheduler, packet)` — IPC dispatch:
- `SpawnProcess { name, priority, ram_mb }` → spawns, returns PID as text
- `KillProcess { pid }` → kills, returns confirmation
- `AdjustPriority { pid, new_priority }` → adjusts, returns new priority
- `RecoveryDecision { pid, action }` (v2.45.0) → applies a crash-dialog decision
  (`Rollback`/`Terminate`/`Resume`) through `Scheduler::resolve_fault`.

#### Fault Tolerance & Recovery (v2.45.0)

Instead of a global halt, a faulting task is frozen, snapshotted (metadata only)
and surfaced to the user as a `Rollback / Terminate / Resume` decision — the same
contract in the host runtime and in the bare-metal kernel.

- `aios-core::fault` — `FaultKind { Panic, Hang, TaskFault }` (with `name()`),
  `RecoveryAction { Rollback, Terminate, Resume }` (with `name()`) and
  `FaultInfo { pid, name, kind, message, timestamp_ms }`. Two IPC payloads carry
  it: `Payload::FaultReport(FaultInfo)` and
  `Payload::RecoveryDecision { pid, action }`
  (`CommandId::FaultReport = 0x0070`, `CommandId::RecoveryDecision = 0x0071`).
- `aios-process-mgr::fault` — `run_isolated` executes a real-thread task under
  `catch_unwind` with a quiet thread-local panic hook, so a panic is turned into
  a `FaultInfo` (`panic_message`, `fault_for`) instead of unwinding the
  scheduler. `TaskSnapshot` captures the task metadata (pid, name, priority, RAM
  quota, block id, crash count) and `restore_to` re-arms it.
- `Scheduler` — real threads record their panic message (`Arc<Mutex<Option<String>>>`),
  `poll_faults()` drains crashed threads into `fault_queue`, and
  `resolve_fault(pid, action)` applies the decision: `Rollback` restores the
  snapshot and requeues `Ready`, `Terminate` removes the task, `Resume` requeues.
  `check_real_threads` no longer clears a `Crashed` task back to `Ready`, and
  `kill_process` prunes the snapshot/fault entries.
- `aios-gui::crash_dialog` — `CrashDialogState` is a pure state machine
  (`present`/`choose`/`take_outcome`/`show`) kept separate from rendering; the
  `GuiRuntime` tick thread polls `poll_faults()` and `App` presents a modal
  dialog that resolves through `resolve_fault` on the next `update()`.

### Live-Update Engine (`aios-live-update`)

#### State Transfer (`state_transfer.rs`)

`StateTransferManager` — captures and restores system state during hot-swap:
- `extract_state(queue, state)` → `Snapshot` — freezes IPC bus + captures state bytes
- `restore_state(queue, snapshot)` — unfreezes bus with saved packets

`Snapshot`: `state: Vec<u8>` + `pending_packets: Vec<IpcPacket>`

#### Hot-Swap Engine (`engine.rs`)

`LiveUpdateEngine` — atomic block replacement with rollback:

**5-step hot-swap** (`perform_swap()`):
1. **Freeze** — extract state from old block, freeze IPC bus
2. **Validate** — SHA-256 check on new binary
3. **Health check** — optional closure validates new block
4. **Store rollback** — save old binary, state, version as `HotSwapEntry`
5. **Restore** — unfreeze IPC bus (in-flight messages preserved)

**Rollback** (`rollback()`):
- Restores old binary, state, and version from `HotSwapEntry`
- Warns if rollback timeout exceeded (configurable, default: 30s)
- Logs `SwapRecord` for audit trail

**`SwapRecord`** audit trail:
- `block_id`, `old_version`, `new_version`, `success`, `rolled_back`, `timestamp`

#### WASM Live Update Engine (`wasm_engine.rs`)

`WasmLiveUpdateEngine` — real WASM module replacement during hot-swap:

**Architecture**: wraps `LiveUpdateEngine` + `WasmSandbox`, maintains `active_blocks: HashMap<BlockId, (WasmBlock, Store<StoreState>)>` mapping deployed WASM instances.

**Deploy** (`deploy_block()`):
1. Reads binary from `BlockRegistry`
2. Compiles via `WasmBlock::new()` → `create_store()` → `instantiate()`
3. Auto-calls `init` and `start` exports if present
4. Stores active `(WasmBlock, Store)` pair for subsequent `call_block_func()`

**Hot-swap** (`swap_block()`):
1. Calls `LiveUpdateEngine.perform_swap()` — freeze IPC, validate SHA-256, health check, store rollback entry
2. Compiles new WASM binary → instantiates → auto-calls `init`
3. Overwrites `active_blocks[id]` with new instance
4. Returns `SwapResult` with old/new version and functions called

**Rollback** (`rollback_block()`):
1. Removes active WASM instance from `active_blocks`
2. Calls `LiveUpdateEngine.rollback()` — restores old binary + state from `HotSwapEntry`

**Function call** (`call_block_func()`):
- Looks up deployed instance, delegates to `WasmBlock::call_func()`

**Params**: `SwapParams { new_binary, new_version, health_check: Option<HealthCheckFn>, isolation }`

---

## Layer 4: User Interface (aios TUI + aios-gui)

### Intent Engine (`intent_engine.rs`)

`IntentEngine` — natural language to IPC packet translation:

**Input**: `"optimize for video editing"` → **Output**: `TranslatedCommand` with IPC packet

**8 intent categories** (keyword-matched, case-insensitive):

| Intent | Keywords | Action |
|--------|----------|--------|
| Memory optimize | "free memory", "clear ram", "reduce memory" | AdjustPriority(pid=0, Background) |
| Video optimize | "optimize video", "video editing", "video rendering" | AdjustPriority(pid=0, Critical) |
| Block update | "update block", "upgrade block" | HotSwap |
| Kill process | "kill", "stop", "terminate" | KillProcess |
| Spawn process | "start", "spawn", "run" | SpawnProcess(256MB, Normal) |
| Priority adjust | "boost", "throttle", "priority" | AdjustPriority |
| Health check | "status", "health", "check" | HealthCheck |
| Topology | "topology", "blocks", "list" | GetTopology |

`IntentContext` provides system state for translation: active processes, loaded blocks, current tier, RAM usage.

**Where it is used (v2.61.0):** the kernel TUI AI Console translates and executes intents over the bridge. The standalone TUI (`aios-tui`) has no message router, so its shell command `intent <request>` uses the same engine for **translation only**: it prints the matched intent, the plan, the IPC command and the target block, and leaves execution to the kernel.

### Background shell tasks (v2.61.0)

Network commands must not block the TUI, so the shell hands them to a background worker and prints the result later:

- **Standalone TUI** (`aios-tui`): `DashboardState` owns two outboxes — `TaskOutbox` (output lines of a shell task) and `FetchOutbox` (bytes of a downloaded block). A generation counter (`task_gen`) lets the main loop drop the result of a superseded task; only one task runs at a time.
- **Kernel TUI** (`aios`): `TuiApp` owns `StoreOut`; `store_spawn()` starts the task on the tokio runtime, `store_poll()` collects the result on the next UI tick.

Both TUIs run the store catalog request, the block download and the web search in a worker thread, so the event loop keeps rendering while the request is in flight.

### Dashboard (`aios/src/tui`)

Ratatui-based kernel TUI with the 7-tab spec layout (`aios` binary), restyled in a classic Far Manager / Midnight Commander two-panel form (v2.31.0):

**Status-bar zone** (top, 1 row, replaces the old header + tab strip):
- Version, active tab name, system status (`OK`/`SAFE`)
- Uptime, AI tier, RAM, bridge status (`ON`/`OFF`)
- `sys:` link status (Wi-Fi connected/grey, from `aios_sys_control` snapshot)

**Main dual-panel zone** (55/45 split): the active tab renders on the left, the **events log** on the right (color-coded: Red=error, Yellow=warn, Cyan=Bridge, grey=info).

**Tab 1 — System & HW**: CPU model, cores/threads, AVX flags, GPU name/VRAM, storage, AI tier; RAM gauge; Hardware Inspector device table and hot-plug toasts.

**Tab 2 — Blocks & Svc**: block table (ID, Name, Version, State, Size) with `j`/`k` selection; keybindings `r`=restart, `k`=unload, `l`=load from disk (prompts a path); bottom pane shows the selected block and the process list

**Tab 3 — AI Console**: LLM chat with `i` to enter query mode, `Enter` to send, `Esc` to leave, `Up`/`Down` prompt history (last 50), `h` help panel; slash commands `/help /status /clear /history /system /model /backend /key /temp /tokens /preset /save /load`; footer shows backend/model/temperature/tokens/state; word-wrapped output with cyan prompts, red errors and a yellow live-streaming partial line; **responses stream** over `LlmEngine::query_stream`; the chat auto-persists as JSON Lines to `AIOS_DATA_DIR/chat.jsonl` (saved after each reply + on quit, restored at boot); `/preset` manages system-prompt templates; backend/model/key changes rebuild the shared `LlmEngine` asynchronously

**Tab 4 — Studio Bridge**: bridge server state (running/disabled), URL, REST/WebSocket endpoints

**Tab 5 — Network & Store**: network config editor (`n` = `key=value` input applied over IPC `net_set`; `g` = show config JSON) plus the installed block store list (`s` = refresh); same operations available from the Shell as `net get`/`net set`/`store list`/`store search`/`store install`

**Tab 6 — Web**: omnibox (full URL / bare host / DuckDuckGo query), word-wrapped page text, scrollable link sidebar. Keys: `g`=omnibox focus, `Enter`=navigate, `j/k`=link nav, `o`/`Enter`=open link, `u/d`=scroll ±1 line, `PageUp`/`PageDown`=scroll ±20 lines, `b`=back in history, `B`=open current page in the native WebView, `n`=open selected link natively, `Esc`=unfocus. Fetches run in background threads (never block the TUI) with a fetch-generation counter that drops stale results; a bounded 20-page cache makes revisits and back-navigation instant
- **Full browser on demand**: `B`/`n` open the page in the real `aios-webview` browser window (WebView2 — JS/CSS/images). The handle lives in a module-level `OnceLock<Mutex<Option<WebBrowser>>>`, so the window is reused, auto-recreated on close, and opening happens on a background thread

**Tab 7 — Shell** (vertical split):
- Command input line with prompt indicator
- Output display area (scrollable command output)
- Command history navigation with ↑/↓; `Esc` clears the current input line
- Every keystroke on the Shell tab goes to the input line, so `q` quits only from other tabs
- `ShellState`: input_buffer, output (Vec<String>), command_history, history_pos
- Available commands: `ps`, `blocks`, `kill <pid>`, `spawn <wasm-path>`, `store list|search|install`, `net get|set`, `cluster status|nodes|spawn|kill|migrate`, `status`, `logs`, `restart`, `help`/`?`, `clear`
- Execution flow: TUI → `shell_execute()` → `SafeModeShell::parse_command` / store manager / net block IPC
- Cluster commands: `cluster status`/`cluster nodes` print the peer view (status/tier/load) plus remote and locally hosted processes; `cluster spawn <name> [ram_mb] [priority] [target_node]`, `cluster kill <node> <pid>` and `cluster migrate <node> <pid> [target_node]` drive `DistributedScheduler` directly (spawn/kill/migrate are blocking up to the ack timeout). Without `AIOS_CLUSTER_PEERS` the handler replies `clustering disabled`

**F1 Help Overlay**:
- Toggled with F1 or '?', dismissed with F1/Esc/'?'
- Shows all keyboard shortcuts and shell commands in a popup window

**Prompt zone**: `AIOS>_` for the shell, `net>_` during network `key=value` input.

**F-key zone** (bottom, 1 row): `1Help 2Tabs 3View 4Edit 5Copy 6Move 7Mkdir 8Del 9Sort 10Rescan`; `F10` re-probes hardware (`refresh_hw`), `q`/`Ctrl+C` quit.

`OrchestratorState` manages:
- Process/Block snapshots (taken each frame for consistent rendering)
- RAM history ring buffer (60 entries)
- Selection state (selected_tab, selected_row)
- Block operation result display + load-from-disk input
- Log buffer (capped at 100 entries)
- Scheduler + Registry synchronization
- Web state: url_input, current_url, page, loading, error, input_focused, scroll, history
- Network state: `net_status` (last config JSON from the `net_settings` block)
- Help overlay visibility (shown/hidden)
- Shell state: input_buffer, output (Vec<String>), command_history, history_pos
- `safe_mode` flag from `--safe-mode`
- Cluster scheduler handle (`cluster: Option<Arc<Mutex<DistributedScheduler>>>`) kept alive when `AIOS_CLUSTER_PEERS` is configured; the cluster tick thread pushes failover events into the kernel log

### Entry Point (`aios/src/main.rs`)

Startup sequence:
1. Initialize `env_logger`
2. Parse `--safe-mode` (and `--bridge-port`) into `AppConfig`
3. `HardwareProfile::detect()` — detect real hardware
4. `AiTier::from_profile()` — classify AI capability, log the tier at boot
5. Create `BlockRegistry` — wire the compiled-in **base set** through the block factory (`hal`, `browser`, `net_settings`) into the `MessageRouter`, then discover + execute disk blocks from `AIOS_BLOCKS_DIR` **unless safe mode** (`blocks::register_base_blocks` / `blocks::load_disk_blocks`)
6. Create `Scheduler` — spawn 3 processes (ai_orchestrator, io_handler, health_monitor)
7. Create `Watchdog` — start heartbeat thread in background
8. Create `EmbeddedContextStore` + `TelemetryStore` — for system telemetry
9. Create `SafeModeShell` — for safe mode recovery commands
10. Start the bridge HTTP/WS server on the configured port **unless safe mode**
11. Enter crossterm raw mode + alternate screen
12. Event loop: poll key events, redraw dashboard, sync watchdog state
13. Restore terminal on exit

Keybindings: `q`/`Ctrl+C`=Quit, `1-7`/`Alt+1-7`=Tab (Alt works while typing in Shell/URL/AI/net input), `Tab`/`F1`/`?`=next tab/help, `W`=Launch GUI desktop shell, `Space`=pause event log, Blocks: `r`=restart `k`=unload `l`=load, Web: `g`/`Enter`/`j`/`k`/`o`/`u`/`d`/`PageUp`/`PageDown`/`b`/`B`/`n`/`Esc`, Network: `n`=edit config `g`=show JSON `s`=refresh store, ↑/↓=Shell history

### Hybrid Block Loader (`aios/src/blocks.rs`)

The kernel is block-oriented end to end: everything above the scheduler is a *block*, and the only difference is where the code comes from.

- **Base set (compiled-in):** `base_factories()` builds a `BlockFactoryRegistry` (`aios-block-mgr::factory`) holding one `BlockFactory` per compiled-in block (`hal`, `browser`, `net_settings`). `base_factories_for(total_ram_mb)` is the profile-aware variant used at boot: it starts from the same compiled-in registrations and drops the blocks whose `base_block_skip_reason(name, total_ram_mb)` is `Some` (today only `browser` below 4096 MiB), logging `AIOS: base block '<name>' skipped for this profile: <reason>`. `register_base_blocks()` (with `None` = full set) instantiates each factory, registers the block in the `BlockRegistry` (`native:<name>:<version>` payload) and wires its `handle_message` into the `MessageRouter`. The base blocks are real `StatefulBlock`s, so they are dispatched over IPC exactly like a disk block — `hal` is now a live `HalBlock`, not a placeholder.
- **Disk set (installer-chosen):** `load_disk_blocks()` calls `BlockRegistry::boot_discover` on `AIOS_BLOCKS_DIR`, then executes every discovered payload whose first bytes are the WebAssembly magic (`\0asm`) through the `BlockExecutor` (`init`/`start` bring-up), one block at a time. A block that exports `handle_message` is additionally wired into the `MessageRouter`, so it is dispatched over IPC exactly like a base block (`DiskBlockInfo.routed` reports this). `.bin` / placeholder payloads are registered only and reported as `registered_only`. Only the blocks the installer copied into `/blocks` are present at boot, so bring-up cost tracks the installed set.
- `BlockExecutor` is kept alive in `OrchestratorState` as `Arc<Mutex<BlockExecutor>>` (router handlers must be `Send`) so executed disk blocks stay resident and service their IPC traffic; base blocks live inside their router handlers.
- `ipc_bus` and `scheduler` remain topology-only placeholders: they appear in the dependency graph but are not serviced over IPC.

### Safe Mode

`--safe-mode` boots a minimal, recoverable kernel: disk blocks are not discovered or executed (compiled-in base blocks only) and the bridge server is not started; the header shows `SAFE MODE`. Core blocks, scheduler, watchdog, LLM engine, TUI and Shell remain available.

### Native WebView Browser (`aios-webview`)

The TUI cannot render real web pages (no CSS/JS engine), so the full-featured browser is a **native window** powered by `wry` (WebView2 on Windows, WebKitGTK on Linux, WKWebView on macOS) on a `winit` event loop:

- `WebBrowser::open(target)` starts a **single process-wide background thread** on first use and reuses it afterwards (GTK may be initialized by exactly one thread): the first call blocks only until the window exists, later calls present and navigate the existing window; the handle never blocks the caller
- Commands (`navigate`, `back`, `forward`, `close`) are posted to the browser's event loop via `winit::EventLoopProxy` and applied asynchronously; `close` hides the window while the engine thread keeps running (the next `open` presents it again)
- **Linux loop integration (v2.43.0):** the engine thread calls `gtk::init()` before building the event loop, enables `EventLoopBuilderExtX11::with_any_thread(true)`, seeds writable `HOME`/`XDG_*`/`DISPLAY=:0` defaults for the headless live boot, and drains `gtk::events_pending()` from `about_to_wait` on a 16 ms `WaitUntil` poll — winit's X11 loop cannot see the glib sources of GTK/WebKit (their own X connection, web-process IPC), so without the pump the window would freeze
- Cookies and storage persist between restarts through a `WebContext` backed by a profile directory (`AIOS_DATA_DIR`/`aios/webview`, or the OS data dir)
- `resolve_target()` implements the omnibox rule shared with the TUI: full `http(s)` URL → as-is, bare host → `https://`, anything else → DuckDuckGo (HTML edition) query
- `launcher` module resolves the `aios-gui` binary (sibling of the current executable, then `PATH`) and spawns the GUI desktop shell

**Feature gating (v2.32.0):** the wry/winit engine lives in the optional `webview` feature (default on; on Linux it pulls the `gtk` crate). `launcher` and `resolve_target` are pure `std` and always compile, so the TUI `W` key works even when the engine is disabled; only `B`/`n` (kernel `WebBrowser`) and the Native Browser desktop window are gated. The retired Linux Live image used to build `aios`/`aios-gui` with `--no-default-features`, but feature unification through `aios-tui → aios-webview` kept the webview enabled: the (now removed) `live/build.sh` FATAL-checked that both primary binaries link `libwebkit2gtk-4.1`, because the browser-from-flash-drive was a shipped requirement of that path. The native UEFI image has no such check.

### GUI Desktop Shell (`aios-gui`)

Native egui/eframe **desktop shell** replacing the tabbed dashboard: a dark desktop, a bottom taskbar with `☰ Пуск` on the left and a system tray on the right. Built-in functions open as separate, movable/resizable egui windows; external programs are launched from the shared catalog in `src/voice.rs`. Hotkey `W` in either TUI launches the GUI desktop via `aios_webview::launcher::launch_gui()`. Built with `--no-default-features`, the Native Browser entry and `F7` are absent.

- **Start menu** (`src/desktop.rs::start_entries()`): searchable list sorted alphabetically case-insensitively by title, combining built-in functions, external `voice::Program` entries and the system actions `Обновить всё` / `Приостановить всё`.
- **Windows and taskbar**: `DesktopState`/`DesktopWindow` track open windows, focus and minimization; a taskbar button focuses or minimizes its window, and re-opening an already-open function focuses it. `F1`-`F9` open the matching function window, `J`/`K` navigate the focused Blocks/Store list, `Escape` closes Start.
- **Real system actions**: `Обновить всё` re-polls system info, processes, blocks, hardware and network settings; `Приостановить всё` suspends every scheduler-tracked process. The former log-only Quick Actions stubs are gone.
- **System Dashboard (F1)**: stat cards (RAM, blocks, processes, watchdog), system panel (CPU/GPU/storage/HW tier), RAM sparkline, priority distribution, processes table (PID, Name, Priority, State, RAM, CPU ms, Crashes) with Refresh/Kill/Suspend/Resume, activity log
- **WASM Blocks (F2)**: block table + Refresh / Load (2-step dialog) / Unload / Hot-Swap; a boot-time loader summary line (`base [...] | disk [...] | failed N | live M`) is rendered under the toolbar, matching the TUI Blocks summary
- **AI Studio (F3)**: async LLM chat — message list, streaming replies (live yellow partial line), Enter-to-send (focus retained), slash commands `/help /status /clear /history /system /model /backend /key /temp /tokens /preset /save /load`, status line (backend/model/temp/tokens/busy); requests stream over a background tokio task so the UI stays responsive. The chat auto-persists to the shared `AIOS_DATA_DIR/chat.jsonl` and `/preset` templates to `AIOS_DATA_DIR/presets.json` (same files as the TUI AI Console)
- **App Store (F4)**: searchable catalog table with Install/Update/Uninstall
- **Network Settings (F5)**: hostname/port/timeouts/private-access/DNS/user-agent form with Save (partial JSON IPC update to `net_settings`) and Reset, plus a live JSON preview
- **Deps (F6)**: dependency graph summary, load order chain, depends/depended-by table
- **Native Browser (F7)**: omnibox, Back/Forward, Open/Close toggle driving the `aios-webview` native window; the first navigation auto-opens the browser
- **Files (F8)**: two-panel file manager (`aios-fm`) on `aios-vfs` — toolbar (Refresh/Switch/Sort/Up/Mkdir/Rename/View/Copy/Move/Delete, HOST r/w), panels with click/double-click selection, modal mkdir/rename dialog, collapsible AI preview, live job progress and capability ACL display
- **Taskbar tray**: AI tier, `RAM used/total`, live IPC packet counter, Wi-Fi state/SSID, active input-layout pair, battery/CPU temperature and effective LLM backend (`local`/`cloud`) on a 2 s refresh, plus the voice mic button and manual command field

#### Live GUI kernel runtime (`aios-gui/src/runtime.rs`)

Since v2.31.2 every desktop window is backed by a **real kernel runtime** (`GuiRuntime`) instead of placeholder data:

- Real `Scheduler` (round-robin, aging threshold 5000 ms, 100 ms time slice) drives the process table, RAM usage and IPC control (`kill/suspend/resume`).
- Real `BlockRegistry` seeded with native modules (`hal`, `ipc_bus`, `scheduler`, `browser`) and augmented by `boot_discover` scanning `AIOS_BLOCKS_DIR` (default `./blocks`). The WASM Blocks window loads/unloads/swaps registry entries; Hot-Swap runs `LiveUpdateEngine::perform_swap` (SHA-256 verified, rollback entry stored) against the installed store copy and then `registry.swap_binary`, with a swap-history panel.
- **Loader report (v2.59.0)**: `GuiRuntime::new` captures a `GuiBlockLoadReport` (the seeded base set, the discovered disk blocks, and any failed discoveries); `GuiBlockLoadReport::summary_line` formats `base [...] | disk [...] | failed N | live M` and `AiosApp::block_load_summary` renders it in the Blocks window. Unlike the TUI, the GUI runtime keeps blocks resident without executing WebAssembly, so `live` is the number of currently-resident registry blocks (`disk` = discovered/registered) rather than an executed-block count.
- Real `Watchdog` receives a heartbeat every second (secret `aios_gui_secret`); the `WD:` indicator mirrors `WatchdogState` (`Monitoring → 0`, `Suspended → 1`, `Recovering → 2`, else safe-mode).
- Shared `IpcBus` (4096 slots) drained by the taskbar IPC counter. `GuiRuntime::start()` no longer seeds `ai_orchestrator`, `io_handler`, `health_monitor` or `telemetry_agg` demo processes and the `ipc_generator` helper is gone: packets arrive only from explicit/manual actions, and the scheduler starts empty. The bus itself stays available for real IPC traffic.
- `HotReloader` re-scans the blocks dir each UI poll and hot-reloads modified binaries.
- Local `BlockMarketplace` (`official` repo, 4 internal offers) + `BlockInstaller` persist real `<name>_<version>.wasm`/sidecar manifests; the App Store install/update/uninstall write and delete real store files and registry entries. Test builds redirect the blocks dir to a unique temp directory so unit tests never touch the repository.
- All background **system** threads start only in `start_runtime()` (invoked from `aios-gui/src/main.rs`), so `AiosApp::new` stays side-effect free and thread-free for tests. `GuiRuntime::start()` is idempotent and launches only the scheduler tick and the watchdog heartbeat; the process table stays empty until a process is explicitly spawned. `aios-gui` has 33 passing tests covering sorted/unique/complete Start entries, window focus/minimize, no demo processes, explicit process kill, manual IPC packets and desktop window navigation.

#### WebSocket telemetry auth and bridge input policy (`aios-bridge`)

Authorization is default-deny for every `/api/` and `/ws/` route. `/ws/telemetry` accepts a token in `?token=<session>` or an `Authorization: Bearer` header and validates it against `AuthStore` on every upgrade — there is no "no users registered" bypass. Public registration is off unless `AIOS_BRIDGE_ALLOW_SIGNUP=true`; CORS is disabled unless `AIOS_CORS_ORIGIN` is set to a valid exact origin. Poisoned shared mutexes are recovered with logging instead of panicking. Intent/workflow/LLM requests enforce prompt, batch and `max_tokens` limits; block binaries are read only from `AIOS_BLOCKS_DIR` (canonicalized path check). Bridge `HotSwap` performs a live-update swap (`LiveUpdateEngine::perform_swap`) then `registry.swap_binary`; `WorkflowExecution` runs nested intents sequentially through the same `execute_intent` path.

---

## Layer 5: Safety & Security (`aios-watchdog`, `aios-security`, `aios-context`)

### Watchdog & Emergency Recovery (`aios-watchdog`)

The AI Orchestrator must never become a single point of failure. The watchdog monitors orchestrator health via cryptographic heartbeats.

#### Heartbeat Protocol (`heartbeat.rs`)

`Heartbeat` — SHA-256 HMAC-authenticated health signal:
- `sequence: u64` — monotonic counter
- `timestamp_ms: u64` — creation time
- `source_hmac: [u8; 32]` — HMAC of (secret + sequence + timestamp)

Verification: `heartbeat.verify(secret)` recomputes the HMAC and compares.

#### Watchdog (`watchdog.rs`)

`Watchdog` — monitors orchestrator health with configurable thresholds:

**States:**
```
Monitoring → Suspended → Recovering → Monitoring (on heartbeat received)
                                         ↓ (timeout)
                                       SafeMode
```

**Configuration** (`WatchdogConfig`):
- `heartbeat_interval_ms` — expected heartbeat frequency (default: 1000ms)
- `max_missed_heartbeats` — consecutive misses before suspension (default: 3)
- `recovery_timeout_ms` — time to wait for recovery before Safe Mode (default: 10s)
- `secret` — HMAC secret key

**Check cycle** (`check_timeout()`):
- **Monitoring**: If heartbeat age > interval, increment missed count. At max misses → `SuspendOrchestrator`
- **Suspended**: Transition to `Recovering`, return `AttemptRecovery`
- **Recovering**: If heartbeat age > recovery timeout → `EnterSafeMode`. If heartbeat received → back to `Monitoring`
- **SafeMode**: Return `InSafeMode`

**Recovery Actions** (`WatchdogAction`):
- `None` — no action needed (severity 0)
- `WaitForRecovery` — waiting for heartbeat during recovery (severity 1)
- `SuspendOrchestrator` — pause orchestrator execution (severity 2)
- `AttemptRecovery` — begin recovery sequence (severity 3)
- `KillProcess(pid)` — terminate a specific process by PID (severity 4)
- `DumpState(path)` — serialize system state to timestamped file (severity 5)
- `EnterSafeMode` — transition to safe mode (severity 6)
- `SafeModeShell` — spawn deterministic CLI shell (severity 7)
- `InSafeMode` — already in safe mode (severity 8)

`is_terminal()` returns true for `KillProcess`, `DumpState`, `EnterSafeMode`, `SafeModeShell`, `InSafeMode`.

**Escalation** (`escalate_actions()`): context-aware recovery based on current state:
- **Suspended**: `KillProcess(0)` + `DumpState(timestamped_path)`
- **Recovering**: `DumpState(timestamped_path)`
- **SafeMode**: `DumpState(timestamped_path)` + `SafeModeShell`

**Events** (`WatchdogEvent`): audit trail of all state transitions with timestamps.

#### Safe Mode Shell (`safe_mode.rs`)

`SafeModeShell` — deterministic CLI for system recovery when AI Orchestrator is suspended:

**Commands:** `ps`/`processes`/`list`, `blocks`/`ls`, `spawn`, `kill <pid>`,
`load`, `unload <id>`, `status`/`info`, `logs`/`log`, `restart`/`recover`,
`help`/`?`, `exit`/`quit`/`q`, plus a mixed-case unknown-command fallback.

Exact output for every branch is pinned in `safe_mode.rs` tests. The standalone
TUI now routes bare `ps`/`processes`/`list`, `blocks`/`ls`, `status`/`info`,
`exit`/`quit`/`q`, `help`/`?`, `logs`/`log`, `kill`, `spawn`, `load`, `unload`,
`restart` and `recover` through `aios-console` legacy adapters and asserts
byte-for-byte equality with `SafeModeShell`, including side effects and shared
event-log entries; those twenty spellings are the migrated safe-mode commands.
The status adapter
takes the scheduler crash-log count from the shared `HostBackend` because the
canonical status does not include it. The exit adapter maps `OutputControl::Exit`
to the legacy line without changing the bare-command shutdown flag, so `console
exit` remains the only shell command that breaks the event loop. The help adapter
maps the generated all-commands help to the legacy wording and keeps the TUI
extras as a separate frontend block. The log seam shares an
`Arc<Mutex<Vec<String>>>` between `SafeModeShell` and `HostBackend`; the console
only reads it for `logs`, and mutation adapters append successful legacy events
to it, so canonical console log entries are never mixed with safe-mode events.
The restart adapter shares the restart counter with canonical `wd reset` via
`HostBackend`, so status and health see the same bounded count; it records the
legacy event on success and keeps the exact limit failure without a `Failed:`
prefix. The final unknown-command fallback now uses
`legacy::unknown_text`, so the TUI prints the exact legacy
`Error: Unknown command: '<input>'. Type 'help' for available commands.` line
without executing `SafeModeShell`; the input is preserved verbatim, including
case and whitespace. `SafeModeShell` is still owned by the TUI for non-shell UI
actions (for example the `x` status key) and for direct legacy-path parity
tests, but no bare shell command executes through it. Canonical `console info`
remains ambiguous.
The same TUI shares its scheduler and registry with `HostBackend`, so `console ps`
and `console block list` observe the safe-mode runtime without changing legacy
output.

**Restart limiting:** Configurable `max_restarts` prevents infinite restart loops.

---

### Capability-Based Security (`aios-security`)

Zero-trust model: no block is trusted by default. All operations require explicit capability tokens.

#### Capability Tokens (`capability.rs`)

`Capability` enum — 15 specific permissions:
- Network: `NetBind`, `NetConnect`, `NetListen`
- Filesystem: `FsRead`, `FsWrite`, `FsDelete`
- Hardware: `HwAccess`
- Memory: `MemAlloc`, `MemShare`
- System: `SchedModify`, `BlockLoad`, `BlockUnload`, `ProcessSpawn`, `ProcessKill`, `SystemConfig`
- Override: `All` (grants everything)

`CapabilityToken` — signed permission grant:
- `block_id: u32` — which block holds this token
- `capabilities: Vec<Capability>` — granted permissions
- `issued_at_ms / expires_at_ms` — time-bounded validity
- `issuer_signature: [u8; 32]` — SHA-256 HMAC of token fields

#### Access Control Layer (`access_control.rs`)

`AccessControlLayer` — central token management:
- `issue_token(block_id, capabilities)` — create and store token
- `check_permission(block_id, required)` — verify capability (returns `Result`)
- `try_check_permission(block_id, required)` — verify + record violations
- `revoke_token(block_id)` — remove token
- `clean_expired()` — remove expired tokens
- `violations: Vec<Violation>` — audit trail of unauthorized access attempts

#### Sandbox (`sandbox.rs`)

`Sandbox` — isolated execution environment per block:
- `check_syscall(name, required_cap)` — validates each system call against allowed capabilities
- `allocate_memory(bytes)` — enforces memory limits
- `max_syscalls` — syscall count limit prevents infinite loops
- States: `Created → Running → Terminated` or `→ Violated`

Violation response: sandbox terminates block, notifies AI Orchestrator for isolation/rollback.

---

### Persistent System Context (`aios-context`)

Local embedded store for historical system awareness. 100% zero-cloud, runs entirely on device.

#### Context Store (`store.rs`)

`EmbeddedContextStore` — unified access to all data collections:
- `telemetry()` / `telemetry_mut()` — CPU/RAM metrics history
- `workflows()` / `workflows_mut()` — learned user patterns
- `stability()` / `stability_mut()` — block reliability scores

#### Telemetry (`telemetry.rs`)

`TelemetryStore` — time-series metrics with FIFO overflow (default: 10k entries):
- `record(entry)` — store metric with timestamp, optional block_id and process_name
- `query_metric(name)` — filter by metric name
- `query_range(start_ms, end_ms)` — time-range query
- `query_by_block(block_id)` — per-block metrics
- `average_value(name)` — computed average
- `peak_ram()` — maximum RAM usage recorded

#### Workflow Patterns (`workflow.rs`)

`WorkflowStore` — learned priority profiles:
- `record(name, trigger_blocks)` — track usage patterns
- `most_used()` — most frequent workflow
- `WorkflowProfile.set_priority(process, priority)` — learned priority recommendations

#### Stability Scores (`stability.rs`)

`StabilityStore` — historical reliability tracking per block binary:
- `record(score)` — upsert by (block_name, version)
- `best_version(block_name)` — highest stability score for rollback decisions
- `record_crash()` — decreases score by 0.1 (floor: 0.0)
- `record_uptime(ms)` — increases score by 0.01 (ceiling: 1.0)
- `is_healthy()` — score >= 0.5

---

## Cross-Crate Data Flow

```
User Input (TUI)
  → IntentEngine.translate() → IpcPacket
  → MessageRouter.dispatch() → BlockHandler
  → Block.handle_message() → Response
  → Scheduler.tick() → Process scheduling
  → LiveUpdateEngine.perform_swap() → Block replacement
  → StateTransferManager.extract_state() → Snapshot
  → DashboardState.update_from_scheduler() → UI refresh
```

All data exchange between blocks uses `IpcPacket` through the `IpcBus`. No direct memory pointers are shared between blocks. State serialization uses `Vec<u8>` for maximum portability.

---

## Multi-Binary Compatibility (`aios-exec-compat`)

### Binary Header Parser (`format.rs`)

`ExecutableType` — identifies binary format from magic bytes:
- `from_bytes(data: &[u8])` — `MZ`→PE, `\x7fELF`→ELF, `AIOS`→native
- `from_extension(path)` — .exe/.dll→PE, .so/.elf→ELF, .aib→AIOS

`BinaryHeader::parse(data)` — extracts: entry_point_offset, is_64bit, machine_arch, subsystem

**Capabilities per ExecutableType**:
- `AiosNative`: no restricted capabilities (native execution)
- `LinuxElf`: FilesystemRead/Write, ProcessCreate, NetworkAccess
- `WindowsPe`: all LinuxElf + RegistryAccess, WinApiCompat

### POSIX Subsystem (`posix.rs`)

`PosixTranslator` trait — translates Linux syscalls to AIOS IPC packets:
- 18 syscall variants: file I/O, process, memory, network
- `translate(request)` → `SyscallResponse` with result/errno/out_data
- `translate_to_ipc(request)` → `IpcPacket` with `Payload::Custom`

### Win32 Subsystem (`win32.rs`)

`Win32Translator` trait — maps Win32/NT API to AIOS kernel routes:
- 16 API variants: file, process, memory, synchronization
- Ordinal-based dispatch (standard Windows SSN values)
- DLL registration for dependency tracking

### Dependency Healer (`dependency_healer.rs`)

- `scan_dependencies()` — scans imported symbols against search paths
- `heal_dependencies()` — combined scan + auto-load pipeline
- Resolution cache, configurable search paths, auto-download support

### Sandbox Compatibility (`sandbox_compat.rs`)

- `CompatSandboxConfig` — per-type limits: memory, files, threads, capabilities
- `CompatProcess` — capability checking, resource limits, syscall blocking
- `CompatSandboxManager` — process lifecycle with max-process limit

---

## WebAssembly Runtime (`aios-wasm`)

### Sandbox (`sandbox.rs`)

`WasmSandbox` — Wasmtime-based sandboxed execution engine:
- Fuel consumption and epoch interruption for resource limits
- `SandboxConfig`: memory page limits, fuel limits, max instances, timeout
- `timeout_ms` is enforced wall-clock by an `EpochTicker` background thread: it increments the engine epoch every `timeout_ms / 4`, and every wasm call re-arms the store deadline (`EPOCH_TICKS_PER_TIMEOUT = 4`), bounding each call while keeping long-lived stores usable

### Block Lifecycle (`block.rs`)

`WasmBlock` — WASM block lifecycle management:
- Compile from raw bytes or WAT text format
- Instantiate with `SandboxConfig`
- Call exported functions with typed parameters
- `MemoryStats`: memory/fuel limits and instantiation status
- Linear-memory accessors for IPC: `has_func`, `memory_size`, `grow_memory`, `write_linear_memory`, `read_linear_memory`

### IPC Dispatch (`executor.rs`)

`BlockExecutor` bridges disk WebAssembly blocks onto the kernel IPC bus:

- **ABI:** a block that wants to receive packets exports
  `handle_message(ptr: i32, len: i32) -> i64`. The host stages a
  bincode-serialized `IpcPacket` in the block's linear memory at `ptr`, calls the
  export, and interprets the packed `i64` result as `(resp_ptr << 32) | resp_len`;
  `resp_len == 0` means "no response", otherwise the host reads `resp_len` bytes
  at `resp_ptr` and bincode-deserializes the reply.
- `dispatch_wasm_packet(id, &IpcPacket)` / `dispatch_wasm(id, &[u8])` perform the
  round trip; `has_export` / `routes_ipc` probe the module for the entry point.
- The request buffer is allocated through the guest's optional `aios_alloc`
  export when present, otherwise in host-grown memory (`StoreState::ipc_scratch`)
  that never overlaps the block's own data; `MAX_IPC_REQUEST` / `MAX_IPC_RESPONSE`
  (4 MiB) bound both directions.
- The handler registered by `aios/src/blocks.rs` locks the shared executor and
  calls `dispatch_wasm_packet`, so `call_block_func`-style traffic reaches disk
  blocks through the same `MessageRouter` as the base set.

### WASI Filtering (`wasi_filter.rs`)

`WasiFilter` — WASI syscall filtering with per-syscall policies:
- `WasiPolicy`: `Allow`, `Deny`, `Log` per syscall
- Preset configurations: `permissive()`, `restrictive()`, `no_network()`

### Isolation (`isolation.rs`)

`IsolationConfig` — shared-nothing isolation levels:
- `None`, `Process`, `Memory`, `Network`, `Full`
- `ResourceLimits`: max memory, CPU time, storage, network, open files per block
- `IsolationBoundary`: per-block isolation registry with cross-block communication control

---

## Hardware Auto-Provisioning & Driver Store (`aios-autohal`)

`aios-autohal` implements the Master Brief "Hardware Auto-Provisioning & Driver Store": it detects devices by fingerprint, fetches/adapts open-source drivers into isolated `.wasm` modules, grants Capability tokens and instantiates them in the Wasmtime sandbox, caches them locally, and surfaces everything with 100% TUI/GUI parity.

### Fingerprint & Manifest (`fingerprint.rs`, `manifest.rs`)

- `HardwareFingerprint { bus, vendor_id, device_id, class_code, serial_or_acpi }` with `BusType` (USB/PCI/Bluetooth/ACPI/NVMe); `extract_fingerprints(&HardwareProfile)` maps `aios-hal` snapshots (USB VID/PID, PCI class/subclass folded into `class_code`, NVMe mass-storage class) into lookup keys (`usb.046d.0825`) and driver ids (`driver.usb.046d.0825`).
- `DriverManifest` — JSON schema (id, name, version, `supported_hardware`, `required_capabilities`, `hash_sha256`, `entry_point`) with `can_serve(fp)` matching (exact or wildcard bus) and strict validation (unknown capabilities rejected, bad hashes rejected). `DriverSource`: Redox Tree / Linux Core / Custom Store / Builtin / Generic.

### Catalog & Fetcher (`catalog.rs`, `fetcher.rs`)

- `catalog.rs` — offline builtin catalog (`BuiltinDriver` with WAT sources) plus the Generic Fallback Driver (`GENERIC_FALLBACK_ID`, zero capabilities).
- `DriverFetcher` pipeline: builtin catalog → custom store registry (`{root}/drivers/{id}/driver.{json,wasm}`) → Redox Tree → Linux Core mirror (`index.json`). Returns `FetchedDriver::Wasm` or `FetchedDriver::Source` (C/Rust); SHA-256 validated on every fetch.

### Adapter (`adapter.rs`)

`SourceAdapter` rewrites port/MMIO call sites (`inb/outb/inw/outw/readl/writel/ioread*`) to `hal_*` host imports declared in a WASI-preamble and compiles C/Rust to `wasm32-wasi`.

### Engine & Self-Healing (`engine.rs`)

`AutohalEngine` — 5-step async pipeline: (1) detection via HAL event loop → (2) local `DriverStore` lookup → (3) network fetch/adapt/compile → (4) SHA-256 validation + `CapabilityToken` grant + Wasmtime instantiation → (5) cache & register. `provision_blocking`/`provision_dedicated` serve IPC/UI paths. **Self-healing:** after 3 consecutive failures a device auto-rolls back to the Generic Fallback Driver with a warning toast (`record_failure` → `rollback_to_generic`); `uninstall_driver` (generic protected) and per-device capability overrides (`set_cap_override`) are supported.

### Store & Registry (`registry.rs`)

`DriverStore`/`DriverIndex` persist under `AIOS_DATA_DIR/drivers` (bincode/serde): fingerprint→driver mapping, failure counters and capability overrides.

### UI Parity (`ui_tui.rs`, `ui_gui.rs`)

- `HardwareInspector` (ratatui) — per-bus device table with status badges ([Active]/[Downloading...]/[Compiling]/[Generic]/[Failed]/[Rolled Back]), capability summary and hot-plug toast strip.
- `HardwarePanel` (egui) — device table (VID/PID, driver source, status colors), download/compile progress bars, interactive security capability matrix (checkboxes) and [Update Driver]/[Rollback to Generic]/[Uninstall]/[Rescan] buttons.
- Both render the same `DeviceView`/`Toast` data produced by the engine.

### Live integration (kernel TUI + GUI)

- `aios-gui` (`AiosApp`): the engine is created at startup (`hw_init`) with an initial provisioning pass; the **Hardware & Drivers tab (F9)** (`tabs/hardware.rs`) renders the shared `HardwarePanel` over refreshed `hw_views`/`hw_toasts` snapshots and routes `GuiAction`s (`Rescan`/`Update`/`Rollback`/`Uninstall`/`SetCapabilities`) back to the engine via `apply_hw_actions`.
- Kernel `aios` (`TuiApp`): `hw_engine`/`hw_views`/`hw_toasts` initialized in `new()` through `init_hw_engine` (inert in safe mode), refreshed each tick (`hw_refresh`) and re-scanned on the `F10` hardware re-probe (`refresh_hw`); `draw_system_tab` renders the `HardwareInspector` as the third block of the System & HW tab.
- TUI and GUI therefore show identical `DeviceView`/toast content for the same machine.

**TUI crash-resilience (v2.31.1):** all draw-path and handler `Mutex` locks (`app.state`, `bridge.registry`, `bridge.scheduler`, `ai_*`) go through the poison-tolerant `locked()` helper (`app_state.rs`), so a background bridge worker that panics while holding those mutexes can no longer panic the renderer. `run_tui` additionally wraps the `run()` loop in `std::panic::catch_unwind` and unconditionally restores the terminal (disable raw mode, leave the alternate screen, show cursor), returning `Err("AIOS TUI crashed: …")` on a caught panic instead of leaving the console in raw mode.

### Hot-plug event loop (`hotplug.rs`)

`HotplugMonitor` — a background thread (one per UI surface) that re-detects the attached `HardwareProfile` and diffs the fingerprint set against the previous poll, emitting `HotplugEvent::Added/Removed` over an `mpsc` channel. The first scan only records the baseline, so startup is silent. Detection is adaptive: on Linux a cheap signal (`cheap_signal`/`dir_signal_hash`) hashes the mtimes of `/sys/bus/{usb,pci,nvme}/devices` — directories that only change when a device is attached/removed — so a full `detect()` runs immediately when the device tree moves (cadence `signal_poll_ms`, default 250 ms), with `poll_ms` (default 1000 ms) as the safety-net full-scan interval; platforms without such a signal use the fixed `poll_ms` cadence. Native push events (Windows `WM_DEVICECHANGE`) are coalesced to at most one full scan per `poll_ms`, so a flapping device never rescan-spams; `HardwareProfile::detect_usb` also skips `0000:0000` PnP artifacts so phantom entries never reach the fingerprint diff. The monitor is never `Send`-shared into the engine; each UI tick drains the channel and applies events on the UI thread: `Added` → `provision_blocking`, `Removed` → `AutohalEngine::remove_device` (unloads the WASM instance + tracked device but keeps the cached driver, so a replug re-provisions instantly). The monitor stops cleanly on `Drop`.

- Kernel TUI: `TuiApp::hw_hotplug` started alongside the engine (inert in safe mode); `hw_poll_hotplug` drains it every tick before `hw_refresh`.
- GUI: `AiosApp::hw_hotplug` started with the engine; `hw_poll_hotplug` drains it in every `update` frame.
- Periodic `F10` re-probe remains as a manual full rescan.

---

## Network Stack (`aios-net`)

- **Crate**: `aios-net` v1.0.0 — TCP/UDP blocks and ICMP reachability for network communication
- **TCP** (`tcp.rs`): `TcpBlock`, `TcpConfig`, `TcpConnection`, `TcpMessage`, `TcpState` — mock state machine
- **Real TCP** (`real_tcp.rs`): `RealTcpBlock` — real `std::net::TcpListener`/`TcpStream` with non-blocking accept, connection management, send/receive, optional `CapabilityToken` enforcement (`CAP_NET_BIND` for `start_listening()`, `CAP_NET_CONNECT` for `connect()`)
- **UDP** (`udp.rs`): `UdpBlock`, `UdpConfig`, `UdpPacket`, `UdpState` — mock state machine
- **Real UDP** (`real_udp.rs`): `RealUdpBlock` — real `std::net::UdpSocket` with `bind()`, non-blocking `send_to()`/`receive_from()`, broadcast via `SO_BROADCAST`
- **ICMP** (`icmp.rs`): `ping(host, count, timeout)` sends real ICMPv4 echo requests (`IcmpSendEcho` on Windows, ICMP datagram/raw socket on Unix), validates the reply checksum and identifier/sequence, and returns `PingReport` with per-reply RTT, TTL, bytes and packet loss; IPv6 literals are rejected explicitly
- Connection tracking, send/receive with channels, broadcast, statistics
- 57 tests covering mock + real TCP/UDP lifecycle and ICMP echo/parse/resolve

---

## Filesystem Abstraction (`aios-core`)

- `FileSystem` — unified file access layer (Virtual, Local, Overlay)
- `FilePermissions` — read/write/executable per file
- `FileEntry` — path, size, is_dir, permissions
- Virtual: in-memory storage with permission checks
- Local: filesystem access through root path
- Overlay: virtual layer over local
- Read-only mode enforcement
- 20 unit tests

---

## Virtual File System (`aios-vfs`) — v2.10.0

- **Crate**: `aios-vfs` v1.0.0 — scheme-addressed async VFS, sandbox for the file manager.
- **Schemes**: `VfsScheme::{AIOS, HOST}`; `VfsPath` parses URI-style paths (`AIOS:///sandbox`, `HOST:///C:/...`) and exposes `parent()`, `join()`, `file_name()`, `to_uri()`.
- **Trait** `VirtualFileSystem` (async, `tokio::fs`): `list`, `read`, `write`, `create_dir`, `delete`, `rename`, `exists`, `metadata`, `open_seek`. `open_seek` returns `Box<dyn AsyncSeekReader + Send + Unpin>` where `AsyncSeekReader = AsyncRead + AsyncSeek` (used by AI-preview reads).
- **Implementations**: `AiosVfs` (sandboxed local dir with `canonicalize_inside` containment check and a shared path ACL via `with_path_acl()`) and `HostVfs` (real host paths, reads/writes gated by ACL tokens `vfs:host:read` / `vfs:host:write`).
- **Operations** (`operations.rs`): `Progress` (atomic bytes/files counters, `fraction()`, `pressure_fraction()`), `CancellationToken`, `total_bytes`, `copy_recursive`, `move_item`, `delete_item`, `read_head`, `read_at`.
- **Security** (`security.rs`): `AclContext` — in-memory capability set, thread-safe via `Mutex<HashSet>`; `PathScope` (scheme + normalized path), `PathAclRule` and `PathAcl` with `grant`/`revoke`/`check`/`require`/`rules` for the `vfs:read` and `vfs:write` rights; the most specific matching scope wins and a child revocation overrides a parent grant. `AiosVfs` enforces the shared ACL after `canonicalize_inside(root, path)` on every read/write operation.
- **AI preview** (`ai_preview.rs`): `analyze_file(name, head)` → `AiPreview { title, headline, lines: Vec<(AiLineKind, String)> }`; parses WASM name sections, detects panics in logs, emits source-level hints.
- 35 unit tests (cancellation, WASM name-section bytes, path containment, path ACL precedence and enforcement, copy/move/delete, preview).

## File Manager (`aios-fm`) — v2.10.0

- **Crate**: `aios-fm` v1.0.0 — two-panel (Volkov/Far style) file manager engine + TUI and GUI renderers.
- **State** (`state.rs`): `PanelSide::{Left, Right}`, `PanelState` (path, cursor, `SortRule::Name/Size/Date/Type`, entries), `human_size`.
- **Commands** (`commands.rs`): `Command` (Navigate/Refresh/Copy/Move/Delete/Mkdir/Rename/View/GrantHostRead/GrantHostWrite/Shutdown) and `Ack` over `tokio::mpsc::unbounded_channel`.
- **Engine** (`engine.rs`): `FileManager::new(fs, acl) -> (FileManager, UnboundedReceiver<Ack>)`; background command loop; Copy/Move/Delete spawn cancellable `tokio::spawn` jobs with `Progress`; `FmSnapshot { panels, active, jobs, acl }`; direct methods `send`, `snapshot`, `switch_panel`, `set_active`, `set_cursor`, `move_cursor`, `toggle_sort`, `selected`, `default_target`, `acl`, `fs`.
- **TUI renderer** (`ui_tui.rs`): `draw(frame, area, &FmSnapshot, rows)` (header with scheme + ACL, two panels, footer with job progress + hotkeys), `key_to_action`, `progress_bar`.
- **GUI renderer** (`ui_gui.rs`): `show(ui, &FmSnapshot, &FmTheme) -> Option<FmClick>` (two columns, click/double-click selection, progress bars, ACL panel).
- 16 unit tests (engine lifecycle, sort/movement, keymap, GUI theme).

---

## Multi-Node Distributed Cluster (`aios-cluster`) — v2.11.0

- **Crate**: `aios-cluster` v1.0.0 — distributed scheduling layer on top of `aios-process-mgr`. A node runs a `DistributedScheduler` behind an `Arc<Mutex<...>>`; an attached `ProcessExecutor` turns it into a **worker** (can host remote processes), a node without one is a pure **coordinator**.
- **Types** (`types.rs`): `NodeId` (u64), `NodeStatus {Unknown, Online, Offline, Leaving}`, `NodeMetrics` (CPU fraction, RAM used/total, process count, `load_fraction()`), `NodeInfo` (id, name, addr, hardware `tier` 1–3), `RemoteProcessId { node, pid }` (globally unique remote identity), `RemoteProcessSpec` (priority 0–4, RAM quota, optional block id / init payload / `[min_tier..=max_tier]` filters), `RemoteProcessStatus`, `PlacementStrategy {RoundRobin, LeastLoaded, ByTier}`.
- **Wire protocol** (`protocol.rs`): `ClusterMessage` enum serialized with bincode and framed as `[u32 LE length][payload]`. Requests carry `request_id` + `from` so replies (SpawnAck/KillAck/SetPriorityAck/GetStateReply) can be matched to a pending operation. `Spawn` optionally carries a process state snapshot (`state: Option<Vec<u8>>`) restored on the destination after spawn; `GetState`/`GetStateReply` fetch that snapshot for stateful migration.
- **Transports** (`transport.rs`): `ClusterTransport` trait (`addr`, `send`, `start`, `shutdown`). `TcpClusterTransport` — real `std::net::TcpListener` per node, connects to peers on demand, one frame per stream. `InMemoryClusterTransport` + `MemoryRegistry` route messages inside one process (deterministic tests / single-machine multi-scheduler).
- **Scheduler** (`scheduler.rs`): `DistributedScheduler` — a background **heartbeat thread** announces `Hello(self_info)` to peers on an interval; peers reply `Metrics`, so every node converges on a live view of the cluster. Liveness: `last_contact` per node; a node silent longer than `failover_threshold` flips to `Offline` in `tick()`. Placement filters online nodes by tier range then applies the strategy (LeastLoaded uses `load_fraction`, tie → lowest node id). `spawn`/`kill`/`set_priority`/`get_state` are blocking calls that drain the inbox until the matching ack or `ack_timeout`. `migrate` relocates a tracked process to another node statefully: it fetches the source snapshot via `get_state`, spawns the copy on the destination (explicit node or the placement strategy, never the source) with the snapshot restored, and only then kills the original — so a failure to fetch state or spawn leaves the source untouched; a relocate back onto the source node is rejected and the extra copy is re-killed. **Checkpoint replication**: every heartbeat period the hosting worker extracts a snapshot of each locally hosted process and broadcasts a fire-and-forget `Checkpoint { from, rid, state }` to all peers, so any coordinator can restore state on failover. On node loss `tick()` respawns that node's tracked processes elsewhere (`failover_respawn`), restoring the newest replicated snapshot. Received checkpoints are timestamped and pruned by `checkpoint_ttl` in `tick()` so stale snapshots from a long-silent node cannot be resurrected. **Metrics authority**: a known node's load is updated only by the dedicated `Metrics` message — the `Hello` snapshot is used only for first join, so a stale idle snapshot cannot overwrite live load. Bounded event log (`events()`, last 100).
- **Executors** (`executor.rs`): `ProcessExecutor` trait (spawn/kill/set_priority/status/metrics/**extract_state**/**restore_state**). `MockProcessExecutor` — deterministic, models 16 GiB RAM for meaningful load fractions, seeds each process state snapshot from `spec.payload`. `SchedulerProcessExecutor` — adapter over the real `aios-process-mgr::scheduler::Scheduler`. Every shared scheduler/state lock recovers from poisoning instead of panicking. State snapshots are opaque bytes; executors store them per process, the heartbeat thread replicates them as checkpoints, and `migrate` moves them across nodes.
- **Config** (`config.rs`): `ClusterConfig` read from `AIOS_CLUSTER_*` env vars or JSON (`node_id`, `node_name`, `addr`, `tier`, `peers`, `heartbeat_ms`, `failover_threshold_ms`, `failover_respawn`, `strategy`, `checkpoint_ttl_ms`). Returns `None` when clustering is not requested.
- **Tests**: 23 unit (protocol 6, transport 2, executor 4, scheduler 9, config 2) + 10 integration (`tests/scheduling.rs`: two-node spawn/kill, round-robin, least-loaded, TCP loopback, failover respawn, priority control, process migration, stateful migration carrying a snapshot, migrate error paths, unknown-node/no-peers errors) + 1 doc test.

---

## Marketplace (`aios-block-mgr`)

- `BlockMarketplace` — block registry with repository management
- `BlockMetadata` — name, version, description, author, sha256, tags
- `RepositoryEntry` — metadata + status (Available/Installed/UpdateAvailable/Deprecated)
- Publish, search, install, uninstall, check updates
- Multiple repository support with cross-repo search
- 18 unit tests

---

## Testing Architecture

- **Unit tests**: 1354 tests embedded in source files under `#[cfg(test)] mod tests` (v2.79.0 release baseline)
- **Integration tests**: 217 tests across the workspace `tests/` targets (integration, stress, chaos, proptest) (v2.79.0 release baseline)
- **Doc tests**: 1 test in crate documentation
- **Baseline total**: 1572 tests at v2.79.0, all passing, zero clippy warnings
- **Current (v2.84.0 re-verification)**: `cargo test --workspace` — **1824 passed / 0 failed in 100 suites**; `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` are clean (see `docs/AUDIT.md` §2)

**Speed test thresholds**:
- Debug mode: < 50us (unoptimized)
- Release mode: < 1us (optimized)

**Mock hardware profiles**: All tests use mock profiles, never requiring real hardware.

---

## Security Model (Current)

- SHA-256 checksums on all block binaries (integrity verification)
- Block state machine prevents invalid transitions
- RAM quota enforcement prevents memory exhaustion
- Crash count limits prevent infinite restart loops
- IPC bus freeze prevents message loss during hot-swap
- **IPC bus backpressure** prevents unbounded queue growth (Reject or DropOldest)
- **IPC bus dedup** prevents duplicate message processing
- **IPC bus metrics** for operational visibility
- **IPC ring buffer transport** for zero-copy heavy payload transfer (>4KB)
- **Watchdog** monitors AI Orchestrator health via HMAC heartbeats; enters Safe Mode on failure
- **Capability tokens** with time-bounded validity and HMAC signatures
- **Access control layer** validates every system call against token capabilities
- **Sandbox** enforces memory limits, syscall counts, and capability checks per block
- **Violation intercept** terminates blocks on unauthorized access
- **Block dependency graph** prevents loading blocks before their dependencies
- **Semantic versioning** ensures compatible block upgrades
- **Memory pressure detection** alerts when RAM usage exceeds thresholds
- **Hardware security bridge** unifies MPK/TEE/IOMMU protection for block isolation
- **CoW persistence** with atomic rollback and crash-recovery journal
- **Compressed telemetry** auto-compresses cold data with ZSTD
- **Web authentication** (v2.30.0, hardened post-audit): local accounts in `<AIOS_DATA_DIR>/users.json`, PBKDF2-HMAC-SHA256 (100 000 iterations) with per-user salts, constant-time hash/signature checks, HMAC-SHA256 session tokens (12 h TTL) signed with a secret from `aios_core::secrets` (`AIOS_AUTH_SECRET` → `<data_dir>/secrets/auth.key` → CSPRNG), five failed logins lock an account for 5 minutes. Register/login share `validate_credentials()` (3–64-byte usernames, 12–1024-byte passwords checked before hashing) and failed-login bookkeeping is pruned/capped at 10,000 entries. `require_auth` is default-deny for `/api/*` and `/ws/*`; CORS is disabled or pinned to `AIOS_CORS_ORIGIN`. All compiled-in issuer/heartbeat secrets were replaced by the same resolver.

**Not yet implemented**: full WebAssembly runtime integration. See `docs/TODO.md`.

---

## Development Roadmap (Phases 22–27)

### Phase 24: EasyLang Engine & No-Code App Builder (`aios-builder`) — *COMPLETED*
- **`aios-builder` crate**: Workflow type (JSON serializable), AutoManifestGenerator (WASM binary analysis + workflow intent keyword matching for capability inference), WorkflowCompiler (WAT text generation → `wat` crate compilation to WASM)
- **EasyLangParser**: line-oriented DSL (`spawn`, `timer`, `load`, `unload`, `kill`, `query`, `compact`, `status`)
- **18 unit tests** covering WASM parsing, capability detection, JSON manifest generation, workflow compilation, DSL parsing

### Phase 24 sub-item: Backend Workflow Execution
- **`POST /api/v1/workflow`**: Batch intent execution endpoint — accepts `{prompts: [...]}`, parses and executes each step sequentially, returns per-step results with capability checking
- **Builder integration**: `runWorkflow()` sends a single batch request instead of N individual intents

### Phase 22: Universal Web & Desktop UI (`aios-studio`) — *COMPLETED*
- **Smart Command Palette:** Input field (`Ctrl+K`) with intent autocomplete, sends `POST /api/v1/intent`
- **Real-time Telemetry Dashboard:** WebSocket Canvas charts for RAM, process table, health cards
- **Capability Consent Center:** Visual block list with capability indicators and quick-action buttons (Stop, Compact Memory)
- **Easy Builder Tab:** Visual workflow step editor — palette of trigger/action blocks, add/remove/reorder steps, sequential execution as intents
- **Static serving:** `tower-http::ServeDir` fallback serves the SPA from `aios-bridge` at `/`

### Phase 23: Multi-Mode AI Engine (`aios-llm`) & Hybrid Intent Router
Three adaptive AI modes depending on hardware resources:

- **Cloud-First (Zero-Resource) — 2–4 GB RAM targets**
  - Zero MB disk/RAM for local models
  - Anonymous request proxying via local kernel → external AI providers (Groq, OpenRouter, Google AI Studio)
  - Request anonymization: geo-marker stripping, personal ID removal

- **Micro-Local (Hybrid) — 4–8 GB RAM targets**
  - Local micro-model (SmolLM/Qwen-0.5B, ~300 MB RAM) for offline system command parsing
  - Cloud fallback for heavy reasoning tasks
  - Automatic mode switching based on network availability

- **Full-Local (Autonomous) — 8+ GB RAM targets**
  - Quantized local models 3B–7B (GGUF INT4/FP8) with KV-Cache freeze
  - ZSTD compression of cold KV-Cache (~300–500 MB)
  - Background cache warming and compression daemon

### Phase 24: EasyLang Engine & No-Code App Builder (`aios-builder`)
- **In-Memory EasyLang Compiler:** Micro-compiler inside the kernel translating declarative text (RU/EN, ~10 keywords) to binary `.wasm` in milliseconds
- **Auto-Manifest Generator:** Automatic capability requirement analysis and `CapabilityToken` manifest generation
- **Visual Workflow Editor:** Embedded in `aios-studio` — "When event X → Execute action Y" drag-and-drop

### Phase 25: Secure Web Surfing & Search (`aios-browser` & `aios-search`) — *COMPLETED*
- **`aios-browser` crate**: `BrowserEngine` with `navigate(url)` → fetches HTML via `reqwest`, parses via `HtmlParser`, renders to text via `Renderer`
  - `HtmlParser`: built on `scraper`/html5ever (WHATWG-compliant) — extracts text content, links, title; structures output with headings `#`/`###`, lists `•`/`1.`, `pre`/`br` preserved, table rows `|`, `hr`, images as `[alt]`; strips `<script>`, `<style>`, `<head>`, `<iframe>` and hidden elements; links resolved against the page base URL and deduplicated, non-web schemes filtered
  - `NetworkClient`: configurable user-agent, timeout, redirect limit; SSRF policy — HTTP(S) only, every hop DNS-resolved and rejected for loopback/private/link-local/non-global addresses, explicit redirect chain, streamed bodies capped by `max_response_bytes` (8 MiB default). DNS names are pinned to the validated addresses for the actual request via `reqwest::ClientBuilder::resolve_to_addrs`, preventing a second resolution from selecting a private address. `BrowserConfig::allow_private_targets` (or `AIOS_BROWSER_ALLOW_PRIVATE`) opts in for local test servers. `BrowserConfig::for_untrusted()` disables the native headless fallback and embedded scripts for caller-supplied URLs so that the validated client stays authoritative.
  - `Renderer`: DOM → markdown-like text output (headings `#`, links `[text](url)`, lists `•`)
  - `Page` type: `url`, `title`, `text_content`, `html`, `links: Vec<Link>`
  - `BrowserConfig`: `user_agent`, `timeout_secs`, `max_redirects`, `sandbox_enabled`, `headless_fallback` (default on), `allow_private_targets` (default off), `max_response_bytes` (default 8 MiB)
  - **Headless render-to-text fallback** (`headless` module, v2.17.0): when the plain fetch yields no readable text (`looks_like_js_shell`, < 80 non-whitespace chars), the engine runs a headless Chromium-class browser (`msedge`/`chromium`/`google-chrome`/`brave-browser`, override `AIOS_HEADLESS_BROWSER`, `--no-sandbox` via `AIOS_HEADLESS_NO_SANDBOX`) with `--headless --dump-dom --virtual-time-budget=5000`, capped at 4 MiB on a blocking thread with a 30 s timeout; the rendered DOM is adopted only when `has_more_content` finds +60 more non-whitespace chars than the plain fetch, otherwise the original HTML stays authoritative
  - **28 unit tests**: text extraction, link parsing, title extraction, URL resolution, head/comment stripping, structured layout
- **`BrowserBlock` (kernel block integration)**: `BrowserBlock` implements `StatefulBlock` in `aios-browser/src/block.rs` and is registered at boot in all binaries (`aios`, `aios-tui`, `aiosd`)
  - IPC commands: `browse` (fetch + parse a page, returns bincode-serialized `Page`), `open_native` (open URL in the OS default browser via the `open` crate), `browser_status` (config + state as JSON); `HealthCheck` supported
  - Owns no persistent runtime — each navigation runs on a dedicated on-demand current-thread Tokio runtime, safe from both sync and async callers (no nested-runtime panic)
  - State extract/restore via bincode (`BrowserConfig` + `BlockState`)
- **`aios-search` crate**: `SearchEngine` with multi-backend anonymous search + AI summarization
  - `DuckDuckGoBackend`: POST to `html.duckduckgo.com/html/`, HTML response parsing
  - `SearXngBackend`: GET with `format=json`, JSON response parsing
  - `BraveBackend`: GET to `api.search.brave.com`, API key via `X-Subscription-Token`, JSON parsing
  - `SearchSummarizer`: integrates with `aios-llm` for TL;DR (2-3 sentence LLM summary of top 5 results)
  - `SearchConfig`: `backend`, `api_key`, `api_url`, `max_results`, `enable_summary`
  - **3 unit tests**: config defaults, engine creation, backend URLs
- **aios-bridge REST endpoints**:
  - `POST /api/v1/browse` — `{"url": "..."}` → title, text_content, links
  - `POST /api/v1/search` — `{"query":"...","backend":"...","max_results":N,"enable_summary":bool}` → results + AI summary

### Phase 25b: Embedded JS Engine & Stateful Browser Session (`aios-browser`, v2.39.0) — *COMPLETED*
- **JS execution pipeline** (`src/script.rs` + `src/script_prelude.js`): every fetched page runs through an embedded pure-Rust engine (boa 0.22)
  - Zero host functions: Rust injects `__dom` (the parsed `DomNode` tree as JSON), `__meta` (url/title/user-agent/listeners/storage) and `__console`, evaluates the prelude, runs page scripts + userscripts, then reads back `JSON.stringify(__dom/__meta/__console)` — no per-bridge-function FFI surface to keep in sync
  - Fresh `boa_engine::Context` per page/`evaluate`: the engine is stateless across IPC calls, deterministic and immune to cross-page leakage; `eval_js` replays the stored post-script HTML without re-running page scripts
  - JS→Rust conversion treats only finite, non-negative `length` as an array and materializes at most 10,000 elements (`MAX_SCRIPT_ARRAY_ITEMS`), so a spoofed `length` cannot hang/OOM the browser task
  - Prelude provides: DOM hydration (getElementById/querySelector(All), textContent/innerHTML mutation, createElement/appendChild/insertBefore/remove, reflective attributes incl. `src`/`href`, `addEventListener` storage + real `dispatchEvent`, `Event`/`CustomEvent` constructors), console shim with level prefixes and caps, `location` (href setter records navigation in `__meta.nav`, followed by the engine — Phase 25d), `localStorage`/`sessionStorage` backed by `__meta`, `alert`/`confirm`/`prompt` shims, `URL`/`URLSearchParams` shims, a host-backed `fetch`/`XMLHttpRequest` that parks requests in `globalThis.__net` for the engine to serve (Phase 25e), timer queue `__aiosFlushTimers` flushed once after all scripts (each callback ≤ 1× per flush)
  - Known limits (documented, noted into the console): `document.write` no-op, no user-input event loop (clicks/keys never dispatch), `error` events stored not auto-dispatched
- **DOM → HTML serializer** (`src/serialize.rs`): round-trips `HtmlParser::parse`, honours VOID/RAW_TEXT element sets, escapes text/attrs, sanitizes script-produced attribute names
- **`BrowserSession`** (`src/session.rs`): engine + per-tab back/forward stacks + bookmarks + userscripts
  - `normalize_url` (adds `https://` when no scheme; exported for UI reuse), `back`/`forward`/`reload`/`new_tab`/`close_tab`/`select_tab`, bookmark add/remove/JSON persistence, `eval_js`, `SessionSnapshot` for live-update state transfer (pages refetched on demand; legacy `(config, state)` blobs still restore)
- **`BrowserBlock` IPC** (`src/block.rs`): previous surface (`browse`, `open_native`, `browser_status`, `HealthCheck`) plus `back`, `forward`, `reload`, `new_tab`, `close_tab`, `select_tab`, `session_status`, `add_bookmark`, `list_bookmarks`, `remove_bookmark`, `eval_js`, `add_user_script`; `browse` routes through the session so history accumulates; state extract = `(BrowserConfig, BlockState, SessionSnapshot)` with fallback restore of the old 2-tuple
- **UI integration**: TUI `aios` web tab — `J` opens a JS console sidebar (script count, errors in red, console lines), footer hint, post-load `scripts=N errors=M console=K` log line, `web_navigate` uses `normalize_url`; `aios-tui::fetch_url` runs the full `BrowserEngine` (fetch → parse → scripts) on a dedicated current-thread runtime

### Phase 25d: Engine Iteration 2 — Lifecycle, Dynamic Scripts, Navigation (`aios-browser`, v2.40.0) — *COMPLETED*
- **Synthetic lifecycle events** (`src/script_prelude.js`, `src/script.rs`)
  - One dispatch core for nodes/document/window: non-enumerable `__ev` listener bags, `fireAt(obj, evt)` runs the `addEventListener` list then the `on<type>` property, catches handler exceptions into the console as `[js] <type> handler error: message + stack`
  - `Event`/`CustomEvent` constructors + working `dispatchEvent` on every node, `document` and `window`; `window.addEventListener/removeEventListener`
  - `ScriptEngine::fire_dom_content_loaded` / `fire_load` (invoked by `run_page_scripts`): `readystatechange` → `DOMContentLoaded` on the document, then after the timer flush `readystatechange` → `load` on window → `load` on document; `document.readyState` walks `loading → interactive → complete` (`evaluate` marks stored pages `"complete"` before running a snippet)
  - Phase order inside `run_page_scripts`: page + user scripts → settle → `DOMContentLoaded` → settle → `__aiosFlushTimers` → settle → `load` → settle → readback (`settle` = dynamic-script rounds then `fetch`/XHR servicing, repeated while responses queue more work — Phase 25e)
- **Dynamic `<script>` execution** (`src/engine.rs`, `src/script.rs`)
  - `ScriptEngine::collect_dynamic_scripts` walks the live `__dom` for unmarked script elements created by page code (`__aiosMarkScripts` pre-marks the parser-inserted batch in `ScriptEngine::new`); inline code runs in place, `src` resolves against the page URL and fetches through `NetworkClient`; each element is marked on collection (returned once); non-JS types skip with a console note, `type="module"` elements are collected with a `module` flag and evaluated as modules
  - Caps shared across all phases of one page: `MAX_DYNAMIC_ROUNDS = 8`, `MAX_DYNAMIC_SCRIPTS = 20`, console notes on both limits
- **Navigation loop** (`src/engine.rs`, `src/html_parser.rs`)
  - `HtmlParser::extract_meta_refresh` (`MetaRefresh { delay_secs, target }`) parses `<meta http-equiv="refresh">`; only `delay_secs == 0` navigates, delayed refreshes log `[nav] meta refresh in Ns ignored`
  - `build_page` now loops: run the pipeline → take script nav (`__meta.nav`) or meta refresh → resolve against the current URL (`Url::join`) → guard same-URL loops / non-http schemes / `MAX_NAV_HOPS = 5` → fetch → repeat; every step logs `[nav]` lines, fetch failures keep the current page with a `[nav] failed to load` error
  - Returned `Page::url` is the final URL of the chain; headless fallback and link extraction use it
- **`unsafe impl Send for ScriptEngine`** (documented on the impl): the boa context legitimately spans async fetches while `BrowserBlock::block_on` needs `Send` futures; the engine stays confined to a single logical task (task polls never overlap)
- **v2.40.0 test coverage**: +23 tests (engine lifecycle/dynamic/navigation incl. a local HTTP test server for end-to-end `location.href`/meta-refresh/hop-limit chains; script-level fire/readyState/dispatch/collect units)

### Phase 25e: Engine Iteration 3 — ES Modules & Host-backed fetch/XMLHttpRequest (`aios-browser`, v2.44.0) — *COMPLETED*
- **ES modules** (`src/script.rs`): `ScriptEngine::run_module` parses the source with boa's `Module::parse(Source, None, context)` and evaluates it via `load_link_evaluate`; `export`/`import` syntax is accepted, module top-level code runs in the page context, and a rejected evaluation is observed through a JS `.then(onFulfilled, onRejected)` probe (boa keeps `PromiseState` crate-private) and reported as a script error. Both static and dynamically-collected `type="module"` scripts take this path.
- **Two-phase network bridge** (`src/script_prelude.js`, `src/script.rs`, `src/network.rs`, `src/types.rs`): the prelude parks each `fetch`/`XMLHttpRequest` in `globalThis.__net` (id → `{resolve, reject, xhr}`, plus a drained `requests` list); the engine calls `pending_requests()`, performs each request with `NetworkClient::request(method, url, body)` (GET/POST/PUT/DELETE/HEAD) and settles the parked promise/callback with `resolve_request(id, FetchResponse)` — zero host functions preserved. `FetchResponse { status, status_text, url, body, headers, error }` treats non-2xx as data; transport failure sets `error` and rejects/`error`-fires.
  - `fetch` resolves to a `Response` (`ok`/`status`/`statusText`/`headers`/`text()`/`json()`/`clone`); `XMLHttpRequest` supports `open`/`setRequestHeader`/`send`/`addEventListener` and fires `readystatechange` → `load` → `loadend` (or `error`). Relative request URLs resolve against the page URL via the `URL` shim.
  - `settle` interleaves `run_dynamic_rounds` and `resolve_fetches` while responses keep queueing work; caps `MAX_FETCH_ROUNDS = 8`, `MAX_FETCHES = 32` (over-cap requests are rejected so no promise hangs).
- **v2.44.0 test coverage**: +4 in-crate tests (module evaluation with `export`, fetch queue+resolve round-trip, transport-error rejection, XHR `onload` delivery) — 106 total in `aios-browser`.

### Phase 28: Headless Daemon (`aios-daemon`) — *COMPLETED*
- **`aios-daemon` crate**:
  - `aiosd` binary: headless server performing the same initialization as `aios-tui` without terminal access
  - Loads built-in blocks (hal, ipc_bus, scheduler) and disk blocks from `AIOS_BLOCKS_DIR`
    (skipped when `AIOS_SAFE_MODE` is set)
  - Opens persistent store (`redb`) at `AIOS_DATA_DIR/context.redb`
  - Spawns system processes (ai_orchestrator, io_handler, health_monitor)
  - Starts watchdog heartbeat thread
  - Background loop: logs heartbeat (processes, RAM, watchdog state) every 10s, persists telemetry every 60s
  - Minimal dependencies: no ratatui, crossterm, egui, or wasmtime
  - Environment config via `AIOS_DATA_DIR`, `AIOS_BLOCKS_DIR`, `AIOS_MOCK_PROFILE`, `AIOS_SAFE_MODE`, `RUST_LOG`
- **`aios-tui` headless mode**:
  - `--headless` CLI flag and `AIOS_HEADLESS=1` env var: skips TUI initialization, runs background loop
- **Docker**:
  - Dockerfile builds `aios` (kernel binary, `--daemon` runs headless) and `aios-daemon` (`aiosd`), copies the `aios-studio` SPA into `/app/aios-studio` (served by the bridge with `ServeDir`), declares `EXPOSE 8080` and sets `AIOS_BRIDGE_PORT`
  - `docker-compose.yml`: the `aios` service publishes `8080:8080`; the `safe-mode` profile starts the kernel with `--safe-mode` and `AIOS_SAFE_MODE=1`
  - Image size reduced from ~800MB to ~120MB

### Phase 26+27: Atomic Updates, Store, Telemetry & Debug (`aios-updater`, `aios-store`, `aios-telemetry`, `aios-debug`) — *COMPLETED*
- **`aios-updater` crate**:
  - `DualBootManager`: A/B slot management with `swap()`, `boot_success()`, `detect_active_slot()`, active/inactive slot info
  - `HotSwapEngine`: Tracks hot-swap operations by `BlockId` with swap counter; wraps aios-live-update
  - `RollbackManager`: Snapshot-based rollback with configurable timeout (default 1s auto-rollback), snapshot pruning
  - **12 unit tests**: slot creation, swap, boot success, hot-swap, rollback scenarios
- **`aios-store` crate**:
  - `ManifestInfo`: name, version, description, author, capabilities (HashSet), wasm_sha256, signature (Ed25519), store_url
  - `ManifestValidator`: SHA-256 content validation, Ed25519 signature verification, capability whitelist
  - `StoreRegistry`: name@version keyed HashMap with `register()`, `get()`, `find_all()`, `list()`, `unregister()`
  - `StoreClient`: HTTP client with `fetch_index()` and `download_block()` for remote store
  - `StoreSource` / `SourceKind`: three block sources — GitHub (`github:owner/repo`), local dir (`local:path`), HTTP update service (`http://host:port`)
  - `BlockInstaller`: on-disk installs `{name}_{version}.wasm` + sidecar JSON in `AIOS_BLOCKS_DIR`; SHA-256 verification, `backup`/`rollback` (`.bak`), `check_updates`, semantic `cmp_version`
  - `StoreManager`: facade over sources + installer — `search`, `install`, `update` (auto-rollback on failure), `uninstall`, `rollback`, `parse_source_spec`, `block_on` (sync contexts)
  - **42 unit tests**: source URLs, catalog scan, installer, rollback, manager flows
- **`aios-telemetry` crate**:
  - `TraceContext`: Span tree with `begin_span()`, `end_span()`, `set_tag()`, `set_status()`, `to_json()` (JSON export)
  - `FlightRecorder`: Ring buffer with kind-based filtering, configurable max_events + retention_secs, `dump()` and `dump_by_kind()`
  - `MetricCollector`: Counters, gauges, histograms with `snapshot()` (MetricSnapshot) and `to_prometheus()` (Prometheus exposition format)
  - **17 unit tests**: span nesting, error status, JSON export, flight recorder record/dump/clear, all metric types
- **`aios-debug` crate**:
  - `CrashReporter`: Generates crash reports with optional zero-knowledge mode (hash redaction, drops flight data)
  - `CrashKind`: Panic, WatchdogTimeout, OOM, BlockCrash, Unknown
  - `PanicHandler`: Custom panic hook routing panic info to CrashReporter; uses std::panic::set_hook
  - **6 unit tests**: report generation, zero-knowledge mode, JSON export, latest/bulk reports
- **aios-bridge REST endpoints**:
  - `GET /api/v1/store/index` — list all registered manifests
  - `POST /api/v1/store/register` — register a new manifest
  - `GET /api/v1/metrics` — Prometheus-format metrics
  - `GET /api/v1/traces` — current TraceContext as JSON
  - `POST /api/v1/crash-report` — trigger a crash report
- **Update service endpoints** (Phase 40, `aios-bridge`):
  - `GET /index.json`, `GET /store/index.json` — raw on-disk block catalog
  - `GET /blocks/{name}.wasm`, `GET /store/blocks/{name}.wasm` — block binary download
  - `POST /api/v1/store/publish` — publish user-created block (base64 wasm + SHA-256 + manifest); serves the local update-service role
- **BridgeContext** enriched with `StoreRegistry`, `MetricCollector`, `FlightRecorder`, `TraceContext`, `CrashReporter`, `PanicHandler`, `blocks_dir` (`AIOS_BLOCKS_DIR`)

### Network Settings Block (`aios-net-config`, Phase 40)
- `NetworkConfig` / `ProxyConfig` / `DnsConfig` / `InterfaceConfig` / `ProxyProtocol` — full network configuration with JSON serialization and partial updates (`apply_updates` with validation: ports 1–65535, IP syntax, proxy URL parsing)
- `NetworkConfigStore` — atomic JSON persistence (temp file + rename) under `AIOS_DATA_DIR`/`network.json`
- `NetSettingsBlock` — `StatefulBlock` on the IPC bus: `net_get`, `net_set <json>`, `net_reset`, `net_persist`; state extract/restore via bincode
- **32 unit tests** across config/validation/store/block

### Kernel `net_settings` integration (Phase 41)
- `net_settings` is registered in the kernel block registry at boot (`aios/src/orchestrator.rs`) and its handler is wired into the `MessageRouter`; the resulting `BlockId` is exposed as `OrchestratorState::net_block_id`
- Kernel TUI (`aios`) hotkey `n` opens a `key=value` input mode; on `Enter` the tokens are converted to a partial-JSON update and dispatched as `net_set` over IPC, with the returned config JSON logged to the Events pane
- Shell command `store publish <file.wasm> [name] [version]` (available in `aios-tui` and through `console ...` in `aios-console`) computes the file SHA-256, base64-encodes the wasm and posts a `StorePublishRequest` to `POST /api/v1/store/publish` (bridge port from `AIOS_BRIDGE_PORT`, default `8080`); name defaults to the file stem, version to `1.0.0`. `HostBackend` builds the same JSON body with `serde_json::json!` without adding an `aios-bridge` dependency, and `HostBackend::set_publish_url` overrides the endpoint in tests
- `StorePublishRequest` / `StorePublishResponse` in `aios-bridge::dto` are both `Serialize + Deserialize` for client round-trips


### TUI Store & Network Shell Commands (`aios-tui`, Phase 40)
- `store list | sources | add-source <spec> | search <q> [--source N] | install <name> [--source N] | update [name] [--source N] | uninstall <name> | rollback <name>`
- `net get | net set key=value ... | net reset` — reads/writes the network config through `NetSettingsBlock` (persisted via `NetworkConfigStore`)

### Ed25519 Signature & Trust Enforcement (`aios-store`, Phase 42 / v2.5.0)
- **Signing model** — every manifest carries an optional `SignatureInfo`; the canonical bytes are `aios-manifest-v1\n` + name + version + description + author + sorted capabilities + size + `wasm_sha256`; `sign_manifest(manifest, &SigningKey) -> SignatureInfo` signs them with Ed25519 (`ed25519-dalek` v2, `rand_core` feature)
- **Verification** — `ManifestValidator::verify_signature` performs a real `verify_strict` check against the embedded signing key; `verify_signature_with_keys(manifest, &[String])` accepts any trusted public key from the list
- **Installer enforcement** — `BlockInstaller.trusted_keys: Vec<String>`: when non-empty, `install_from_bytes` rejects unsigned manifests and any manifest not signed by one of the trusted keys. Constructed via `with_trusted_keys(dir, keys)` / `from_env(dir)`; `Default` reads `AIOS_TRUSTED_PUBLIC_KEYS` (`,`/`;`-separated). The installer sidecar now persists the full `ManifestInfo` including the signature, so signed installs stay verifiable
- **Per-source trust policy** — `StoreSource.trusted_public_keys` (`#[serde(default)]`); `StoreManager::verify_source_manifest(source, manifest)` is enforced in `install()` and `update()`. The default GitHub source inherits its official key from `AIOS_OFFICIAL_PUBLIC_KEY` via `official_public_key()`. With no keys configured, signatures are still checked against the embedded key (unsigned installs allowed)
- **TUI shell** — `store sign <file.wasm> [name] [version] [--key <secret_hex>]` signs a local wasm (key from `AIOS_STORE_SIGNING_KEY` if `--key` is omitted) and writes the signed sidecar; `store verify <name>` checks SHA-256 + Ed25519 of an installed block
- **Signed publish** — `store publish ... [--key <secret_hex>]` builds the manifest, signs it with Ed25519 and includes the `SignatureInfo` in `StorePublishRequest`; the bridge verifies the signature via `ManifestValidator::verify_signature` before `install_from_bytes` (which is constructed with `from_env` and therefore also applies the local `AIOS_TRUSTED_PUBLIC_KEYS` policy to signed publishes; unsigned publishes remain allowed unless keys are configured)
- **Unified console parity** — `HostBackend` implements the same `store sign` and `store publish` semantics as the TUI. `store sign` computes SHA-256, writes `{name}_{version}.json` next to the input using only the final path component of name/version, and prints the Ed25519 public key; `store publish` includes the signature when `--key` or `AIOS_STORE_SIGNING_KEY` is set. Both are registered subcommands and are no longer reported as deferred.
- **`store trust <source> [--key <public_hex>] [--clear]`** — sets/clears `StoreSource.trusted_public_keys` from the `aios-tui` shell (key hex validated as a real Ed25519 public key), persisted through `StoreManager::save_config` into the source config; `store sources` shows the trusted-key count per source

---

## Layer 6: Integrated Binary (`aios/`)

### Overview
The `aios` crate is a unified system binary that merges all 17+ workspace crates into a single executable. It provides:
- Interactive TUI dashboard (ratatui-based) for system monitoring and control
- Headless daemon mode for Docker/background deployments
- Real hardware probing on startup
- Centralized orchestration of all subsystems

### Modules
- `hw_probe.rs` — Real hardware detection using sysinfo + platform-specific APIs
- `orchestrator.rs` — Async initialization of IPC, Scheduler, BlockRegistry, AccessControl, Watchdog, LLM, WASM, Bridge (skips disk blocks + bridge in safe mode)
- `tui/` — Ratatui interactive dashboard with 7 tabs (System/Blocks/AI Console/Bridge/Network & Store/Web/Shell) and event log

### Binary Modes
- `aios` — Interactive TUI mode (default)
- `aios --daemon` — Headless daemon mode (background server)
- `aios --safe-mode` — Minimal recoverable kernel (no disk blocks, no bridge, `SAFE MODE` badge)

### TUI Hotkeys
| Key | Action |
|-----|--------|
| Tab / F1 / ? | Next tab / help overlay |
| 1-8 | Direct tab select |
| Alt+1-8 | Direct tab select even while typing in the Shell / Web URL / AI query / net input line |
| q / Ctrl+C | Quit |
| W | Launch the AIOS GUI desktop shell (`aios-gui`); found on `PATH` or next to the `aios` executable (exposed on live as `DISPLAY=:0` X/Xorg session) |
| Space | Pause/resume log scroll |
| r / k / l (Blocks) | Restart / unload / load selected block |
| g / j / k / o / u / d / b / B / n (Web) | Omnibox / link nav / open / scroll / back / native viewer |
| n / g / s (Network & Store) | Edit net config / show config JSON / refresh store list |
| Files (Tab 8): Tab/↑/↓, Enter, Backspace, F2-F3, F5-F9, g/w, r | Switch panels / navigate, open dir or AI-preview, parent dir, rename, view, copy, move, mkdir, delete, sort, grant host read/write, refresh |

### AI Console (Tab 3, Phase 43 / v2.6.0, Phase 45 / v2.9.0)
- Interactive LLM chat: `i` enters query mode, `Enter` sends; each query re-applies the current console `LlmConfig` to the shared `BridgeContext.llm` engine, so console settings and the HTTP `/api/v1/llm/query` endpoint stay consistent
- Slash-command system handled in `TuiApp::handle_ai_command`: `/help /status /clear /history /system /model /backend /key /temp /tokens /preset /save /load`; backend/model/key changes rebuild the engine asynchronously via `apply_config_async`
- Built-in help panel (справка) toggled with `h` or `/help`, styled reference of keys + commands; prompt history (last 50) navigable with `Up`/`Down`
- Status footer `backend | model | temp | tokens | state` (`streaming...` / `done: Nms` / error); `/status` reports config + detected local GGUF models via `aios_llm::local::detect_local_models`
- **Streaming (Phase 45)**: `submit_ai_query` spawns `LlmEngine::query_stream(&req, tx)` on a tokio task; deltas accumulate in `TuiApp.ai_stream` (rendered live in yellow) and the final text is appended to the transcript. `aios-llm` streams SSE deltas for cloud backends (`extract_stream_delta`, OpenAI + Google AI Studio shapes) and per-token deltas for local backends (`generate_tokens` callback loop)
- **Chat persistence (Phase 45)**: the transcript is kept in `TuiApp.ai_log` (`Vec<AiMessage>` where `AiMessage { role, text }`), auto-saved as JSON Lines to `AIOS_DATA_DIR/chat.jsonl` (default `aios_data/chat.jsonl`) after each completed reply and on quit via `save_chat`, restored at boot via `load_chat`; manual control with `/save` / `/load`
- **Prompt templates (Phase 45)**: `TuiApp.ai_presets` (`BTreeMap<name, text>`) seeded with `assistant`/`code`/`translator`/`explainer`; `/preset <name>` applies a template as the system prompt, `/preset <name> <text>` defines one, `/preset list` / `/preset del <name>` manage the set
- `aios-llm` exposes `LlmEngine::config()`, `provider_name()`, `backend_label()` for config introspection

### Startup Sequence
1. Hardware probe (CPU, RAM, GPU, OS)
2. Initialize IPC bus (SharedIpcBus)
3. Create Scheduler with RAM-aware config
4. Initialize BlockRegistry — wire the compiled-in base set (`hal`, `browser`, `net_settings`) via the block factory into the `MessageRouter`, then discover + execute disk blocks from `AIOS_BLOCKS_DIR` (default `./blocks`) unless safe mode
5. Setup AccessControl + Watchdog
6. Initialize LLM Engine (cloud backend by default)
7. Initialize WASM Executor (BlockExecutor)
8. Create BridgeContext with all subsystems
9. Spawn Bridge HTTP server (axum, port from `--bridge-port`, default 8080) — skipped in safe mode
10. Start TUI event loop (or enter daemon loop)

The browser works out of the box on a fresh machine: no config file, no installed browser, and no network are required to start — the block is active in the topology, dispatchable over IPC, and the Web tab (`B`/`n`/omnibox) opens any URL in the native WebView.

## Layer 7: Live USB Deployment — retired (v2.77.0)

The Linux Live USB path (the `live/` directory with `build.sh`, `init.rs`,
`sfs-up.sh`, `aios-install`, `grub.cfg`, etc., plus the
`scripts/build-live-iso.*` and `scripts/fix-wsl2.ps1` helpers) was removed in
v2.77.0. Bare-metal deployment now uses only the native UEFI-only GPT/ESP image
described in the Native UEFI Boot Chain section: `aios-image` builds it and
`aios-install` writes it to a disk. The `aios-init` initramfs tooling in Layer 8
is retained as a standalone component, but is no longer wired into a shipped
Linux Live ISO. Earlier milestones mentioning `live/` remain in the changelog
as history.

## Layer 8: `aios-init` & standalone initramfs (`aios-init/`, `build_initramfs.sh`)

### Overview
`aios-init` is the dedicated Rust `/init` for the AIOS initramfs: a statically compiled (`x86_64-unknown-linux-musl`) PID 1 supervisor that mounts the core VFS, hands over to the AIOS block, and never panics — eliminating `Kernel panic: No working init found`.

### Responsibilities (boot order)
1. Install `sigaction` handlers: SIGTERM/SIGINT/SIGHUP set a shutdown flag; SIGCHLD (`SA_NOCLDSTOP`) wakes the reap loop; SIGPIPE ignored.
2. Mount core VFS: `/proc` (proc), `/sys` (sysfs), `/dev` (devtmpfs; if unavailable, `mknod` `/dev/console` 5:1, `/dev/null` 1:3, `/dev/tty` 5:0), `/tmp` (tmpfs), `/run` (tmpfs).
3. Open `/dev/console` and `dup2` it to fd 0/1/2 so all boot logs reach the console.
4. `set_user_env()`: full BusyBox/Alpine `PATH`, `TERM=linux`, `DISPLAY=:0`, `AIOS_DATA_DIR=/tmp/.aios`, `XDG_RUNTIME_DIR=/run/aios` — so the supervised dashboard and kernel blocks get a ready-to-use userspace environment.
5. `bring_userspace()` (live variant): spawn + supervise `/bin/sh /sfs-up.sh` — loads storage/loop/squashfs/ext4 and GPU (dependency-ordered DRM, fbdev/vesa fallbacks) modules, finds and bind-mounts the Alpine userspace from `aios.squashfs`, brings up networking (`rcS`) and udev, then starts **Xorg on `:0`/VT7**. When no userspace is available the system stays console TUI-only (previous behavior).
6. Spawn and supervise `/system/aios-core` (fallback `/installer`), restarting up to 3 times (300 ms backoff) on crash.
7. Reap every child with `waitpid(-1, WNOHANG)` so orphaned grandchildren never become permanent zombies.
8. On SIGTERM/SIGINT: forward to the block, wait up to 5 s, then SIGKILL.
9. Emergency fallback: if no block exists or restarts are exhausted, start a rescue shell (`/bin/sh` → `/bin/busybox sh` → `/bin/ash`); if no shell is present, park in an idle reap loop — never a kernel panic.

### Building the initramfs
```
rustup target add x86_64-unknown-linux-musl
./build_initramfs.sh                     # initramfs.cpio.gz (kernel TUI + init)
./build_initramfs.sh --keep-rootfs       # keep rootfs/ staging dir
./build_initramfs.sh --no-aios-core      # skip the aios kernel binary (rescue-shell only)
BUSYBOX_PATH=/usr/bin/busybox.static ./build_initramfs.sh   # + rescue shell
```
The script runs `cargo build --release --target x86_64-unknown-linux-musl` for `aios-init`, and (unless `--no-aios-core`/`SKIP_AIOS_CORE=1`) `cargo build -p aios --release --target x86_64-unknown-linux-musl --no-default-features` for the real kernel TUI. It stages the layout under `rootfs/`, copies `aios-init` to `/init` and `aios` to `/system/aios-core`, then packs `find . | cpio --null -ov --format=newc | gzip -9`. A cleanup guard refuses to remove any path outside the script directory; `--keep-rootfs` retains the staging dir. When `/system/aios-core` is present, `aios-init` boots straight into the full kernel TUI; the rescue shell remains the fallback only (v2.13.0).

### Live image variant (historical, removed in v2.77.0)
The retired `live/build.sh` used to pack the aios-init initramfs by default: `aios-init` as `/init`, the `aios` binary as `/system/aios-core`, busybox only as the rescue shell — the kernel booted directly into the kernel TUI without a squashfs root; a dedicated GRUB menu with `init=/init console=tty0` entries was written. The legacy busybox initramfs (mounts the squashfs + `switch_root`, `init.rs`) was kept behind the `USE_BUSYBOX_INIT=1` opt-out (v2.14.0; aios-init was opt-in via `USE_AIOS_INIT=1` in v2.13.0). This path is no longer shipped; see the retired Layer 7 note above.

### Kernel command line
- GRUB: `menuentry "AIOS" { linux /boot/vmlinuz init=/init console=tty0 quiet; initrd /boot/initramfs.cpio.gz; }`
- Syslinux: `LABEL aios\n KERNEL /boot/vmlinuz\n APPEND init=/init console=tty0 quiet\n INITRD /boot/initramfs.cpio.gz`
- `init=/init` tells the kernel to run the binary instead of `/sbin/init`; `console=tty0` routes kernel + init output to the primary console.



## System Control Plane (`aios-sys-control`) - v2.29.0
Shared system-control plane consumed by the TUI, GUI, bridge and integration tests:
- `net_manager` - Wi-Fi management over two backends (`WifiBackend::Simulated` ether with 3 networks / `WifiBackend::Host` driving `netsh wlan`), DHCP Discover/Request/Ack wire codec, persisted lease, `SysControlHub` aggregating net + layout + power into a serializable `SysStatusSnapshot` with `status_line()`.
- `input_i18n` - EN/RU input-layout manager: hotkeys (`alt_shift` default), per-window overrides, active-first `status_segment()`.
- `power_mgr` - battery/thermal sampling (`Mock` or host sysfs) + `ThermalGovernor` hysteresis 80/70 C that offloads the LLM to cloud (`Groq`) when hot and returns it to local when cool.
- `keyring` - `KeyringVault`: AES-256-GCM secrets in redb, canary-checked master password, PBKDF2-HMAC-SHA256 120k rounds, TEE-bound sealing key, master re-key.

Kernel v2.29.0 adds milestones 3 and 4 to the bare-metal track: `sched.rs` - PIT-tick round-robin scheduler (switch every TIMER_HZ/4 ticks) doing frame-copy context switches inside the timer ISR, plus a ring-0 worker; `user.rs` - two ring-3 demo programs (raw machine-code loops over `int 0x80`) mapped user-mode at CODE_BASE 0x40000000 / STACK_TOP 0x7F000000; `ipc.rs` - per-pid mailboxes (MAX_PID=4 x MAILBOX_DEPTH=16) behind the DPL-3 `int 0x80` gate (IDT flags 0xEE) with a packet header mirroring `aios_core::ipc_protocol`; runtime proof lines `[stats] switches/sent/recv` every 5 s, asserted by `scripts/qemu-smoke.ps1`. Since v2.38.11 the `SYS_SEND` destination is the target's **scheduler slot** (worker = 1, tasks = 2/3/4 in spawn order, plus the GUI client = 5 since v2.38.25) — programs send to peer-slot constants `SLOT_A`/`SLOT_B`, not `PID_*`, and the mailbox counters/`mailbox_len` feed the on-screen dashboard. The gate grew to six calls (`SYS_SEND`/`SYS_RECV`/`SYS_WRITE`/`SYS_GETPID`/`SYS_SLEEP`/`SYS_GUI`), `MAX_TASKS`/`MAX_PID` went 5 → 6 in v2.38.25 for the fourth ring-3 demo task, and `SYS_GUI` (id 6) is the window-server entry point (CREATE/PRESENT/GET_EVENT, see the `gui` bullet). Since v2.46.0 the gate also serves `SYS_PING` (id 7, the cooperative hang-watchdog heartbeat) and `SYS_FAULT_POLL` (id 8, which drains the kernel fault event ring for a ring-3 monitor — the freestanding twin of the host `FaultReport` IPC). Since v2.49.0 it serves the `SYS_SOCK_*` family (ids 9–18: `SYS_SOCKET`/`BIND`/`CONNECT`/`SEND`/`RECV`/`CLOSE`/`STATUS`/`LISTEN`/`ACCEPT` — the last two from v2.50.0 — and `SYS_SOCK_ADDR`, v2.51.0, which hands ring-3 its own bound IPv4), the ring-3 face of the kernel socket layer; every buffer argument is validated across its full range through `memory::translate_user` (the only `int 0x80` user-pointer gate) before being copied byte-by-byte by `user_read`/`user_write`, invalid buffers return `u64::MAX`, and each call is gated on socket ownership (`sock::owner(fd) == pid`). Since v2.51.0 the fifth ring-3 demo task (program E) exercises the whole family from userspace over loopback — a UDP ping-pong plus, since v2.52.0, a TCP `listen`/`connect`/`accept`/echo round per loop (`sock-e ok` / `sock-t ok`; see the `user` bullet).
## Native UEFI Boot Chain (`aios-boot`, `aios-bootinfo`, `aios-config`, `aios-wasmhost`, `aios-easylang`, `aios-image`, `aios-bootstore`, `aios-storectl`, `aios-install`, `aios-sbctl`) — v2.76.0 (Secure Boot since v2.79.0)

AIOS boots on bare metal without Linux, Windows or a third-party boot loader:
`aios-boot` is its own UEFI application, `aios-bootinfo` is the loader/kernel
contract, `aios-image` writes the disk image from scratch on the build machine,
and `aios-install` copies that validated image to a disk. UEFI-only (GPT + ESP,
no BIOS/El Torito path in v1).

- `aios-boot` (`boot/aios-boot`, target `x86_64-unknown-uefi`, `uefi` 0.41 with
  `alloc`/`global_allocator`/`panic_handler`) produces one PE32+ image,
  `\EFI\BOOT\BOOTX64.EFI` (~64 KiB). The crate is split so most of it is
  host-testable:
  - `config` — parses `\aios\boot.conf` (`kernel`, `config`, `block` keys) and
    discovers the drop-ins in `\aios\config\*.conf` and `\aios\blocks\*.wasm`
    (`BootConfig::discover`): bare file names only, extension filtered,
    de-duplicated against the explicit entries with an ASCII case-insensitive
    compare (the ESP is FAT32) and sorted by name so the boot is deterministic;
  - `elf` — validates and loads the static `ET_EXEC` kernel (`PT_LOAD`
    segments, `p_offset`/`p_vaddr`/`p_memsz`, zero-filled `.bss` pages);
  - `paging` — own `FramePool` and 4-level tables: kernel image at
    `0xffff_ffff_8000_0000` in PML4[511], HHDM
    (`0xffff_8000_0000_0000`) in PML4[256], kernel heap/MMIO in PML4[510],
    the 64 KiB boot stack (`0xffff_fe00_0000_0000`) in PML4[508]; 2 MiB and
    4 KiB mappings with `PTE_NX`/`PTE_PCD`;
  - `BootError` — one error enum shared by every loader step.
  UEFI-only modules: `main` (GOP framebuffer, `ExitBootServices`, `BootInfo`
  fill), `framebuffer`, `serial` (COM1) and `runtime` (the `wcslen` libcall
  that LLVM's loop idiom emits for `CStr16::from_ptr`).
- Hand-off: the final `mov cr3` + `jmp` runs from a 4 KiB identity-mapped stub
  page. It loads the kernel page tables, sets `rsp = BOOT_STACK_TOP - 8` (the
  SysV entry alignment the kernel expects) and jumps to `_start` with
  `rdi = HHDM_BASE + phys(BootInfo)`; `cr3`, `cli` and the jump are the last
  things the firmware-provided environment sees.
- `aios-bootinfo` (`boot/aios-bootinfo`, `no_std`, used by both sides):
  `#[repr(C)] BootInfo { magic = AIOSBOOT, version = 1, size, hhdm_offset,
  kernel_phys/virt/size, rsdp, framebuffer, memory_region_count,
  module_count, memory_regions[128], modules[64] }`. Fixed layout, 8800 bytes,
  no pointers; `memory_regions()`/`modules()` return the valid prefixes and
  `is_valid()` re-checks magic, version and size.
- `aios-image` (`boot/aios-image`, pure Rust, no `mkfs.vfat`/`diskpart`):
  builds the loader and the kernel, stages `boot/assets` in `boot/out/assets`,
  compiles every `aios/easylang/*.easy` into `aios/blocks/<stem>.wasm` (M3.3)
  and drops the sources from the ESP, then writes a GPT disk with a 320 MiB
  FAT32 ESP (4 KiB clusters, 2 FATs, 32 reserved sectors, VFAT long names plus
  generated 8.3 aliases such as `EMPTY~1.WAS`) and re-parses MBR, GPT header,
  FAT32 boot sector and the required paths (`/EFI/BOOT/BOOTX64.EFI`,
  `/aios/kernel.elf`, `/aios/config/system.conf`, `/aios/blocks/empty.wasm`,
  `/aios/blocks/hello.wasm`).
  `--headless` boots the image under QEMU/OVMF, captures COM1 to
  `boot/out/serial.log` and prints it; `--no-run` only builds and verifies.
  Since M4.2 the initial store is materialised as real files in the staging tree
  through `aios-storectl::init` before `copy_tree` writes them to the FAT, and
  `verify_assets` checks that tree with `aios-storectl::verify`. `--reuse-assets`
  keeps an already-edited `boot/out/assets` instead of re-staging it, which is
  how the update/rollback QEMU scenarios rebuild the image without losing the
  new generation. Since M5.2, `--sign-keys DIR` Authenticode-signs the loader
  through `aios-sbctl` before it is copied to the ESP (keeping the signed copy
  at `boot/out/BOOTX64.signed.efi` and verifying it first), `--enroll-out DIR`
  emits the PK/KEK/db `.auth` payloads (requires `--sign-keys`), `--secure-fw`
  selects the OVMF Secure Boot code firmware and `--ovmf-vars PATH` selects a
  pre-enrolled NVRAM template.
- `aios-sbctl` (`boot/aios-sbctl`, `std`, standalone, excluded from the
  workspace, v2.79.0, M5.2): the host signing utility with `keygen`, `sign`,
  `verify` and `enroll` commands. It shells out to `openssl` for RSA keys,
  X.509 certificates, PKCS#7 signing and CMS verification; there are no
  external Rust dependencies and no `sbsign`. PE/COFF signing follows the
  edk2/`sbsign` Authenticode convention: `eContentType` =
  `SPC_INDIRECT_DATA_OBJID`, the digest covers the `SpcIndirectDataContent`
  body without its outer SEQUENCE header, `eContent` holds the whole DER value,
  and the blob is installed in the PE certificate table. Enrollment emits
  `EFI_VARIABLE_AUTHENTICATION_2` buffers with a 16-byte GMT `EFI_TIME`, a
  `WIN_CERTIFICATE` whose `wCertificateType` is `WIN_CERT_TYPE_EFI_GUID (0x0ef1)`
  (the PE-only `0x0002` makes EDK2 reject PK with `EFI_SECURITY_VIOLATION`), an
  `EFI_CERT_TYPE_PKCS7_GUID` certificate and a detached CMS signature over
  edk2's `(VariableName, VendorGuid, Attributes, TimeStamp, Payload)`; the
  payload itself is an `EFI_SIGNATURE_LIST` with `EFI_CERT_X509_GUID` and the
  DER certificate. 16 unit tests cover DER/PE parsing, signing layout and
  enrollment serialization.
- Stack alignment invariant: every kernel stack top must be 16-byte aligned.
  `[u8; N]` statics only have alignment 1, and `core::fmt::Arguments` marks its
  "static `&str`" encoding with `args & 1`; an odd argument-array address is
  decoded as a multi-exabyte string. `sched::stack_top()` rounds stack tops
  down to `STACK_ALIGN` and `spawn_kernel`/`spawn_user` assert the invariant.
- `aios-config` (`boot/aios-config`, `no_std`, allocation-free, v2.70.0): the
  one `key = value` reader both sides use — `#` comments, trimmed keys of ASCII
  letters/digits/`. _ -`, optional `"`/`'` quoting, empty values allowed, a
  malformed line is skipped, and a repeated key is last-one-wins. `Config` only
  borrows the text and re-parses it lazily (`entries`/`get`/`get_u32`/
  `get_bool`/`count`/`duplicates`), and `parse_u32`/`parse_bool` decode a bare
  value (decimal or `0x` hex, `true/yes/on/1` and `false/no/off/0`), so the
  kernel never copies or allocates a configuration file.
- Configuration layers (M2, v2.70.0): `boot.conf` only bootstraps the boot;
  every other `\aios\config\*.conf` is discovered and handed over as
  `MODULE_CONFIG`, and every `\aios\blocks\*.wasm` as `MODULE_BLOCK`. The
  kernel's `config` module (`aios-kernel/src/config.rs`) parses each layer in
  place through the HHDM, logs `[cfg] layer '<name>' = N key(s)`, every
  `key = "value" (layer, effective|informational)` pair and any duplicate key,
  and resolves a lookup last-to-first, so a drop-in such as `config/net.conf`
  overrides the base `system.conf` without editing it. `KeyPolicy` and
  `policy(key)` declare the policy: only `blocks.<name>.rights` and
  `blocks.<name>.fuel` are `Effective` (consumed by the WASM host);
  `system.*`, `boot.*`, `net.*` and unknown keys are `Informational` — parsed,
  validated and logged with their source, never applied as runtime knobs.
  `validate_informational()` checks `boot.debug`/`net.dhcp` as booleans and
  `net.mtu` as a positive integer and warns about invalid values; their count is
  reported in the boot summary as `Summary.invalid_values` alongside the layer
  and entry counts. Timer, heap and version deliberately have no config keys:
  `interrupts::TIMER_HZ`, `heap::HEAP_SIZE` and `aios-kernel/src/version.rs`
  (`SEMVER` = `env!("CARGO_PKG_VERSION")`, `KERNEL` =
  `concat!("AIOS kernel v", env!("CARGO_PKG_VERSION"))`) are compile-time facts
  from `aios-kernel/Cargo.toml`, the single source of truth; the boot log prints
  the effective timer/heap values and the config line with those constants.
  The shipped templates in `boot/assets/aios/config/{system,net,blocks}.conf`
  carry the same policy comments, with identical copies under
  `boot/out/assets/aios/config/` (matching SHA-256); `net.conf` and
  `blocks.conf` are intentionally unlisted in `boot.conf` as the drop-in proof.
- `aios-wasmhost` (`boot/aios-wasmhost`, `no_std` + `alloc`, v2.71.0, M3.1):
  the sandboxed WebAssembly host for `MODULE_BLOCK`. `module` parses and
  statically validates the binary in place (no copy): `i32`-only signatures with
  at most 8 parameters and 1 result, one linear memory of at most 16 pages
  (1 MiB), at most 16 imports, 64 functions, 16 exports and 16 data segments;
  every opcode, branch depth, local/function index and `if`/`else` balance is
  checked, and the table, global, element and data-count sections are rejected
  instead of being ignored. `cap` is the capability gate: `Rights` (`log`,
  `exit`, `clock`, `yield`, with the same spelling as `*.conf`) plus
  `Token { block, rights, fuel }`. `interp` executes the validated body without
  recursing on the Rust stack: fixed call frames 16 deep, a 96-slot value
  stack, 64 locals and 64 control frames; every instruction spends one unit of
  fuel (`aios.log` costs `1 + len/64`) and every load/store is checked against
  the declared pages. The only host surface is `Env` (`log`, `now_ms`,
  `idle`) and the ABI imports `aios.log(ptr, len)`, `aios.exit(code)`,
  `aios.now_ms() -> i32` and `aios.yield()`; a token that does not grant the
  matching right turns the call into `Trap::Denied`. The crate has no hardware
  dependency: 42 unit tests and one doc test cover the parser, validator,
  rights, arithmetic, locals, loops/branches, calls, memory, fuel, start/exit
  and the traps, so the kernel can rely on it before M3.2 wires it in.
- Kernel block runner (`aios-kernel/src/blocks.rs`, v2.72.0, M3.2): called once
  from `_start` as step 6, right after `heap online` and before the GDT/IDT and
  the scheduler, so a block runs single-threaded and nothing can preempt it. For
  every `MODULE_BLOCK` it derives the stem from the module name
  (`blocks/hello.wasm` → `hello`), reads `blocks.<stem>.rights` (comma-separated
  `log`/`exit`/`clock`/`yield`, absent = no host access) and
  `blocks.<stem>.fuel` (absent = `DEFAULT_FUEL`) through the `config` layer,
  parses the image in place through the HHDM, refuses a block whose
  `required_rights()` the token does not cover, allocates one `Vec<u8>` of
  `pages × 64 KiB` from the kernel heap, instantiates it (data segments + start
  function) and calls the `run` or `_start` export with no arguments.
  `Env` is implemented by `SerialEnv`: `aios.log` becomes
  `[serial] [block] <name>: <text>`, `aios.now_ms` reads `interrupts::TICKS`
  at `TIMER_HZ`. The runner logs a proof line per block
  (`ran: entry 'run' (N page(s))`, `used X of Y instruction(s)`), turns
  `Trap::Exited` into a normal stop, counts skip/fail separately and never stops
  the kernel on a bad block. The stock image proves the whole chain with
  `blocks/hello.wasm` (discovered, not listed in `boot.conf`) printing
  `hello from aios-wasmhost`, and the policy comes from the discovered
  `config/blocks.conf`.
- `aios-easylang` (`boot/aios-easylang`, `no_std` + `alloc` library plus a host
  CLI, v2.73.0, M3.3): the EasyLang Core compiler, the source language for
  blocks. The language is an imperative `i32` DSL — `let`, assignment,
  `log "text"`, `exit`, `yield`, `if`/`else`, `while`, `and`/`or`/`not`,
  comparisons and arithmetic, `now_ms()`, `true`/`false`, `#` comments and
  newline/`;` separators. `lexer` and `parser` build a positioned AST;
  `codegen` runs two passes (analysis resolves names, chooses the imports and
  packs the strings; emission writes the sections) and targets exactly the
  `aios-wasmhost` subset: one exported `run`, only the host imports the
  program uses (in `log`, `exit`, `now_ms`, `yield` order), at most 16
  function-scoped variables, `while` as `block`/`loop`, `and`/`or` normalized
  to 0/1, and every string literal in one data segment on one page. A compile
  error carries `line:column`. See [`docs/EASYLANG.md`](EASYLANG.md) for the
  reference. The `aios-easylang` binary is the same compiler for the command
  line (`aios-easylang hello.easy [-o out.wasm] [--check]`). 21 unit tests
  compile, validate and run programs through `aios-wasmhost`.
- `aios-bootstore` (`boot/aios-bootstore`, `no_std` without `alloc`, v2.75.0,
  M4.1/M4.2): the persistent store format and commit policy shared by the loader
  and the image builder. Nothing in it touches a filesystem, so the whole
  contract is unit-testable on the host (26 tests). The store lives in `/aios/store`:

  ```text
  /aios/store/manifest.a    manifest copy A
  /aios/store/manifest.b    manifest copy B
  /aios/store/slot0/header  slot 0 commit record
  /aios/store/slot0/...     payload files of slot 0
  /aios/store/slot1/header  slot 1 commit record
  /aios/store/slot1/...     payload files of slot 1
  ```

  - `manifest` — a fixed 1000-byte little-endian structure: 40-byte header
    (`AIMS` magic, version, header size, `sequence`, `generation`,
    `active_slot`, `previous_slot`, slot count, `entry_count`) and 16 entries of
    60 bytes (48-byte NUL-padded module name such as `blocks/hello.wasm`,
    32-bit size, CRC-32, kind, slot, flags). The CRC32 covers the whole buffer
    with the checksum field treated as zero; names are validated (relative,
    no `..`/`\`/NUL) and must be unique ignoring case.
  - `slot` — a 32-byte header per slot: `AISL` magic, version, size,
    `SlotState` (`Empty`/`Staging`/`Active`/`Retired`/`Bad`), `sequence`,
    `manifest_crc` and `entry_count`, plus its own CRC32. A header only
    "matches" when it is bootable, carries the selected manifest's sequence and
    CRC, and counts exactly the manifest entries of its slot. The CRC binding
    means a header cannot vouch for a different manifest generation.
  - `Store::load(copy_a, copy_b)` picks the valid copy with the highest
    `sequence`; a single valid copy wins regardless of sequence, and a tie
    prefers copy A. `stale_copy()` always returns the copy a writer must
    replace (the lower sequence, or the invalid one), so an interrupted write
    can never destroy the newest manifest.
  - `UpdatePlan` (from `plan_update`/`plan_rollback`) carries the new manifest
    bytes, the target slot and the copy to overwrite. `staging_header`,
    `active_header` and `retired_header` derive the exact slot headers, with
    `plan_update` keeping the still-active entries in the manifest so a later
    rollback can point at them without rereading payloads.
  - Commit order (documented on the crate root): payload files → `Staging`
    header → `Active` header → stale manifest copy → `Retired` header for the
    old slot. A crash before the manifest write leaves the old generation
    valid; a crash after it leaves the new one valid.
- Store integration (M4.1):
  - `aios-image` writes the initial generation while building the image: slot 0
    receives every staged `*.wasm` block, slot 1 is left empty for the first
    live update, both manifest copies are identical at sequence 1, slot 0 has
    an `Active` header and slot 1 an `Empty` one. It then reads the store back
    (manifest, slot-0 header, CRC and entry count) before the image is accepted.
  - `aios-boot` reads `manifest.a`/`manifest.b`, selects the newest copy, tries
    the active slot first and falls back to `previous_slot` (the rollback path),
    and verifies the slot header plus the size and CRC32 of every payload file.
    A generation is accepted or rejected as a whole, so a partially copied slot
    is never run. When the store is valid it replaces `/aios/blocks` discovery
    (the store is the authoritative complete set, so mixing versions is
    impossible) and logs `+N from store`; `block = ...` entries from `boot.conf`
    are preserved. A missing or broken store is not a boot error: the loader
    logs the reason and falls back to the drop-in directory.
  - The stock QEMU/OVMF image proves the chain: `aios-image: verified store
    sequence 1, slot0 header 2 entries`, `aios-boot: store manifest sequence 1
    (copy 0), active slot 0, previous slot 1`, `store slot 0 verified at
    sequence 1 (2 block(s))`, `modules = 5 (3 conf, 2 wasm; ... +1 from store)`
    and the block still prints `hello from aios-wasmhost`.
- `aios-storectl` (`boot/aios-storectl`, `std`, standalone, v2.75.0, M4.2): the
  offline writer that turns an `UpdatePlan` into real ESP files. `StoreIo` is a
  tiny key/value trait over paths relative to the ESP (`aios/store/...`);
  `DirStore` implements it for a host directory, so the tool works on a mounted
  ESP, on `boot/out/assets` or on an in-memory `MemoryStore` in tests. Public
  operations: `init` (fresh generation in slot 0), `commit` (new generation in
  the inactive slot), `rollback` (switch the manifest back without copying
  payloads) and `verify` (repeat the loader's checks). `commit` follows the
  `UpdatePlan` order exactly — payloads → `Staging` header → `Active` header →
  stale manifest copy → `Retired` header of the old slot — and `--fail-after
  payloads|staging|active|manifest|retired` stops after any checkpoint to
  simulate a power loss. `verify` reports the sequence, both slot indices and
  every rejected slot with its reason, and the CLI exits non-zero when no slot
  is bootable. 11 unit tests cover all five interruption points, a corrupted
  payload falling back to `previous_slot`, a rollback, duplicate/invalid names
  and the alternating manifest copy.
- `aios-install` (`boot/aios-install`, `std`, standalone, v2.76.0, M5.1): the
  host-side installer that replaces the PowerShell flasher for the native UEFI
  image. `inspect` validates the protective MBR, both GPT header CRCs, the
  geometry (`first_usable = 34`, `last_usable + 32 + 1 = backup_lba`), the
  `AIOS ESP` entry and its FAT32 boot sector; `plan` refuses a target smaller
  than the image and the image file itself; `execute` writes 1 MiB chunks
  through separate read/write handles and compares every chunk before
  advancing, then re-inspects and fingerprints the target prefix. The CLI
  prints the plan by default and needs `--write --yes` to modify anything;
  `--list` probes `\\.\PhysicalDrive0..15` read-only on Windows. A physical
  disk must be unmounted first: `std` has no volume-lock API. The restored
  target boots under QEMU/OVMF byte-identically to the image. On Windows a raw
  disk is sized with `IOCTL_DISK_GET_LENGTH_INFO` (`SeekFrom::End` is invalid on
  block devices) and opened with `CreateFileW` using
  `FILE_SHARE_READ|FILE_SHARE_WRITE`; writes use a sector-aligned buffer and
  skip `flush`. v2.76.1 verified a real 31 GB Kingston USB stick: 336 609 792
  bytes written and read back with CRC32 `466BA1BB`, one 320 MiB ESP partition
  at offset 1 MiB.
- Commit invariant fixed in M4.2: `UpdatePlan::retired_header()` carries the
  **new** sequence and the new manifest CRC (not `sequence - 1`), because the
  loader validates a slot against the manifest it just selected. Without this
  the active slot could boot but the automatic `previous_slot` fallback failed
  its header check, so an update that broke the new payload bricked the block
  set instead of rolling back.
- QEMU/OVMF proof of the write path (each scenario rebuilds the image from the
  staged tree with `--reuse-assets`):
  - successful `update`: `store manifest sequence 2 (copy 0), active slot 1,
    previous slot 0`, `store slot 1 verified at sequence 2 (1 block(s))`, and
    the new `blocks/updated.wasm` prints `hello from aios-wasmhost`;
  - tampered payload in slot 1: `store slot 1 unusable: payload checksum
    mismatch`, `store slot 0 verified at sequence 2 (2 block(s))` and the old
    `blocks/hello.wasm` runs instead;
  - `update --fail-after staging`: the loader still sees sequence 1, active
    slot 0 and the original block. A separate CLI rollback switches back to
    slot 0 at sequence 3 with no payload copy.
- QEMU/OVMF Secure Boot proof (M5.2): with `--secure-fw` and an enrolled
  `--ovmf-vars` template, the image built with `--sign-keys` logs
  `aios-boot: entering kernel`, `[cfg] system = AIOS 2.79.0 x86_64 (3 layer(s),
  10 key(s), 0 invalid)` and `periodic 1000 Hz tick armed`. The same setup with
  an unsigned or tampered loader never enters the kernel (`kernel_entries=0`,
  `aios_boot_lines=0`) and OVMF reports `Access Denied` for `Boot0001`. The
  enrollment files pass detached OpenSSL CMS checks and are also accepted by a
  real OVMF `SetVariable` (`PK/KEK/db: OK`, NVRAM attr `0x27`); the signed image
  boots with that NVRAM. The physical stick was not rewritten.

## Bare-Metal Kernel (`aios-kernel`, `aios-kernel-run`) — Limine + GOP since v2.34.0

A fresh `x86_64-unknown-none` microkernel. Since **v2.34.0** it boots as a **Limine-protocol** ELF from a hybrid BIOS+UEFI ISO (the same Limine the Live image uses) and renders through a native **GOP/VBE framebuffer** console; the earlier `bootloader::BiosBoot` + VGA-text path was removed. It is the seed of a self-hosted kernel and provides console I/O, interrupts (GDT/TSS, IDT, PIC remap, PIT + Local APIC timers, PS/2 keyboard), paging (own page-table walker + frame allocator + kernel heap), preemption and kernel IPC.

Since **v2.78.0** the same crate has **two build-time-selected boot paths** (see the Native UEFI Boot Chain section above for the other one), selected by the optional Cargo feature `limine` (`default = []`):
- `--features limine` (used by `aios-kernel-run` for the hybrid BIOS+UEFI ISO/USB image): Limine resolves the requests, and the no-argument `_start` translates its responses into the shared `aios_bootinfo::BootInfo` before calling `kmain`. The `linker.ld` `KEEP`s the `.requests*` sections so the requests survive linking.
- no features (used by `aios-image` for the native UEFI GPT/ESP image): `aios-boot` supplies `BootInfo` in `rdi` to `_start(bootinfo: *const BootInfo)`, which also calls `kmain`.

`kmain`, `boot::init()` and every later subsystem stay loader-agnostic. On the Limine path `modules` and `kernel_phys/virt/size` are zero (no module payloads are staged); `config::init()` handles zero modules. The framebuffer address is converted from Limine's HHDM virtual address back to physical before `Framebuffer::new` adds the offset once.

- `aios-kernel`: `no_std`/`no_main` crate. Boot contract:
  - `.cargo/config.toml` links with `-Tlinker.ld`; `linker.ld` is the Limine higher-half script (base `0xffffffff80000000`, `ENTRY(_start)`, keeps `.requests_start`/`.requests`/`.requests_end`).
  - `main.rs`: with `--features limine` it declares the Limine request statics (`BaseRevision`, `EntryPoint(_start)`, `StackSize(64 KiB)`, `Hhdm`, `Memmap`, `Rsdp`, `Framebuffer`) and a no-argument `#[no_mangle] pub unsafe extern "C" fn _start() -> !`; `limine_bootinfo()` converts the responses into `BootInfo`. Without the feature, `_start(bootinfo: *const BootInfo)` is the unchanged `aios-boot` entry point. Since v2.78.0 both call the shared `kmain` and log the active loader via `LOADER_NAME`.
  - `serial` — COM1 polling UART driver (`outb`/`inb`), `kprintln!`.
  - `framebuffer` — `Framebuffer` over the Limine GOP/VBE buffer: `put_pixel`/`fill_rect`/`clear`/`scroll_up`/`read_pixel`, 2/3/4 bytes-per-pixel chosen from the Limine channel masks; `colors` palette. The base pointer is Limine's `address` verbatim — per the protocol it already carries the HHDM offset (`phys + hhdm`), so no extra translation is ever applied (v2.38.13 documents this on `base_addr()`). Since v2.38.13 `direct_test(color)` fills the whole panel strictly through `core::ptr::write_volatile`, addressing pixels by the physical stride (`pitch / bytes_per_pixel`), so the compiler cannot elide the stores and the scanline walk matches hardware layout even when `pitch != width * bytes_per_pixel`. Since v2.38.39 `fill_rect` fills row-wise through the private `write_row` (one `pack` per call, 4/3/2 bpp branch chosen once) instead of a naive per-pixel loop, and the module gained `fill_rect_vgrad` (two-stop vertical wash, one channel lerp per row) plus `get_color` (pixel readback used by the vector layer's blender).
  - `console` — spin-locked text console on the framebuffer (glyphs drawn through the font below, wraps at the screen width, scrolls); `vprintln!`. Since v2.38.10 it reserves the bottom `GLYPH_H` row as a fixed overlay strip; `scroll_up(pixels, fill, region_height)` is bounded to the console region so overlay pixels (heartbeat square, drawn by `idle_loop`) are never scrolled/ghosted. Since v2.38.21 the console only owns a compact top strip (`CONSOLE_TOP_ROWS` = 6 rows); the interactive TUI panel owns every row below it, so the boot log scrolls in its own small window and the panel is the screen only after the desktop is left. Since v2.38.22 `print`/`write_bytes` return early while `gui::active()`, so the windowed desktop is never garbled by kernel log lines (serial logging is unaffected); since v2.81.0 that desktop is the boot screen.
  - `font8x8` — vendored public-domain 8x8 bitmap font, `BASIC: [[u8; 8]; 128]` (Basic Latin). The `font8x8` crate 0.3.1 is `std`-only and cannot be used here.
  - `psf` — (v2.38.13) `no_std` parser for the Linux console-font formats: PSF1 (`36 04` magic, 256/512-glyph mode flag, width fixed at 8 bits, height from the header) and PSF2 (`72 B5 4A 86` magic; header size, glyph count, bytes/glyph, width and height as little-endian `u32`), plus a bounds-checked `glyph(index)` accessor over the bitmap area. `synth_psf2_basic()` builds a valid PSF2 stream from the embedded `font8x8::BASIC` glyphs so the whole parse/render path is exercised at boot without shipping a separate font binary.
  - `port` — `outb`/`inb`/`outl`/`inl`/`io_wait` helpers for x86 port I/O.
  - `acpi` — (v2.38.20) minimal ACPI table walk: RSDP signature/revision check, root system table (XSDT 8-byte or RSDT 4-byte entries) and FADT lookup. `init()` accepts the Limine RSDP pointer in either form — already HHDM-offset or raw physical — via `virt_addr()` (the offset must be applied exactly once); system-table entry slots and FADT fields always go through `memory::physical_to_virtual`. Publishes `acpi_found()`, FADT revision, DSDT physical address; the FADT is the liveness gate for the EC probe.
  - `ec` — (v2.38.20) ACPI Embedded Controller probe over the fixed 0x62/0x66 ports (the ACPI EC is not listed in the FADT): `read_ram(off)` point reads with every IBF/OBF wait bounded (up to 1,000,000 iterations so boards without an EC degrade cleanly), `dump_full()` reads all 256 RAM bytes, `probe()` logs the full dump in 16-byte rows. `active()` = FADT found and 16+ bytes read back.
  - `lid` — (v2.38.20) lid switch detection from EC RAM: an 8-byte scan window rings through the 256-byte space; the first bit flip pins a candidate offset+bit, later scans of the same bit confirm polarity. Live `lid_open()` value, `candidate_offset()`/`candidate_bit()` for MSI calibration, `lid_state()` (`--`/`?`/`0`/`1`) and `lid_detail()` (`1@0x..b`) feed the TUI. The RAM layout and open/closed polarity are board-specific (see BUGS).
  - `thermal` — (v2.38.20, honesty pass v2.68.4) CPU digital thermal sensor: gated on CPUID.06H:EAX[0] (leaf-1 EDX bit 22 as a fallback since v2.68.4), TjMax from MSR 0x1A2 bits 16:23 (falling back to a documented 100 °C when the register is unavailable or implausible), live temperature from MSR 0x19C (7-bit digital delta, `temp = TjMax - delta`); the status word's invalid-readout bit (31) and implausible deltas are rejected instead of showing a fake 0 °C/TjMax. Every RDMSR runs inside a **#GP-safe assembly probe** (`_aios_probe_rdmsr` / `_aios_probe_rdmsr_fault`): the vector-13 arm of the dispatcher redirects a fault to the fault label, so an MSR that raises #GP (QEMU advertises DTS but faults on the thermal MSRs) degrades to a synthetic 45..60 °C wave instead of halting. The synthetic value is marked `~` in the TUI status bar (`sim=1` in the System tab, `temp …c~ (simulated)` in the GUI System window) and never drives the gate; `critical()` (≥ `THRESHOLD_C` = 90 °C) pauses ring-3 tasks on real DTS readings only.
  - `ps2` — (v2.38.20) IRQ-free polled PS/2 driver over the i8042 (status 0x64 bit5 MOBF = aux / bit0 OBF = keyboard): `drain()` harvests up to 32 bytes per call, `init()` raises the AUX port (`CMD_ENABLE_AUX` 0xA8), switches the mouse to report mode (`SET_REPORTING` 0xF4) and reads its ID over `GET_ID` 0xF2 (the ACK is drained separately so the stored ID is real: 0x00 PS/2, 0x03 IntelliMouse, 0x04 five-button). 3-byte packets decode with sign-extended `sign9` dx/dy (y inverted); keys reuse `scancode_to_char`. Since v2.38.23 `feed_key` held a one-key `EXT` latch; since v2.83.0 it swallows only the 0xE0/0xE1 prefix and queues the following make code (Ctrl bytes only update the Ctrl flag), so Win/arrow/Fn payloads reach `gui::handle_scancode()` through the shared FIFO and `idle_loop()` no longer keeps a second extended-state latch; since v2.38.36 `feed_key` pushes each surviving byte into the shared 256-slot keyboard FIFO (`interrupts::key_push`) together with the IRQ1 path, so polled and IRQ-fed bytes queue in arrival order. Atomically publishes `KEY_SEQ`/`KEY_SCANCODE`, `MOUSE_SEQ`/`MOUSE_DX`/`MOUSE_DY`/`MOUSE_BUTTONS`, `MOUSE_ID`; since v2.38.38 `decode_packet` additionally latches the button press edge (`prev & !cur & 0x07`) into `MOUSE_BTN_EDGE`, consumed by `take_button_edges()` (swap to zero), so a press+release pair coalesced into one `drain()` still surfaces a press to the main loop (BUGS v2.38.38). Since v2.38.30 the mouse deltas are *accumulated* (`fetch_add` in `decode_packet`) and drained by the reader (`mouse_dx`/`mouse_dy` swap to zero): the PS/2 controller splits movements larger than one packet, and the old store-per-packet published only the newest chunk when several packets arrived between two input polls (see BUGS v2.38.30). Since v2.38.35 `decode_packet` also **drops whole packets whose 8042 overflow bits are set** (`b0 & 0xC0 != 0` — the controller already discarded that movement; decoding the wrapped sign bits produced multi-hundred-pixel jumps) without bumping `MOUSE_SEQ` or publishing the button nibble, and the reader's drain goes through `drain_axis`: one application is clamped to `MOUSE_MAX_STEP = 256` px per axis with the remainder left in the accumulator (`fetch_add` back, applied on later iterations — see BUGS v2.38.35).
  - `crt` — **removed in v2.36.0**: the strong `memcpy`/`memmove`/`memset`/`memcmp`/`bcmp` this module defined produced a circular PLT for `memset` and hung the boot (see BUGS); the kernel now relies on `compiler_builtins`' weak `mem*`.
  - `pci` — PCI configuration-space access over the legacy `0xCF8`/`0xCFC` ports; full bus/device/function enumeration into a fixed array of `PciDevice` records (ids, class/subclass/prog-if, BARs, IRQ) with `class_name()` decoding — the discovery layer for the AHCI/NVMe and xHCI drivers.
  - `ahci` — AHCI (SATA) block driver (v2.36.0): enables PCI memory space + bus-mastering, resets the HBA (`GHC.HR`/`AE`), builds per-port command-list / FIS-receive / command-table frames with `memory::alloc_frame`, starts each implemented port, reads `PxSIG` after start (skipping ATAPI/SEMB/PM), issues `IDENTIFY DEVICE` (word-swapped model + 28/48-bit capacity) and moves sectors with `READ`/`WRITE DMA EXT` over a PRDT entry; `AhciDrive`/`read_sectors()`. The controller BAR is reached through `memory::map_mmio`.
  - `nvme` — NVMe block driver (v2.37.0): enables PCI memory space + bus-mastering, maps the 64-bit BAR0 through `memory::map_mmio`, resets the controller (`CC.EN=0`), programs `AQA`/`ASQ`/`ACQ` and waits for `CSTS.RDY`; issues `Identify Controller` (ASCII `MN`/`SN`), `Create I/O CQ`/`SQ`, `Identify Namespace` (NSZE + LBA format) and polled `Read` commands (PRP1 into one DMA frame); `NvmeDrive`/`read_blocks()`.
  - `xhci` — USB 2/3 dual-mode host controller driver: enables the PCI function, finds the capability/operational/runtime/doorbell registers via `CAPLENGTH`/`RTSOFF` (v2.38.12 reads these from `base+0x18` and masks, not from the HCIVERSION-derived offset), programs the DCBAA + scratchpad + event/command/EP0/EP1 rings and raises the first boot keyboard on the root ports (port reset → Enable Slot → Address Device → EP0 control transfers → boot protocol → Configure Endpoint for the HID interrupt-IN pipe). No MSI yet: the scheduler idle loop polls the event ring (`xhci::poll`) to harvest 8-byte HID boot reports and re-arms the endpoint; a key is published as `KEY_SCANCODE` (PS/2 set-1 make code) + a `KEY_SEQ` bump for press detection, and `poll`/`idle_loop` decode it with `scancode_to_char` (`usb key '<c>' (0x..)`). Since v2.38.14 the per-poll/per-event diagnostics are gated behind the F8 `DEBUG_MODE` (`dbg_kprintln!`), keeping a normal boot quiet while the one-time bring-up readbacks stay logged; the USB → HID → scancode → char path is verified end-to-end in QEMU via monitor `sendkey` (`usb-kbd`); since v2.38.40 `usage_to_scancode` covers the full boot-keyboard set (letters/digits/punctuation tail with the `0x2F`..`0x39` slots corrected, F1–F12, Caps/Scroll/Num Lock, keypad `/ * - +`/digits/dot via `KP_SCANS`; since v2.83.0 navigation usages `0x49..=0x52` map to Set-1 Home/End/Page/arrow codes so Start and in-kernel app lists work on USB keyboards) and unmapped usages are logged as `[xhci] usb key usage 0xNN unmapped` behind `DEBUG_MODE`. Since v2.38.15 a HID boot-mouse (interface protocol 2, e.g. QEMU `usb-mouse`) is raised on a second slot/root port (the keyboard's port is skipped) with its own interrupt-IN ring — `poll` harvests both slots off the shared event ring and publishes sign-extended `MOUSE_DX`/`MOUSE_DY` plus `MOUSE_BUTTONS` with a `MOUSE_SEQ` bump per report (since v2.38.38 `harvest_mouse` also latches the press edge `prev & !cur & 0x07` into `MOUSE_BTN_EDGE`, consumed by `take_button_edges()`) (since v2.38.30 it additionally `fetch_add`s into accumulators drained by `xhci::take_mouse_delta`, so back-to-back reports between two input polls sum instead of overwriting — since v2.38.35 that drain clamps one application to `MOUSE_MAX_STEP = 256` px per axis and parks the remainder back in the accumulator, mirroring `ps2::drain_axis`; the plain `MOUSE_DX`/`MOUSE_DY` stay latest-report for the HID diagnostics tab), which the idle loop turns into a clamped 8x8 arrow cursor on the framebuffer (`mouse-seq` is shown in the TUI driver row).
  - `nic` — (v2.38.34, expanded v2.48.0) wired ethernet drivers selected by an **exact PCI vendor/device-id table** (class 0x02, non-Wi-Fi), not vendor alone: `driver_kind()` maps to a six-variant `Kind` and `Nic::init` brings up the first **supported** part, logging `nic <kind> <vid>:<did> mac=…` or `nic unsupported <vid>:<did> (<reason>)` for the rest. Backends: **e1000** and **e1000e**/`igb` (shared MMIO descriptor-ring core, `STATUS.LU`), **rtl8139** (PIO linear RX ring, `BMSR` link), **rtl8169**/8168/810x (MMIO 16-byte descriptors, `OWN` in `opts1`, `Cfg9346` lock, `TPPoll.NPQ` kick, `PHYstatus` link), **virtio-net legacy** (`0x1AF4:0x1000/0x1001`, I/O BAR, split virtqueues with the used ring at the next 4 KiB boundary, `VIRTIO_NET_F_MAC`) and **AMD PCnet** (`0x1022:0x2000/0x2001`, 16-bit I/O, `SSIZE32` init block `BCR20=0x0102`, `TDMD` kick). All are polled (no interrupts): every backend exposes `link_up`/`poll_rx`/`send` and carries a `kind`, `qsize` and shared `rx_pos`/`tx_pos` cursors (the e1000 RA table copy writes each MAC as two little-endian `u16`s — v2.38.34 fixed a 4-into-2-byte slice panic on the first received frame).
  - `net` — (v2.38.34, hardened v2.48.0) the network stack: uplink selection (`uplink_mac`/`uplink_send` prefer an associated Wi-Fi station, else the wired NIC, else `nic: none (wifi uplink only)`), DHCP client, ARP cache, ICMP echo, DNS query and the `State` machine (`no-nic` → `checking` → `link-only`/`internet`) probed periodically and on demand (`net::recheck` — the GUI `Test` button); `cmos_save`/`cmos_load` persist the settings across boots and `net::apply` (the GUI `Apply`) validates all five dotted quads before committing. Since v2.48.0 the client is robust: DHCP handles **NAK** (restart discovery, or drop the lease after a rebind NAK), **renews** at T1 (unicast REQUEST) and **rebinds** at T2 = 7/8 of the lifetime (broadcast REQUEST without server id), with a **static fallback** when DHCP exhausts its retries; `net::poll` performs **link-change recovery** (a down→up / up→down edge clears `NET_OK`, drops to `Phase::Idle`, resets the ARP cache and restarts acquisition); the ARP cache is **ageing** (`ARP_TTL` = 5 min, evicted by `arp_expire`) and the reachability recheck backs off exponentially (capped at 60 s) after consecutive failures. Polled every tick from `main`'s idle loop alongside `wifi::poll`. Since v2.49.0 it also hosts the routing helpers the socket layer uses (`now_ticks`, `bound_ip`, `next_hop`, `arp_resolve`, `emit_ipv4`, `l4_checksum`) and dispatches inbound L4 payloads to `sock` (`PROTO_TCP`/`PROTO_UDP` → `sock::deliver_tcp`/`deliver_udp`), with `net::poll` ticking `sock::tick` each call. Since v2.50.0 `emit_ipv4` routes frames addressed to the bound IP / `127.0.0.1` into a **loopback ring** (`LOOPQ`, 16 × 1472 bytes) drained by `drain_loopback` on the next poll — so kernel servers and clients can talk on one machine without a NIC, and deliveries never re-enter the socket state while the sender still borrows it. Since v2.56.0 the ring also carries the frame's destination address (`LOOP_DST`) and loops `255.255.255.255`, subnet‑directed broadcasts and class‑D multicast frames back, so the socket layer can fan a datagram out to every member on the destination port.
  - `sock` — (v2.49.0, server side v2.50.0) the kernel IPv4 **socket layer**: a fixed table of `MAX_SOCKS = 16` descriptors (`static mut SOCKS`), each either a connectionless **UDP** endpoint (a 3-slot datagram ring, MTU 1024) or a **TCP** socket covering both directions: a minimal client (active open: `SynSent` → …) and a minimal server (`listen` → `Listen`, a SYN spawns a child in `SynRecv` answered with a SYN-ACK, the client's final ACK promotes it to `Established`, and `accept` hands the child out; states `Closed`/`Listen`/`SynRecv`/`SynSent`/`Established`/`CloseWait`/`FinWait1`/`FinWait2`/`LastAck`/`TimeWait`). The API is `open`/`bind`/`listen`/`accept`/`connect`/`send`/`recv`/`close`/`status`/`owner`, plus the inbound hooks `deliver_udp`/`deliver_tcp` (called from `net`) and `tick` (retransmit/`TIME_WAIT` timers). TCP is deliberately simple — stop-and-wait over an 8 KiB transmit queue (a send larger than `TCP_MSS` is queued whole and segmented, one segment in flight, the next kicked by each ACK that completes the in-flight one), an 8192-byte RX ring, 4 retransmits at 50 tick spacing, advertising a fixed `TCP_WINDOW = 64240` (no window scaling; only
`cwnd`/`ssthresh` congestion-control accounting since v2.54.0); UDP ports
match on the bound local port and require the reply to come from the connected peer when `connect` was called. Since v2.56.0 the UDP side also covers broadcast and multicast: connecting to `255.255.255.255` arms a broadcast member and connecting to a class‑D address (224.0.0.0/4) joins that multicast group, and `deliver_udp` (which now receives the frame destination) fans broadcast/group datagrams out to **every** member socket on the destination port while unicast delivery still matches exactly one peer. Idle ports are allocated from `49152..60000`. `MAX_SOCKS` was raised from 8 to 16 (v2.55.0), enlarging the socket table and allowing more concurrent connections. The ring‑3 face is the `SYS_SOCK_*` syscall family (ids 9–18, see `syscalls`), and the paths are exercised from the TUI shell (`udp`/`http`/`socktest` — the latter also drives a 3000-byte segmented bulk transfer with reassembly verification) and, since v2.51.0, directly from a ring-3 demo task that runs a loopback UDP ping-pong plus (since v2.52.0) a TCP `listen`/`connect`/`accept`/echo round over the syscalls (`sock-e ok` / `sock-t ok` + the `[sock]` serial lines prove both vertical slices end to end).
  - `user` — the ring-3 demo programs (raw machine code over the `int 0x80` gate, each mapped at `CODE_BASE + pid × 2 MiB` with a 4-page stack): A/B ping-pong over the per-slot mailboxes (`SYS_SEND`/`SYS_RECV`, proof strings `u1`/`u2`), C the sleeper (`SYS_SLEEP` rounds), D the GUI client (`SYS_GUI` fill/key/click), and E (v2.51.0) the socket client — since v2.52.0 a **full socket round per loop**: two UDP sockets ping-pong `ring3`/`ack!` over the loopback ring, then a TCP client `connect`s to a listening socket, `accept` hands out the promoted child, the pair echoes `tcp`/`ok`, and the child is reclaimed by a double `SYS_SOCK_CLOSE` (the first close only sends a FIN and leaves the slot in `FinWait2`); verdicts `sock-e ok` / `sock-t ok` via `SYS_WRITE`. Fds persist in r8d/r9d (UDP) and r12d/r13d/r14d (TCP), the IP snapshot in ebx — all preserved by the kernel gate; every failed handshake step retries after a short sleep. The fifth demo is why `MAX_TASKS`/`MAX_PID` are 7 (demos occupy slots 2..6).
  - `wifi` — (v2.38.34, honest since v2.38.36) Wi-Fi station + simulated AP over a **test radio**, explicitly marked sim. Since v2.38.36 `wifi::init(&pci_devices)` detects a real controller via `pci::is_wifi` (network class `02`, subclass `80`) and reports `[wifi] card: <bdf>` / `card: none`; the default radio is **real** (`radio: real (test radio off, shell: wifi sim)`), a real scan without a card is refused (`[wifi] scan: no wifi card`) and an empty result yields the `no networks found` hint; the simulated radio arms only through the shell commands `wifi sim` / `wifi real` / `wifi` (`set_sim` toggles `SIM_ON`, logged as `test radio enabled (sim, 3 APs)` / `test radio disabled`), which is how the B/C smokes arm it and F proves it off. The sim offers three built-in APs (`AIOS-Test` open, `SecureNet` WPA2-PSK passphrase `AIOS-Test`, `Neighbor` open), wildcard probe scan (beacons/probe responses into up to 8 results), open/WPA2 association with the full 4-way handshake — PBKDF2-SHA1 (4096) PMK, PRF-512 PTK, RFC 3394 AES key wrap for the GTK, EAPOL M1–M4 with HMAC-SHA1 MIC, AES-CCM (CCMP) data encryption — and an AP-side LAN on `10.0.9.0/24` that answers DHCP/ARP/ICMP/DNS (`ap dhcp offer/ack -> 10.0.9.15`) so `net`'s probes and `state internet` run over real traffic. Three lock-free frame queues (`TXQ` STA→AP, `RXQ` AP→STA, decrypted `ETHQ` to `net`) are pumped 8 frames per tick by `wifi::poll`, which also times out scans (`SCAN_TICKS`) and association attempts (`CONNECT_TICKS`); the station MAC is `02:00:00:00:00:AA`.
  - `gdt` — 7-entry GDT (null, kernel/user code+data, 64-bit TSS with a dedicated double-fault IST stack); `Descriptor` is `#[repr(C, packed)]` so `lgdt` reads the base from the correct offset. `reload_segments` (v2.38.12) reloads SS to `0x10`, far-returns into `CS=0x08` through this GDT and refreshes DS/ES/FS/GS to `0x10` — Limine hands off the CPU with *its own* GDT selectors (`cs=0x28/ss=0x30`), and until CS/SS were reloaded the first `iretq` re-validated `cs=0x28` against AIOS's GDT, which holds the TSS descriptor in slots 5/6 → `#GP` (see `BUGS.md`); `ltr` installs the TSS.
  - `idt` — 256-entry IDT built from the assembly-generated `aios_handler_table` (vector stubs in `.text`, table relocated at load); gate 8 uses the double-fault IST.
- `interrupts` — `aios_interrupt_common` C shim (`aios_handle_interrupt`) dispatches by vector: fatal faults (0/6/8/13/14) print and halt (since the v2.84 diagnostic update the ring-0 `#PF` arm uses `fatal_fmt` to paint `PAGE FAULT <class> addr/ip/err/rsp` on the framebuffer, with `pf_kind` decoding the error-code bits, so a crash is diagnosable without COM1; ring-3 `#PF` goes to `fault::intercept`), IRQ range `0x20..=0x2F` is PIC-EOI'd (IRQ0/PIT bumps `TICKS` at `TIMER_HZ` (1000 Hz since v2.78.2), IRQ1/keyboard `key_push`es the raw byte — since v2.38.36 into a 256-slot lock-free ring `KEY_FIFO` with atomic HEAD/TAIL instead of the old single `LAST_SCANCODE` slot, so a 1–2 s idle-loop stall can no longer overwrite un-consumed make/break codes; `ps2::feed_key` shares the same FIFO), and the dedicated LAPIC vector `0x90` (v2.38.12) bumps `TICKS`, draws the timer bar, LAPIC-EOIs and feeds `sched::tick` like the PIT path (its first firing logs the saved frame `cs/ss/rip` once). Since v2.38.20 the vector-13 arm first checks `thermal::probe_active()` and hands a probing RDMSR fault to `thermal::on_probe_gp` (rewrites the frame RIP to `_aios_probe_rdmsr_fault`) instead of halting. `init_pic` remaps the 8259 PIC to `0x20`/`0x28` and masks all but IRQ0/IRQ1; `init_pit` (mode 2, rate generator, since v2.68.3) programs channel 0 for `TIMER_HZ`. `set_pit_masked(true)` masks PIT IRQ0 once the LAPIC timer is live, keeping a single hardware tick source. `pit_count`/`delay_ms`/`wait_for_irq32` drive the same countdown directly (no IRQ involved) for the software-tick fallback.
- `lapic` — (v2.38.12) Local APIC timer driver, the primary hardware tick source on any board: prefers x2APIC (IA32_APIC_BASE + the extended MSR range), falls back to xAPIC MMIO mapped via `memory::map_mmio`, calibrates `bus_hz` against the PIT countdown (`bus_hz = lapic_delta * 1_193_182 / pit_delta`, reload-aware via `pit_elapsed` since v2.68.3) and arms a periodic `TIMER_HZ` tick on vector `0x90` (1000 Hz since v2.78.2). `active()` feeds the dashboard mode (`HW-LAPIC`). Works with no 8259 PIC at all.
- `sched` — tick-driven round-robin over `MAX_TASKS = 7` trap frames (slot 0 = idle, worker = 1, five ring-3 demos in slots 2..6). Ring-3 tasks are preemptive (switched at any hardware timer IRQ — PIT or LAPIC); ring-0 tasks switch only cooperatively via `yield_kernel` (`int 0xfa`), guarded by the `hw_tick` predicate which also covers vector `0x90`. Before v2.38.12, boards whose legacy PIC never delivered IRQ0 (UEFI IO-APIC routing) had **no preemption interrupts** — a ring-3 task that never syscalls yielded the tick economy to nobody — so demo programs keep sleeping (`SYS_SLEEP`) forever and must never end in a busy loop; the LAPIC timer restores preemption on those boards. `idle_loop` still detects no-hardware-ticks (`IRQ32_SEEN == false`) and synthesizes the `TIMER_HZ` cadence by polling the PIT countdown. Since v2.38.20 `schedule()` also applies a thermal gate: while `thermal::critical()` is set (real DTS readings only since v2.68.4) it skips ring-3 tasks (`frame.cs == USER_CS`), so demo tasks pause on a hot die while kernel and idle tasks still run and the TUI keeps refreshing. Since v2.45.3 the outgoing-context refresh also drives the **hang watchdog**, made progress-aware in v2.46.0: per-slot `STALL_SINCE`/`STALL_RIP` track when each ring-3 task's saved `rip` last changed, and per-slot `LAST_ACTIVITY` (bumped by every `int 0x80` through `sched::note_activity`, also the `SYS_PING` heartbeat) tracks its last syscall. A task that has **both** an unchanged `rip` **and** no syscall since the stall anchor for `HANG_WATCHDOG_TICKS` (10 s) is handed to `fault::record_hang` and frozen — a genuine infinite spinner is caught without a CPU exception, while bounded buffer-fill loops (which move their `rip`) and syscall-busy tasks never false-positive. `task_snapshot(pid)` exposes a task's saved frame (sealed `TaskSnapshot`) for the Resume integrity gate.
  - `memory` — paging core over the Limine HHDM (`physical_offset = hhdm.offset`). `translate` walks the active page tables (PML4/PDPT/PD/PT, huge pages) reading physical memory through the HHDM; `map_page`/`unmap_page` edit tables and allocate page-table frames on demand, flushing the TLB with `invlpg`; `init` builds a bump frame allocator over the Limine `MEMMAP_USABLE` regions supplied as `MemRegion { start, end }`; `selftest` maps a scratch page, writes/reads a pattern and verifies `translate` before and after unmap. `verify_region(virt, bytes)` (v2.38.13) walks the live tables and reports a `RegionCheck { pages_total, pages_present, pages_writable }`, counting huge-page leaves per 4 KiB sub-page with the leaf's own flags — the boot path uses it to prove the whole GOP VRAM range is mapped. `map_mmio(phys, size)` maps device BAR pages into a dedicated window at `0xffffff00_2000_0000` (PML4 slot 510) — the HHDM covers RAM only; `physical_to_virtual(phys)` returns the HHDM alias used for DMA buffers. Ring-3 syscall buffers use a separate gate: `USER_SPACE_LIMIT` and `user_range(virt, len)` reject null (for non-zero lengths), higher-half/canonical violations and `virt + len` wrap, while `translate_user(virt, len, write)` walks every touched page with `walk_user_leaf()` and requires `PRESENT`/`USER` at PML4/PDPT/PD/leaf (plus `WRITABLE` on writes, including huge pages). `translate` itself stays for kernel/driver/DMA addresses only.
  `user_read`/`user_write` return `Option` and never dereference a rejected range;
  `write_user_string` validates every byte and stops safely at `SYS_STR_CAP` or
  on `addr + 1` overflow.
  - `heap` — 4 MiB kernel heap (`HEAP_START = 0xffff_ff00_0000_0000`, `HEAP_SIZE = 4 MiB`) mapped page-by-page by `memory::map_page`; `#[global_allocator]` free-list allocator — first-fit over an *address-sorted* free list: `alloc` splits with the remainder left at its original address (order survives), `dealloc` inserts in place and coalesces with **both** adjacent neighbours (the old list-head-only merge stranded adjacent fragments and caused long-run OOM), spin-lock guarded; `init_heap` sets up the single free block, `test_heap` allocates `Vec`/`String`/`Box` and stress-cycles 200 strings, `fragmentation_selftest` (v2.38.33, run from `main` right after `test_heap`) carves 24 × 96 KiB blocks, frees them in allocation order and re-acquires 1.5 MiB contiguously via `try_reserve_exact`, logging `heap: fragmentation self-test ok` (smoke-grepped) or `FAILED`. The GUI backbuffer sits at `HEAP_START + 4 MiB` — abutting but page-disjoint.
- `fault` — (v2.45.0, extended v2.45.1/2/3/4/2.46.0) Ring-0 fault interceptor, the
  kernel twin of the host `run_isolated`. `intercept(frame, kind)` is called from
  the exception handlers for a **user** frame: it captures a full-CPU-frame
  `TaskSnapshot` (`TaskSnapshot::capture` — pid, `rip`/`cs`/`rsp`/`rflags`/`ss`
  and all 16 general-purpose registers, sealed by the FNV-1a `hash_of()` state
  hash; `verify()` re-checks a record), enqueues a `FaultRecord`
  (`FaultCode { DivideError, InvalidOpcode, GeneralProtection, PageFault,
  Hang }`, `count =` nth fault of the pid) into the bounded **system fault
  queue** (`FAULT_QUEUE_LEN = MAX_TASKS`, FIFO), freezes the task
  (`sched::freeze_task`) and calls `schedule(frame)` to switch away.
  `publish` also appends a `FaultEvent { pid, kind, count, rip }` to the bounded
  **kernel fault event ring** (`FAULT_EVENT_LEN = 16`, oldest dropped on
  overflow), drained by `poll_event()` / the `SYS_FAULT_POLL` syscall — the
  freestanding twin of the host `FaultReport` IPC; `events_queued()` reports the
  backlog.
  `peek()`/`queued()` feed the GUI overlay (which resolves the front,
  oldest-first) and `resolve(Recovery)` applies `Resume` (integrity gate: the
  frozen task's live frame — `sched::task_snapshot` — must match the queued
  snapshot's sealed hash, else the task is force-Terminated; `state verified` /
  `resume state mismatch -> terminate`), `Terminate` (`remove_task`) or
  `Rollback` (`restore_task` writes the **whole captured register bundle** back
  into the task frame so the resume re-enters the exact fault-time CPU state,
  re-verifying `verify()`), then pops the front — the next queued record becomes
  current without closing the dialog. A per-task **fault budget**
  (`MAX_FAULTS_PER_TASK = 3`) locks the dialog to Terminate once
  exceeded; every resolution is pushed into a bounded history ring (`FAULT_HISTORY_LEN = 8`) with `fault_total()` /
  `resolution_count()` aggregates, and `history_records()` (oldest-first copy of
  the ring) backs the shell `faults` command (v2.45.2). Since v2.45.3
  `record_hang(snapshot)` is the **hang watchdog** entry — the scheduler calls
  it for a ring-3 task that neither changed its `rip` nor made a syscall for
  `sched::HANG_WATCHDOG_TICKS` (10 s), charging the same budget and freezing the
  task through the same dialog (`FaultCode::Hang`, dialog title `Task Hung`);
  unlike `intercept` it runs inside `schedule`, so it never re-schedules.
  Kernel-context frames still take `interrupts::fatal_with`.
- `tui` — interactive on-screen TUI (v2.38.11, view-only dashboard; **interactive
  since v2.38.19**): the TUI is the **diagnostic fallback screen** since v2.81.0
  (the native framebuffer desktop is primary) — a compact boot-log strip
  (`console::CONSOLE_TOP_ROWS` = 6 rows) stays on top and `render()` paints the
  panel over the whole remaining height (`panel_rows()` = total glyph rows − log
  strip − status bar, tab bar first, content below) at ~20 Hz (every 5 ticks)
  from `idle_loop`, plus a two-sided
  bottom status bar (version, active `tab n/7`, tick counter, tick mode
  `HW-LAPIC`/`HW-IRQ`/`SOFT`, PIT countdown on the left; last PS/2 scancode, USB
  key/mouse seq on the right). Since v2.38.20 the status bar right side shows
  `k=0x.. uk=… pk=… m=… pm=… lid=… T=…c` (PS/2 scancode, PS/2 USB key/mouse
  seqs, lid state, live temperature) and the System tab adds a `lid=… temp=…
  tjmax=… sim=… ps2key=… ps2mouse=id0x..` line via `lid_state()`/`lid_detail()`.
  Seven tabs — System / Sched / USB / IPC / Storage
  / Shell / About — show live telemetry driven only by atomics or integral
  counters (no locks): scheduler `switch_count` + per-task state, IPC
  `syscalls::stats` + per-task `mailbox_len`, xHCI controller/keyboard/mouse
  counters, AHCI/NVMe status (`-1` absent, `0` failed, `1` ok, `2` read-verified),
  memory `frames_allocated`. Input: `1`-`7` switch tabs (outside the Shell tab,
  where digits type instead), the clamped USB-mouse arrow is repainted last on
  every frame so no repaint can erase it and a left-click on the tab bar switches
   tabs, and the Shell tab runs an in-kernel command shell (`Enter`=0x1C runs,
   `Backspace`=0x0E edits, `Esc`=0x01 clears; every entered command is echoed
   to serial as `[serial] [shell] > <line>` since v2.38.36, and `wifi` /
   `wifi sim` / `wifi real` toggle the Network window's radio — default
   **real**, sim armed explicitly). Panel text renders through the
  `psf` parser (`ensure_font()` re-parses the boot-synthesised PSF2 stream into a
  cached font) with the `font8x8` table as fallback.
- `gui` — windowed desktop (v2.38.22; damage-based repaint since v2.38.23; RAM
  backbuffer + dirty-rect blit + 60 FPS cadence since v2.38.24): the single
  user-facing GUI, entered from `idle_loop()` at boot since v2.81.0. The
  console/TUI is a diagnostic fallback: `tui` or `Esc` with no focused window
  leaves the desktop. While `gui::active()` the console's `vprintln!` path is
  bypassed and the heartbeat pauses. Owns a dark desktop background, a
  bottom-left `☰ Пуск` button and a sorted searchable Start panel
  (`start_entries`/`start_click`, `START_*` layout), a bottom task bar with one
  button per open window (stable order, see below), and a z-ordered window list
  (title bar with the minimize/maximize/close cluster, draggable body,
  edge/corner resize handles) in `MAX_WINS = 8` static slots. Text uses the
  TUI's char-aware `draw_text`/`draw_char`/`text_width` helpers and the shared
  `console::TEXT_ADVANCE = GLYPH_W + SCALE` step, so every glyph keeps a 2 px
  gap and Cyrillic Start titles and text fields render from `font8x8::CYRILLIC`.
  `draw_text` clips to the rectangle installed by `tui::set_text_clip` /
  `gui::set_surface_clip` (window, Start panel, task bar, modal and crash
  dialogs) and to its `max_px`; overlong task bar/Start labels use `fit_label` /
  `short_label` with `...` instead of spilling onto a neighboring surface.
  `clear_text_clip()` restores the full framebuffer for TUI fallback renders.
  Input:
  `Tab` cycles focus, `Esc` closes/leaves, printable keys type into the focused
   window; mouse clicks open/focus/close/drag (and, since v2.38.27, resize —
   `hit_resize` maps a press inside `RESIZE_BORDER = 5` px of a window's outer
   edges to an `EDGE_L/R/T/B` mask checked after the close button and before
   the title/body; the drag is keyed by `WinKind`, not a slot index, because
   `focus_window` → `bring_to_front` may shift slots when a drag starts).
   Input is read from both key/mouse
  sources via lock-free atomics and never blocks the idle loop; the GUI and the
  renderer are decoupled (input events only mutate window state + a dirty `Rect`,
  painting happens later in `render()`). Since v2.38.23 every mutation records a
  dirty `Rect` (`dirty_all`/`dirty_rect`/`dirty_window`). Since v2.38.24
  `render()` paints the whole frame into a RAM backbuffer
  (`Framebuffer::from_ram`, `ensure_backbuffer` — mapped once from the frame
  allocator at `BACKBUF_BASE = 0xFFFF_FF00_0040_0000`, never freed; 4 MiB for a
  1280×800 panel; failure degrades to direct-VRAM painting via `BACKBUF_FAIL`)
  and pushes only the damaged rectangle to VRAM with
  `Framebuffer::blit_region` (one `copy_nonoverlapping` per scanline) — the
  panel sees only complete frames, so there is no flicker or tearing. VRAM is
  written exactly once per dirty row. Since v2.38.39 painting goes through
  the `vector` module — a coverage-based anti-aliased layer (`fill_round` /
  `fill_round_grad` / `stroke_round` / `fill_circle` / `stroke_ellipse` /
  `line` / `shadow_round`, corner masks `TL/TR/BR/BL/ALL/TOP`, colour
  helpers `mix`/`blend`) that keeps edge cost O(perimeter) by reusing row
  fills for interiors: windows and the Start panel get 7px rounding, a soft drop
  shadow and an edge stroke, the desktop and task bar get two-stop washes
  (`Framebuffer::fill_rect_vgrad`), the tray globe becomes a true circle
  with ring/meridian/equator, and every `dirty_rect` is padded by
  `SHADOW_PAD = 7` so shadow spill repains with its shape. Continuous paths (cursor, drag, live
  System/Uptime windows) mark only their own rectangles via
  `mark_live_dirty()`. `idle_loop` drives `gui::render()` on its own cadence —
  60 Hz (one tick in ~17 of the 1000 Hz timer since v2.82.0, down from 250 Hz in
  v2.79.0 and 100 Hz in v2.78.2) — plus an immediate repaint after every mouse
  report, while the TUI renders at 30 Hz (was 100 Hz) and the lid/thermal probes
  stay on a ~20 Hz gate. The lower cadence is above what the eye needs and
  avoids burning CPU on invisible frames. The
  runaway-pointer gate is measured in elapsed ticks (`TIMER_HZ`, one second)
  rather than a packet count, so it no longer trips ten times faster at 1000 Hz.
  Most extended (0xE0/0xE1) scan codes are dropped before the GUI/TUI tables
   ever see them; since v2.81.0 Win (`0x5B`), Up/Down (`0x48`/`0x50`) and
   Enter (`0x1C`) are forwarded to `gui::handle_ext_scancode()` for Start
   navigation (see `ps2`/`main`/`xhci`).
   System window reuses `tui::tick_mode()`/`lid_state()`
   and `version::KERNEL`.
    Since v2.82.0 the five ring-3 demo tasks are **off by default** in the
    desktop build: `user::start_demos()` is idempotent and spawns them only when
    the Start entry `Пуск → Ring-3 демо` is activated, or at boot when the
    `ring3-demo` Cargo feature is enabled (headless proof runs via
    `AIOS_KERNEL_FEATURES=limine,ring3-demo`). `user::demos_started()` hides the
    Start entry once they run. The A/B IPC programs yield with `SYS_SLEEP` +
    `SYS_PING` instead of a busy-spin, the GUI client presents at 250 ms, and
    `syscalls.rs` logs each task's first `SYS_SLEEP` only plus every 64th IPC
    value, so an idle desktop reaches `[sched] idle`.
     Since v2.38.25 the window system is open to ring-3: `syscalls.rs` routes
    `SYS_GUI` (id 6) to `gui::client_syscall`, which dispatches on `rdi` —
    CREATE (`rdi = 0`, `rsi` → 16-byte `CreateReq { w, h, buf, title }` of user
    VAs) validates the request, the full buffer surface and every title byte
    through `memory::translate_user` — the only `int 0x80` user-pointer gate,
    rejecting kernel addresses, overflow and non-user mappings with `u64::MAX` —
    caps sizes (8..=512 px, ≤ 512×512×4 bytes), copies the NUL-terminated title
    (≤ 24 bytes) and registers a `ClientWin` in the `CLIENTS` registry
    (`MAX_CLIENTS = 4`); PRESENT (`rdi = 1`) re-damages the
    client's `WinKind::Client(id)` window (re-spawning it via `spawn_client` if
    it was closed, so `leave()` never kills a live client); GET_EVENT
    (`rdi = 2`, since v2.38.26) drains the client's single pending input event
    (`EV_NONE`/`EV_KEY | ascii<<8`/`EV_CLICK | x<<8 | y<<24`, keep-first,
    unknown pid → `u64::MAX`); any other `rdi` → `u64::MAX`. Input injection
    (v2.38.26): `handle_scancode` hands printable keys to the focused client
    instead of the typed-text note, `dispatch_click` posts body-relative
    clicks before the z-order shuffle (addressed by client id, so
    `bring_to_front`'s slot shift cannot orphan them), and `raise_clients()`
    focuses the frontmost client so input flows immediately. Because a client can
    unmap or re-purpose its buffer after CREATE, `draw_client()` re-validates the
    whole surface through `translate_user` (read-only) on **every** composite and
    skips the blit if any page fails, instead of dereferencing a stale pointer.
    Rendering composites
    the client's dense pitch-aware buffer through `Framebuffer::blit_at` below
    the title bar, or — since v2.38.27, once the window has been resized —
    through the nearest-neighbour `Framebuffer::blit_scaled`, so the client
    keeps its native 96×64 buffer while the window scales (resize clamps:
    `RESIZE_MIN_W = 96`, `RESIZE_MIN_H = TITLE_H + 64`, right/bottom edges
    bounded by the framebuffer minus the task bar; releasing the drag logs
    `[gui] resize … -> WxH`, and `draw_window` clips body/title text at the
    window's own right edge); `win_live_title` shows the registry title in the title bar
    and task bar;     `enter()` raises registered client windows above the built-in
    stack via `raise_clients()` (they register while the console still owns the
    screen, so Welcome/System/Clock would otherwise spawn on top).
    Since v2.38.28 windows carry `minimized`/`maximized` flags plus a saved
    `restore` rectangle: the title bar decodes a right-anchored 48 px
    `TitleBtn { Minimize, Maximize, Close }` cluster (`title_btn()`, checked
    before the title-drag/resize zones, so `hit_resize` skips minimized or
    maximized windows), `minimize_window` hides the window (render skips it,
    hit-testing ignores it, `cycle_focus` filters it) and drops focus to the
    topmost visible window, `toggle_maximize` snaps to the desktop minus the
    task bar and restores the saved rectangle on the second press, and
    `focus_window` un-minimizes — every task-bar/Start click flows through it,
    logging `[gui] restore … -> WxH` (`[gui] minimize …`,
    `[gui] maximize|restore … -> WxH` for the other transitions). Task-bar
    buttons are laid out by `taskbar_slots()`/`kind_rank` (Welcome < System <
    Clock < About < clients by id) instead of z-order, so a focus shuffle can
    never move a button under the pointer; minimized windows get the dim
    `BAR_MIN` fill. Hotkeys use plain F-key make codes (`idle_loop` filters
    break codes via `sc & 0x80` and E0/E1 prefixes, so no modifier tracking is
    needed). Since v2.81.0 `F1`-`F9` open or focus the nine `APPS` entries via
    `open_app`, while window controls moved to `F10` (`toggle_maximize`),
    `F11` (`close_window`) and `F12` (`minimize_window`); each returns `true`
    so nothing leaks into the typed-text note. The targets come from
    `focused_kind()`/`focused_idx()`, and the transitions log `[gui] close …` /
    `[gui] focus …` (the focus label is captured before `focus_window` because
    `bring_to_front` re-slots the window). Start toggles with the Win key
    (`0x5B` on PS/2, HID `0xE3` on USB), navigates with ↑/↓ (`0x48`/`0x50`),
    activates with Enter (`0x1C`) and closes with Esc;
    `handle_ext_scancode()` ignores those codes while a crash or modal dialog
    owns the keyboard. Since v2.38.30 `close_window` also gates on unsaved text:
    a window with `note_len > 0` (reached via Esc / `X` / `F11`) opens the
    modal confirm dialog — `MODAL: Option<(WinKind, title)>` — instead of
    closing, and `close_window_raw` is the forced teardown the confirm path
    uses. The dialog is a fixed 464×140 overlay drawn above windows and the
    task bar in `render()` (repainted whenever partial damage overlaps it),
    with hit plates that match `draw_modal` exactly. While `MODAL` is set the
    whole WM is gated: `handle_scancode` → `modal_key` (Enter/Esc resolve,
    everything else logs `[gui] modal blocks scancode 0xNN`), mouse presses →
    `modal_click` (plates act, the rest logs `[gui] modal blocks click (x,y)`),
    and `focus_window`/`spawn`/`cycle_focus` refuse outright — focus-steal
    prevention lives at both the input gates and these choke points.
    Since v2.38.31 a closing window also records its logical z-rank in
    `CLOSED_Z: [Option<(WinKind, u8)>; MAX_WINS]` (from `close_window_raw`,
    proof line `[gui] close <label> (z <rank>)`, cleared by `leave()`), and
    both open paths go through `place_window`: it inserts the window at the
    remembered rank (logging `[gui] reopen <label> at z <rank>`), appends on
    top when there is no memory (first open — the pre-v2.38.31 behaviour),
    and compacts the slot array so `WINS` stays a dense bottom→top sequence.
    `spawn_client` gained the modal gate too, so a client present during a
    dialog can no longer spawn focused underneath it.
    Since v2.38.32 window bodies can host widgets: `Widget::Button` plates are
    declared in window-local coordinates in per-kind tables (`widgets_for`,
    `WELCOME_WIDGETS` on Welcome) carrying a `WidgetAction::{Open(WinKind),
    CloseHost}`, painted by `draw_widgets` after the kind-specific body
    content (`BTN_BG` plate, centered label, `BAR_ON` while hovered), and
    hit-tested by `dispatch_click` before any ring-3 client event (proof line
    `[gui] widget <host>/<label> click`, no client event when a plate
    swallows the click). Hover state lives in
    `WIDGET_HOVER: Option<(WinKind, usize)>`, recomputed on cursor moves by
    `update_widget_hover` with the same topmost-window-first rule as the
    click path, frozen while `MODAL` is set and cleared by `leave()`; a
    hover change dirties only the host windows. An `Open` action skips the
    host `focus_window` — focusing the host first would lift it above the
    freshly placed window and mirror its restored v2.38.31 z-rank — while
    `CloseHost` reuses `close_window`, keeping the modal note gate.
    Since v2.38.34 the desktop also hosts a **Network** window
    (`WinKind::Network`, label `Net` in Start, also opened from
    the tray globe at the task bar's right end): `DHCP`/`Static` mode
    buttons, five address fields (`ip`/`mask`/`gw`/`dns1`/`dns2`) +
    `Apply`/`Test`, and the Wi-Fi group (`ssid`/`pass` + `Scan`/
    `Connect`/`Disc`). The fields are window-local `Widget::Button` plates
    with `WidgetAction::NetField(0..6)` — clicking focuses one, keys type
    into `NET_DRAFT` via `net_type`/`net_append`/`net_backspace` (`Tab`
    cycles `net_focus_next`), and opening the window loads the live config
    through `net_draft_load`. The command buttons run `net_button`:
    `Dhcp`/`Static` flip the draft mode, `Apply` parses all five dotted
    quads and commits through `net::apply` (persisting to CMOS; status
    `applied` or `bad ip`/`bad mask`/`bad gw`/`bad dns1`/`bad dns2`/
    `need ip`), `Test` calls `net::recheck`, `Scan` calls
    `wifi::start_scan` (honest in real mode since v2.38.36: without a
    detected card it refuses with `[wifi] scan: no wifi card` and the
    window status `no card`; the status line also shows `wifi card: <bdf>`
    / `card: none`, and an empty result renders the `no networks found`
    hint), `Connect` calls `wifi::connect` (`wait` on
    accept, the radio's error text on refusal), `Disc` calls
    `wifi::disconnect`. Proof lines: `[gui] tray globe click`,
    `[gui] widget Network/<label> click`, `[serial] [gui] net connect:
    no ssid` / `net connect err: … (ssid len N)`, plus the per-character
    typing trail `[serial] [gui] type '<c>' focus=N ssid_len=N pass_len=N`
    and `[serial] [gui] backspace focus=N len=N` (v2.38.36).
    Since v2.38.35 there is also a **Task Manager** window
    (`WinKind::Tasks`, label `Tasks` in Start, 440×248 at
    (460,140)): a `Task`/`Status` header strip, one 16 px row per open
    window in `taskbar_slots` (rank) order — selected via
    `task_row_click` into `TASK_SEL: Option<WinKind>`, drawn with the
    `BAR_ON` plate, status `Running`/`Background`/`Minimized` — and
    `End Task`/`Switch To` plates (`TaskBtn`) over an `N tasks` foot
    with `sel <label>`. `task_end` funnels through `close_window` (the
    v2.38.30 modal note gate applies), `task_switch` focuses and raises
    through `focus_window`, and both log `no selection` / `<label>
    gone` when `TASK_SEL` is stale; `task_key` maps `Enter` on the
    focused Tasks window onto `task_switch`. `kind_rank`/`taskbar_slots`
    place `Tasks` at rank 5 (clients 6+id) so the list mirrors the task
    bar, and `spawn()` logs `[gui] open <label>` for every window.
    Proof lines: `[gui] open Task Manager`, `[gui] tasks select
    <label>`, `[gui] tasks end <label>`, `[gui] tasks switch <label>`,
    `[gui] widget Task Manager/End Task|Switch To click`.
    Since v2.47.0 the widget layer is generic and interactive: `Widget::Button`
    is joined by `TextField`, `List`, `Scrollbar` and `Menu`, every variant
    exposing a window-local box through `widget_rect` so `widget_index_at`
    (hit-test), `hover_widget` and `draw_widgets` cover all kinds uniformly.
    Since v2.63.0 a sixth variant, `Widget::Checkbox` (a box + caption with an
    `id` into the host form store), joins them: `draw_checkbox` paints the box
    (`BAR_ON` border while hovered) and a check mark when on, `widget_click`
    toggles the bit through `form_check_toggle` and logs `[gui] widget
    <host>/check <label> = on|off`. Since v2.64.0 a seventh variant,
    `Widget::Radio` (a round box + caption, `group` + `value`), joins them:
    `draw_radio` paints the circle outline and a filled dot when selected,
    `widget_click` selects the value through `form_radio_select` and logs
    `[gui] widget <host>/radio <label> = <value>`. Since v2.67.0 an eighth
    variant, `Widget::Slider` (a track + thumb, `id` + `min`/`max`), joins them:
    `draw_slider` paints the caption, a sunken track with the filled portion in
    `BAR_ON`, a `TEXT` thumb and the numeric value; a track click maps its x to a
    value through `slider_value_at` and stores it with `form_slider_set`,
    logging `[gui] widget <host>/slider <label> = <value>`.
    Since v2.68.0 text fields hold UTF-8 scalars: `form_type_char` appends the
    encoded char when it fits whole in `LAB_FIELD_CAP`, `form_backspace` removes
    one whole scalar, and the field painter walks chars with the caret counted
    in scalars; since v2.81.0 both the caret and `draw_field_text` use the
    shared Cyrillic-aware glyph path instead of painting non-ASCII as `?`. A
    kernel-internal clipboard (`CLIP`, 128 bytes) backs
    `Ctrl+C`/`Ctrl+X`/`Ctrl+V` through `form_clip_copy`/`form_clip_cut`/
    `form_clip_paste`, logging `[gui] clipboard copy|cut|paste <n> bytes` and
    reporting `no field`/`full`/`empty` on the status line. `CTRL_DOWN` (interrupts) is
    fed by the PS/2 `0x1D`/`0x9D` make/break bytes (including the `0xE0`
    extended forms) and by `xhci::harvest_report`'s HID modifier byte, which at
    the same time restores Shift on USB keyboards.
    Since v2.65.0 every host owns its widget state: `WidgetForm` (fields, lens,
    focus, menu, sel, scroll, msg, `checks`, `radios`, `sliders`) lives in the
    `FORMS: [WidgetForm; FORM_SLOTS]` global, one slot per built-in window
    (`form_slot`), so a Network checkbox never aliases the Widget Lab.
    `form_load` resets a slot on open; `form`, `form_msg`,
    `form_check_toggle`, `form_checked`, `form_radio`, `form_radio_select`,
    `form_slider` and `form_slider_set` are host-generic.
    The desktop gains a **Widget Lab** window (`WinKind::Widgets`, label
    `Widgets` in Start, 460?332) that hosts two text fields, a
    ten-row scrollable list with a bound scrollbar, an `Edit` dropdown, a
    close button, two checkboxes (`Auto-connect` / `Persist config`), two radios
    (`DHCP` / `Static`) and a `Volume` slider. Keyboard input is routed to the
    focused host before the global shortcuts by `widget_key` (called from
    `handle_scancode` after `task_key`/`net_key`): since v2.66.0 the routing is
    host-generic ? it applies to any window with a widget table except Network ?
    and dispatches through `form_focus_cycle`/`form_backspace`/`form_type_char`/
    `form_enter`: `Tab` cycles the widget focus, `Backspace` edits the focused
    field, `Enter` activates (fields acknowledge, the list picks the selected
    label), printable keys type into a field or move the list with `n`/`p`, and
    `Escape` dismisses an open dropdown. Mouse clicks on the variants are
    dispatched by `widget_click` (field focus, list row select, scrollbar jump,
    menu toggle, checkbox toggle, radio select, slider set), and an open dropdown
    is served first by `lab_dropdown_click` ? its item clicks run `lab_menu_pick`
    before ordinary hit-testing, a click on the plate toggles, a click elsewhere
    dismisses and falls through. The room for the `Widgets` kind (rank 6, clients
    7+id) is reserved in `kind_rank`/`taskbar_slots`. Proof lines: `[gui] widget
    Widget Lab/field <n> focus`, `[gui] widget Widget Lab/list row <n> click`,
    `[gui] widget Widget Lab/menu open|close`, `[gui] widget Widget Lab/menu
    item <n> click`, `[gui] widget Widget Lab/check <label> = on|off`, `[gui]
    widget Widget Lab/radio <label> = <value>`, `[gui] widget Widget Lab/slider
    <label> = <value>`.
    Since v2.81.0 the shell is the boot screen: `idle_loop()` calls
    `gui::enter()` before its driver polls. `start_entries()` assembles a sorted
    catalog from `START_BASE` (system windows and actions), the nine `APPS`,
    live `SYS_GUI` ring-3 clients and system actions (`Обновить всё`,
    `Консоль ядра`); `start_matches()` folds Latin and Cyrillic case
    insensitively. Since v2.83.0 `open_app()` dispatches Dashboard to the real
    `WinKind::System` window, Network (including the program alias) to
    `WinKind::Network`, Terminal to the kernel console, and App Store, Files,
    Calculator and Text Editor to real in-kernel `WinKind::App(id)` windows;
    `app_load()` initializes their state and `app_key()`/`app_click()` serve keys
    and body clicks before generic widget forms. `STORE_IDS` is the six-program
    App Store catalog; `FILE_ROWS` are live `system/...` pseudo-files; `CALC`
    holds integer calculator state. Serial proof lines are `[gui] store select`,
    `[gui] store launch`, `[gui] app launch` and `[gui] calc result`. Genuinely
    host-only entries still open notice windows because the bare-metal kernel
    has no process launcher, WebView, audio or LLM backend. The host `aios-gui`
    crate is legacy reference code and is never launched from the kernel or
    either host TUI.
- `main` — reads the Limine framebuffer/rsdp/hhdm responses, runs the framebuffer read-back self-check, filters the memory map into `[MemRegion; 64]`, then initializes memory + heap, GDT/TSS (the GDT reload of CS/SS), IDT, PIC and PIT, brings up the LAPIC timer (`lapic::init()`) and masks PIT IRQ0 when it is live, `sti`, prints an on-screen `[probe] irq32_seen/ticks delta` liveness readout, enumerates the PCI bus and brings up the AHCI and NVMe storage drivers (after `sti`, so faults are handled), schedules the kernel worker and, since v2.82.0, only starts the five ring-3 tasks when the `ring3-demo` feature is on (otherwise they wait for `Пуск → Ring-3 демо`); the tasks are the four IPC/GUI demos plus, since v2.51.0, the socket ping-pong client. It declares the atomic driver-status statics `G_AHCI`/`G_NVME`/`G_XHCI` (stamped by each driver's init), and enters `idle_loop` where the heartbeat square and `gui::render()` (or `tui::render()` after the desktop is left) are driven; since v2.81.0 `idle_loop()` calls `gui::enter()` at boot. Keys and mouse reports are routed to the screen owner: `gui::handle_scancode`/`gui::on_mouse` when `gui::active()`, otherwise `tui::*`. Keyboard bytes come from the 256-slot FIFO drained with `while let Some(sc) = key_pop()` at the top of each iteration (v2.38.36 — extended/shift filtering happens on the drained stream, so a stalled loop no longer loses burst input to the old single-slot overwrite), and on a PS/2 button edge the band first flushes every pending `mouse_dx`/`mouse_dy` (up to 64 `drain_axis` steps) before calling `on_mouse`, so a press is dispatched at the pointer's settled position rather than mid-backlog (v2.38.36, the missed-click fix). Mouse deltas are drained **every idle-loop iteration** — both bands read `ps2::mouse_dx`/`mouse_dy` and `xhci::take_mouse_delta` unconditionally, so the 256 px clamp's carry-over residue is applied even when no new report arrived, while packets that arrived while the loop was elsewhere accumulate instead of overwriting each other (v2.38.30) — and since v2.38.38 a latched press edge (`take_button_edges()` in both bands) or a button-state change (`btn_changed = seq_changed && buttons != last`) is delivered through the new `deliver_pointer`, which replays up to three states (stale release → press carrying dx/dy → final release) so a press+release pair coalesced between two loop iterations still reaches `gui::on_mouse` (BUGS v2.38.38); the per-application clamp itself lives in the drivers (`MOUSE_MAX_STEP`, v2.38.35). Before anything is logged or rendered the loop calls the screen owner's `gui::input_allowed`/`tui::input_allowed` (v2.38.33 runaway gate): 60 consecutive packets pushing an already-clamped cursor further into an edge suppress the pointer input until a moving packet pushes away, and movement itself is aggregated into at most one serial line per band per second (`MoveLog`, `USB_MOVE`/`PS2_MOVE` statics — `[serial] usb mouse +N pkts dx=… dy=…`); button changes keep their per-report lines. It logs the Limine handoff selectors (`boot selectors cs/ss/ds`) and the chosen tick source (`timer=LAPIC|PIT`) to serial. Since v2.38.13 the framebuffer section additionally logs the virtual address and `framebuffer pages: N present, M writable, K total` (`memory::verify_region`), runs the `write_volatile` `direct_test` with a `sfence` + readback of pixel (0, 0) plus the OK-green probe, then `psf_check()` synthesises a PSF2 stream from `font8x8::BASIC`, parses it back, verifies the 'A' glyph round-trips and paints it at the bottom-centre of the screen.
  - Panic handler prints a versioned `KERNEL PANIC [semver]: …` line to the
    framebuffer console and serial, paints an allocation-free on-screen report
    via `console::panic_report("KERNEL PANIC: ", …)` with `version::KERNEL`, then
    `halt_loop`s. Fatal interrupts use the same path with a `FATAL [semver]` prefix,
    so a screenshot identifies the exact build that failed.
- `aios-kernel-run`: builds the kernel with the Limine path enabled (`cargo build --target x86_64-unknown-none --release --features limine`, since v2.78.0), stages the Limine ISO tree (`boot/aios-kernel`, `boot/limine.conf`, the Limine BIOS/UEFI CD stages, `EFI/BOOT/BOOTX64.EFI`), creates the hybrid ISO with `xorriso` + `limine bios-install`, and produces the byte-identical USB image `out\aios-kernel-usb.img` (isohybrid — boots on legacy BIOS via the Limine MBR and on UEFI via the ESP). Boots in QEMU — UEFI/OVMF (real GOP) by default, legacy BIOS otherwise; `AIOS_QEMU_USB=1` attaches the USB image as a mass-storage device instead of the CD-ROM. Overrides: `AIOS_LIMINE_DIR`, `AIOS_LIMINE_TOOL`, `AIOS_XORRISO`, `AIOS_QEMU`, `AIOS_QEMU_UEFI`, `AIOS_QEMU_USB`, `AIOS_SKIP_QEMU`. Since v2.84.0 the builder reads the `[package] version` from `aios-kernel/Cargo.toml` and reduces it to a release train with `release_train()` (`2.84.0` → `v2.84+`), writing a two-entry `limine.conf`: `/AIOS kernel v2.84+ (current, Limine + GOP)` and `/AIOS kernel v2.79 rollback (Limine + GOP)`. The current ELF is staged as `boot/aios-kernel`; the rollback ELF defaults to `out/aios-kernel-v279.elf` via `AIOS_FALLBACK_KERNEL` (the builder asserts the file exists, so a clean clone must supply a real v2.79 ELF) and its label comes from `AIOS_FALLBACK_VERSION` (default `v2.79`). A stale `out\aios-kernel.iso` is deleted before each build, and `verify_hybrid_image()` then checks the MBR `55 AA` signature and an ISO9660 `CD001` descriptor at `0x8001`/`0x8801`/`0x9001`, so a truncated or non-hybrid image never reaches QEMU or the flash script.
- Flash to a real USB stick: run `scripts\flash-usb.ps1` as Administrator (it writes the image to `\\.\PhysicalDriveN`, verifies SHA-256 and the MBR `55 AA` signature). It locks and dismounts the target volumes (`FSCTL_LOCK_VOLUME`/`FSCTL_DISMOUNT_VOLUME`), opens the disk with `FileShare.ReadWrite`, pads each write to a sector boundary and calls `Flush($true)`, then unlocks and runs bounded `Update-Disk` + `pnputil /rescan-disks` recovery (each in a `Start-Job` governed by `AIOS_FLASH_RECOVERY_TIMEOUT`, default 30 s) plus a bounded eight-attempt readiness wait. The whole image is hashed again from the raw disk and compared with the source SHA-256 before the MBR check; an unprivileged run relaunches itself elevated with all output written to a single `%TEMP%` log replayed by the parent. A bare-metal AIOS stick boots on both legacy BIOS and UEFI laptops. For the native UEFI-only GPT image use `aios-install` instead: it validates the GPT/ESP layout before writing (see the Native UEFI Boot Chain section above).
- Verify: `cargo run --release` in `aios-kernel-run` — serial shows `aios-kernel: Limine handoff`, HHDM offset, RSDP, `framebuffer = WxH pitch=… bpp=… usable=true`, `framebuffer self-check OK (readback 0x…)`, `usable memory regions = N`, `paging selftest OK.`, `heap: …`, `interrupts online.`, `[sched] ready`, `five ring-3 tasks armed`. A QEMU `screendump` confirms the console draws (foreground/background pixel histogram).

### Dual-kernel Limine image: historical stick vs. current builder output (v2.84.0)
- **Historical, stale (v2.81.0 → v2.84.0 patch):** the reflashed Kingston stick
  used a byte-patched copy of the known-good hybrid ISO rather than a full
  `xorriso` rebuild (those host binaries were unavailable). Its `/boot` held
  `AIOS_KER.;1` (Rock Ridge `aios-kernel`, extent 32) for the desktop ELF and
  `AIOS_KER2;1` (Rock Ridge `aios-ker2`, extent 600) for the old v2.79 ELF, and a
  202-byte `limine.conf` at ISO sector 1780 with `timeout: 3`. The v2.84.0 refresh
  replaced only the default ELF at ISO LBA 32 (1,054,032 bytes, SHA-256
  `BA534ECF802153D64D70D04FD0BBE9A5A4CD10E2FE2D7FFA1D3D4C0E1CB563D9`; image
  SHA-256 `C172387199711E544EF5CEE1F112170B49096C77C8284F07064EBD78B0D43929`),
  while the v2.79 fallback ELF and its hash stayed unchanged. That stick was read
  back in full (SHA-256 `8E0CE7F3F3CF5A0B66D6D9CCD2CFC196A5E74A6DE3540A4C55DEB98D0ED296A5`)
  with ELF magic at both offsets, and QEMU Legacy BIOS confirmed the new NIC boot
  line plus a full DHCP discover/request/bound cycle before the physical write.
- **Why it is stale:** the byte patch overwrote the UEFI stage at LBA 32, so that
  physical stick is still Legacy-BIOS-only, and its hand-written menu
  (`/AIOS Desktop v2.84 (native kernel)` / `AIOS Legacy v2.79 (old GUI)`) no
  longer matches what the builder generates. No fresh rebuild was performed
  here: `xorriso.exe` and the Limine binaries are absent on this host, so this
  section records the existing artifact, not a newly verified image.
- **GUI-verification copy (v2.84.0):** for the glyph-spacing/text-clip fix a
  fresh release ELF (`1,046,336` bytes, SHA-256
  `FD69FF2DDE3969C74BC9CEDAFC3F4E2DB7C3A48A0CA8724987F6F7C7B713EE17`) was
  patched into ISO LBA 32 of a copy of the image above, with the `AIOS_KER.;1`
  directory record updated to the new length. The copy
  (`out/aios-kernel-usb-limine-v284-gui.img`, `4,683,776` bytes, SHA-256
  `3B9DA3E1D0E890DF8FCA5FD87FB2C15738E550E28FCEA0F6E993D4DE6050525F`) was
  checked in Legacy BIOS QEMU (GOP `1280x800`, DHCP bound, windows opened
  with `F4`/`F5`/`F9`) and then written to the physical stick (see below); it
  inherits the UEFI limitation above.
- **Physical deployment (v2.84.0 GUI fix):** the image above was written to
  Disk 3 after explicit approval. `scripts/flash-usb.ps1` reported a successful
  write but read back the previous image because all three volume
  lock/dismount attempts failed; a checked `diskpart clean` + P/Invoke
  `WriteFile`/`FlushFileBuffers` path was used instead
  (`boot/out/flash-gui-disk3.log`). Full 4,683,776-byte read-back SHA-256 is
  `3B9DA3E1D0E890DF8FCA5FD87FB2C15738E550E28FCEA0F6E993D4DE6050525F`, with
  LBA 32 ELF hash
  `FD69FF2DDE3969C74BC9CEDAFC3F4E2DB7C3A48A0CA8724987F6F7C7B713EE17`, directory
  record size `1,046,336`, MBR `55 AA` and the hand-written v2.84 menu. UEFI
  parity still requires a clean hybrid image.
- **Physical deployment (post-v2.84.0, page-fault diagnostics):** after explicit
  approval the diagnostic image
  `out/aios-kernel-usb-limine-v284-pfdiag.img` (`4,683,776` bytes, SHA-256
  `3DB811A18CB93C69B17B3776F63D5CDC8177B7FD8E62F9F9F8044866A43692E9`; kernel
  ELF `1,046,488` bytes, SHA-256
  `BD2D6AEEDCE91159877BFBADF0DFDD068D22EA86276F4EB0C91FD5CCE95CDB02`) replaced
  the GUI-fix image on Disk 3 via the same checked `diskpart clean` +
  `WriteFile`/`FlushFileBuffers` path (`boot/out/flash-pfdiag-disk3.log`).
  Full-image read-back SHA-256 matches; LBA 32 ELF magic `7F 45 4C 46`, payload
  hash `BD2D6AEE…C95CDB02`, directory record size `1,046,488`, MBR `55 AA` and
  the hand-written menu were confirmed by an independent verifier
  (`boot/out/verify-pfdiag-disk3.log`). The checked flash script's optional
  post-read `ReadFile(LBA32)` step failed on its already-used handle after the
  streaming SHA loop; the independent whole-disk verifier passed, so the image
  on the stick is correct. UEFI parity still needs a clean hybrid rebuild.
- **Current truth (builder-produced, v2.84.0):** `aios-kernel-run` stages two
  ELFs into a clean hybrid ISO/IMG and writes the menu
  `/AIOS kernel v2.84+ (current, Limine + GOP)` (default) and
  `/AIOS kernel v2.79 rollback (Limine + GOP)`, validated by
  `verify_hybrid_image()` (MBR `55 AA` and ISO9660 `CD001`). A clean reflash with
  that generated image is still required on the physical stick to restore
  UEFI + BIOS parity; until then the stick remains the Legacy-BIOS-only artifact
  described above.

## Bootable AIOS Installer (Live ISO → disk, v2.33.0)

The bootable ISO is built from `iso/stage` (Limine BIOS/UEFI CD images, `limine.conf`,
`vmlinuz`, self-baked `initramfs.cpio.gz`) with `xorriso` (El Torito BIOS + UEFI) and
`limine.exe bios-install` (isohybrid MBR). It presents a two-entry Limine menu:

- **AIOS Live (kernel TUI)** — `rdinit=/init`: boots the exact initramfs used for
  flash/ISO live testing.
- **AIOS Installer** — `rdinit=/installer` with `aios.target=<dev>` / `aios.yes`
  accepted as optional automation switches.

Both entries set `console=ttyS0 console=tty0` (tty0 last): `/dev/console` resolves to the
VGA screen, so the kernel TUI, installer and rescue shell are visible on real hardware
(serial-only `console=ttyS0` left the laptop screen black — fixed in v2.33.2).

### Pre-init GPU module loader (v2.33.3)

The initramfs `/init` entry is not the raw `aios-init` binary: it is a `aios-loader` shell
script that runs *before* any userspace, solving the chicken-and-egg of fbcon needing a
frame buffer while the GPU module only creates one under a DRM driver. The loader:

1. mounts `proc`, `sys`, `dev` (busybox `mknod /dev/console c 5 1`), then `mkdir -p /dev/shm`
2. `modprobe`s all five x86 GPU DRM drivers (`radeon`, `nouveau`, `gma500_gfx`, `i915`,
   `amdgpu`); any that are absent or fail are silently skipped
3. `dd`-extracts the original `aios-init` ELF binary — **embedded inside the script itself**
   after a `#BIN\n` marker — into `/tmp/aios-init`, using baked fixed-width 10-digit offsets:
   `BINOFF:=000000001234` (skip= byte offset of the marker in `/init`), `BINSZ:=0000000382240`
4. `chmod 755 /tmp/aios-init` and `exec /tmp/aios-init`

The original `aios-init` is therefore still PID 1 and the rest of the boot chain
(`aios-init` → `bring_userspace()` / sfs-up.sh → `/system/aios-core` → kernel TUI) is
unchanged; the loader is transparent.

### Kernel initramfs unpacker constraints

Only **in-place content replacement** of the `/init` entry is reliable:
- The kernel unpacker writes entries until the first `TRAILER!!!`; entries appended after it
  are silently ignored (the file appears at byte range in the archive but the kernel never
  creates it on disk).
- Removing or renaming the `/init` entry (changing its name) makes the kernel reject the
  entire archive with `VFS: Cannot open root device`.
- Therefore, the script replaces the raw bytes of the original `/init` entry payload at the
  same offset in the raw cpio, and the rest of the archive (6657 entries) is untouched.
- `BINOFF`/`BINSZ` must use **fixed-width** placeholders (`0000000000`) — a variable-width
  replacement shifts the script length, moves the `#BIN` marker, and causes the wrong bytes
  to be extracted; `ash` then sees the ELF as a script and falls back to line-by-line parsing.

### Installer runtime

`/init` or `/installer` were the initramfs PID 1 of the retired Linux Live path;
the installer wrapper (`/installer`) brought up proc/sys, a `tmpfs` `/dev` with
busybox `mdev -s`, then `exec`ed the old `aios-install`
(`aios-core/live/aios-install`, removed in v2.77.0). Device bring-up had no udev:

1. storage/filesystem kernel modules are loaded with busybox `modprobe` (the
   initramfs ships the kernel module tree incl. `modules.dep`/`modules.symbols`),
2. `mdev -s` repopulates `/dev`, and `ensure_nodes()` `mknod`s partition nodes
   straight from `/proc/partitions` because devtmpfs races the module load.

### Install to a target disk

`aios-install` validates the device, then:

1. `wipefs -a` + `sfdisk --wipe always` with a GPT layout of three partitions:
   **1 MiB BIOS boot** (`type=21686148-6449-6E6F-744E-656564454649` — mandatory for
   GRUB to embed a BIOS `core.img` on GPT), **512 MiB EFI System**, and the rest as
   **ext4 root**.
2. forced formatting: `mkfs.fat -F 32 -I` (EFI) and `mkfs.ext4 -F` (root) so stale
   filesystem signatures never block on a confirmation prompt.
3. `tar` copy of the live rootfs (the initramfs including `boot/vmlinuz` and the
   baked `boot/initramfs.cpio.gz`) onto the target.
4. optional block selection (`install_blocks`): reads the catalog at
   `/usr/share/aios/blocks` and the default selection `/etc/aios/blocks.conf`,
   prompts with `all`/`none`/name list (default taken from the conf; `AUTO=1`
   uses the default without prompting), copies the chosen
   `{name}_{version}.wasm|.bin` (+ sidecar `.json`) into the target `/blocks`
   (the directory the kernel scans via `AIOS_BLOCKS_DIR`) and records the final
   selection in `/etc/aios/blocks.conf` on the target.
5. GRUB installed for **BIOS** (`--target=i386-pc`) to the target disk and for
   **UEFI** (`--target=x86_64-efi --removable`) into `/boot/efi`; stderr is captured
   to `grub-err.txt` on the target and `rc` echoed.
6. `grub.cfg` written with an **unquoted heredoc** so `${DEV}` is shell-expanded to
   the real device (`root=/dev/sda3 console=ttyS0`, `initrd /boot/initramfs.cpio.gz`).

The installed disk then boots SeaBIOS/GRUB → kernel → initramfs → `aios-init`
(PID 1) → `/system/aios-core` → AIOS TUI. Full loop verified in QEMU and the ISO
written raw to a physical USB stick.

Every step is timestamped into an install journal at `/aios-install.log` on the
target root partition (ending with an `AIOS INSTALL COMPLETE` marker), so a failed
physical install is diagnosable by re-booting the target. `aios.verbose` on the
cmdline (or `--verbose` passed to `aios-install`) additionally dumps the final GPT
table via `sfdisk --dump` before the copy. If the target root already carries the
AIOS label, an interactive run prompts `Type YES to FORCE reinstall` before wiping;
the unattended `aios.yes` path proceeds without the extra prompt. The root partition
always takes the rest of the disk and its actual size is recorded in the journal.

## Official store blocks and `block run`

The store catalog has two accepted shapes: the original `Vec<ManifestInfo>` array
and the object form produced by `aios-official-store/tools/indexgen`
(`schema_version`, `name`, `description`, `blocks`). `aios_store::catalog::parse_index`
converts both into `ManifestInfo`, choosing the English text from localized
`description` objects, stripping the `sha256:` prefix from `wasm_hash`, and
mapping capability aliases such as `filesystem:read` onto `CAP_FS_READ`.
`StoreSource::block_url` now points at `blocks/<name>/block.wasm` and falls back
to the legacy flat `blocks/<name>.wasm` URL on HTTP 404; local sources read
nested `block.wasm` plus sidecar `manifest.json` first. When a manifest omits
`wasm_sha256` or `wasm_size_bytes`, the catalog loader computes both from the
downloaded binary so `store install` can still verify it.

`block run <name> <text...>` completes the console path from install to
execution. `HostBackend` finds the installed binary through `StoreManager`,
registers it once in the shared `BlockRegistry`, and keeps
`BTreeMap<String, BlockId>` so repeated calls reuse the same instance. The text
after the block name is wrapped in `IpcPacket::new(..., CommandId::Custom,
Payload::Custom(name, bytes))` and delivered with
`BlockExecutor::dispatch_wasm_packet`; `Payload::Text` replies are printed line
by line. `HostBackend::set_executor` injects the orchestrator's executor, and
`TuiApp::new` passes it, so console-launched blocks preserve their state across
commands and share the same live WASM instances as boot-time blocks. This is
why `block run calculator ans+1` works after `block run calculator 2+3*4`,
unlike `wasm call`, which passes no payload and creates a fresh instance each
time.

## Network settings, host link APIs and NIC provisioning (v2.84.0)

`aios-net-config::NetworkConfig::apply_updates` now accepts the full `dns`
shape: an object with `primary`, `secondary` and `search_domains`, an address
string, a search-domain array or `null`. It also validates and replaces
`interfaces` as an array of typed `InterfaceConfig` objects, rejects malformed
fields, and preserves the config version while raising it to at least `2`.
The host DHCP client keeps one transaction id across DISCOVER and REQUEST,
reads it from the correct DHCP header offset (`p[4..8]`) and ignores replies
that belong to another transaction.

`aios-sys-control::host_net` parses `netsh interface ipv4 show config` on
Windows and `ip -4 addr show` plus sysfs on Linux into `AdapterState` rows;
its parsers are pure and need no hardware. `NetManager` adds `recheck()`,
`adapters()` and `lease()`: recheck performs fresh link I/O and refreshes the
cached link/lease snapshot, adapters reports the observed OS interfaces, and
lease persistence no longer overwrites unrelated config fields. `HostBackend`
exposes these as `console net recheck|apply|adapters|lease`; `net apply`
persists first and then rechecks, instead of claiming that a cached status is
fresh.

`aios-console::net_text` is a shared `no_std`-compatible module for strict
dotted-quad parsing, `key=value` updates, validation and stable status/lease
formatting. `KernelBackend` uses it for
`net show|config|status|lease|set|apply|validate|reset|adapters|recheck`, so
the kernel text path is exercised by workspace tests; `net lease` says
`no DHCP lease acquired in this session` only while no lease exists.

`aios-autohal::nic` classifies PCI network functions as `NicClass::Ethernet`
(class `0x02`, subclasses `0x00`/`0x01`) or `NicClass::Wifi` (`0x02/0x80`).
`SupportedHardware::for_class` adds masked class matching while exact
vendor/device entries stay available; the catalog ships `NIC_WAT` plus exact
and class-wide NIC builtins, and `driver nics` reports detected cards, their
driver, state and token status. `ProvisioningEngine` now retains the signed
`CapabilityToken` for each live instance, exposes it via `tokens()`/`token()`
and removes it on rollback, removal or uninstall.

`aios-kernel::nic` exposes `Kind::name()`, `NicDeviceInfo`,
`SUPPORTED_DEVICES` and `describe()`, so the boot log names both the exact
driver kind and the backend. These additions are statically verified; no new
real-hardware NIC bring-up is claimed.

## Voice architecture (2026-11-27)

The host adds `aios-voice` as an input layer in front of the existing EasyLang/WASM execution path. `MicrophoneSource` streams 16 kHz mono PCM through `CaptureController`, which owns VAD and level metering; `SttEngine` abstracts local Candle Whisper and Whisper-compatible HTTP backends, with `UnavailableSttEngine` as a fail-closed default. `VoicePipeline` publishes phase events on a broadcast bus so REST, WebSocket and both UIs observe the same state.

The bridge's `VoiceRuntime` consumes `Transcribed` events on a background worker. Generation is delegated to `ScriptGenerator`, which uses `ScriptLlm` (real LLM adapter, mock or deterministic offline fallback), enforces a code-only EasyLang grammar, retries on validation failure and compiles via `WorkflowCompiler`. Generated WASM is never trusted: `AutoManifestGenerator` extracts capabilities from guarded `aios` imports and `AccessControlLayer` validates them against a voice session token before `WasmBlock::instantiate_with_policy` installs an identical `HostPolicy`. Host calls are audit-only; they do not spawn processes or open network connections, and non-http URLs for `net_check`/`browse` are rejected.

Privileged scripts are staged as `AwaitingConfirmation` with their permission list, and only an explicit `confirm:true` request issues a token with the required capabilities. Successful workflows are persisted with the originating phrase for traceability.
