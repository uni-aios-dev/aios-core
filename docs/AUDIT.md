# AIOS Full Project Audit

> Version: v2.28.1 · Date: 2026-08-22 · Machine: Windows x64 (MSVC)
> Companion: `docs/SCHEME.md` / `docs/SCHEME.ru.md` — program scheme with function map.

## 1. Scope and Method

Full audit of the workspace (39 member crates + 3 standalone crates) performed on 2026-08-22:

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
| `cargo test --workspace` | ✅ **1338 passed / 0 failed** in 91 suites (unit + integration + doc-tests) |

## 3. Codebase Statistics

- **244 `.rs` files**, **~59,400 lines of Rust** total.
- **~1,336 test annotations** (`#[test]` + `#[tokio::test]`) across the workspace — consistent with the 1338 executed tests.
- Standalone (excluded from workspace): `aios-init` (334 ln), `aios-kernel` (~1.3k ln), `aios-kernel-run` (74 ln).

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
4. **Bare-metal track** (`aios-kernel`) has no automated tests by nature; verification is QEMU smoke output documented in CHANGELOG (v2.26–v2.28). Milestones M3/M4 should add serial-output self-checks.
5. **Deferred work** remains: webview `build_as_child` embedding, formal verification of safety properties (TODO → Deferred).
6. **Doc sync**: all six bilingual doc pairs are present and consistent as of this audit (ARCHITECTURE, CHANGELOG, BUGS, TODO, INTERFACE + new SCHEME/AUDIT).

## 7. Conclusion

The codebase is healthy: builds cleanly, clippy-clean, fully formatted, and 1338 tests pass with zero failures after one flaky-threshold fix. Architecture rules hold across all mapped crates. Recommended next development step remains the microkernel roadmap: **Milestone 3 — preemption** (timer-driven scheduler + context switch + ring 0/3), followed by M4 (kernel IPC).
