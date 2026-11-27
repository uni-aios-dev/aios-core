# AIOS Interface — Usage Guide

## Legacy BIOS USB boot menu (v2.84.0)

The Kingston DataTraveler stick is a patched hybrid Limine image. On a
Legacy-BIOS laptop it shows a menu for 3 seconds (`timeout: 3`):

1. `/AIOS Desktop v2.84 (native kernel)` — the default entry; boots the native
   framebuffer desktop directly in `aios-kernel`. This payload includes the
   in-kernel `net` commands and NIC inventory from v2.84.0.
2. `/AIOS Legacy v2.79 (old GUI)` — the previous kernel, kept as a fallback.

Use ↑/↓ and Enter to choose; if no key is pressed, the first entry boots. The
stick does not boot on UEFI firmware because the old UEFI stage was overwritten
when the kernel was patched into ISO LBA 32. To boot the native UEFI-only image
instead, use the GPT/ESP image described below on a UEFI-capable machine.

`aios-kernel-run` now stages both ELFs and can build a clean two-entry menu
(`/AIOS kernel v2.84+ (current, Limine + GOP)` and `/AIOS kernel v2.79 rollback
(Limine + GOP)`); a real fallback ELF must be supplied via
`AIOS_FALLBACK_KERNEL`, and `AIOS_FALLBACK_VERSION` relabels the fallback entry.
The stick has been reflashed with the GUI-fix image
(`aios-kernel-usb-limine-v284-gui.img`), not with that clean hybrid image, so it
remains Legacy-BIOS-only. `scripts/flash-usb.ps1` locks/dismounts volumes, writes
sector-aligned padded blocks and verifies the full image by SHA-256 read-back
plus the MBR `55 AA` signature; if volume locks fail, use a checked raw
`diskpart clean` + `WriteFile`/`FlushFileBuffers` write instead (see BUGS v2.84.0
post-deploy).

## Native UEFI image (host tools)

The UEFI-only image built by `aios-image` (`boot/out/aios-esp.img`) is written
to a disk from the build/service machine with `aios-install`:

```
aios-install --list
aios-install --image boot/out/aios-esp.img --device \\.\PhysicalDrive2          # validate only
aios-install --image boot/out/aios-esp.img --device \\.\PhysicalDrive2 --write --yes
```

- Without `--write` the command only validates the image and prints the plan
  (image size, CRC32, GPT backup LBA, ESP range, target kind and size); nothing
  is modified. Writing additionally requires `--yes` and destroys data on the
  target.
- `<PATH>` may also be a raw file (for tests and file-backed disks). A target
  larger than the image is accepted; bytes after the image are preserved.
- Before writing to a physical disk, unmount all of its volumes — `std` has no
  volume-lock API, so the tool cannot do this for you. `--list` probes
  `\\.\PhysicalDriveN` read-only on Windows and may be empty on virtualised
  hosts.
- On Windows both `--list` and writing to `\\.\PhysicalDriveN` require an
  **elevated (Administrator)** process: a non-elevated session lists no disks
  and opening the device fails with `Access Denied`. Run the command from a
  PowerShell started "As Administrator" (UAC confirmation is required). A dry
  run is still the default; the write needs `--write --yes`.

### Secure Boot signing (M5.2)

`aios-sbctl` is a standalone host tool (`boot/aios-sbctl`) for the AIOS
PK/KEK/db hierarchy, Authenticode loader signing and enrollment payloads. It
uses the `openssl` CLI, so `openssl` must be on `PATH`:

```
aios-sbctl keygen --dir keys
aios-sbctl sign --image boot/out/BOOTX64.EFI --keys keys --out boot/out/BOOTX64.signed.efi
aios-sbctl verify --image boot/out/BOOTX64.signed.efi --keys keys
aios-sbctl enroll --keys keys --out enroll
```

- `keygen` writes `PK.key/crt/cer`, `KEK.key/crt/cer` and `DB.key/crt/cer` into
  the key directory. Keep the `*.key` files private: possession of `DB.key`
  allows signing a loader that OVMF will trust.
- `sign` writes the signed PE to `--out` (or replaces the input when omitted)
  and prints the PE SHA-256 and the PKCS#7 size; `verify` checks the signature,
  the certificate and the PE digest, then prints `signature OK`.
- `enroll` writes `PK.auth`, `KEK.auth` and `DB.auth` into `--out`. They are
  `EFI_VARIABLE_AUTHENTICATION_2` buffers for `SetVariable` with
  `EFI_VARIABLE_TIME_BASED_AUTHENTICATED_WRITE_ACCESS` and
  `wCertificateType = WIN_CERT_TYPE_EFI_GUID (0x0ef1)`. `virt-fw-vars` cannot
  consume them as certificate files, so a NVRAM template can be enrolled either
  from the DER `*.cer` files or by passing these buffers to `SetVariable` from a
  UEFI application; the latter path was verified in OVMF for all three files.

`aios-image` can do the signing and the QEMU run in one step:

```
cargo run --manifest-path boot/aios-image/Cargo.toml -- --no-run --sign-keys keys --enroll-out enroll --out boot/out/aios-sb.img
cargo run --manifest-path boot/aios-image/Cargo.toml -- --headless --timeout 20 --secure-fw --ovmf-vars path/to/OVMF_VARS.fd
```

- `--sign-keys DIR` signs the loader and keeps the verified copy at
  `boot/out/BOOTX64.signed.efi`; `--enroll-out DIR` is only accepted together
  with `--sign-keys`.
- `--secure-fw` switches QEMU to `edk2-x86_64-secure-code.fd`, and
  `--ovmf-vars PATH` supplies an already-enrolled NVRAM template (the stock
  template has no keys). Without enrolled variables a signed loader is still
  rejected.
- Writing to a physical disk remains a separate, explicit `aios-install`
  operation. Signing does not flash anything; the v2.79.0 stick mentioned in the
  historical M5.2 notes was not rewritten during that phase.

---

## Live USB (retired in v2.77.0)

The old Linux Live USB image (`AIOS-LIVE`, a hybrid BIOS+UEFI ISO built from
`live/build.sh`) is no longer shipped: the `live/` directory and the
`scripts/build-live-iso.*` / `scripts/fix-wsl2.ps1` helpers were removed. Use
the native UEFI-only GPT/ESP image and `aios-install` described above instead.

---

## Overview

AIOS provides three interfaces for system management:

| Interface | Type | Binary / Path | Use Case |
|-----------|------|---------------|----------|
| **Web** | Browser SPA (HTML/CSS/JS) | `aios-studio/` served at `http://<host>:<port>/` | Remote management, visual dashboard, any device |
| **TUI** | Terminal (ratatui) | `aios-tui` | SSH, interactive, keyboard-driven |
| **GUI** | Kernel framebuffer shell | `aios-kernel` (`gui.rs`) | The single user-facing AIOS Desktop: Start menu, windows and taskbar, mouse + keyboard |
| **Daemon** | Headless server | `aiosd` | Docker, background, CI/CD, no terminal required |

The host `aios-gui` (egui/eframe) tree is legacy reference tooling and is not
launched by the kernel or either host TUI. The kernel desktop and the host TUI do
not share a widget model: the desktop owns its own framebuffer windows and reads
live kernel state directly.

---

## Unified text console (`aios-console`, in development)

The `aios-console` crate defines the command language that will replace the
duplicated parsers in the three current interfaces. It is pure text: one input
line, text lines out, no panels, menus, mouse or hotkeys.

```
<group> [<subcommand>] [flags] [arguments]
> process spawn worker --priority normal --ram 128
> net set ip=10.0.0.5 gw=10.0.0.1
> block load ./browser_2.0.wasm --name browser
```

- Command and subcommand names are case-insensitive; arguments keep their case.
- Quotes (`"a b"`, `'a b'`), backslash escapes and `#` comments are supported.
- `help [group|command]` is generated from the registry, not hand-maintained.
- Scope tags in help: `[B]` both backends, `[H]` host only, `[K]` kernel only.
- Result codes: `0` OK, `1` usage, `2` denied, `3` unavailable, `4` backend error.
  Successful results end with `OK in <ms> ms`; failures start with `ERR ...`.
- `clear` resets the visible output and scrollback; `exit [code]` closes the
  console. These are frontend control actions, not printed text.

| Old spelling | Canonical commands | Reason |
|--------------|--------------------|--------|
| `spawn` | `process spawn`, `block load`, `cluster spawn` (deferred) | process vs wasm block vs cluster |
| `load` | `block load`, `block mock` | real wasm file vs synthetic safe-mode block |
| `restart` | `wd reset`, `block rollback`, `hw rescan` | restart vs update revert vs hardware refresh |
| `info` | `status`, `sys info`, `kernel info` | runtime vs hardware vs kernel telemetry |
| `status` / `info` | `status`, `wd status`, `bridge status`, `net status`, `hub status` | one status per subsystem |
| `logs`, `log` | `log show` | one log source |
| `net get` | `net show` | show vs set/reset/link status |
| `ls`, `list` | `block list`, `vfs ls`, `ps` | blocks vs files vs processes |
| `processes` | `ps` | safe-mode process-list spelling |
| `ver` | `kernel ver` | kernel version is a telemetry subcommand |
| `cls` | `clear` | host-shell spelling of `clear` |
| `wifi` | `wifi status` | bare legacy name defaults to status |

Parity tests cover the vocabulary of `aios-tui`, the `aios` shell, the kernel
shell, `SafeModeShell` and `aios-shell`: `aios-console/tests/legacy_parity.rs`
pins canonical, ambiguous and deferred results, while unit tests in
`aios-watchdog`, `aios-tui` and `aios-shell` compare the pure parser/translator
seams. Output-level tests pin the exact legacy lines for `help`/`?`, unknown
commands, `kill`/`store`/`net` usage and empty `ps` in `aios/src/tui/mod.rs`,
plus the canonical host wording for `help`, `clear` and usage errors in
`aios-console/tests/host_backend.rs`. The standalone TUI additionally compares
all migrated bare spellings, including `help`/`?`, `logs`/`log` and the mutation
commands `kill`/`spawn`/`load`/`unload`/`restart`/`recover`, byte-for-byte with
`SafeModeShell`. The `aios-shell` unit tests compare all bare POSIX builtins
(`help`/`?`, `clear`/`cls`, `pwd`, `echo`, `history`, `alias`, `unalias`, `cd`,
`env`/`export`/`unset`, `source`/`.`, `jobs`/`fg`/`bg`) against their legacy
text generators, with a guarded temporary `AIOS_DATA_DIR` for the history and
alias files and stateful checks for environment, `OLDPWD`, current directory,
`source` recursion and job-control effects.

Ambiguous legacy names are not silently expanded: the console prints the list
of canonical alternatives. GUI, WebView, web browsing, LLM intents, cluster
control, pipes/redirection and theming are commented out in `deferred.rs` and
report `ERR unavailable: ...` with the reason. `store sign` and `store publish`
have real `HostBackend` implementations and are no longer deferred.

`HostBackend` already executes the implemented commands against the real host
runtime. Process/block/store/VFS and introspection commands: `ps`,
`process spawn|kill|renice|suspend|resume|affinity`,
`block list|info|load|mock|unload|activate|deps|verify`,
`store list|sources|source|search|install|update|check-updates|uninstall|
rollback|trust|verify|sign|publish`, `vfs ls|cat|head|write|mkdir|rm|mv|cp|stat|exists|
resolve|readat`, `metrics`, `health`, `telemetry`, `ipc`, `trace list`,
`sys info|tier` and `hw detect|rescan|profile|fingerprints`. Scheduling,
networking and security: `sleep`, `fault resolve`,
`sched mode|quantum|aging|rt-deadline|rt-clear`,
`net show|set|reset|validate|config|status|adapters|lease|apply|recheck`,
`caps list|issue|revoke|check`, `sandbox start|stop|log|mem` and
`hw-protect status|mpk|tee|iommu|clear`. Runtime and management adapters:
`wasm compile|call|mem|grow`, `wasi policy`, `exec detect|caps|<program>`,
`deps scan|heal`, `compat list|kill`, `context stats|export|compact|persist`,
`workflow list|priority`, `stability list|best|worst`, `crash report`,
`driver list|nics|failures|remove|rollback|install|provision`,
`hotplug start|events|drain`, `wifi status|scan|connect|disconnect|sim|real`,
`hub status|scan|wifi|layout`, `alias`, `unalias` and `wd status|reset`.
`alias`/`unalias` maintain session aliases expanded by `execute_line()` before
dispatch; every invocation is recorded as a `TraceContext` span shown by
`trace list`. `HostBackendConfig.context_db_path` selects the redb file used by
`context persist`. Live-update commands use `PersistedLiveUpdateEngine`:
`block swap|rollback` records a real swap with a rollback entry, and
`update history|pending|recover` read swap records, pending rollbacks and crash
recovery. Swap output says that the swap was recorded and a rollback entry
stored; it does not claim live block code was replaced.

