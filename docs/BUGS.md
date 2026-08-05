# AIOS Known Bugs & Workarounds

## Current: No known defects (Clean Build)

As of v2.8.0, all tests pass, clippy reports zero warnings, and the 18 bugs found in the v2.7.0 bug-fix pass (BUG-021…BUG-038) are fixed and covered by regression tests. The v2.8.0 restructure of the kernel TUI to 7 tabs, the `--safe-mode` boot flag and the GUI AI Studio / Network Settings tabs added no new known defects. See the Historical Issues section and `docs/CHANGELOG.md` v2.8.0.

### KNOWN LIMITATION: signed-manifest enforcement is opt-in via env
- **Status:** BY DESIGN
- **Symptom:** `store install` / `store update` reject unsigned or un-trusted blocks when `AIOS_TRUSTED_PUBLIC_KEYS` (or a source's `trusted_public_keys`) is set, while the same install succeeds without it
- **Workaround:** Only set `AIOS_TRUSTED_PUBLIC_KEYS` on systems that require verified-only installs; leave it unset (default) to allow unsigned blocks while still verifying signatures against the embedded key
- **Note:** The bridge `store publish` path uses `BlockInstaller::new` (no env keys), so publishing unsigned blocks keeps working regardless of the environment

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
