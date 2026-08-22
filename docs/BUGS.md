# AIOS Known Bugs & Workarounds

## RESOLVED: `test_stress_rt_scheduler_500` flaked on a loaded machine (hard 2 s wall-clock threshold)
- **Status:** FIXED in v2.28.1 (found during the v2.28.1 full workspace audit, Windows x64)
- **Symptom:** `cargo test --workspace` failed once with `RT scheduling took 2.0974127s (>2s)` in `tests/stress_test.rs:113`. The functional assertion (500 RT processes scheduled) passed; only the wall-clock budget tripped.
- **Root cause:** the speed limit was hard-coded to `2000 ms` regardless of build profile, violating the AGENTS.md rule that all speed tests carry **dual debug/release thresholds**. A debug-build scheduler loop on a machine running parallel test binaries can legitimately exceed 2 s.
- **Fix:** dual threshold — `5000 ms` under `cfg!(debug_assertions)`, `2000 ms` in release. Re-run of the suite is green (11/11).
- **Workaround / notes:** none needed post-fix; if CI flakes recur on other wall-clock stress tests, consider percentile budgets or CPU pinning.

## RESOLVED: aios-kernel heap returned corrupt data after `Vec` growth (stale block size on alloc)
- **Status:** FIXED in v2.28.0 (found during milestone 2 heap testing under QEMU)
- **Symptom:** the milestone 2 heap test printed `heap: Vec<u64> 1000 elems, sum=18446198715943183352` instead of `999000`; later allocations were inconsistent.
- **Root cause:** the free-list allocator took a free block, split it, but never recorded the *allocated* size in the block header — the header still held the pre-split (full) block size. `dealloc` then used that stale size for the coalescing adjacency check, adding a wrong size to the merged block and corrupting the free list.
- **Fix:** `heap.rs` writes the exact allocated size (`needed`) into `(*block).size` immediately after the split decision, before returning the payload.
- **Workaround / notes:** none needed post-fix; verified by the milestone 2 QEMU run (`sum=999000`, stress `len_sum=5100`, `final Vec sum=1498500`).

