# AIOS Full Project Audit

> Audit baseline: v2.28.1 · Date: 2026-08-22 · Machine: Windows x64 (MSVC)
> Post-audit verification: v2.84.0 · Date: 2026-11-26 — see §8.
> Companion: `docs/SCHEME.md` / `docs/SCHEME.ru.md` — program scheme with function map.

## 1. Scope and Method

Baseline full audit of the workspace (39 member crates + 3 standalone crates) performed on 2026-08-22; post-audit fixes and the current verification snapshot are in §8:

| Check | Command | Requirement |
|---|---|---|
| Build | `cargo build --workspace` | success |
| Lint | `cargo clippy --workspace --all-targets` | 0 warnings |
| Format | `cargo fmt --all -- --check` | clean |
| Tests | `cargo test --workspace` | all green |
| Docs sync | manual review of `docs/*` EN/RU pairs | in sync |
| Structure | per-crate module/function mapping | documented in SCHEME.md |

## 2. Results

| Check | Result |
|---|---|
| `cargo build --workspace` | ✅ OK (~7 min, dev profile `debug=false`) |
| `cargo clippy --workspace --all-targets` | ✅ **0 warnings** |
| `cargo fmt --check` | ✅ clean (exit 0) |
| `cargo test --workspace` | ✅ **1824 passed / 0 failed** in 100 suites at the v2.84.0 re-verification (baseline 2026-08-22: 1338 passed / 91 suites) |

Historical baseline rows are kept for comparison; the current gates and the fix list are in §8.

## 3. Codebase Statistics

- Baseline snapshot: **244 `.rs` files**, **~59,400 lines of Rust**. Current v2.84.0 snapshot: **358 `.rs` files**, **~125.5k non-blank lines** (~136.9k with blanks) — see `docs/SCHEME.md` §12.
- Baseline: **~1,336 test annotations** (`#[test]` + `#[tokio::test]`) across the workspace, consistent with the 1338 executed tests. Current: 2,011 annotations in 232 files, 1824 executed tests in 100 suites.
- Standalone at baseline (excluded from workspace): `aios-init` (334 ln), `aios-kernel` (~1.3k ln), `aios-kernel-run` (74 ln). The bare-metal track now has 13 standalone crates (see `docs/SCHEME.md` §1/§10).

### Per-crate table (files / lines / tests)

| Crate | Files | Lines | Tests |
|---|---|---|---|
| aios-autohal | 12 | 4399 | 73 |
| aios-tui | 4 | 4346 | 40 |
| aios (kernel TUI) | 6 | 3630 | 9 |
| tests/ (integration) | 14 | 3882 | 162 |
| aios-gui | 18 | 3282 | 10 |
| aios-cluster | 8 | 2961 | 31 |
| aios-process-mgr | 7 | 2608 | 73 |
| aios-block-mgr | 8 | 2102 | 75 |
| aios-store | 8 | 2112 | 58 |
| aios-exec-compat | 6 | 1947 | 89 |
| aios-hal | 3 | 1810 | 34 |
| aios-vfs | 5 | 1865 | 29 |
| aios-wasm | 5 | 1576 | 56 |
| aios-net | 5 | 1411 | 51 |
| aios-fm | 6 | 1319 | 16 |
| aios-kernel (standalone) | 10 | 1274 | hw/QEMU only |
| aios-live-update | 5 | 1218 | 23 |
| aios-watchdog | 5 | 1181 | 47 |
| aios-context | 7 | 1092 | 36 |
| aios-core | 9 | 1098 | 38 (+proptest) |
| aios-optim | 6 | 964 | 39 |
| aios-bridge | 5 | 1539 | 0 unit / 24 integ |
| aios-net-config | 5 | 858 | 32 |
| aios-tee | 4 | 841 | 28 |
| aios-mpk | 4 | 816 | 27 |
| aios-ipc | 5 | 806 | 29 |
| aios-security | 5 | 763 | 31 |
| aios-llm | 5 | 724 | 13 |
| aios-builder | 5 | 598 | 23 |
| aios-ringbuf | 6 | 653 | 16 (+proptest) |
| aios-persistence | 5 | 680 | 12 |
| aios-compress | 5 | 572 | 16 |
| aios-iommu | 4 | 528 | 25 |
| aios-telemetry | 4 | 542 | 17 |
| aios-browser | 8 | 1354 | 36 |
| aios-updater | 4 | 406 | 18 |
| aios-init (standalone) | 1 | 334 | — |
| aios-webview | 2 | 323 | 7 |
| aios-debug | 3 | 322 | 10 |
| aios-search | 5 | 438 | 7 |
| aios-daemon | 1 | 155 | 0 (integ covered) |
| aios-studio (SPA, non-Rust) | — | — | — |