`wd`, `bridge`, `auth` and `fm` run through injection seams from
`aios-console/src/control.rs` (`WatchdogControl`, `BridgeControl`,
`AuthProvider`, `FileManagerControl`). The `aios` binary implements the first
three in `aios/src/console_control.rs` over the shared watchdog, the bridge
listener's graceful-shutdown channel and `AuthStore`, so `bridge stop` requests a
graceful shutdown and `bridge start` spawns the listener again. `wd reset` resets
the shared watchdog through `WatchdogControl::reset` and only then increments the
bounded restart counter. `aios-tui` injects a `FileManagerConsoleControl`
(`aios-fm/src/console_control.rs`) over its live `FileManager`, so `console fm
jobs`/`fm cancel <id>` operate on real background transfers. `aios-shell` injects
its `JobTable` as a `JobControl` seam, so `console jobs`/`fg <id>`/`bg <id>` see the
same live background processes as the bare builtins and deliver a real `SIGCONT`.
Without an injected adapter these commands report `unavailable`; the remaining
commands still report `ERR unavailable`, but with a concrete reason from
`aios-console/src/unavailable.rs`:

| Command | Why it is unavailable |
|---------|-----------------------|
| `jobs`, `fg`, `bg` | no host-owned job table with OS process groups, signals or TTY foreground in `aios-process-mgr` (`JobControl` supplies it in `aios-shell`) |

The table is host-only and describes a `HostBackend` with no injected control
seams. `console fm jobs` and `console fm cancel <id>` become real as soon as a
live `FileManagerControl` is injected: `aios-tui` does this after starting its
`FileManager`, mapping the real job snapshot to console text and cancelling the
actual `Progress` token. Likewise, `console jobs`/`fg <id>`/`bg <id>` become real
with a live `JobControl`: `aios-shell` shares its process table with the bare
builtins, lists live jobs and resumes the requested id. `console
state extract|restore <block>` is real on a host: it snapshots the live linear
memory of an executed WASM block and restores it on request; restore requires a
prior extract in the same session and a live instance (`block run <block>` first).
Host `console net recheck|apply|adapters|lease` are now real: `NetManager`
queries the OS, refreshes the link/lease snapshot and prints adapter rows. The
kernel backend uses the same `net` command names over its own settings; its
`net recheck` calls `crate::net::recheck()` and prints
`network recheck requested`.

The generic `has no host backend yet` message is kept only for commands outside
this investigated set.

`console acl grant|revoke <cap> <path>` is real on a host: it stores a
path-scoped rule in `aios-vfs::security::PathAcl` (rights `vfs:read` /
`vfs:write`, `AIOS://` / `HOST://` schemes or an absolute `AIOS://` path) that
`AiosVfs` enforces after canonicalization; the console prints the rule count and
the sorted policy, so a child revocation overrides a parent grant. Before the
first rule for a right that right stays open, and `HostBackend` shares one
`PathAcl` with `vfs`. `console ping <host>` is real too: `aios-net::icmp::ping`
sends ICMPv4 echo requests (Windows `IcmpSendEcho`, Unix ICMP datagram/raw
socket) and prints `PING <host> (<addr>): N data bytes`, one line per reply,
`X packets transmitted, Y received, Z% packet loss` and, when replies arrive,
`round-trip min/avg/max = ... ms`. It accepts 1..16 requests and a 1 ms..60 s
timeout; IPv6 hosts and a missing ICMP socket are reported as backend errors.

`metrics` prints a counter snapshot built from real runtime state; pass `--json`
for a structured object or `--prometheus` for the exposition text format.
Counters cover uptime, process states, crash events, RAM, block states, restarts
and the session log. `health` reports `HEALTHY`, `DEGRADED` or `UNHEALTHY` with
one line per detected issue: suspended or crashed processes, Frozen/Error
blocks, recorded crash events, RAM above 85% and the restart limit.

