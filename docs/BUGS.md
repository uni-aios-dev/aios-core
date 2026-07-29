# AIOS Known Bugs & Workarounds

## Current: None (Clean Build)

As of v1.0.0, all 708 tests pass and clippy reports zero warnings.

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

### BUG-012: Recovery log get_pending_entries ignores completed IDs
- **Status:** FIXED (v1.0.0)
- **Symptom:** `test_recovery_log_pending` fails — `get_pending_entries()` returns completed entries instead of filtering them out
- **Root Cause:** The function skipped `COMPLETED:` marker lines but never collected the IDs from those markers to exclude matching entries. All entries with `status == "pending"` were returned regardless of whether they had been marked complete.
- **Fix:** Collect completed IDs from `COMPLETED:` lines first, then filter entries by excluding those whose ID is in the completed set.
- **Affected file:** `aios-persistence/src/recovery.rs:108-140`