> The per-crate table is the 2026-08-22 baseline; current crate counts and top-crate sizes are in `docs/SCHEME.md` §12.

## 4. Findings and Actions Taken

### F1. Flaky timing threshold in RT stress test — FIXED
- **Where:** `tests/stress_test.rs::test_stress_rt_scheduler_500` (line ~113).
- **Symptom:** failed on this machine under parallel test load: "RT scheduling took 2.0974127s (>2s)". Logic itself was correct (500 processes scheduled).
- **Root cause:** hard-coded 2 s limit for a wall-clock measurement in a debug build on a loaded machine — violates the project rule that speed tests use **dual debug/release thresholds**.
- **Fix:** limit is now `5000 ms` under `cfg!(debug_assertions)` and `2000 ms` in release. Test re-run: green (11/11).
- Files: `tests/stress_test.rs`, `docs/BUGS*.md`.

### F2. Roadmap inconsistency — FIXED
- `docs/TODO.md` listed **Phase 46 (aios-autohal)** as unchecked `[ ]` while every sub-item was checked and shipped in v2.22.0–v2.25.0. Marked `[x]` (EN+RU).

### F3. Documentation coverage
- The workspace had no call-level scheme document; added `docs/SCHEME.md` + `docs/SCHEME.ru.md` (per-crate function map, boot flow, IPC flow, kernel milestones, integration-test map).

## 5. Architecture Rule Compliance

| Rule (AGENTS.md) | Status |
|---|---|
| Inter-crate communication via `aios_core::ipc_protocol` types only | ✅ verified during mapping (bus/router/state-transfer all operate on `IpcPacket`) |
| No direct memory pointers between blocks | ✅ data exchange via `IpcPacket`; WASM isolation is shared-nothing |
| Every `StatefulBlock` implements `extract_state()`/`restore_state()` | ✅ trait-enforced (`aios-core::block`) |
| Speed tests dual thresholds (debug/release) | ⚠️→✅ violation found in F1 and fixed |
| Unit tests inside `#[cfg(test)]`, integration in `tests/` | ✅ followed everywhere |
| Error handling returns `aios_core::error::Result<T>` | ✅ dominant pattern (`AIOSException`) |

## 6. Observations & Risks (no action required now)

1. **Timing-sensitive tests**: even with relaxed debug thresholds, wall-clock stress assertions can flake on heavily loaded CI machines. Consider cgroup/CPU pinning or percentile-based budgets if CI flakes recur.
2. **`aios-bridge` has zero unit tests** — behavior is covered by `tests/bridge_tests.rs` (24) and endpoint smoke paths, but pure logic (`IntentParser`, DTO round-trips) would benefit from inline tests.
3. **`aios-daemon` has no unit tests** — acceptable for an 183-line binary; lifecycle is exercised indirectly by Docker entrypoint usage.
4. **Bare-metal track** (`aios-kernel`) still has no automated hardware tests; verification is QEMU smoke output in CHANGELOG. M3/M4 shipped, serial self-checks exist, and `aios-kernel-run` now has 2 unit tests covering version parsing and the dual-entry Limine menu (§8).
5. **Deferred work** remains: webview `build_as_child` embedding, formal verification of safety properties (TODO → Deferred).
6. **Doc sync**: all six bilingual doc pairs are present and consistent as of this audit (ARCHITECTURE, CHANGELOG, BUGS, TODO, INTERFACE + new SCHEME/AUDIT).

## 7. Conclusion

The baseline audit closed with a clean, clippy-clean, fully formatted workspace: 1338 tests passed with zero failures after one flaky-threshold fix. The current v2.84.0 snapshot is also clean: 1824 tests in 100 suites, zero failures. Architecture rules hold across all mapped crates. Milestones M3/M4 shipped; the remaining work recorded in §8 is provisioning reliability — hybrid image rebuild and flashing — plus hardware verification of the dual-kernel boot path.

## 8. Post-audit fixes (v2.84.0, 2026-11-26)

The following fixes were implemented in code after the baseline audit. They are recorded here for traceability; the current function map is `docs/SCHEME.md`.

### P1. Version honesty

- `aios-kernel/src/version.rs` is the single source of truth for the displayed release: `SEMVER = env!("CARGO_PKG_VERSION")` and `KERNEL = concat!("AIOS kernel v", env!("CARGO_PKG_VERSION"))`.
- Boot banner and config line (`main.rs`), panic/fatal reports (`main.rs`, `interrupts.rs`), TUI status/About/`ver` (`tui.rs`) and GUI About/window titles/Files preview (`gui.rs`) all read these constants.
- `aios-kernel/Cargo.toml` (`version = "2.84.0"`) is the only version literal; no stale hardcoded `v2.x.y` display strings remain in `aios-kernel/src` except intentional loader/fallback identifiers.