`telemetry` uses the `aios-context` ring:
`telemetry record <metric> <value> [--ram=MB] [--block=ID] [--process=name]`
stores a sample (RAM defaults to the scheduler's current usage); `query
[metric]`, `latest`, `avg [metric]`, `peak` and `clear` inspect or reset it.

`ipc` operates on the same `IpcBus` as the HTTP bridge: `stats` shows queue
depth, sent/received/dropped counters and average latency; `send <src> <dst>
[text] [--priority=N]` queues a packet, `peek` shows the front packet, and
`reroute`, `flush`, `freeze` and `unfreeze` manage the queue. Frozen packets are
held by the console until `unfreeze`.

`sys info` prints a hardware inventory from `aios-hal` (CPU, SIMD flags, memory,
GPU/NPU and device counts) plus the derived AI tier; `sys tier` prints only the
tier, its description, model limit and batch size. `hw detect` shows the cached
snapshot, `hw rescan` discards it and forces a fresh HAL detection, and
`hw profile [name]` selects a deterministic mock profile (`modern`, `legacy`,
`legacy-2012`, `nvidia`, `intel-meteor-lake`, `qualcomm-x-elite`) for reproducible
inspection. `hw fingerprints` lists the USB, PCI and NVMe lookup keys, driver
ids and class codes extracted from that profile. These commands describe
hardware, unlike `status`, which reports runtime state.

`source <file>` runs a UTF-8 script through the same dispatcher. Blank lines and
`#` comments are skipped; the first non-zero exit code is reported while later
lines still run; `clear`/`exit` controls from a script reach the frontend, and
nested `source` calls are capped at 8 levels. Path arguments preserve Windows
separators, so `source C:\Users\script.txt` and
`block load C:\blocks\mod.wasm` work unchanged.

In the `aios` shell tab the new backend is available through the `console`
prefix, for example `console process spawn worker --ram 64`. Bare `clear`/`cls`
are routed through the same path (case-insensitively): they emit no lines and
clear the shell buffer exactly as before, and are recorded in the unified
console history. Bare `help`, `?`, `ps`, `blocks`, valid `kill`, `logs`, `status`,
`restart`, `store list`, the fixed `kill`/`spawn`/`store`/`net` usage lines and
unknown commands also use exact-output console adapters, so their text is
unchanged, as do bare `cluster` commands over the shared `DistributedScheduler`
and bare `spawn <path>` (which records legacy block-load log lines but prints no
shell lines). Valid `store search`/`store install` and `net get`/`net set` also
use console formatters while keeping the async store task and IPC router; no bare
command is handled by the legacy dispatcher in normal use. Bare `restart` still
re-probes hardware, but its output and log lines come from `aios-console`.
`console help` lists the canonical commands; `console clear` and `console exit`
keep their existing behaviour.

The standalone `aios-shell` accepts the same prefix, case-insensitively:
`console ps`, `CONSOLE metrics --json` and bare `console` (help) all reach the
same `HostBackend` through `execute_line()`. Aliases are not expanded on the
`console` path, so a shell alias named `console` cannot shadow it. `console clear`
clears the terminal and `console exit <code>` exits the shell. All bare POSIX
builtins (`help`/`?`, `clear`/`cls`, `pwd`, `echo`, `history`, `alias`, `unalias`,
`cd`, `env`/`export`/`unset`, `source`/`.`, `jobs`/`fg`/`bg`) now go through
`execute_shell_adapter` with `aios-console` text formatters; the exact legacy
text (including the 60-column ANSI help banner) is preserved by parity tests.
The frontend still owns the real side effects: history/alias files, `$VAR`
expansion, environment mutation, `OLDPWD`/current directory, sourced-file
execution, PID pruning, `SIGCONT` delivery, job listing and screen I/O. A
recursive `source` reports an error instead of panicking. Canonical
`console jobs`/`fg`/`bg` now use that same job table through the injected
`JobControl` seam; a bare `HostBackend` without the seam still reports
`unavailable`. Only inherently frontend-side lines remain on the legacy path:
external programs, GUI/TUI launchers, `logs`/`hw`/`services`,
`type`/`exec`/`which`, `aios-shell-reload` and `exit`.

The kernel shell uses the same shared, case-insensitive `console <...>` prefix:
`CONSOLE VER` and `CoNsOlE echo MiXeD Case` reach the backend, while bare legacy
commands keep their old case-sensitive spelling. Bare or whitespace-only
`console` prints generated help. Its backend implements
`help`, `echo`, `version`, `kernel info|ver`, `faults`, `fault resolve`,
`wifi status|sim|real|scan|connect|disconnect`, `udp`, `http` and `socktest`;
other commands return `ERR unavailable`. Both `console clear` and bare `clear`
clear the shell scrollback.

Bare kernel TUI commands use the same text through
`aios-kernel/src/tui.rs::execute_kernel_adapter` and the `no_std` formatters in
`aios-console/src/legacy.rs`: `help`, `ver`, `info`, `faults`, `wifi` with
`sim`/`real`, `echo`, `clear` and unknown commands keep their exact legacy text,
verified byte-for-byte against `run_command_legacy`. `tabs`, `gui`, `tui`, `udp`,
`http` and `socktest` still use the legacy path; they are deferred in the
canonical registry.

---

## TUI (`aios-tui`)

### Launch

```bash
cargo run --bin aios-tui
```

### Layout

Far Manager / Midnight Commander style: top status bar, dual 50/50 panels (tab content left, events right), command prompt, and F-key strip at the bottom.

```
┌───────────────────────────────────────────────────────────────┐
│ AIOS v0.1.0  │ Overview  │ Status: OK  │ Uptime ...  │ Tier  │  ← Status bar
├──────────────────────────┬────────────────────────────────────┤
│                          │                                    │
│     Active tab content   │           Events log              │  ← Main (dual)
│     (55% width)          │           (45% width)             │
│                          │                                    │
├──────────────────────────┴────────────────────────────────────┤
│ AIOS> _                                                        │  ← Prompt
├───────────────────────────────────────────────────────────────┤
│  1Help  2Tabs  3View  4Edit  5Copy  6Move  7Mkdir  8Del  9Sort 10Rescan │  ← F-key bar
└───────────────────────────────────────────────────────────────┘
```

### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `1`-`8` | Switch tab (Overview / Processes / Blocks / Metrics / Deps / Web / Shell / Files) |
| `Alt`+`1`-`8` | Switch tab even while typing in the Shell command line or the Web URL bar |
| `j` / `k` | Navigate down / up in current list |
| `K` | Kill selected process |
| `U` | Unload selected block |
| `L` | Load block from disk (prompts for name + version) |
| `H` | Hot-swap block binary |
| `r` | Refresh data |
| `s` | Show telemetry |
| `x` | Show status |
| `W` | Reports that one GUI is supported — the native `aios-kernel` desktop; no host GUI process is launched |
| `q` | Quit (all tabs except the Shell tab, where `q` is typed into the command line) |
| `F10` / `Alt`+`q` | Quit from **any** tab (including Shell and Files) |
| `F1` or `?` | Toggle help overlay |
| `:` | Enter command mode in Shell tab |
| `Enter` | Execute command in Shell tab |
| `↑` / `↓` | Navigate command history in Shell tab |

### Tabs

- **Overview**: System info (CPU, GPU, storage) + activity log
- **Processes**: Table with PID, Name, Priority, State, RAM, CPU, Crashes + detail panel
- **Blocks**: Table with ID, Name, Version, State, Size + load/unload/hot-swap operations
- **Metrics**: RAM gauge, priority distribution, RAM history chart
- **Deps**: Dependency graph table + load order chain
- **Web**: Omnibox (search query or URL), page text content (headings highlighted), scrollable links list — pages load in the background and up to 20 recent pages are cached, so going back with `b` is instant
- **Shell**: Interactive command line with command history, fetch, search, open, clear commands
- **Files**: Two-panel file manager (Volkov/Far style) over the `aios-vfs` virtual file system — navigate `AIOS://` and `HOST://`, copy/move/delete/mkdir/rename with background progress, AI file preview, capability-gated host access

### Files Tab Keys (when Files tab is active)

The Files tab shows two panels (left/right). The active panel is highlighted in the header. Files start in the `AIOS://` sandbox; press `g`/`w` to grant the `HOST://` read/write capabilities, then navigate to `HOST://` for real host paths.

| Key | Action |
|-----|--------|
| `Tab` | Switch the active panel |
| `↑` / `k` / `↓` / `j` | Move selection up / down |
| `Enter` | Open the selected directory, or AI-preview the selected file |
| `Backspace` | Go to the parent directory |
| `F3` / `o` | AI-preview the selected file |
| `F5` | Copy the selected item to the other panel |
| `F6` | Move the selected item to the other panel |
| `F7` | Create a directory (type the name, `Enter` confirms, `Esc` cancels) |
| `F8` | Delete the selected item |
| `F2` | Rename the selected item (type the new name, `Enter` confirms) |
| `F9` / `s` | Cycle the sort rule (Name / Size / Date / Type) |
| `g` | Grant the `vfs:host:read` capability |
| `w` | Grant the `vfs:host:write` capability |
| `r` | Refresh the panels |
| `q` | Quit the TUI (also `F10` / `Alt`+`q`) |
| `Esc` | Close the AI preview / cancel the input dialog |

### Web Tab Keys (when Web tab is active)

The omnibox accepts either a full URL (`example.com`, `https://...`) or a plain search query (`how does AIOS work`) — queries are searched via DuckDuckGo automatically. After you press `Enter` the omnibox loses focus, so you can immediately navigate the results with `j`/`k` and open a link. Fetches run in the background (the TUI stays responsive) and recent pages are cached, so `b` back-navigation is instant. The links window scrolls with your selection and shows the visible range in its title. A **navigation sidebar** on the left shows the current page (marked `▸`) and the visit history; focus it with `\` and jump to any past page.

| Key | Action |
|-----|--------|
| `g` | Focus omnibox |
| `Enter` | Search / navigate (when omnibox focused); the omnibox auto-unfocuses after `Enter` |
| `o` / `Enter` | Open selected link (when omnibox not focused) |
| `j` / `k` | Move link selection down / up |
| `b` | Go back to the previously visited page |
| `B` | Open the current page in the **full native browser** window (WebView2 — JS/CSS/images); the window is reused across presses |
| `n` | Open the selected link in the native browser window |
| `u` / `d` | Scroll page text up / down by 1 line |
| `PageUp` / `PageDown` | Scroll page text up / down by 20 lines |
| `\` | Toggle navigation sidebar focus (history list) |
| `j` / `k` | Move sidebar selection down / up (when sidebar focused) |
| `Enter` / `o` | Open the selected sidebar entry / reload the current page (when sidebar focused) |
| `Esc` | Unfocus omnibox or sidebar |

### Shell Tab Keys (when Shell tab is active)

| Key | Action |
|-----|--------|
| `:` | Enter command mode (the prompt switches to `AIOS:cmd>`) |
| any printable key | Types into the command line and switches the prompt to command mode |
| `Enter` | Execute command |
| `↑` / `↓` | Navigate command history |
| `Esc` | Clear the input line and leave command mode |
| `F10` / `Alt`+`q` | Quit the TUI |

### Shell Commands

| Command | Arguments | Description |
|---------|-----------|-------------|
| `fetch` | `<url>` | Download and load a block from a URL (runs in the background) |
| `search` | `<query>` | Web search via DuckDuckGo (runs in the background) |
| `open` | `<query or url>` | Open/search on the Web tab |
| `intent` | `<request in natural language>` | Translate a request into an IPC intent and show the plan (see below) |
| `clear` | — | Clear shell output |
| `ps` / `processes` / `list` | — | List running processes |
| `blocks` / `ls` | — | List loaded blocks |
| `kill` | `<pid>` | Kill a process |
| `spawn` | `<name> [prio] [ram_mb]` | Spawn a process (prio: 0-4) |
| `load` | `<name> [version]` | Load a block |
| `unload` | `<id>` | Unload a block |
| `status` / `info` | — | System status |
| `logs` | — | View safe-mode event log |
| `restart` | — | Restart the orchestrator |
| `store` | `<sub>` | Block store management (see below) |
| `net` | `<sub>` | Network settings (see below) |
| `help` / `?` | — | Show all available commands |
| `console` | `<command> [args]` | Unified console command (`console help` lists canonical commands) |
| `exit` | — | Exit the safe-mode shell |

The `console` prefix is case-insensitive through the shared
`aios_console::frontend::console_prefix` helper and is matched before the TUI
extras and the `SafeModeShell` fallback, so an existing command named `console`
cannot shadow it. Bare or whitespace-only `console` prints generated help.
`console clear` clears the shell output and `console exit [code]` closes the TUI
through the normal shutdown path. `console ps` and `console block list` share the
same `Arc<Mutex<...>>` `Scheduler` and `BlockRegistry` as the dashboard and
`SafeModeShell`, so they observe the live TUI runtime. Bare `ps`, `processes`,
`list`, `blocks`, `ls`, `status`, `info`, `exit`, `quit`, `q`, `help`, `?`, `logs`,
`log`, `kill`, `spawn`, `load`, `unload`, `restart` and `recover` now use the
unified `HostBackend` through exact-output adapters; their text is byte-for-byte
identical to the legacy safe-mode shell. The mutation adapters run real
scheduler/registry operations through the shared handles and append successful
legacy events to the shared log, which `logs`/`log` then reads exactly as before;
failures keep the legacy `Error: Failed: <error>` wording, except the restart
limit (`Max restarts (<max>) reached. Manual intervention required.`, printed as
`Error: Max restarts ...`). `restart`/`recover` use the host restart counter
instead of `WatchdogControl::reset`, so only canonical `wd reset` calls the
injected watchdog seam. Unknown commands are rendered by
`legacy::unknown_text` from the verbatim input (including case and whitespace)
and never execute `SafeModeShell`. The help adapter keeps the separate TUI
extras block. Bare `exit`/`quit`/`q` still only print the legacy line — use
`console exit` to close the TUI. Canonical `console info` stays ambiguous — only
the bare legacy alias is adapter-backed. All safe-mode bare commands and the
unknown-command fallback are adapter-backed; TUI-specific commands (`intent`,
`fetch`, `search`, web/network controls) keep their own handlers.

Network commands never block the UI: `fetch`, `search`, `store search`, `store install` and `store update` run in a background worker and their result is printed in the shell output when it arrives. While such a task is running the shell prints `Task already running: <label>` if another one is started.

### Intent Shell Command

`intent <request in natural language>` translates a request with the built-in `IntentEngine` and prints the resulting plan:

```
AIOS:cmd> intent free up memory
  intent: Free up memory
  plan: Optimizing memory: currently 4096MB/16384MB used. Throttling background processes and freeing caches.
  packet: AdjustPriority -> block 3 (from block 0)
  note: translation only — the kernel AI console executes intents
```

The standalone TUI has no message router, so the command only **translates** the request: it shows the matched intent, the plan, the IPC command and the target block, but does not execute anything. The kernel TUI (AI Console) executes intents over the bridge. When nothing matches, the shell prints `no intent matched` and a list of example phrasings.

### Store Shell Commands

The block store fetches blocks from three kinds of sources: GitHub (`github:owner/repo`), a local directory (`local:path`) and an HTTP update service (`http://host:port`). Installations land in `AIOS_BLOCKS_DIR` as `{name}_{version}.wasm` + sidecar JSON; updates are backed up (`.bak`) and auto-rollback on failure.

The kernel loads two kinds of block at boot: the compiled-in **base set** (`hal`, `browser`, `net_settings` — built into the binary through the block factory) and **disk blocks** discovered under `AIOS_BLOCKS_DIR` and executed block by block. Store-installed blocks therefore appear in the same `Blocks & Svc` tab as the base set; the loader summary line there reports `base`/`disk`/`failed`/`live` counts. A disk block that exports `handle_message` is registered on the message router and is dispatchable over IPC exactly like a base block; the boot log reports how many were routed.

| Command | Arguments | Description |
|---------|-----------|-------------|
| `store list` | — | List installed blocks (newest version per name) |
| `store sources` | — | List configured sources |
| `store add-source` | `<github:owner/repo\|local:path\|http://url>` | Register a new source |
| `store search` | `<query> [--source NAME]` | Search the source catalog |
| `store install` | `<name> [--source NAME]` | Install a block (newest version) |
| `store update` | `[name] [--source NAME]` | Check for and apply updates (auto-rollback on failure) |
| `store uninstall` | `<name>` | Remove every installed version of a block |
| `store rollback` | `<name>` | Restore the previous version from backup |
| `store publish` | `<file.wasm> [name] [version] [--key <secret_hex>]` | Publish a local wasm file to the running update service via `POST /api/v1/store/publish` (bridge port from `AIOS_BRIDGE_PORT`, default `8080`); the file's SHA-256 is verified server-side. With `--key` (or `AIOS_STORE_SIGNING_KEY`) the manifest is Ed25519-signed before upload and the bridge verifies the signature (and its local trust policy) before installing |
| `store trust` | `<source> [--key <public_hex>] [--clear]` | Manage a source's trusted keys: `--key` adds a 32-byte public key, `--clear` removes all of them; with no flags prints the current trusted keys. A source with trusted keys only accepts manifests signed by one of them (persisted via `store add-source` config) |
| `store sign` | `<file.wasm> [name] [version] [--key <secret_hex>]` | Sign a local wasm file with Ed25519: computes SHA-256, builds the manifest and writes the signed sidecar JSON (`{name}_{version}.json`) next to the file; the key comes from `--key` or `AIOS_STORE_SIGNING_KEY` (32 bytes, 64 hex chars); prints the public key |
| `store verify` | `<name>` | Verify an installed block: recompute the SHA-256 of the installed binary and check the Ed25519 signature of its sidecar manifest; reports `SHA-256: OK/MISMATCH` and `Signature: OK/INVALID/none` |

### Network Shell Commands

Reads/writes the network configuration through the `NetSettingsBlock`, persisted as JSON under `AIOS_DATA_DIR`/`network.json`.

| Command | Arguments | Description |
|---------|-----------|-------------|
| `net get` | — | Show the current network configuration (JSON) |
| `net set` | `key=value [key2=value2 ...]` | Apply a partial update (e.g. `hostname=myhost`, `listen_port=9090`, `dns={"primary":"1.1.1.1"}`); valid keys: `hostname`, `listen_port`, `connect_timeout_ms`, `max_connections`, `user_agent`, `allow_private_access`, `proxy` (JSON object or `null`), `dns` (object, address string, search-domain array or `null`), `interfaces` (array), plus dotted keys such as `dns.primary=1.1.1.1` |
| `net reset` | — | Restore factory defaults and persist |

### F1 Help Overlay

The F1 help overlay shows all keyboard shortcuts and shell commands as a full-screen opaque panel (it replaces the dashboard background, so nothing blends into the help text).

| Key | Action |
|-----|--------|
| `F1` or `?` | Toggle help overlay |
| `F1`, `Esc`, or `?` | Dismiss help overlay |

---

## Kernel TUI (`aios`)

### Launch

```bash
cargo run --bin aios
# or from the compiled binary:
./target/release/aios.exe

# Safe mode (skip third-party disk blocks, disable the bridge)
cargo run --bin aios -- --safe-mode
```

At boot the header and the System tab show the detected **AI Tier** (e.g. `Tier1`/`Tier2`) plus CPU/RAM; safe mode is shown as `SAFE MODE`.

The kernel TUI uses the same Far Manager / Midnight Commander style as `aios-tui`: a **status bar** (version, active tab, uptime, tier, RAM, bridge status, sys link), a **dual 50/50 main panel** (active tab left, events log right), an **`AIOS>_` prompt** (or `net>_` in network input mode), and a **F-key strip** at the bottom. The active tab name replaces the old tab strip. The `sys:` link status and bridge status now live in the top status bar instead of the old footer line.

On narrow windows the status bar text clips at the right edge (Far/MC behaviour) and the F-key strip clips too; the tab titles and `AIOS v…` version are never chopped mid-word. On very short windows the System & HW **RAM gauge** overlays nothing — it stays inside the left panel; when the window is too small for the CPU/OS blocks, those blocks naturally compress instead of the drawer breaking.

Every user-visible version surface — the boot banner and config line, panic/fatal reports, the kernel TUI status bar, About tab and `ver` command, and the GUI About window, window titles and Files preview — now reads `aios-kernel/src/version.rs`: `SEMVER = env!("CARGO_PKG_VERSION")` and `KERNEL = concat!("AIOS kernel v", env!("CARGO_PKG_VERSION"))`. `aios-kernel/Cargo.toml` is currently `2.84.0`, so every surface reports v2.84.0. It is no longer a hard-coded `v2.57.0` string or a `system.version` config value.

### Tabs

| # | Tab | Content |
|---|-----|---------|
| 1 | System & HW | CPU, RAM, OS/kernel, GPU, AI Tier, RAM gauge, **Hardware Inspector** |
| 2 | Blocks & Svc | Block list (select/restart/unload/load) + a `base [...] / disk [...] / failed / live` loader summary + process list |
| 3 | AI Console | LLM chat with slash commands |
| 4 | Studio Bridge | Bridge server status, URL, REST/WebSocket endpoints |
| 5 | Network & Store | Network settings (IPC) + installed block store |
| 6 | Web | Text-mode browser (omnibox, links, history, native viewer) |
| 7 | Shell | Command line (`ps`, `blocks`, `store`, `net`, ...) |

The **System & HW tab (1)** also embeds the **Hardware Inspector** widget: a per-bus device table (USB/PCI/NVMe/Bluetooth/ACPI) with driver status badges (`[Active]` / `[Downloading...]` / `[Compiling]` / `[Generic]` / `[Failed]` / `[Rolled Back]`), the granted capability summary per device, and a hot-plug toast strip at the bottom (**`Events`**, latest 4, fixed height — the device table never reflows as toasts arrive). After 3 consecutive failures a device auto-rolls back to the Generic Fallback Driver and a warning toast explains the switch. A background hot-plug monitor (see `docs/ARCHITECTURE.md`) reflects live unplug/replug without manual action: a removed device shows `[Hardware] ... removed -> driver cached, re-provisions on replug`, and re-plugging it provisions instantly from the cached driver. `F10` remains available for a manual full re-probe.

### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `Tab` / `F1` / `?` | Next tab / help overlay |
| `1`-`7` | Direct tab select |
| `Alt`+`1`-`7` | Direct tab select even while typing in the Shell / Web URL / AI input / net line |
| `W` | Reports that one GUI is supported — the native `aios-kernel` desktop; no host GUI process is launched |
| `Space` | Pause/resume event log |
| `L` | Toggle EN/RU input layout (the sys status line shows the active pair) |
| `F10` | Re-probe the hardware (System & HW tab refresh) |
| `q` / `Ctrl+C` | Quit |

The bottom **event log** now lives as the right-hand panel of the dual layout (45% width). It carries a `sys:` status line refreshed every 2 s: Wi-Fi state/SSID/RSSI, the active input-layout pair (`[RU/EN]`), battery + CPU temperature, and the effective LLM backend (`local`, or `cloud` after thermal throttling). Press `Space` to pause the log, or `L` to switch layouts from any tab; the same snapshot is rendered in the GUI taskbar tray and exposed over REST (`/api/v1/sys/status`).
### Blocks Tab (2)

| Key | Action |
|-----|--------|
| `j` / `↓` | Move block selection down |
| `k` / `↑` | Move block selection up |
| `r` | Restart the selected block (the original binary and version are preserved, the block is re-registered and the new id is activated) |
| `K` | Unload the selected block |
| `l` / `L` | Load a block from disk (prompts for a path) |

### AI Console (Tab 3)

The AI Console is an interactive chat with the LLM. Press `i` to enter query mode, type a message (or a `/`-command), press `Enter` to send. The footer shows the live backend/model/temperature/tokens/state. Responses **stream in live**: the growing answer is rendered in yellow while the request is in flight and the final text is appended to the transcript when done. Long responses are word-wrapped to the pane width; user prompts are shown in cyan, errors in red.

| Key | Action |
|-----|--------|
| `i` | Enter query mode |
| `Enter` | Send the query or run a slash command |
| `Esc` | Exit query mode / close help |
| `Up` / `Down` | Navigate the last 50 prompts |
| `h` | Toggle the built-in help panel |
| `q` | Quit (when not typing) |

Slash commands are typed at the input line (e.g. `/status`) and executed with `Enter`:

| Command | Arguments | Description |
|---------|-----------|-------------|
| `/help` | — | Open the help panel + command summary |
| `/status` | — | Show backend, model, API key state, parameters, detected local GGUF models |
| `/clear` | — | Clear the chat output |
| `/history` | — | List the recent prompts |
| `/system` | `<prompt>` | Set the system prompt (no argument prints the current one) |
| `/model` | `<name>` | Set the model (no argument prints the current one) |
| `/backend` | `<groq\|openrouter\|google\|micro\|full>` | Switch backend/provider; local backends reset the API key |
| `/key` | `<api-key>` | Set the API key (no argument clears it) |
| `/temp` | `<0.0-2.0>` | Set sampling temperature |
| `/tokens` | `<1-8192>` | Set max output tokens |
| `/preset` | `<name>` | Apply a prompt template as the system prompt |
| `/preset` | `<name> <text>` | Define/override a prompt template |
| `/preset` | `list` / `del <name>` | List templates / delete one |
| `/save` | — | Persist the chat to disk (`AIOS_DATA_DIR/chat.jsonl`) |
| `/load` | — | Restore the chat from disk, replacing the current transcript |

Backend/model/key changes rebuild the shared `LlmEngine` asynchronously, so the HTTP `POST /api/v1/llm/query` endpoint uses the same configuration as the console. Cloud backends need an API key (`AIOS_LLM_API_KEY` or `/key`); local backends need a GGUF model (`AIOS_MODEL_PATH`).

The chat is **auto-persisted**: after every completed reply and on quit it is saved as JSON Lines to `AIOS_DATA_DIR/chat.jsonl` (default `aios_data/chat.jsonl`), and restored into the transcript on the next boot. Built-in prompt templates: `assistant`, `code`, `translator`, `explainer` (customize or add your own with `/preset <name> <text>`).

### Network & Store (Tab 5)

| Key | Action |
|-----|--------|
| `n` | Edit network settings (input line, applied over IPC as `key=value` pairs) |
| `g` | Show the current network configuration (JSON) |
| `s` | Refresh the installed block store list |

The same network commands are available from the Shell as `net get` / `net set`. Store operations (`store list`, `store search`, `store install`) are also available from the Shell.

### Web Tab (6)

The built-in text-mode browser loads pages in the background (the TUI stays responsive). The omnibox accepts a full URL, a bare host (`example.com`), or a plain search query (searched via DuckDuckGo). A sidebar lists the links of the current page; the page text wraps to the pane width and can be scrolled. The current page can be saved as a **bookmark** (`a`) and managed in the **bookmarks panel** (`m`); bookmarks persist in `AIOS_DATA_DIR/web_bookmarks.json`. Multiple pages can be open as **browser tabs** (`t` opens a new one, `x` closes the active one, `[`/`]` switch); each tab keeps its own page, scroll, link selection and back-history. Page **JavaScript executes on load** (embedded boa engine): script errors and `console` output are collected per page and can be viewed in the **JS console panel** (`J`), and a `scripts=N errors=M console=K` line is written to the log when a page ran scripts. For JavaScript-heavy sites that serve an almost empty HTML shell, the browser automatically falls back to a headless Chromium-class render (uses `msedge`/`chromium`/`google-chrome` on `PATH` or in standard install locations; override the binary with `AIOS_HEADLESS_BROWSER`, add `--no-sandbox` with `AIOS_HEADLESS_NO_SANDBOX=1`) and shows the fully rendered content.

| Key | Action |
|-----|--------|
| `g` | Focus the omnibox |
| `Enter` | Search / navigate (when the omnibox is focused) |
| `j` / `k` | Move link selection down / up |
| `o` / `Enter` | Open the selected link |
| `u` / `d` | Scroll page text up / down by 1 line |
| `PageUp` / `PageDown` | Scroll page text up / down by 20 lines |
| `b` | Go back to the previously visited page |
| `t` | Open a new browser tab |
| `x` | Close the active browser tab (the last one cannot be closed) |
| `[` / `]` | Switch to the previous / next browser tab |
| `B` | Open the current page in the full native browser window (WebView2) |
| `n` | Open the selected link in the native browser window |
| `a` | Save the current page as a bookmark (name prefilled with the page title) |
| `m` | Toggle the bookmarks panel (replaces the links list while open) |
| — | Inside the bookmarks panel: `j`/`k` move, `o`/`Enter` open, `d` delete, `Esc` close |
| `J` | Open the **JS console panel** (script count, script errors in red, `console.log` lines; page scripts now execute on load) |
| `Esc` | Unfocus the omnibox, or close the bookmarks / JS console panel |

### Shell Tab (7)

| Command | Arguments | Description |
|---------|-----------|-------------|
| `ps` | — | List running processes |
| `blocks` | — | List loaded blocks |
| `kill` | `<pid>` | Kill a process |
| `spawn` | `<wasm-path-or-file>` | Load a block from disk |
| `store list` | — | List installed blocks |
| `store search` | `<query>` | Search the block store catalog (runs in the background) |
| `store install` | `<name>` | Install a block from the store (runs in the background) |
| `net get` | — | Show the current network configuration (JSON) |
| `net set` | `key=value [key2=value2 ...]` | Apply a partial network update |
| `cluster status` | — | Cluster overview: self node, peers (status/tier/load), remote and local processes |
| `cluster nodes` | — | List cluster peer nodes with status, tier and load |
| `cluster spawn` | `<name> [ram_mb] [priority] [target_node]` | Run a process on the cluster (placement strategy or explicit target node) |
| `cluster kill` | `<node> <pid>` | Terminate a remote process |
| `cluster migrate` | `<node> <pid> [target_node]` | Relocate a remote process to another node, carrying its state snapshot (stateful spawn-then-kill) |
| `status` | — | Uptime, bridge state, AI tier, RAM, block count |
| `logs` | — | Show the last 20 event log entries |
| `restart` | — | Re-probe the hardware profile and rescan the hardware engine |
| `help` / `?` | — | Show all available commands |
| `clear` | — | Clear the shell output |

`Esc` clears the current input line. Every keystroke on the Shell tab is captured by the input line, so `q` quits only from other tabs. Output lines longer than the panel width wrap to the next line instead of being clipped (impacting multi-byte text correctly); the shell keeps the last ~300 lines in its scrollback.

`store search` and `store install` do not block the UI: the catalog request runs as a background task and the result is appended to the shell output when it arrives. While a store task is running, a second one is rejected with `store task already running: <label>`.

`restart` re-probes the hardware profile (`aios::hw_probe::probe()`) and rescans the hardware engine (`HwEngine::rescan()`), which re-detects the attached devices and refreshes the driver/block list. It does not restart the orchestrator process.

Clustering is enabled by configuring `AIOS_CLUSTER_*` env vars (see `docs/ARCHITECTURE.md`); without `AIOS_CLUSTER_PEERS` the `cluster` commands reply `clustering disabled (set AIOS_CLUSTER_PEERS to enable)`. Remote process spawns run on real `aios-process-mgr` scheduler threads via `SchedulerProcessExecutor`.

### Safe Mode

With `--safe-mode` AIOS boots with a minimal shell only: third-party disk blocks are not discovered, the bridge HTTP/WebSocket server is disabled, and the header shows `SAFE MODE`. Core blocks, the scheduler, the watchdog, the LLM engine and the TUI/Shell remain available.

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `AIOS_BRIDGE_PORT` | `8080` | TCP port of the REST/WebSocket bridge (the Studio SPA is served from the same port) |
| `AIOS_SAFE_MODE` | unset | `1`/`true`/`yes`/`on` — same as `--safe-mode` (CLI flag and env var are OR-ed) |
| `AIOS_BLOCKS_DIR` | `blocks` | Directory scanned for disk blocks at boot |
| `AIOS_DATA_DIR` | `aios_data` | Chat history, bookmarks and other persistent state |
| `AIOS_LLM_BACKEND` | `cloud` | `cloud`, `micro-local` or `full-local`; the local backends preload the GGUF model from `AIOS_MODEL_PATH` |
| `AIOS_MODEL_PATH` | unset | Path to a local GGUF model (Qwen2/Qwen3) preloaded by the local LLM backends |
| `AIOS_LLM_API_KEY` | unset | API key for the cloud LLM backends |

---

## Native kernel desktop (`aios-kernel`, AIOS Desktop)

### Launch

The desktop is the boot screen: `idle_loop()` calls `gui::enter()` as soon as
the scheduler is live. There is no host process to start.

```bash
cargo build --manifest-path aios-kernel/Cargo.toml --target x86_64-unknown-none
cargo run --manifest-path aios-kernel-run/Cargo.toml       # Limine/QEMU smoke boot
```

`Esc` with no focused window or `Start → Консоль ядра` drops to the kernel TUI
diagnostic screen; the shell commands `tui` / `gui` switch between that fallback
and the desktop. Running `cargo run --bin aios-gui` is **not** part of this path
(see the legacy host section below).

### Layout

```
┌──────────────────────────────────────────────────────────────┐
│                                                              │
│                      AIOS Desktop                            │
│           (functions open as separate windows)               │
│                                                              │
│   ┌─────────────────┐   ┌──────────────────────┐             │
│   │ WASM Blocks F2  │   │ System Dashboard F1  │             │
│   └─────────────────┘   └──────────────────────┘             │
│                                                              │
├──────────────────────────────────────────────────────────────┤
│ ☰ Пуск │ System Dashboard │ WASM Blocks │ …                 │  ← Taskbar
└──────────────────────────────────────────────────────────────┘
```

### Start menu

`☰ Пуск` at the bottom left opens a sorted panel with a search field. A row
shows a title and a hint (`F1 · Система`, `ring-3 · клиент`, `Система · задачи`,
`Программа · calculator`). The catalog joins built-in windows, the nine `F1`-`F9`
functions, real in-kernel apps, live ring-3 clients and the system actions
`Ring-3 демо` / `Обновить всё` / `Консоль ядра`. The `Ring-3 демо` entry starts
the five dormant ring-3 tasks once (it disappears while they run); the normal
desktop boots with them off. Search matches title and hint case-insensitively
in Latin **and** Cyrillic (`систем`, `net`, `F5`). The panel shows ten rows at a
time and scrolls with ↑/↓ or the mouse wheel.

### In-kernel apps

`Пуск → App Store` (`F4`) opens a six-program catalog: ↑/↓ selects a row, Enter
or the `Run` button launches it. The Calculator accepts the same keys on the
keyboard or by clicking its 4×4 grid: digits, `+ - * /` (`x` also multiplies),
`C`, Backspace and `=`/Enter; `12-3=9` is the serial-proven result. Files lists
six live `system/...` rows selectable with ↑/↓ or a click, and the Text Editor
edits six ASCII lines (Enter moves to the next line, ↑/↓ or a click selects a
line). Terminal opens the kernel console, and `Настройки сети` opens the same
Network window as `F5`. Only browser/WebView/AI Studio/voice entries remain
honest host-only notices.

### Keyboard shortcuts

| Key | Action |
|-----|--------|
| `Win` / `☰ Пуск` | Toggle the Start panel |
| `↑` / `↓` | Move the Start, App Store, Files or Editor selection |
| `Enter` | Open a Start entry, run an App Store row, compute in Calculator, or move to the next Editor line |
| `Esc` | Close Start; with no focused window, leave to the kernel TUI |
| `F1`-`F9` | Open/focus System Dashboard, WASM Blocks, AI Studio, App Store, Network Settings, Dependencies, Native Browser, Files, Hardware & Drivers |
| `F10` / `F11` / `F12` | Maximize/restore, close, minimize the focused window |
| `Tab` | Cycle focus between open windows |
| Printable keys | Type into Start search, Calculator, Editor or the focused window |

Win and arrow navigation work on both PS/2 and USB keyboards: PS/2 queues the
make byte after `0xE0`/`0xE1` into the same FIFO as USB, xHCI maps HID usages
`0x49..=0x52` to Set-1 codes, and `gui::handle_scancode()` routes them to Start
or the focused app. Crash and modal dialogs still own the keyboard first, so
those codes can never open Start over a blocking dialog.

### Mouse

- Click `☰ Пуск`, type a query, then click a row (or use ↑/↓ + Enter).
- Click a taskbar button to focus or minimize its window; buttons follow a stable
  order and appear only for open windows.
- Drag a title bar to move a window; drag an edge/corner to resize it; use the
  right-anchored minimize / maximize / close plates or `F10`-`F12`.
- Click widget plates and text fields inside windows (Network, Task Manager,
  Widget Lab); the kernel-internal clipboard works with `Ctrl+C`/`Ctrl+X`/`Ctrl+V`.
- In in-kernel apps, click Calculator buttons, the App Store `Run` button, or a
  Files/Text Editor row to move the selection.

### Rendering and limits

Text is drawn by the `no_std` framebuffer renderer. ASCII and Cyrillic use the
same char-aware helpers; the generated `font8x8::CYRILLIC` table covers Russian
titles, hints, fields and notifications. Glyphs advance by `TEXT_ADVANCE`
(16 px glyph plus a 2 px gap), so neighbouring letters no longer touch. Every
surface — window, Start panel, task bar, dialog — installs its own text clip;
labels that do not fit are cut with `...` (task bar, Start entries) or bounded by
the window edge, so no message paints outside its own rectangle. The Network
window is `420x400` with its Wi-Fi rows on the same 16 px grid, and button/checkbox
plates are sized for the wider advance. Windows are painted into a RAM
backbuffer and only dirty rectangles are blitted to the scanout buffer, so there
is no flicker or tearing.

The kernel is bare metal: there is no process launcher, WebView, audio stack,
voice recognition or LLM backend. Calculator, App Store, Files, Text Editor and
the Terminal/Network launchers are native `no_std` windows; browser, WebView,
voice control and remote AI Studio entries remain honest notice windows. The
palette is fixed in `aios-kernel/src/gui.rs`; `aios-gui/src/theme.rs` applies
only to the legacy host tree.

---

## Legacy host GUI (`aios-gui`, reference only)

### Launch

```bash
cargo run --bin aios-gui
```

This crate is kept as a historical reference for the egui/eframe desktop model
(Start catalog, window state, theme). It is **not** the AIOS Desktop: neither the
kernel nor the host TUIs launch it, and `W` no longer starts a host GUI process.
The sections below describe that legacy host UI only; their window features,
voice control, WebView and host process actions are not available in the native
kernel desktop described above. The native window title was `AIOS Desktop v1.0.0`.

### Layout

```
┌──────────────────────────────────────────────────────────────┐
│                                                              │
│                      AIOS Desktop                            │
│           (functions open as separate windows)               │
│                                                              │
│   ┌─────────────────┐   ┌──────────────────────┐             │
│   │ WASM Blocks F2  │   │ System Dashboard F1  │             │
│   └─────────────────┘   └──────────────────────┘             │
│                                                              │
├──────────────────────────────────────────────────────────────┤
│ ☰ Пуск │ ■ System Dashboard │ □ WASM Blocks │ … │ 🎤 …     │  ← Taskbar
└──────────────────────────────────────────────────────────────┘
```

### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `F1`-`F9` | Open the matching desktop window (focus an already-open window) |
| `j` | Move selection down in the focused Blocks / App Store window |
| `k` | Move selection up in the focused Blocks / App Store window |
| `Escape` | Close the Start menu |

The taskbar tray shows the effective AI tier, RAM and IPC counters, Wi-Fi state/SSID, the active input-layout pair, battery/CPU temperature and the effective LLM backend (`local`/`cloud`), refreshed every 2 s; the same data feeds the TUI `sys:` log line and `/api/v1/sys/status`.

### Mouse

- Click `☰ Пуск` at the left of the taskbar to open the Start menu; type in its search field to filter
- Click a Start entry to open a built-in window or launch a program
- Click a taskbar button to focus or minimize its window
- Click a window title to focus it; use its close button to close it
- Click rows in tables to select, buttons for actions (Kill, Suspend, Load, etc.), and type in search boxes (App Store)

### Voice control

The right-hand side of the taskbar (system tray) has a `🎤 Голос` button (it
turns into `🎙 Слушаю...` while the microphone is open) and a command text field.
The same catalog powers the external-program entries in the Start menu.

- **Recording**: the first click opens the default input device via `cpal`; the
  second click stops it. Audio is downmixed to mono, resampled to 16 kHz and
  written to `AIOS_DATA_DIR/voice.wav`.
- **Transcription**: the WAV is passed to an external STT engine — the
  `AIOS_STT_CMD` command line if set, otherwise the first of `whisper-cli`,
  `whisper`, `main`, `vosk-transcriber` found on `PATH`. whisper.cpp timestamp
  prefixes are stripped automatically.
- **Text fallback**: when no engine is installed the button reports it and the
  text field accepts the command directly (Enter submits), so the feature still
  works without a speech engine.
- **Actions**: the transcript is matched case-insensitively (Russian and
  English keywords) and dispatched:

| Phrase | Action |
|--------|--------|
| `калькулятор` / `calculator` | `galculator`, `gnome-calculator`, `kcalc`, `xcalc`, `calc` |
| `настройки` / `settings` | `gnome-control-center`, `systemsettings`, `xfce4-settings-manager` |
| `браузер` / `browser` | `firefox`, `chromium`, `google-chrome`, `epiphany` |
| `терминал` / `terminal` | `xterm`, `gnome-terminal`, `konsole`, `alacritty`, `foot` |
| `файлы` / `files` | `thunar`, `pcmanfm`, `nautilus`, `nemo`, `dolphin` |
| `звук` / `volume` | `pavucontrol`, `alsamixer` |
| `сеть` / `network` | `nm-connection-editor`, `gnome-control-center` |
| `редактор` / `editor` | `gedit`, `mousepad`, `leafpad`, `kate` |
| `скриншот` / `screenshot` | `gnome-screenshot`, `spectacle`, `scrot` |
| `обои` / `wallpaper` | rotates an image from `/usr/share/backgrounds`, `/usr/share/wallpapers`, `/usr/share/pixmaps` or `AIOS_WALLPAPERS` via `feh`/`xwallpaper`/`hsetroot`/`nitrogen` |
| `перезагрузка` / `выключение` | not executed automatically — the GUI points to the system menu (root required) |

The result (what was launched or why it failed) is shown in the taskbar tray and
appended to the activity log.

### Desktop Windows

#### System Dashboard (F1)
- **Stat cards**: RAM (used/total), Blocks count, Processes count, Watchdog status
- **System panel**: CPU model, cores, threads, AVX2/AVX-512/SSE4.2, GPU, storage, detected HW Tier
- **RAM Usage**: progress bar + sparkline chart (last 60 samples)
- **Priority Distribution**: bar chart (Background / Low / Normal / High / Critical)
- **Processes**: table (PID, Name, Priority, State, RAM, CPU ms, Crashes) + Refresh, Kill, Suspend, Resume
- **Activity Log**: scrollable log with color-coded messages

#### WASM Blocks (F2)
- **Loader summary**: a boot-time report line under the toolbar — `base [...] | disk [...] | failed N | live M` — mirrors the TUI Blocks summary (`live` is the number of resident registry blocks; the GUI runtime does not execute WebAssembly)
- **Table**: ID, Name, Version, State (badge), Size, Dependencies
- **Actions**: Refresh, Load Block (2-step dialog), Unload, Hot-Swap
- **Load Dialog**: Step 1 — enter block name, Step 2 — enter version, Enter/Cancel

#### AI Studio (F3)
- **Chat panel**: message list, **streaming replies** (live yellow partial line while in flight), error highlights
- **Input**: type a message or a slash command, `Enter` (or the Send button) submits; focus stays in the input
- **Commands**: `/help /status /clear /history /system /model /backend /key /temp /tokens /preset /save /load` (same grammar as the kernel TUI AI Console)
- **Status line**: backend, model, temperature, token budget, busy/error state
- **Async model**: responses stream over a background tokio task; the UI stays responsive, partial text appears live
- **Persistence**: the chat auto-saves to `AIOS_DATA_DIR/chat.jsonl` after every completed reply and on window close (restored at boot); `/preset` templates persist to `AIOS_DATA_DIR/presets.json`

#### App Store (F4)
- **Search box**: filter by name, description, or tags
- **Table**: Name, Version, Author, Status (badge), Downloads
- **Actions**: Install, Update, Uninstall
- **Status bar**: shows operation result

#### Network Settings (F5)
- **Form**: hostname, listen port, connect timeouts, private access toggle, DNS, user agent
- **Actions**: Save (applies a partial JSON update over IPC to `net_settings`), Reset (restores defaults)
- **Preview**: live JSON of the config being edited

#### Dependencies (F6)
- **Summary**: block count + edge count
- **Load Order**: visual chain (block A → block B → block C)
- **Table**: Block name, Depends On, Depended By

#### Native Browser (F7)
- **Omnibox**: type a full URL (`https://...`), a bare host (`example.com`), or a plain search query (`rust scheduler`) — Enter resolves and loads it
- **Navigation buttons**: Back, Forward; **Open Browser** / **Close** toggle
- **Native engine**: the browser window is a real WebView (WebView2 / WebKitGTK / WKWebView) with full cookies, JavaScript and history support; the first navigation opens the window automatically
- **Non-blocking open**: the WebView is spawned on a background thread, so the desktop stays responsive while it starts; repeated open attempts during startup are ignored and the status line reports `Opening browser: ...` / failure
- **Status line**: shows the resolved target or the last action/error

#### Files (F8)
- **Toolbar**: Refresh, Switch panel, Sort, Up, Mkdir, Rename, View, Copy, Move, Delete, HOST r / HOST w (grant `vfs:host:read` / `vfs:host:write` capabilities)
- **Two panels**: navigate `AIOS://` (sandbox) and `HOST://` (real paths); single click selects and activates the panel, double-click opens a directory or AI-previews a file
- **Mkdir / Rename dialog**: text field + OK / Cancel (Enter / Esc)
- **AI Preview**: collapsible panel with the smart preview of the selected file;
  its version line is read from `version::KERNEL` in `aios-kernel/src/gui.rs`, so
  it shows the same v2.84.0 as the About window and window titles
- **Jobs**: background copy/move/delete operations show live progress bars and completion status
- **ACL line**: shows which HOST capabilities are currently granted

#### Hardware & Drivers (F9)
- **Toast strip**: hot-plug / provisioning events (`[Hardware] Detected USB 046D:0825 -> Fetching WASM Driver... [OK]`) in color-coded Info/Success/Warn/Error
- **Summary table**: Bus, Device, VID:PID, Driver, Source, Status (color-coded badge), Capabilities — 100% parity with the TUI Hardware Inspector
- **Progress bars**: per-device Downloading / Compiling progress rendered inline
- **Per-device details**: driver id, last error, and the interactive **security capability matrix** (checkboxes per `Capability`; any change applies immediately as a per-device override)
- **Actions**: `Rescan` (re-detect hardware), `Update Driver` (re-fetch/re-provision), `Rollback to Generic`, `Uninstall` (disabled for the protected Generic Fallback Driver)

#### Live kernel runtime (v2.31.2)

Since v2.31.2 the desktop windows display **real** kernel state instead of placeholder data:

- **Refresh is automatic**: process list, memory usage, IPC packet count and the watchdog indicator are polled from the `GuiRuntime` every 250 ms; the Start menu entry `Обновить всё` re-triggers every data source at once.
- **Processes are explicit**: no demo processes are seeded at startup. The scheduler starts empty; processes appear only when spawned explicitly, and `Приостановить всё` suspends every process currently tracked by the scheduler.
- **Process actions are real**: Kill / Suspend / Resume operate on live scheduler threads (cooperative `TerminateFlag` / `SuspendFlag` on real OS threads).
- **Hot-Swap is real**: the WASM Blocks window reads the installed store copy (path shown in the dialog), performs a SHA-256 verified live-update swap + registry `swap_binary`, and appends an entry to the **Swap history** collapsible panel (old/new version + signature + timestamp + result).
- **App Store is real**: Install / Update / Uninstall write and delete real `<name>_<version>.wasm` files and registry entries in the local `official` marketplace; Status badges switch `Available → Indexed → Installed`.
- **Watchdog indicator**: `WD:` shows the real watchdog state (countdown, `Monitoring/Suspended/Recovering`, safe-mode).

### Taskbar and Tray

The bottom taskbar has `☰ Пуск` on the left, a button for each open function window (click to focus or minimize it), and a right-hand tray with the voice mic button, the manual command field, the AI tier, `RAM used/total`, the live IPC packet counter and the system status line (Wi-Fi, layouts, battery/CPU temperature, LLM backend).

### Theming

Dark theme with customizable colors in `aios-gui/src/theme.rs`. These colors
apply only to the legacy host UI; the native desktop palette is compiled into
`aios-kernel/src/gui.rs`:

| Color | Usage |
|-------|-------|
| `accent` (#00C8DC) | Headers, focused window, highlights |
| `success` (#32C850) | Running blocks, OK status, positive values |
| `warning` (#F0B41E) | Suspended, High priority, warnings |
| `danger` (#E63C3C) | Crashed, Critical, errors |
| `info` (#64A0FF) | Low priority, informational |
| `muted` (#A5A5B9) | Dimmed text, labels, Terminated |
| `surface` (#181820) | Main background |
| `surface_alt` (#22222E) | Card backgrounds, alternating rows |
| `button_bg` (#2E2E3E) | Button backgrounds, distinct from cards |
| `extreme_bg_color` (#1E1E2A) | TextEdit input field backgrounds |

---

## Web (`aios-studio`)

### Launch

The web interface is served automatically by `aios-bridge` as static files. Run:

```bash
cargo run --bin aios-tui   # or any binary that starts aios-bridge
# Open http://localhost:8080 in your browser
```

The SPA is available at the root URL. The bridge port is set with the `AIOS_BRIDGE_PORT` environment variable (default `8080`); `start_server()` receives the resulting `host:port` string.

### Dashboard (default tab)

- **Health cards**: System status, Watchdog state, Process count, Memory usage
- **RAM chart**: Real-time Canvas line chart with last 120 data points (12 seconds at 100ms intervals)
- **Process table**: PID, Name, Memory, CPU time, Status with color-coded state badges

### Smart Command Palette

- **Open**: `Ctrl+K` or `Cmd+K` (macOS)
- **Close**: `Escape` or click outside the palette
- **Usage**: Type any natural-language command and press Enter
- **Supported**: process actions (list, kill, spawn), block actions (load, unload), system queries (status, memory, CPU), memory compaction
- **Languages**: English + Russian (bilingual intent parser)
- **Result display**: Shows description, JSON result, and list of used capability tokens
- **Destructive-action confirmation** (v2.31.3): prompts matching `kill / terminate / unload / stop / remove / delete / compact / free` open the `#confirm-overlay` second-chance dialog (warning icon + message + Cancel / Proceed; Enter confirms, Esc cancels) before the intent is sent.

### Security Center

- **Blocks table**: ID, Name, Version, State, Stop button per block (Stop asks for confirmation, then sends `unload block <name>`)
- **Capability tokens grid**: All six tokens (CAP_PROCESS_KILL, CAP_PROCESS_SPAWN, CAP_BLOCK_LOAD, CAP_BLOCK_UNLOAD, CAP_SCHED_MODIFY, CAP_MEM_ALLOC) with descriptions
- **Quick actions**: Compact Memory (confirmation required), List Blocks, List Processes

### Connection Status

- **Left sidebar footer**: Color-coded status indicator
  - Green = Connected
  - Yellow = Reconnecting (exponential backoff)
  - Red = Disconnected / Error
- WebSocket auto-reconnect with 1s → 15s exponential backoff

### Authentication

As of v2.30.0 the Web Studio requires a local account before it will load the dashboard:

- **First run**: the sign-in screen shows two tabs — *Sign in* and *Create account*. The first user on a fresh instance creates the account (username ≥ 3 chars, password ≥ 6 chars). A new account is registered on that AIOS instance and signs you in.
- **Session token**: on success the server returns a self-signed HMAC token with a 12 h lifetime. The UI stores it in `localStorage` (`aios_token`) and sends it as `Authorization: Bearer <token>` on every API call via `apiFetch`.
- **Re-login**: if the server replies `401`, the UI clears the token and returns to the sign-in screen automatically.
- **Sign out**: the sidebar footer has a *Sign out* button (also shows the current username + avatar).
- **Server-side protection**: the whole `/api/v1/*` surface is gated by the `require_auth` middleware, except the public `auth/register`, `auth/login` and `health` routes plus the WebSocket telemetry route. The bridge secret comes from `AIOS_AUTH_SECRET` (fallback derived from the data dir); users live in `<AIOS_DATA_DIR>/users.json`.
- **WebSocket telemetry auth** (v2.31.2): the `/ws/telemetry` route stays in the middleware allowlist but validates an optional `?token=<session>` query inside the handler; when a user exists, a session missing/invalid token is rejected and the WS connection is closed. `app.js` appends the stored token automatically, so the dashboard connects seamlessly after sign-in.
- **CORS**: disabled by default (the UI is same-origin) or restricted to a single origin via `AIOS_CORS_ORIGIN`.

### Requirements

- Any modern browser (Chrome, Firefox, Safari, Edge)
- No JavaScript framework required — 0 npm dependencies
- WebSocket support (all modern browsers)

---

## Architecture Notes

- **Kernel desktop** (`aios-kernel/src/gui.rs`) is the single user-facing GUI:
  a `no_std` framebuffer shell with its own windows, Start catalog and taskbar.
  It reads live kernel state directly and never launches a host process.
- **Host TUI** uses `ratatui` + `crossterm` — runs in any terminal, works over SSH;
  the kernel TUI (`src/tui.rs`) is a diagnostic fallback in the bare-metal image.
- The legacy host GUI used `egui` + `eframe` with a native GPU window and a shared
  `DashboardState` / `AiosApp` model; it is reference code, not the AIOS Desktop.
- The kernel desktop and host TUI do not share a widget model. The host interfaces
  request operations via the scheduler/block manager; the kernel desktop reads
  atomic kernel counters directly.

---

## Daemon (`aiosd`)

### Launch

```bash
# Default (headless, for Docker)
aiosd

# With custom directories
AIOS_DATA_DIR=/mnt/data AIOS_BLOCKS_DIR=/mnt/blocks aiosd

# Using mock profile
AIOS_MOCK_PROFILE=legacy aiosd
```

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `AIOS_DATA_DIR` | `/app/data` | Persistent database directory |
| `AIOS_BLOCKS_DIR` | `/app/blocks` | Disk block binaries directory |
| `AIOS_MOCK_PROFILE` | `modern` | Hardware profile: `modern`, `legacy`, `none` |
| `AIOS_SAFE_MODE` | unset | `1`/`true`/`yes`/`on` — skip disk block discovery and boot in safe mode |
| `RUST_LOG` | `info` | Log level: `error`, `warn`, `info`, `debug`, `trace` |

The daemon performs the same initialisation as `aios-tui` but runs without a terminal interface:
1. Loads built-in blocks (hal, ipc_bus, scheduler)
2. Loads disk blocks from `AIOS_BLOCKS_DIR` (skipped in safe mode)
3. Opens the persistent database at `AIOS_DATA_DIR/context.redb`
4. Spawns system processes (ai_orchestrator, io_handler, health_monitor)
5. Starts watchdog heartbeat thread
6. Logs heartbeat with process count, RAM usage, watchdog state every 10 seconds
7. Persists telemetry to database every 60 seconds

## Running All Interfaces

The host interfaces (Web, host TUI, Daemon) can run simultaneously on a host:
the Web SPA connects to `aios-bridge` via HTTP/WebSocket as a remote client, and
the host TUI is an in-process interface with its own `Scheduler` and
`BlockRegistry`. The native kernel desktop is not a host interface — it boots
inside `aios-kernel` on bare metal, shows the framebuffer GUI first, and exposes
the in-kernel TUI as a fallback via `tui`/`gui` or `Esc`.

---

## Bare-Metal Microkernel Interactive TUI (v2.38.19)

The microkernel TUI is the **diagnostic fallback screen** (since v2.81.0 the
native framebuffer desktop is the primary screen): a compact boot-log strip
(`CONSOLE_TOP_ROWS` = 6 glyph rows) sits at the top and the interactive panel
fills the whole remaining height down to the bottom status strip, so the full
tab content is always visible regardless of the framebuffer size. The panel
(`src/tui.rs`, `panel_rows()` = total rows − log strip − status bar) is
repainted at ~20 Hz (every 5 ticks) from `idle_loop`. Unlike v2.38.11-18
(a view-only static dashboard), the panel is driven by keyboard and USB mouse
input.

### Tabs

| # | Tab | Contents |
|---|-----|----------|
| 1 | System | banner, uptime, tick counter, tick mode (`HW-LAPIC`/`HW-IRQ`/`SOFT`), allocated frames, frame regions, IPC totals, lid state, CPU temperature (`temp=… tjmax=… sim=…`), PS/2 keyboard/mouse status (`ps2key=… ps2mouse=id0x..`) |
| 2 | Sched | switch counter, current pid, per-task state (`running`/`sleep`/`ready`/absent) |
| 3 | USB | xHCI controller state, keyboard report counter + last scancode, mouse report counter + buttons/dx/dy |
| 4 | IPC | total send/recv, per-task mailbox occupancy |
| 5 | Storage | AHCI/NVMe controller + LBA-read state (`none`/`error`/`ready`/`ready+rw`), MMIO window bytes used |
| 6 | Shell | in-kernel command shell (`help`, `tabs`, `info`, `ver`, `clear`, `echo …`, `gui`, `tui`, `faults`, `wifi`, `wifi sim`, `wifi real`, `udp …`, `http …`, `socktest`) |
| 7 | About | version banner, controls help |

### Input

- `1`-`7` (outside the Shell tab) switch tabs.
- A **left-click** on the tab bar switches tabs; the mouse 8x8 arrow is clamped
  to the framebuffer and is redrawn on top of every panel repaint, so switching
  tabs or refreshing the status bar can never erase it.
- **Runaway gate (v2.38.33, time-based since v2.79.0)**: if reports keep pushing
  the already-clamped cursor further into the same edge for one second, pointer
  input is suspended —
  `[tui] pointer runaway: input suppressed (N ms pushing into the edge)`
  — and re-arms only when a packet moves away from the edge
  (`[tui] pointer runaway released`). Serial movement logs are aggregated to
  one `[serial] ps2 mouse … +N pkts dx=… dy=…` line per second per device.
- In the Shell tab: printable keys append to the prompt, `Enter` (0x1C) runs
  the command, `Backspace` (0x0E) edits, `Esc` (0x01) clears the line. Every
  entered command is echoed to serial as `[serial] [shell] > <line>` so the
  exact keystrokes that reached the shell are provable.
- `wifi` (status), `wifi sim` (arm the simulated test radio) and `wifi real`
  (back to the honest real mode) control which radio the Network window's
  `Scan`/`Connect` use — the default is **real** (v2.38.36).
- `udp A.B.C.D PORT TEXT` (v2.49.0) opens a UDP socket, sends `TEXT` to the
  target and waits up to ~2 s for a reply, printing `sent N bytes` and the first
  60 bytes of the response (or `no reply (timeout)`). `http A.B.C.D PORT [HOST]`
  opens a TCP connection, issues `GET / HTTP/1.0`, and prints the byte count,
  the status line and a short body preview. Both require a bound IP (`dhcp`/
  `static` first) and drive the socket layer through the `SYS_SOCK_*` syscalls.
- `socktest` (v2.50.0; broadcast/multicast phases v2.56.0) runs a loopback
  self-test over the bound IP without any NIC traffic: a two-way UDP exchange,
  a broadcast (`255.255.255.255`) and a multicast (class D, `239.1.2.3`)
  fan-out each reaching two receivers, plus a full TCP `listen`/`connect`/
  `accept`/request/echo/close lifecycle and a segmented 3000-byte bulk transfer,
  printing one `PASS`/`FAIL` line per phase — runtime verification of the socket
  layer (both directions, handshake and FIN) that works without QEMU NIC models.
- `faults` (v2.45.2, queue v2.45.4, events v2.46.0) prints the fault-recovery
  aggregate — `total`, `resume`, `terminate`, `rollback`, `queued`, `events`
  counters — then one line
  per resolved record in the ring (`pid N <kind> (crash #k) rip=… hash=…`,
  oldest first), a `queued:` line per still-pending fault, and a `(dialog
  pending …)` marker while a task awaits a decision. `events` is the unread
  depth of the kernel fault event ring drained by ring-3 via `SYS_FAULT_POLL`.
- Keys consumed by the TUI are kept out of the console echo; unconsumed keys
  still print `[key] …` / `usb key …` to the console and serial as before.

### Windowed GUI (`gui` command, v2.38.22)

The kernel shell command `gui` switches the whole screen from the console+TUI to
a full-screen windowed desktop (`src/gui.rs`). `tui` (or `Esc` with no window
focused) returns to the console/TUI.

- **Desktop layout** (v2.38.39): washed dark-blue background (a touch lighter
  at the top), rounded 104×56 icon tiles with an accent pill, per-app vector
  glyphs (sliders / clock face / info circle / wireframe globe / stacked
  bars / widget grid) and centered labels, bottom task bar with a vertical wash and a top
  hairline, one rounded plate per open window plus the link-state globe — a
  circle with ring, meridian and equator inside a recessed pill — at the far
  right.
- **Windows** (v2.38.39): 7px rounded corners with a soft drop shadow and a
  subtle edge stroke; title bar (focused = bright blue, unfocused = dimmed)
  with a hairline underneath, a minimize / maximize / close button cluster
  (the close plate follows the frame's top-right radius) and live content:
  - *Welcome* — types the message line; shows the control help.
  - *System* — the same telemetry as the TUI System tab (tick, tick mode,
    switches, IPC totals, temperature, lid, PS/2/USB counters, allocated frames).
  - *Uptime* — `HH:MM:SS` clock with the tick counter.
  - *About* — version banner (open via its icon).
  - *Network* — DHCP/Static addressing, the five address fields and the
    Wi-Fi panel (open via the *Net* icon or the tray globe; see the
    v2.38.34 section below).
  - *Task Manager* — the open-window list with End Task / Switch To
    (open via the *Tasks* icon; see the v2.38.35 section below).
  - *Widget Lab* — the widget-set demo: two text fields, a scrollable list
    with its scrollbar, an `Edit` dropdown, a `Close` button, two
    checkboxes (`Auto-connect` / `Persist config`) and two radio buttons
    (`DHCP` / `Static`)
    (open via the *Widgets* icon; see the v2.47.0, v2.63.0 and v2.64.0 sections below).
- **Input**:
  - Mouse: click an icon to open (or focus) that window; click a window body or
    task-bar button to focus; drag a title bar to move (clamped to the bounds);
    drag an edge or corner to resize (v2.38.27 — see below); click the title
    cluster to minimize, maximize/restore or close (v2.38.28 — see below);
    click the desktop background clears the drag.
  - Keyboard: `Tab` cycles window focus; `Esc` closes the focused window and,
    with no windows left, returns to the TUI; printable keys (incl. after
    `Enter`) type into the focused window; function keys are hotkeys
    (v2.38.29 — see below). When a Widget Lab window is focused the keys are
    routed into its focused widget instead — `Tab` cycles the widget focus,
    `Backspace` edits, `Enter` activates and printable keys type (v2.47.0 —
    see below).
- **Behaviour**: one instance per window kind; the arrow composites last. While
  the GUI is active the kernel log (`vprintln!`) and heartbeat pause — the
  desktop owns the screen; serial logging keeps running so `[serial] tick …`
  still proves liveness.
- **Pointer runaway gate (v2.38.33, time-based since v2.79.0)**: a stream that
  keeps pushing the clamped cursor further into the same edge for one second
  switches pointer input off — `[gui] pointer runaway: input suppressed (N ms
  pushing into the edge)` — until a packet moves away from the edge
  (`[gui] pointer runaway released`); button events stay suppressed in the
  meantime, so a stuck touchpad cannot click the task bar. Mouse movement
  logs are aggregated to one line per second per device
  (`[serial] usb mouse +N pkts dx=… dy=…` / `ps2 mouse …`).
- **Pointer step clamp (v2.38.35)**: one input application never moves
  the cursor more than 256 px per axis — the remainder stays queued in
  the driver accumulator and is applied on the next iterations, so a
  backlog built during a full repaint glides instead of teleporting onto
  the task bar. PS/2 packets whose 8042 overflow bits are set (the
  controller already dropped that movement) are discarded whole instead
  of decoding the wrapped sign bits as motion.
- **Repaint (v2.38.23)**: rendering is damage-based — discrete actions
  (open/close/focus/leave) repaint the whole frame once, while continuous paths
  (cursor, drag, the live System/Uptime windows) refresh only their own dirty
  rectangles; an idle frame never rewrites the framebuffer (no LCD
  flicker). The IRQ timer bar (top-right, `TIMER_HZ` = 1000 Hz since v2.78.2)
  is suppressed while the GUI owns the screen.
- **Double buffer + 60 FPS (v2.38.24)**: all drawing paints into a RAM
  backbuffer (`[serial] gui backbuffer NNNN KiB` on entry); each frame only the
  dirty rectangle is published to the panel, one row copy at a time — the LCD
  sees only complete frames, so there is no tearing and no mouse trail. Since
  v2.82.0 the GUI repaints at 60 Hz (was 250 Hz in v2.79.0 and 100 Hz in v2.78.2)
  and the kernel TUI at 30 Hz (was 100 Hz), while lid/thermal probes stay on a
  ~20 Hz gate — all above what the eye needs, so an idle desktop no longer burns
  CPU on invisible frames while the static picture stays byte-identical. If the
  backbuffer cannot be mapped the GUI degrades to the v2.38.23 direct-VRAM
  renderer automatically.
- **Ring-3 client windows (v2.38.25)**: the desktop can host windows owned by
  ring-3 tasks. The client calls `SYS_GUI` CREATE once (16-byte request: width,
  height and user addresses of its pixel buffer and NUL-terminated title — every
  range is validated across all its pages with `memory::translate_user`, the only
  `int 0x80` user-pointer gate, which rejects kernel/high-half addresses,
  overflow and missing write rights with `u64::MAX`; sizes are capped at
  512×512), then `SYS_GUI` PRESENT every few ticks. `draw_client()` re-validates
  the full buffer through `translate_user` on **every** composite, so a client
  that unmaps or re-purposes its buffer after CREATE cannot make the kernel touch
  a stale pointer. The kernel composits its dense buffer into
  a window below the title bar (title shown in the title bar and task bar) and
  re-opens the window if the user closed it, so a live client always gets its
  surface back. The demo client (fourth ring-3 task) shows a 96×64 surface
  whose solid colour rotates every 6 ticks; `enter()` raises registered client
  windows above the built-in stack. The window behaves like any other — focus,
  drag, close — and a closed one reappears on the next PRESENT.
  Serial proof: `[serial] [gui] ring3 pid 5 registered client window 0 (96x64)`.
- **Input events for client windows (v2.38.26)**: focused client windows are
  interactive. The kernel queues input into the client's one-slot event buffer:
  printable keystrokes become `EV_KEY | ascii << 8`, clicks inside the body
  become `EV_CLICK | rel_x << 8 | rel_y << 24` (coordinates relative to the
  body, below the title bar). The application polls `SYS_GUI` GET_EVENT once
  per cycle and gets `EV_NONE` when idle (keep-first: a second event arriving
  before the first is polled is dropped). The demo client reacts live: a key
  turns its surface red, a body click turns it green, and each event is echoed
  to the serial log through `SYS_WRITE` (`ui key` / `ui click`).
  Serial proof: `[gui] client 0 click (48, 36)` + `[sysc] pid 5 write 8: ui click`,
  `[gui] client 0 key 'r'`.
- **Window resize (v2.38.27)**: every window — built-in or ring-3 client —
  resizes live by dragging within 5 px of its outer edge or corner (the close
  button wins the top-right corner; the title bar still moves the window).
  Width and height clamp to 96 px × (title bar + 8 text rows) at minimum; the
  left/top edge anchors the opposite corner, the right/bottom edge stops at the
  screen edge above the task bar. A ring-3 client keeps its native buffer
  resolution — the kernel scales the buffer nearest-neighbour into the new body
  size, so the 96×64 demo surface fills any window without the app doing
  anything, and input events keep working after the resize (click coordinates
  stay body-relative to the new size). Releasing the drag logs the final
  geometry: `[gui] resize client 0 -> 296x82`.
- **Minimize / maximize (v2.38.28)**: the title-bar cluster is 48 px wide at
  the right edge — minimize `[w-48,w-32)`, maximize/restore `[w-32,w-16)`,
  close `[w-16,w)`. Minimizing hides the window from the desktop; it stops
  taking clicks, resizes and `Tab` focus, its task-bar button turns dim, and
  clicking that button (or a desktop icon) restores it at the original
  geometry. Maximize snaps the window to the whole desktop above the task bar;
  the second press restores the saved geometry. Restoring a minimized window
  brings it to the front and logs `[gui] restore client 0 -> 96x82`; the other
  transitions log `[gui] minimize client 0` and
  `[gui] maximize client 0 -> 1280x782`. Task-bar buttons keep a stable
  order (Welcome, System, Uptime, About, clients) — they never swap places
  when the focus or z-order changes.
- **Keyboard hotkeys (v2.38.29)**: window management also works from the
  keyboard on plain function keys (no modifiers needed — they cannot collide
  with typing):
  | Key | Action |
  |-----|--------|
  | `F2` | maximize / restore the focused window |
  | `F4` | close the focused window (same as `Esc` / the `X` button) |
  | `F5` | minimize the focused window |
  | `F9` | cycle window focus (twin of `Tab`) |

  The actions reuse the mouse paths, so the same serial proof lines appear:
  `[gui] maximize|restore … -> WxH`, `[gui] minimize …`, `[gui] close …`,
  `[gui] focus …`. A hotkey with no focused window does nothing.
- **Close confirmation dialog (v2.38.30)**: closing a window that still
  contains typed text (Esc, the `X` button or `F4`) opens a modal `Confirm`
  dialog centered on the desktop — `Close <label>?`, a
  `Typed text will be lost.` hint and a centered `OK` / `Cancel` button pair.
  Resolve it with `Enter` (close for real), `Esc` (keep the text), or by
  clicking either plate. While the dialog is up the window manager is
  frozen: all other keys and clicks are consumed and logged
  (`[gui] modal blocks scancode 0xNN` / `[gui] modal blocks click (x,y)`),
  so focus cannot move to another window or a ring-3 client.
  `OK` logs `[gui] close <label>` followed by
  `[gui] modal close <label> confirmed`; `Esc` / `Cancel` logs
  `[gui] modal close <label> canceled` and the typed text stays. Closing a
  window with an empty note never shows the dialog.
- **Crash recovery dialog (v2.45.0, budget v2.45.1, hang v2.45.3, frame/progress v2.46.0)**: when a
  ring-3 task faults
  (divide error, invalid opcode, general protection or page fault) the kernel no
  longer halts — the task is frozen and a `Task Error` overlay opens over the
  desktop (above windows and the confirm dialog) showing `pid <n> faulted:
  <reason> (crash #N/3)` and the captured
  `snap rip=… rsp=… hash=0x…` (the hash seals the snapshot's **full CPU frame** —
  all 16 general-purpose registers plus `rip`/`cs`/`rsp`/`rflags`/`ss`, FNV-1a).
  Since v2.45.3 the same overlay also catches a
  **hang**: a ring-3 task that neither changes its instruction pointer nor makes a
  syscall for 10 s is frozen by the scheduler watchdog (the `SYS_PING` progress
  heartbeat also counts as activity), the
  title reads `Task Hung`, and the line reads `pid <n> hung: hang (crash #N/3)`;
  the serial log shows `[hang] pid <n> stalled at rip=… frozen`. Resolve it by
  clicking a plate or pressing a key:
  | Key | Action |
  |-----|--------|
  | `R` | **Rollback** — restore the task's captured full CPU frame and resume |
  | `T` | **Terminate** — remove the task from the scheduler |
  | `Enter` / `S` | **Resume** — ignore the fault and requeue the task |

  While the overlay is up it owns the keyboard and mouse (other keys log
  `[gui] crash dialog blocks scancode 0x..`, clicks outside the plates log
  `[gui] crash dialog blocks click (x,y)`). **Fault budget:** a task that faults
  more than 3 times locks the dialog to Terminate only — the hint line becomes
  `fault budget exceeded: only T = Terminate`, the Rollback/Resume plates are
  dimmed, and pressing `R`/`Enter`/`S` logs `[gui] crash dialog blocks
  resume|rollback: over fault budget`. Opening logs
  `[gui] crash dialog open pid <n>`; resolutions log `[fault] pid <n>
  <rollback|terminate|resume>` followed by a totals line
  `[fault] totals faults=N resume=N terminate=N rollback=N`. **Rollback**
  restores every captured register and logs `state verified` (or `mismatch`);
  **Resume** first checks the frozen task's live frame against the queued
  snapshot's sealed hash — an intact task is unfrozen (`state verified`), a
  clobbered one is force-Terminated (`resume state mismatch -> terminate`). Only
  ring-3 tasks
  are rescued: a fault in kernel context (or a double fault) still halts. Since
  v2.45.4 multiple faults/hangs may be pending at once in the **system fault
  queue**: the dialog shows the oldest and, when more are waiting, appends
  `(+N more queued)` to the task line; resolving one advances to the next (logged
  `[fault] queue: pid <n> <kind> next (K queued)`) without the dialog closing.
  See `BUGS.md` (OPEN v2.45.0) for the no-memory-image limitation.
- **Restore of closed z-order (v2.38.31)**: a window remembers its place in
  the stack when it closes. Reopening it — desktop icon, task-bar button, or
  a ring-3 client presenting again — puts it back at that spot *between* its
  old neighbours instead of on top, so the stacking you arranged survives a
  close/open round-trip. Closing logs `[gui] close <label> (z <rank>)`
  (0 = bottom), a successful restore logs `[gui] reopen <label> at z <rank>`;
  a window opened for the first time has no memory and goes on top as before.
- **Welcome buttons / widget layer (v2.38.32)**: the Welcome window hosts
  four clickable buttons under its text — `System` and `Uptime` on the first
  row, `About` and `Close` on the second. Hovering a plate lights it from
  `BTN_BG` to `BAR_ON`; clicking logs `[gui] widget Welcome to AIOS GUI/<label>
  click`. `System` / `Uptime` / `About` open (or focus) that built-in window —
  a reopened window keeps its remembered z-rank — while `Close` closes the
  Welcome window itself through the same paths as `Esc`/`X`/`F4`, so a typed
  note still raises the `Confirm` dialog. Plates are window-local: they move
  and resize together with their host window.
- **Network window (v2.38.34, drivers v2.48.0)**: the desktop's networking panel
  — open it from the tray globe (bottom-right corner of the task bar, green =
  internet reachable) or the `Net` icon (fourth in the left column). Since
  v2.48.0 the wired uplink picks the first **supported** NIC from an exact
  PCI vendor/device-id table — e1000, e1000e/igb, rtl8139, rtl8169/8168, virtio
  -net (legacy) and AMD PCnet — and the DHCP client handles NAK/renew/rebind,
  link-change recovery and ARP-cache ageing, so the address re-acquires itself
  after a cable replug or lease expiry. Contents:
  - *Mode*: `DHCP` / `Static` buttons pick how the address fields are used
    (status line shows `dhcp` / `static`).
  - *Address fields*: `ip`, `mask`, `gw`, `dns1`, `dns2` — click one to
    focus it, then type digits/dots/letters with the keyboard (`Tab` cycles
    through all fields; `Shift` types capitals, Backspace deletes). A field
    shows the value it will apply (defaults `0.0.0.0`).
  - `Apply` parses all five fields and commits them (persisting to CMOS for
    the next boot); status line: `applied`, or the failing field
    (`bad ip` / `bad mask` / `bad gw` / `bad dns1` / `bad dns2`), or
    `need ip` when Static mode has no address. Serial proof:
    `[serial] [net] config applied dhcp=… ip=…`.
  - `Test` triggers a manual connectivity recheck — status `check!`,
    serial `[serial] [net] manual check requested`; the globe and
    `state checking|internet|link-only|no-nic` follow the probes.
  - *Wi-Fi*: `ssid` and `pass` fields plus `Scan`, `Connect`, `Disc`, over a
    status line that reports the detected controller — `wifi card: <bdf>` or
    `card: none` (v2.38.36 PCI class `02`/subclass `80` detection).
    **Real mode is the default** (`radio: real (test radio off, shell: wifi
    sim)`): `Scan` without a card is honestly refused (status `no card`,
    serial `[wifi] scan: no wifi card`) and an empty result renders the
    `no networks found` hint line. Arm the simulated radio from the shell
    (`wifi sim`, `wifi real`, `wifi`) — only then does `Scan` (status
    `scan ok`) collect the three test networks — `AIOS-Test` (open),
    `SecureNet` (WPA2, passphrase `AIOS-Test`), `Neighbor` (open) — in
    ~0.5 s (`scan done (3 networks)`). `Connect` (status `wait`, or
    `no ssid` if the SSID field is empty) runs the full association:
    `connect <ssid> (wpa2|open)`  auth  assoc  for WPA2 the 4-way
    handshake  DHCP over the AP LAN  `[serial] [net] bound
    ip=10.0.9.15`  `internet ok`. `Disc` (status `disc ok`) tears the
    link down (`disconnect`). The sim radio is a **simulated test radio** -
    no real 802.11 controller behind it; all AP-side traffic (beacons,
    auth/assoc, EAPOL, DHCP/ARP/ICMP/DNS on `10.0.9.0/24`) is answered
    in-kernel, and it is labelled *sim* in the boot log. Field typing is
    logged per character on serial (`[gui] type '<c>' focus=… ssid_len=…
    pass_len=…`, `[gui] backspace focus=… len=…`) so a truncated input is
    provable.
  - Serial proof: `[gui] tray globe click`, `[gui] widget Network/<label>
    click` (`Apply`/`Test`/`Scan`/`Connect`/`Disc`; the address/SSID
    fields log with an empty label).
- **Task Manager window (v2.38.35)**: the desktop's Windows-style task
  manager — open it from the `Tasks` icon (fifth in the left column).
  Contents:
  - *Header*: a `Task` / `Status` strip on the task-bar background.
  - *Task rows*: one 16 px row per open window in the same stable order
    as the task bar (Welcome, System, Uptime, About, Network, Task
    Manager, then ring-3 clients by id), each with its status —
    `Running` (focused), `Background` (open, unfocused) or `Minimized`.
    Clicking a row selects it: the row highlights with the task-bar
    plate colour and serial logs `[gui] tasks select <label>`.
  - `End Task` closes the selected window through the normal close
    path — a typed note still raises the v2.38.30 `Confirm` dialog, and
    success logs `[gui] tasks end <label>` followed by the usual
    `[gui] close <label>`. With nothing selected (or a window that is
    already gone) it logs `[gui] tasks end: no selection` /
    `[gui] tasks end: <label> gone` instead of acting.
  - `Switch To` (or `Enter` while the Task Manager is focused) brings
    the selected window to the front and focuses it —
    `[gui] tasks switch <label>` — with the same `no selection` /
    `<label> gone` proofs for a stale selection.
  - The foot shows `N tasks` and the current `sel <label>`.
  - Serial proof: `[gui] open Task Manager` (every window open now logs
    `[gui] open <label>`), `[gui] widget Task Manager/End Task click`,
    `[gui] widget Task Manager/Switch To click`.
- **Widget set / Widget Lab (v2.47.0)**: the widget layer grows past buttons
  into a generic set shared by every host — **text fields**, a **scrollable
  list** with a bound **scrollbar** and a **dropdown menu** — and the focused
  host now receives keyboard input (routed *before* the global shortcuts).
  The demo host is the *Widget Lab* window (460×332), open from the
  `Widgets` icon (sixth in the left column). Since v2.66.0 the keyboard routing
  is host-generic: any window with a widget table (except Network, which keeps
  its dedicated form handler) receives the same Tab/Backspace/Enter/typing keys
  into its own per-window form store. Contents:
  - *Fields*: `name` and `note` — click one (or `Tab` to it) to focus, then
    type; empty fields show a dim placeholder, focused fields get a lit
    border and a caret, `Backspace` deletes.
  - *List*: ten palette rows (five visible) with a scrollbar at its right;
    click a row to select it (the selected row highlights, brighter when the
    list is focused), or focus the list and move with `n`/`p`; `Enter` picks
    the selected label into the header status line. Clicking the scrollbar
    jumps the scroll proportionally.
  - *Menu*: the `Edit` plate opens a dropdown (`Clear name` / `Clear note` /
    `Reset list` / `Close`); hovering lights a row, clicking one runs it —
    the three edits update the demo form, `Close` closes the window.
  - *Checkboxes* (v2.63.0): `Auto-connect` and `Persist config` — a square box
    with a caption; click one to toggle it (the box fills with a check mark and
    the caption brightens), and the header status line reports the new state.
  - *Radio buttons* (v2.64.0): `DHCP` / `Static` in one group — a round box
    with a caption; clicking one selects it (a filled dot appears, the caption
    brightens) and clears the other in the group, logged as
    `[gui] widget Widget Lab/radio <label> = <value>`.
  - *Slider* (v2.67.0): `Volume` 0–100 — a horizontal track with a thumb; the
    filled portion and the numeric value at the right show the current value,
    and clicking anywhere on the track jumps the thumb there proportionally,
    logged as `[gui] widget Widget Lab/slider <label> = <value>`.
  - *Close* button: closes the Lab through the normal path (a typed note
    still raises the `Confirm` dialog).
  - Press `Escape` to dismiss an open dropdown without closing the window.
  - *Clipboard* (v2.68.0): with a text field focused, `Ctrl+C` copies, `Ctrl+X`
    cuts and `Ctrl+V` appends from a kernel-internal 128-byte buffer shared by
    every window (not the firmware clipboard); the status line reports
    `copied`/`cut`/`pasted`/`no field`/`full`/`empty`. Text fields hold UTF-8 and render
    non-ASCII as `?` until a scalable font lands. The hint row lists
    `ctrl+c/x/v copy/cut/paste`.
  - Serial proof: `[gui] widget Widget Lab/field <n> focus`,
    `[gui] widget Widget Lab/list row <n> click`,
    `[gui] widget Widget Lab/menu open|close`,
    `[gui] widget Widget Lab/menu item <n> click`,
    `[gui] widget Widget Lab/check <label> = on|off`,
    `[gui] widget Widget Lab/radio <label> = <value>`,
    `[gui] widget Widget Lab/slider <label> = <value>`,
    `[gui] clipboard copy|cut|paste <n> bytes`.
- **Keyboard ext safety (v2.38.23)**: extended (0xE0/0xE1-prefixed) keys —
  Windows, arrows, F-rows, keypad-alt — are dropped at the source on both input
  paths and never reach the GUI/TUI: they type nothing and cannot crash the
  kernel. Plain printable keys still type into the focused window as above.
- **Panic is visible even without serial**: if the kernel panics or faults while
  the GUI is active, the screen clears to an allocation-free on-screen report:
  `KERNEL PANIC [v2.84.0]: …` or `FATAL [v2.84.0] …`, painted via
  `console::panic_report` with `version::KERNEL`, instead of freezing on the
  desktop. The version in the screenshot identifies the exact build that failed.
  A ring-0 page fault adds `PAGE FAULT <class> addr=… ip=… err=… rsp=…` to that
  report, so no serial cable is needed to diagnose a long-uptime crash.

Text is rendered on the PSF path across the whole panel: the embedded
synthesized PSF2 stream (round-trip validated at boot) is parsed back and used
as the glyph source, with the `font8x8` table as the fallback.

The bottom `GLYPH_H` strip hosts a two-sided status bar (version, active
`tab n/7`, tick counter, tick mode, PIT countdown on the left; last PS/2
scancode `k=0x..`, USB key `uk=…` and PS/2/USB mouse sequences `m=`/`pm=`,
lid state `lid=…`, live CPU temperature `T=…c` on the right) that leaves the
central cell free for the boot-time green 'A' glyph and the far-right 2 Hz
heartbeat square. The top-right timer tick bar is untouched. A `~` suffix on the temperature marks a synthetic reading.

### Platform sensors (v2.38.20)

At boot the kernel walks the ACPI tables (`acpi::init`, serial `[acpi] rev=…
XSDT@… fadt rev=…`), then brings up three platform-sensor/input modules: the
embedded-controller probe (`ec`, `[ec] …` / `[lid] …`), the CPU digital thermal
sensor (`thermal`, `[thermal] dts tjmax=…` or the simulated fallback), and the
PS/2 i8042 pair (`ps2`, `[ps2] aux/mouse up id=…` + `[ps2] i8042 ready
(kbd+mouse)`).

- **Lid**: shown as `lid=0`/`lid=1` (closed/open) in the status bar and System
  tab, driven by the EC RAM scan heuristic. On boards without an ACPI EC the
  value stays `--` (e.g. QEMU logs `[lid] ec unresponsive (0 bytes), lid n/a`).
- **Temperature**: `T=…c` in the status bar (a `~` suffix marks a synthetic
  reading) and `temp=… tjmax=… sim=…` in the System tab; the GUI System window
  prints `temp …c~ (simulated)` in that case. With usable DTS MSRs the die
  temperature is `TjMax - delta` (TjMax from 0x1A2, or a documented 100 °C when
  that register is unreadable/implausible); an invalid status word (bit 31) or
  an implausible delta counts as no reading instead of showing a fake 0 °C or
  TjMax. Without a usable DTS the synthetic 45..60 °C wave is drawn (`sim=1`)
  and never drives the scheduler gate; at ≥ 90 °C of a *real* reading ring-3
  tasks pause (`[serial] [thermal] CRITICAL temp=… (ring-3 paused)` ↔ resumed).
- **Touchpad/keyboard**: the laptop's native PS/2 devices feed the same input
  paths as USB — keys reach `handle_scancode` (Shell tab, tab switching), the
  touchpad relocates the mouse arrow and left-click switches tabs. The PS/2
  mouse ID (`0x00` PS/2, `0x03` IntelliMouse, `0x04` five-button) is shown as
  `ps2mouse=id0x..`.

## Running store blocks (`block run`)

Installed community-store blocks are driven from the unified text console with
`block run <name> <text...>`. Everything after the block name is passed to the
guest as one text request; the same WASM instance is reused, so stateful blocks
continue across invocations:

```
store source local:F:/aiosproject/AIOS/aios-official-store
store install calculator --source=local:F:/aiosproject/AIOS/aios-official-store
block run calculator 2+3*4          # 14
block run calculator ans+1          # 15
block run calculator width=3
block run calculator width*width    # 9
block run calculator history
store install minesweeper --source=local:F:/aiosproject/AIOS/aios-official-store
block run minesweeper new 9 9 10
block run minesweeper open 1 1
block run minesweeper flag 5 5
block run minesweeper show
```

The calculator understands `help`, `reset`, `history`, `ans`, expressions and
`<name> = <expression>` with `+ - * / % ( )`. The minesweeper understands
`help`, `new [w h mines]`, `open <x> <y>`, `flag <x> <y>`, `show`, `status` and
`reset`; coordinates are 1-based and the first opened cell is never a mine.
Use `block run`, not `wasm call`: the latter sends no payload and instantiates
the module anew, which discards block state.

## Network console and NIC inventory (v2.84.0)

The unified console now exposes the same network vocabulary on both backends.
`net get` remains an alias of `net show`.

| Command | Result |
|---------|--------|
| `net show` / `net config` | Print saved or kernel-held IPv4 settings (`dhcp`, `ip`, `mask`, `gateway`, `dns1`, `dns2`) |
| `net status` | State, NIC kind, MAC, settings and bound lease in one snapshot |
| `net set <key=value ...>` | Partial update; kernel accepts `dhcp`, `ip`, `mask`, `gw`, `dns1`, `dns2` and their aliases, host accepts config keys and dotted/nested JSON |
| `net validate` | Reject static mode with no IP or malformed settings |
| `net apply` | Host: persist config then recheck the link; kernel: reapply settings |
| `net reset` | Restore defaults |
| `net adapters` | Host: OS adapter table (`INTERFACE STATE IPV4 GATEWAY DHCP`); kernel: wired + Wi-Fi rows |
| `net recheck` | Host: fresh `NetManager` link probe; kernel: `crate::net::recheck()` |
| `net lease` | Bound DHCP lease details, or `no DHCP lease acquired in this session` before acquisition |

On the host, nested JSON needs shell escaping, e.g.
`console net set dns={\"primary\":\"8.8.8.8\"}`. `driver nics` adds a
`DEVICE CLASS DRIVER STATE TOKEN` table for detected Ethernet/Wi-Fi hardware;
`TOKEN` is `valid`, `expired` or `none`. The kernel `net` commands share the
`aios-console::net_text` parser with workspace tests, so their formatting is
not a host-only stub.