## RESOLVED: aios-kernel milestone 1 triple-faulted during GDT/IDT setup (packed descriptor layout)
- **Status:** FIXED in v2.27.0 (found during milestone 1 interrupt bring-up under QEMU)
- **Symptom:** the kernel booted to milestone 0, then died inside `gdt::init` with no panic message. QEMU `-d int` showed a single `v=0d` (#GP) at `aios_reload_segments` / `ltr`, then `v=08` (double fault) and a triple fault; the register dump at the fault showed `GDT= d960000000000000 0000003f` and `IDT= d9f0000000000000 00000fff` — the GDTR/IDTR bases were truncated (low 16 bits of the real base).
- **Root cause:** `struct Descriptor { limit: u16, base: u64 }` was `#[repr(C)]`; alignment of `u64` pushed `base` to offset 8 and left 6 padding bytes at 2..8. The CPU reads an `lgdt`/`lidt` operand as a contiguous 10-byte descriptor — limit at offset 0, base at offset 2. So the loaded base became `padding + first two base bytes` (e.g. `0xd960` instead of `0x1000001d960`), pointing GDT/IDT at garbage. Every descriptor load through the new GDT then faulted: `retfq`/`mov ds, 0x10` → `#GP`, exception delivery via the broken IDT → `#DF` → triple fault.
- **Fix:** mark both descriptors `#[repr(C, packed)]` (base now starts at offset 2, matching the CPU's view) and access the `descriptor` field through `addr_of_mut!` (packed fields are unaligned). `gdt.rs` and `idt.rs`.
- **Workaround / notes:** none needed post-fix; verified by the milestone 1 QEMU run (`[serial] Milestone 1: interrupts online.`, `tick 1s/2s/...`, `key 'h' (0x23)` via QEMU monitor).

## RESOLVED: aios-kernel `aios_reload_segments` code landed in a data section (`.section` leak across `global_asm!`)
- **Status:** FIXED in v2.27.0 (found during milestone 1 interrupt bring-up)
- **Symptom:** `aios_reload_segments` was linked into `.data.rel.ro` (a data section) instead of `.text`; `nm` reported it as `D aios_reload_segments` inside the `.data.rel.ro` range.
- **Root cause:** Rust concatenates all `global_asm!` blocks into one assembly stream. The generated `irq_stubs.S` emits `.section .data.rel.ro` for `aios_handler_table`, and the assembler keeps that section as current; the next `global_asm!` (the `gdt` module) has no explicit section, so its code landed in `.data.rel.ro`.
- **Fix:** explicit `.text` at the top of the `gdt` module's `global_asm!`. Also removed the unnecessary far-return CS reload (`retfq`); the bootloader already runs the kernel with CS=`0x08`/SS=`0x10`, which resolve to the kernel's own descriptors after `lgdt`, so `reload_segments` only reloads DS/ES/FS/GS.
- **Workaround / notes:** none needed post-fix.

## RESOLVED: aios-kernel milestone 0 triple-faulted on first boot (VGA write to unmapped 0xB8000)
- **Status:** FIXED in v2.26.0 (found during first boot verification under QEMU)
- **Symptom:** QEMU booted the bootloader, the kernel entry point was reached, then the CPU triple-faulted. The serial log ended with `Jumping to kernel entry point at VirtAddr(0x100000028a0)`; QEMU `-d int` showed `old: 0xe new 0xe` then `v=08` (double fault), with `RAX=0xB8000`, `IP=0x10000001a40` (`movw $0x720,(%rax)`) and `CR2=0xE0`.
- **Root cause:** The bootloader's default `Mappings` sets `physical_memory: Option::None` — only the kernel segments, boot info, stack and framebuffer are mapped, low physical memory is not. The VGA clear loop wrote to the text buffer at physical `0xB8000`, which had no mapping → page fault. Because the kernel has not installed an IDT yet, the faulting IDT-descriptor fetch at base 0 (vector 0xE → linear `0xE0`, hence `CR2=0xE0`) itself faulted → double fault → triple fault.
- **Fix:** `aios-kernel` now builds a `BootloaderConfig` with `config.mappings.physical_memory = Some(Mapping::Dynamic)` and passes it to `entry_point!(kernel_main, config = &BOOTLOADER_CONFIG)`. `kernel_main` forwards `physical_memory_offset` to `vga::vga_init`, which re-points `VgaWriter::buffer_addr` to `0xB8000 + offset`, so the VGA buffer is reached through the physical-memory map.
- **Workaround / notes:** none needed post-fix; verified by the milestone 0 QEMU smoke run (`[serial] Milestone 0 OK.`).

## Current: No known defects (Clean Build)

As of v2.13.0, all tests pass, clippy reports zero warnings, and the 18 bugs found in the v2.7.0 bug-fix pass (BUG-021…BUG-038) are fixed and covered by regression tests. The v2.8.0 restructure of the kernel TUI to 7 tabs, the `--safe-mode` boot flag and the GUI AI Studio / Network Settings tabs, the v2.9.0 / v2.9.1 AI chat persistence, `/preset` templates and streaming work, the v2.9.2 button-contrast fix, the v2.9.5 Live USB image, the v2.10.0 `aios-vfs`/`aios-fm` file manager, the v2.11.0 `aios-cluster`, the v2.12.0 `aios-init` initramfs init, the v2.13.0 `/system/aios-core` kernel-TUI handover, the v2.20.0 stateful process migration (executor state snapshots + `GetState`/`GetStateReply` + state-carried `migrate`), the v2.21.0 checkpoint replication (heartbeat broadcast + TTL pruning + automatic failover restore), the v2.22.0 `aios-autohal` hardware auto-provisioning and the v2.25.0 native push-based hot-plug notifications added no new known defects. See the Historical Issues section and `docs/CHANGELOG.md`.

### RESOLVED: VRAM shown as 4.0 GB for GPUs above 4 GiB in the kernel TUI
- **Status:** FIXED in v2.25.2 (found during live verification)
- **Symptom:** `aios` (kernel TUI) detected the GPU model correctly but always reported 4.0 GB VRAM (an RTX 3060 12 GB showed "4.0 GB VRAM"), while the GUI/HAL correctly showed 12288 MB.
- **Root cause:** `hw_probe::probe_gpu` read VRAM from the WMI field `win32_VideoController.AdapterRAM`, which is 32-bit. NVIDIA drivers return wrapped/truncated values for GPUs above 4 GiB — the RTX 3060 reports 4293918720 (0xFFF00000), i.e. 4.0 GiB minus 1 MiB. The existing `0xFFFFFFFF` guard only covered the "unknown" sentinel, not the wrap.
- **Fix:** `probe_gpu` now prefers `aios_hal::hardware::HardwareProfile::detect()`, which reads the real VRAM via `nvidia-smi --query-gpu=memory.total` (MiB). The WMI path remains only as a last-resort name source; a `gpu_from_hal` converter plus regression tests were added.
- **Workaround / notes:** none needed post-fix; covered by regression tests in `aios/src/hw_probe.rs`.

### RESOLVED: `Cannot start a runtime from within a runtime` at TUI startup
- **Status:** FIXED in v2.25.1 (found during live verification)
- **Symptom:** `aios` (kernel TUI) panicked on `thread 'main'` right after HAL detection (`HAL: NVIDIA GPU detected … Detected 16 cores, …`) with `Cannot start a runtime from within a runtime` from `tokio-1.53.1/src/runtime/scheduler/multi_thread/mod.rs:91`.
- **Root cause:** `main` is `#[tokio::main]`, so the main thread already lives inside a tokio runtime. The startup provisioning pass (`AutohalEngine::rescan` → `provision_blocking`) and other synchronous wrappers (`DriverFetcher::sync_get`/`find_driver_sync`, `StoreManager::block_on`) each built a *fresh* tokio runtime and called `block_on` from inside the running runtime, which tokio forbids.
- **Fix:** new `aios_core::runtime::block_on_future` helper — outside a runtime it builds a fresh runtime; inside a multi-thread runtime it parks the worker with `block_in_place` and blocks on the existing handle (non-`Send` futures included); all synchronous wrappers route through it.
- **Workaround / notes:** none needed post-fix; covered by the regression test `provision_blocking_is_safe_inside_tokio_runtime`.

### RESOLVED: `Kernel panic: No working init found` on initramfs boot
- **Status:** FIXED in v2.12.0 (design-level)
- **Symptom:** When the initramfs did not contain a working `/sbin/init` (or the busybox init script was missing/not executable), the kernel aborted with `Kernel panic: No working init found. Try passing init= option to kernel.`
- **Root cause:** The previous initramfs `/init` was a shell script (`live/init.rs`) that depended on busybox being present and executable; any packaging error left the kernel with nothing valid to run.
- **Fix:** New `aios-init` crate is a statically linked (`x86_64-unknown-linux-musl`) Rust `/init` binary (see `docs/ARCHITECTURE.md` Layer 8). It never panics: if `/system/aios-core` or `/installer` is missing it drops to a rescue shell (`/bin/sh` → `/bin/busybox sh` → `/bin/ash`), and if no shell exists it parks in an idle `waitpid` reap loop instead of exiting (an exiting PID 1 is what triggers the kernel panic).
- **Workaround / notes:** pass `init=/init console=tty0` on the kernel command line (GRUB/Syslinux) so the binary is used explicitly; run `./build_initramfs.sh` (optionally `BUSYBOX_PATH=...` for the rescue shell). Since v2.13.0 the script also builds and stages the real `aios` kernel binary as `/system/aios-core`, so the boot lands in the full kernel TUI; the rescue shell is only the fallback.

### KNOWN LIMITATION: `aios` static-musl build needs native TLS libraries on the build host
- **Status:** BY DESIGN
- **Symptom:** `build_initramfs.sh` (and `live/build.sh` in the default aios-init mode) build `aios` for `x86_64-unknown-linux-musl`; `reqwest` 0.12 without an explicit `rustls` feature links native-tls/OpenSSL, so the musl cross-build requires system OpenSSL dev/static libraries (the Alpine live container installs `openssl-dev`).
- **Workaround:** Build on a host that provides OpenSSL for musl (Alpine or an equivalent container); alternatively use `./build_initramfs.sh --no-aios-core` / `SKIP_AIOS_CORE=1` to produce a rescue-shell-only initramfs.
- **Note:** If the aios build fails or is skipped, the script warns and continues — boot still works via the rescue shell.


### KNOWN LIMITATION: `HOST://` access requires capability tokens
- **Status:** BY DESIGN
- **Symptom:** The file manager starts both panels on `AIOS://` (sandboxed); navigating to `HOST://` shows an empty listing, and host operations fail with `denied: missing capability`
- **Workaround:** Grant `vfs:host:read` (`g`) and `vfs:host:write` (`w`) tokens in the Files tab; tokens live in `AclContext` for the current process only and are not persisted
- **Note:** `HostVfs` still refuses paths outside the host root even with tokens (path-containment via `canonicalize_inside`)

### KNOWN LIMITATION: GUI file manager needs a writable sandbox dir
- **Status:** BY DESIGN
- **Symptom:** If `AIOS_DATA_DIR` points to a read-only location, the GUI Files tab shows "FM runtime failed / VFS root init"
- **Workaround:** Point `AIOS_DATA_DIR` at a writable folder before launching `aios-gui` (default sandbox: `AIOS_DATA_DIR/vfs_sandbox`)

### KNOWN LIMITATION: `aios-autohal` Bluetooth/ACPI fingerprints are lookup-ready but not yet sourced
- **Status:** BY DESIGN
- **Symptom:** `HardwareFingerprint`/`BusType` include Bluetooth and ACPI, but `aios-hal::HardwareProfile` does not yet surface such devices, so `extract_fingerprints` currently returns only USB/PCI/NVMe entries.
- **Workaround:** None needed — the variants exist so the inspector tree and driver lookup keys (`bt.*`, `acpi.*`) already cover them; a future `aios-hal` update plugs them in without changes to `aios-autohal`.

### FIXED: Linux GPU probe did not compile (`hw_probe.rs`)
- **Status:** FIXED in v2.9.5
- **Symptom:** `cargo build` for `target_os = "linux"` failed with `E0308` in `aios/src/hw_probe.rs`: `String::from_utf8(Command::new("lspci").arg("-v").output())` — `output()` returns `Result<Output, io::Error>`, the code expected `Vec<u8>` (missing `.ok()?.stdout`)
- **Root cause:** The Linux GPU-probe branch was never compiled (only Windows/macOS were); two call sites forgot the stdout extraction present in the Windows branch
- **Fix:** Added `.ok()?.stdout` to the `nvidia-smi` and `lspci` Linux sites and the equivalent macOS `system_profiler` site
- **Regression coverage:** Linux static-musl release build now compiles (see `live/build.sh`); no CI covers non-Windows targets yet

### FIXED: GUI TextEdit fields invisible on light system theme
- **Status:** FIXED in v2.9.3
- **Symptom:** Input fields on the GUI Network Settings tab (and other `TextEdit` inputs) showed light text on a white background — effectively unreadable
- **Root cause:** eframe 0.31 seeds `Visuals` from the OS system theme; on a Windows machine in light mode `extreme_bg_color` (the TextEdit background) stayed `#FFFFFF` because `AiosTheme::apply` only overrode `dark_mode` and part of the widget palette
- **Fix:** `apply` now sets every `Visuals` surface field explicitly (see `docs/CHANGELOG.md` v2.9.3); verified by pixel analysis on the running GUI (`#1E1E2A` field, `#D4D4DF` text)
- **Regression coverage:** manual pixel-level verification only; no automated GUI test exists yet

### KNOWN LIMITATION: signed-manifest enforcement is opt-in via env
- **Status:** BY DESIGN
- **Symptom:** `store install` / `store update` reject unsigned or un-trusted blocks when `AIOS_TRUSTED_PUBLIC_KEYS` (or a source's `trusted_public_keys`) is set, while the same install succeeds without it
- **Workaround:** Only set `AIOS_TRUSTED_PUBLIC_KEYS` on systems that require verified-only installs; leave it unset (default) to allow unsigned blocks while still verifying signatures against the embedded key
- **Note:** The bridge `store publish` path uses `BlockInstaller::from_env` (honours `AIOS_TRUSTED_PUBLIC_KEYS`), so a *signed* publish is gated by the local trust policy while *unsigned* publishes stay allowed unless trusted keys are configured (see `docs/ARCHITECTURE.md`, Phase 42)

### KNOWN LIMITATION: `store publish` needs a running update service
- **Status:** BY DESIGN
- **Symptom:** `store publish <file.wasm>` fails with a connection error when no bridge server is listening on `AIOS_BRIDGE_PORT` (default `8080`)
- **Workaround:** Run the kernel (`cargo run --bin aios`) so the bridge serves `POST /api/v1/store/publish`, or point `AIOS_BRIDGE_PORT` at the running update service
- **Note:** The wasm payload is base64-encoded and verified against the computed SHA-256 server-side before install

### KNOWN LIMITATION: Store remote sources require network
- **Status:** BY DESIGN
- **Symptom:** `store search` / `store install` / `store update` against a GitHub or HTTP source fail with a download error when offline or the remote is unreachable
- **Workaround:** Use a local source (`store add-source local:<path>`) for offline installs, or run the built-in update service (`aios-bridge` on `AIOS_BLOCKS_DIR`) and point `store add-source http://host:port` at it
- **Note:** Block binaries are verified against the manifest SHA-256 on install; a tampered payload is rejected with a checksum error

### KNOWN LIMITATION: TUI Web tab renders text only
- **Status:** BY DESIGN
- **Symptom:** The terminal Web tab cannot render CSS/JS/images — pages are shown as structured text (headings `#`, lists `•`/`1.`, tables `|`, `hr`, images as `[alt]`)
- **Note:** This is an inherent terminal limitation, not a regression. For full-fidelity browsing use the native browser: press `W` in any TUI or open the GUI **Browser** tab (F7)
- **Note (v2.17.0):** For JS-heavy sites the text view falls back to a headless Chromium-class render (`--dump-dom`) when the plain fetch yields no readable text. This requires a Chromium-class browser (`msedge`/`chromium`/`google-chrome`) installed and reachable; override the binary with `AIOS_HEADLESS_BROWSER`, add `--no-sandbox` (containers) with `AIOS_HEADLESS_NO_SANDBOX=1`. Without a browser the page is shown as fetched.

### KNOWN LIMITATION: AI Console chat log is a single JSONL file, unbounded
- **Status:** BY DESIGN
- **Symptom:** `AIOS_DATA_DIR/chat.jsonl` grows without rotation; every message (user + assistant, full text) is appended on each reply and the whole transcript is re-written on quit. The TUI AI Console and the GUI AI Studio share the same default file, so the most recent writer wins on boot
- **Workaround:** Delete or archive `aios_data/chat.jsonl` when it grows too large; `/clear` clears the on-screen transcript but does not delete the file
- **Note:** The file is parsed line-by-line so a corrupt trailing line does not break startup

### KNOWN LIMITATION: `/preset` templates are session-scoped (TUI vs GUI files)
- **Status:** RESOLVED in v2.9.1 — presets persist to `AIOS_DATA_DIR/presets.json` in both the TUI and the GUI; custom templates survive restarts
- **Symptom (resolved):** Presets defined with `/preset <name> <text>` were lost on exit before persistence was wired up
- **Note:** TUI and GUI share the same `AIOS_DATA_DIR/presets.json` default path, so the most recent writer wins on boot

## Historical Issues (Fixed)

### BUG-001: Speed test failures in debug mode
- **Status:** FIXED
- **Symptom:** `test_serialize_speed` and `test_deserialize_speed` panic with threshold exceeded in debug builds
- **Root Cause:** Debug builds are 10-20x slower than release due to lack of optimizations. Original thresholds assumed release-mode performance.
- **Fix:** Added `cfg!(debug_assertions)` dual thresholds: 50us for debug, 1us for release
- **Affected files:** `aios-core/src/ipc_protocol.rs:210,234`, `tests/integration_test.rs:95`

### BUG-002: Windows PATH not inherited by cargo
- **Status:** WORKAROUND IN PLACE
- **Symptom:** `cargo` command not found when running from opencode CLI
- **Root Cause:** Windows environment variable propagation between processes
- **Workaround:** Prepend PATH with `$env:Path = [System.Environment]::GetEnvironmentVariable("Path","Machine") + ";" + [System.Environment]::GetEnvironmentVariable("Path","User")` before every cargo invocation
- **Affected:** All cargo commands

### BUG-003: Clippy `skip_while_next` lint on intent engine
- **Status:** FIXED
- **Symptom:** `skip_while().next()` triggers clippy warning
- **Fix:** Replaced with `position().nth()` iterator pattern
- **Affected file:** `aios-tui/src/intent_engine.rs:128-131`

### BUG-004: Clippy `needless_borrow` in block loader
- **Status:** FIXED
- **Symptom:** `hex::encode(&actual)` triggers needless borrow warning
- **Fix:** Changed to `hex::encode(actual)`
- **Affected file:** `aios-block-mgr/src/loader.rs:17`

### BUG-005: Unused imports in integration tests
- **Status:** FIXED
- **Symptom:** 8 clippy warnings for unused imports and mutable variable
- **Fix:** Removed unused imports (`BlockState`, `AIOSException`, `ProcessState`, `HealthCheckFn`, `StateTransferManager`, `Arc`, `Mutex`, `thread`) and `mut` keyword
- **Affected file:** `tests/integration_test.rs:1-17,323`

### BUG-006: Private `now_ms()` function shared across modules
- **Status:** FIXED
- **Symptom:** `E0603: function now_ms is private` in aios-security when access_control.rs and sandbox.rs tried to call `capability::now_ms()`
- **Fix:** Changed `fn now_ms()` to `pub fn now_ms()` in capability.rs
- **Affected files:** `aios-security/src/capability.rs:141`, `access_control.rs:74`, `sandbox.rs:61,72`

### BUG-007: Watchdog tests failed without sleep between checks
- **Status:** FIXED
- **Symptom:** `test_missed_heartbeats_trigger_suspend` and related tests panic because heartbeat age was 0ms (freshly created), never exceeding the 100ms interval
- **Fix:** Added `thread::sleep(Duration::from_millis(120))` between check_timeout calls; adjusted loop count from 3 to 2 (since 3 overdue checks triggers suspend)
- **Affected file:** `aios-watchdog/src/watchdog.rs:207-259`

### BUG-008: Sandbox memory limit test arithmetic error
- **Status:** FIXED
- **Symptom:** `assertion failed: !sb.allocate_memory(1)` because 500+499+1=1000 which is NOT > 1000
- **Fix:** Changed to 500+500+1 so 1000+1 > 1000 triggers the limit
- **Affected file:** `aios-security/src/sandbox.rs:164`

### BUG-009: Context store missing type imports
- **Status:** FIXED
- **Symptom:** `E0425: cannot find type TelemetryStore/WorkflowStore/StabilityStore` in store.rs
- **Fix:** Added `use crate::telemetry::TelemetryStore` etc. imports
- **Affected file:** `aios-context/src/store.rs:1-3`

### BUG-010: schedule_next early-break prevents correct aging behavior
- **Status:** FIXED (v0.4.0)
- **Symptom:** `test_scheduler_aging_starvation_prevention` fails — low-priority process with aging boost not selected over high-priority process
- **Root Cause:** Inner loop in `schedule_next()` had `break` after finding the first Ready process in each priority queue. With aging, a later process in the queue could have a higher effective priority than an earlier one, but the `break` prevented evaluating all of them.
- **Fix:** Removed `break` so all Ready processes in a queue are evaluated. Candidate selection picks the one with highest effective priority.
- **Affected file:** `aios-process-mgr/src/scheduler.rs:320`

### BUG-011: Flaky dependency graph tests due to non-deterministic topological sort
- **Status:** FIXED (v0.4.0)
- **Symptom:** `test_unload_order_reversed` and `test_block_dependency_graph_ordering` intermittently fail
- **Root Cause:** Kahn's algorithm processes nodes via `VecDeque` seeded from `HashMap` iteration, which is non-deterministic. Independent nodes (no dependency between them) can appear in any order. Tests assumed fixed ordering.
- **Fix:** Changed tests to verify only dependency constraints (if X depends on Y, then Y appears before X in load order), not absolute positions of independent nodes.
- **Affected files:** `aios-block-mgr/src/dependency.rs:225-236`, `tests/integration_test.rs`

## Potential Future Issues

### RISK-001: AtomicU64 packet_id wrapping
- **Description:** `PACKET_COUNTER` in ipc_protocol.rs is an `AtomicU64` that will wrap around after 2^64 packets
- **Impact:** Extremely unlikely in practice (would take years at high throughput)
- **Mitigation:** Not currently addressed; could add wrapping_add or reset logic

### RISK-002: Non-recursive `find_by_name` search
- **Description:** `BlockRegistry::find_by_name` does linear scan of all entries
- **Impact:** O(n) per lookup; negligible at current scale but could matter with thousands of blocks
- **Mitigation:** Could add secondary name -> id HashMap index

### RISK-003: No block unloading during active hot-swap
- **Description:** `BlockLoader::unload_block` logs a warning but does not prevent unloading an active block
- **Impact:** Could cause in-flight IPC messages to target a removed block
- **Mitigation:** Active blocks should be frozen before unload in production use

### RISK-004: Watchdog not integrated with orchestrator thread
- **Status:** FIXED (v0.3.0)
- **Description:** Watchdog crate was implemented but not wired to the AI Orchestrator thread in main.rs
- **Fix:** Watchdog heartbeat thread now runs in background during TUI session; dashboard header shows live watchdog state
- **Affected files:** `aios-tui/src/main.rs`, `aios-tui/src/dashboard.rs`

### RISK-005: WebView runs on a background thread — macOS incompatible
- **Description:** `WebBrowser` in `aios-webview` runs the winit event loop on a background thread; on macOS winit/wry require the event loop on the main thread
- **Impact:** Browser window will not open on macOS builds
- **Mitigation:** Documented; not addressed. On macOS spawn the webview from the app's main thread or use `build_as_child` inside the egui window (see TODO Phase 34)

### RISK-006: GUI Browser is a companion window, not embedded
- **Description:** The GUI Browser tab opens the webview in a separate OS window next to the egui dashboard rather than as a child viewport of the tab
- **Impact:** Cosmetic UX gap — window focus/navigation differs from an embedded browser
- **Mitigation:** Future work: `build_as_child` on Windows/macOS/X11 to render inside the egui tab (see TODO Phase 34)

### BUG-012: Recovery log get_pending_entries ignores completed IDs
- **Status:** FIXED (v1.0.0)
- **Symptom:** `test_recovery_log_pending` fails — `get_pending_entries()` returns completed entries instead of filtering them out
- **Root Cause:** The function skipped `COMPLETED:` marker lines but never collected the IDs from those markers to exclude matching entries. All entries with `status == "pending"` were returned regardless of whether they had been marked complete.
- **Fix:** Collect completed IDs from `COMPLETED:` lines first, then filter entries by excluding those whose ID is in the completed set.
- **Affected file:** `aios-persistence/src/recovery.rs:108-140`

### BUG-013: Kernel (`aios`) booted with an empty block registry — no browser
- **Status:** FIXED (v2.2.0)
- **Symptom:** On a fresh machine the kernel started with zero registered blocks; `aios-browser` was a plain library (no `StatefulBlock` impl), so no browser was available at boot and there was no auto-launch of the OS browser.
- **Root Cause:** `aios/src/orchestrator.rs:73` created an empty `BlockRegistry`; the 3-block boot sequence existed only in `aios-tui`/`aiosd`; `BrowserEngine` never implemented `handle_message`.
- **Fix:** Added `BrowserBlock` (`StatefulBlock`) in `aios-browser/src/block.rs`; kernel now registers hal/ipc_bus/scheduler/browser at boot, boot-discovers `AIOS_BLOCKS_DIR`, wires the browser handler into the `MessageRouter`, and the TUI `b` hotkey opens URLs in the OS default browser via the block.
- **Affected files:** `aios-browser/src/block.rs`, `aios/src/orchestrator.rs`, `aios/src/tui/mod.rs`, `aios/src/tui/ui.rs`, `aios/src/tui/app_state.rs`, `aios-tui/src/main.rs`, `aios-daemon/src/main.rs`

### BUG-014: DuckDuckGo search dropped first letter of result URLs
- **Status:** FIXED (v2.2.0)
- **Symptom:** `test_duckduckgo_parse_results` failed — every result URL lost its leading `h` (e.g. `ttps://example.com`)
- **Root Cause:** `DuckDuckGoBackend::parse_html_response` advanced past `href="` (6 chars) by 7
- **Fix:** Offset corrected from `+7` to `+6`
- **Affected file:** `aios-search/src/backends.rs:68`

### BUG-015: HtmlParser::extract_text included <head>/<title> text
- **Status:** FIXED (v2.2.0)
- **Symptom:** `test_html_parser_extract_text` failed — page text was `"Test Hello world"` instead of `"Hello world"`
- **Root Cause:** `extract_text` stripped only `<script>`/`<style>` and tags, leaving `<head>`/`<title>` text in the body text
- **Fix:** `extract_text` now strips `<head>...</head>` before tag removal; added `test_extract_text_strips_head`
- **Affected file:** `aios-browser/src/html_parser.rs:22-36`

### BUG-016: Chaos rapid-fire test asserted plaintext of redacted report
- **Status:** FIXED (v2.2.0)
- **Symptom:** `test_chaos_reporter_rapid_fire` failed — `json.contains("event #0")` was false
- **Root Cause:** Events with even indices used `zero_knowledge=true`, so `event #0` was SHA-256 hashed and never appeared in the JSON output
- **Fix:** Assertions now check redaction semantics: `event #0` absent, `event #1`/`event #99` present, `"redacted":true` present
- **Affected file:** `tests/chaos_test.rs:341-344`

### BUG-017: WorkflowCompiler emitted init/start with a return value
- **Status:** FIXED (v2.2.0)
- **Symptom:** `test_e2e_easylang_wasm_pipeline` failed — `result.functions_called` never contained `init`/`start`
- **Root Cause:** `WorkflowCompiler::generate_wat` exported `init`/`start` with `(result i32)`, but `BlockExecutor::execute_block` invokes them with an empty results buffer; the calls errored (logged as warnings) and the functions were not recorded
- **Fix:** `init`/`start` now export without a result, matching the executor contract and its unit fixtures
- **Affected file:** `aios-builder/src/compiler.rs:60-62`

### BUG-018: Bridge /api/v1/metrics never populated
- **Status:** FIXED (v2.2.0)
- **Symptom:** `test_e2e_bridge_http_endpoints` failed — Prometheus text contained no `HELP`
- **Root Cause:** `BridgeContext::metric_collector` was constructed but no handler recorded metrics, so `to_prometheus()` always returned an empty string
- **Fix:** Added axum request middleware `record_metrics` that records `http_requests_total` (counter), `http_last_latency_ms` (gauge) and `http_request_latency_ms` (histogram) for every request
- **Affected file:** `aios-bridge/src/server.rs`

### BUG-019: Fault-tolerance test asserted mid-quantum preemption
- **Status:** FIXED (v2.2.0)
- **Symptom:** `test_fault_tolerance_scheduler_survives_crash` failed — "Replacement (high priority) should be next"
- **Root Cause:** The scheduler continues the current process until its time-slice quota expires (time-slicing, no preemption), but the test scheduled once, then expected the newly spawned High process to run immediately while the current quantum was still active
- **Fix:** The test now spawns the replacement before the final `schedule_next()`, matching the scheduler contract verified by `test_priority_scheduling`
- **Affected file:** `tests/stress_fault_tolerance.rs:266-275`

### BUG-020: Safe-Mode Shell commands always returned "Unknown command"
- **Status:** FIXED (v2.2.2)
- **Symptom:** On the `aios-tui` Shell tab every SafeModeShell command (`ps`, `kill`, `spawn`, `status`, `logs`, `restart`, `help`, `blocks`, `load`, `unload`) printed `Error: Unknown command`; only `fetch`/`search`/`open`/`clear` worked
- **Root Cause:** `execute_shell_cmd` in `aios-tui/src/main.rs:160` mapped every unrecognized command to `ShellCommand::Unknown(cmd.to_string())`, bypassing `SafeModeShell::parse_command` — only the TUI's own four commands reached the SafeModeShell, so the entire safe-mode command set was unreachable
- **Fix:** Commands now route through `SafeModeShell::parse_command`; `help`/`?` additionally list the TUI-specific commands; `blocks` output now prints the block state cleanly (`Active`, not `Some(Active)`) via `registry.topology_with_state()`
- **Affected files:** `aios-tui/src/main.rs:160-177`, `aios-watchdog/src/safe_mode.rs`

### BUG-021: extract_text returned empty text for pages with `<!DOCTYPE html>`
- **Status:** FIXED (v2.7.0)
- **Symptom:** `HtmlParser::extract_text` returned an empty string for any page whose root is `<!DOCTYPE html><html>…`; all browsed pages appeared blank in the TUI Web tab
- **Root Cause:** `extract_text` iterated the body's own text instead of walking the element children of the document root, so a doctype-first document produced no text
- **Fix:** `extract_text` now iterates the element children of the document root; added regression test `test_extract_text_with_doctype`
- **Affected file:** `aios-browser/src/html_parser.rs`

### BUG-022: IpcBus `DropOldest` evicted the most critical packet
- **Status:** FIXED (v2.7.0)
- **Symptom:** with `BoundedBusPolicy::DropOldest`, overflow discarded the highest-priority queued packet and kept the least important one
- **Root Cause:** `DropOldest` popped from the front, but the queue is ordered highest-priority-first (send time order for equal priorities)
- **Fix:** `DropOldest` now pops from the back (lowest priority); added `test_drop_oldest_keeps_highest_priority`
- **Affected file:** `aios-ipc/src/bus.rs`

### BUG-023: Bridge status handler never listed the newest process
- **Status:** FIXED (v2.7.0)
- **Symptom:** `GET /api/v1/status` and the `status` intent showed every process except the newest one
- **Root Cause:** the handler probed PIDs `0..process_count`, but process IDs start at 1, so the last (newest) process was always skipped
- **Fix:** the handler now iterates `scheduler.all_processes()`; the TUI processes tab uses the same source
- **Affected files:** `aios-bridge/src/server.rs`, `aios/src/tui/ui.rs`

### BUG-024: Bridge `MetricType::All` hardcoded process_count to 0
- **Status:** FIXED (v2.7.0)
- **Symptom:** metrics for the `All` metric type always reported `process_count = 0`
- **Root Cause:** the count was read after `scheduler` was dropped (moved into the report), so it always evaluated to 0
- **Fix:** `process_count` is captured before the scheduler is dropped
- **Affected file:** `aios-bridge/src/server.rs`

### BUG-025: TUI Web back-navigation ping-ponged forever
- **Status:** FIXED (v2.7.0)
- **Symptom:** pressing `b` to go back to page A, then `b` again, returned to B instead of staying on A — A↔B infinite loop
- **Root Cause:** `load_url` always pushed the URL onto the history stack, including when called for back-navigation, re-adding the page that was just popped
- **Fix:** `load_url` gained `push_history: bool`; back navigation pops without re-pushing; all call sites updated
- **Affected file:** `aios-tui/src/main.rs`

### BUG-026: Rapid `B` presses spawned multiple native browser windows
- **Status:** FIXED (v2.7.0)
- **Symptom:** pressing `B` repeatedly in the TUI Web tab started several native browser instances
- **Root Cause:** no guard between the keypress and the spawn; each press launched the OS browser
- **Fix:** a `WEB_BROWSER_SPAWNING` atomic guard allows one spawn in flight until the child is reported
- **Affected file:** `aios-tui/src/main.rs`

### BUG-027: GUI browser open blocked the egui UI up to 45 s
- **Status:** FIXED (v2.7.0)
- **Symptom:** opening the WebView from the GUI Browser tab froze the dashboard for up to 45 s (window init timeout)
- **Root Cause:** `WebBrowser::open` ran synchronously on the egui thread
- **Fix:** the open runs on a background thread (`pending_browser`/`pending_browser_error` slots, `browser_opening` guard); `poll_browser_open` picks up the result each frame; repeated opens during startup are ignored
- **Affected file:** `aios-gui/src/app.rs`

### BUG-028: DuckDuckGo `uddg` redirect URL not decoded
- **Status:** FIXED (v2.7.0)
- **Symptom:** result URLs pointed at `https://duckduckgo.com/l/?uddg=%2F...` instead of the real target
- **Root Cause:** `DuckDuckGoBackend` returned the redirect URL as-is
- **Fix:** `resolve_duckduckgo_url` unwraps the `uddg` parameter, skipping non-http/s values; `aios-search` adds the `url` dependency; 4 tests
- **Affected files:** `aios-search/src/backends.rs`, `aios-search/Cargo.toml`

### BUG-029: `save_telemetry` clobbered earlier batches
- **Status:** FIXED (v2.7.0)
- **Symptom:** saving telemetry twice left only the latest batch in the store
- **Root Cause:** every batch was written under the same key
- **Fix:** keys are assigned from a monotonic `TELEMETRY_NEXT_KEY` counter persisted in `META_TABLE`; added `test_save_telemetry_does_not_clobber_previous_batches`
- **Affected file:** `aios-context/src/persistence.rs`

### BUG-030: compressed-telemetry chunk keys collided each round
- **Status:** FIXED (v2.7.0)
- **Symptom:** each compression round overwrote the previous chunk, so only the last compression survived
- **Root Cause:** chunk keys were derived from a timestamp/metric key that was identical across rounds
- **Fix:** chunks use a monotonic `next_chunk_id`; removed `chrono_block_name`; added `test_multiple_compression_rounds_do_not_collide`
- **Affected file:** `aios-context/src/compressed_telemetry.rs`

### BUG-031: `response_err` discarded the error message
- **Status:** FIXED (v2.7.0)
- **Symptom:** IPC error responses carried an empty payload, so callers saw a generic failure with no message
- **Root Cause:** `response_err` built the response with `Payload::Empty`
- **Fix:** the message is carried as `Payload::Text(msg)`; added `test_response_err_carries_message`
- **Affected file:** `aios-core/src/ipc_protocol.rs`

### BUG-032: capability `remaining_ms` was inverted
- **Status:** FIXED (v2.7.0)
- **Symptom:** long-lived capabilities reported ~0 ms remaining; `remaining_ms` grew as expiry approached
- **Root Cause:** `remaining_ms` computed `now − expires`
- **Fix:** now `expires_at_ms.saturating_sub(now_ms())`; added a test for a future expiry
- **Affected file:** `aios-security/src/capability.rs`

### BUG-033: priority-inheritance counter never incremented
- **Status:** FIXED (v2.7.0)
- **Symptom:** `total_inheritances` always reported 0 even when priority boosts happened
- **Root Cause:** the field was declared but never incremented
- **Fix:** the counter increments in both the `acquire_lock` and `request_resource` boost paths and is surfaced via `state()`; tests added
- **Affected file:** `aios-process-mgr/src/priority_inheritance.rs`

### BUG-034: `restore_linear_memory` silently truncated oversized data
- **Status:** FIXED (v2.7.0)
- **Symptom:** restoring a larger state snapshot into linear memory silently dropped the trailing bytes
- **Root Cause:** the copy length was `min(data, memory)`
- **Fix:** restore now fails explicitly when data exceeds the linear memory; `aios-live-update` logs a warning; added `test_restore_linear_memory_rejects_oversized_data`
- **Affected files:** `aios-wasm/src/sandbox.rs`, `aios-live-update/src/wasm_engine.rs`

### BUG-035: CPU affinity applied to the scheduler thread
- **Status:** FIXED (v2.7.0)
- **Symptom:** `set_cpu_affinity` pinned the scheduler thread (OS affinity targets the calling thread) instead of the spawned process thread
- **Root Cause:** the OS call was invoked from the scheduler context
- **Fix:** the mask is stored per-thread (`Arc<Mutex<Vec<usize>>>`) and applied by the spawned thread itself before running the payload; `validate_cores` pre-validates the mask; `set_cpu_affinity` no longer touches the calling thread
- **Affected files:** `aios-process-mgr/src/cpu_affinity.rs`, `aios-process-mgr/src/scheduler.rs`

### BUG-036: TUI/bridge lock-order inversion
- **Status:** FIXED (v2.7.0)
- **Symptom:** deadlock risk — the TUI blocks tab locked `scheduler → registry` while the bridge used `registry → scheduler`
- **Fix:** both sides now lock `scheduler → registry`
- **Affected file:** `aios/src/tui/ui.rs`

### BUG-037: WMIC `AdapterRAM` 32-bit overflow
- **Status:** FIXED (v2.7.0)
- **Symptom:** GPUs with more than 4 GB VRAM reported a bogus ~4 GB; `0xFFFFFFFF` overflow
- **Fix:** `0xFFFFFFFF` (`AdapterRAM` > 4 GB) is treated as unknown (0)
- **Affected file:** `aios/src/hw_probe.rs`

### BUG-038: wasm `timeout_ms` never enforced — epoch deadline never reached
- **Status:** FIXED (v2.7.0)
- **Symptom:** no host-side ticker ever incremented the engine epoch, so `timeout_ms` was not enforced as wall-clock time; only the fuel limit bounded runaway wasm (an infinite loop ran until fuel ran out)
- **Root Cause:** `set_epoch_deadline(1)` armed the store, but nothing called `Engine::increment_epoch()`, so the deadline was unreachable (engine epoch stayed 0)
- **Fix:** a per-engine background ticker (`EpochTicker`) calls `Engine::increment_epoch()` every `timeout_ms / 4`; every store is armed with `EPOCH_TICKS_PER_TIMEOUT = 4` ticks, and `call_func`/`instantiate` (plus the executor's `init`/`start`) re-arm the deadline before each wasm call so long-lived stores keep working while every call is bounded by `timeout_ms`
- **Tests:** `test_epoch_timeout_interrupts_runaway_wasm` (infinite loop interrupted in ~150 ms with fuel that alone would take ~10 s) and `test_epoch_deadline_rearmed_between_calls`
- **Affected files:** `aios-wasm/src/sandbox.rs`, `aios-wasm/src/executor.rs`

### BUG-039: GUI/TUI crash on hardware detect — `wmic` CSV rows indexed past bounds
- **Status:** FIXED (v2.9.1)
- **Symptom:** `aios-gui` (and any binary running `HardwareProfile::detect`) panicked at startup with `index out of bounds: the len is 2 but the index is 2` in `aios-hal/src/hardware.rs` — the GUI window never opened
- **Root Cause:** `detect_memory` parsed `wmic memorychip get Capacity,Speed,DimmLocator /format:csv` by indexing `parts[2]` after only checking `parts.len() >= 2`; on machines where wmic emits short rows (e.g. a blank Speed column collapsing to a 2-field line) the indexing panicked
- **Fix:** extraction into a pure helper `HardwareProfile::parse_wmic_memory_csv` that requires `parts.len() >= 3` (Node + Capacity + Speed) before touching any index; malformed/short rows are skipped instead of crashing
- **Tests:** `test_parse_wmic_memory_csv_full_rows` (two DIMMs summed, speed read) and `test_parse_wmic_memory_csv_short_rows_no_panic` (short 2-field row skipped, valid row still parsed)
- **Affected files:** `aios-hal/src/hardware.rs`