### P2. Config honesty

- `aios-kernel/src/config.rs` adds `KeyPolicy { Effective, Informational }`, `policy(key)`, `validate_informational()` and `Summary.invalid_values`. Only `blocks.<name>.rights` and `blocks.<name>.fuel` are `Effective`; `system.*`, `boot.*`, `net.*` and unknown keys are `Informational` — parsed, validated and logged with their source, but never applied as runtime knobs.
- `boot.debug` / `net.dhcp` booleans and positive-integer `net.mtu` are validated; invalid informational values are counted and warned about.
- `main.rs` logs every `key = "value" (layer, policy)` and a summary `[cfg] system = ... (N layer(s), N key(s), N invalid layer(s), N invalid value(s))`. Timer `interrupts::TIMER_HZ`, heap `heap::HEAP_SIZE` and the release version are compile-time facts, not configuration keys.
- Stock templates `boot/assets/aios/config/{system,net,blocks}.conf` carry matching Effective/Informational comments; their copies in `boot/out/assets/aios/config` are byte-identical (SHA-256 verified).

### P3. Ring-3 pointer gate

- `aios-kernel/src/memory.rs` defines `USER_SPACE_LIMIT` (`0x0000_8000_0000_0000`), `user_range(virt,len)`, `translate_user(virt,len,write)` and `walk_user_leaf()`. `translate_user` is the **only** gate used by `int 0x80` for user buffers: it rejects null (except `len == 0`), higher-half / non-canonical addresses, `virt + len` wrap, unmapped pages, supervisor-only PTEs and read-only pages on writes. It checks `PRESENT|USER` at PML4/PDPT/PD/leaf and `WRITABLE` at every level when writing, including huge pages.
- `syscalls.rs` uses `user_read` / `user_write` returning `Option`, `write_user_string` with an overflow-safe loop, `u64::MAX` on invalid socket buffers, and one full-range validation per buffer. `gui.rs` `SYS_GUI CREATE` validates the 16-byte `CreateReq`, buffer area and title via `translate_user`; `draw_client()` revalidates the client buffer on every composite. `memory::translate()` remains reserved for kernel/driver/DMA use.

### P4. Flashing reliability

- `scripts/flash-usb.ps1` locks and dismounts volumes via `DeviceIoControl` (`FSCTL_LOCK_VOLUME` / `FSCTL_DISMOUNT_VOLUME`), opens physical disks with `FileMode.Open` + `FileShare.ReadWrite`, writes 512-byte sector-aligned padded 1 MiB blocks, calls `Flush($true)` and unlocks afterwards.
- Recovery runs bounded `Update-Disk` + `pnputil /rescan-disks` jobs with `AIOS_FLASH_RECOVERY_TIMEOUT` (default 30 s) and probes disk readiness instead of hanging on `ERROR_NOT_READY`.
- Full-image SHA-256 read-back must match the source, and the MBR `55 AA` signature is checked. The elevated relaunch redirects stdout/stderr to temp logs and replays them.

### P5. Hybrid build

- `aios-kernel-run/src/main.rs` parses the kernel version from the `[package]` section of `aios-kernel/Cargo.toml`, derives the current menu label as `v<major>.<minor>+` (`v2.84+`), stages both current and fallback ELFs and writes a two-entry Limine menu (current + `v2.79` rollback). The stale ISO is deleted before rebuilding.
- `verify_hybrid_image()` checks MBR `55 AA` and ISO9660 `CD001` markers on both the ISO and the USB IMG. Fallback defaults to `aios-kernel-run/out/aios-kernel-v279.elf` when present; clean clones must supply a real ELF via `AIOS_FALLBACK_KERNEL`, and `AIOS_FALLBACK_VERSION` defaults to `v2.79`.
- **Not executed here:** no fresh hybrid rebuild was run in this environment — `xorriso.exe` (`iso/msys/usr/bin`) and the Limine binaries (`iso/limine/limine-binary`) are absent. Only code/unit-test verification was possible (`aios-kernel-run` tests 2/2). The existing physical stick remains **Legacy-BIOS-only** until flashed with a clean rebuilt hybrid image.

### Verification (current)

| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | ✅ clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ 0 warnings |
| `cargo test --workspace` | ✅ 1824 passed / 0 failed in 100 suites |
| `cargo check` / `cargo clippy` for `aios-kernel` (default and `--features limine`) | ✅ pass |
| `aios-kernel-run` tests | ✅ 2/2 passed |
| PowerShell parser check (`scripts/flash-usb.ps1`) | ✅ pass |
| `cargo test --manifest-path aios-kernel/Cargo.toml` | fails with duplicate lang item `panic_impl` **by design** for the freestanding `no_std` binary; not a required gate |
