# AIOS Development Roadmap

## Completed

- [x] Phase 1: Workspace scaffold + IPC binary protocol
- [x] Phase 2: Hardware Abstraction Layer with detection and tier classification
- [x] Phase 3: Block Manager (registry, loader, message router)
- [x] Phase 4: Process Manager (scheduler, crash resilience, IPC control)
- [x] Phase 5: Live-Update Engine (hot-swap, rollback, state transfer)
- [x] Phase 6: AI Orchestrator (intent engine) + TUI dashboard
- [x] Phase 7: Integration tests (10 full-lifecycle tests)
- [x] Documentation: Architecture, changelog, bugs, TODO, agent rules
- [x] Phase 8: AI Watchdog & Emergency Recovery Engine (heartbeat, safe mode shell)
- [x] Phase 9: Capability-Based Security & Sandboxing (tokens, access control, sandbox)
- [x] Phase 10: Persistent System Context Store (telemetry, workflows, stability)
- [x] Phase 11: System Integration (watchdog↔TUI, context↔scheduler, aging, priority queue)
- [x] Phase 12: Docker + CI/CD infrastructure
- [x] Phase 13: Persistent DB (redb), CLI Shell, storage detection, RT scheduler, stress tests
- [x] Phase 14: Multi-Binary Compatibility (`aios-exec-compat`) — POSIX/Win32 translation, dependency healing, sandbox compat
- [x] GUI Dashboard (`aios-gui`) — egui/eframe native window, 6 tabs, dark theme, keyboard navigation
- [x] Interface Documentation (`docs/INTERFACE.md`) — full GUI/TUI usage guide, bilingual
- [x] Scheduler Real OS Threads — `RealThread`, `TerminateFlag`, `SuspendFlag`, cooperative kill/suspend
- [x] BlockExecutor — WASM block execution bridge between `BlockRegistry` + `WasmSandbox`
- [x] WatchdogRunner — real background thread with `AtomicBool` stop, timeout detection, action collection
- [x] RealTcpBlock — real `std::net::TcpListener`/`TcpStream` sockets with non-blocking accept
- [x] WasmLiveUpdateEngine — real WASM module deploy/swap/rollback during live update
- [x] RealUdpBlock — real `std::net::UdpSocket` with broadcast, plus `port()` accessor
- [x] Integration Tests: Real I/O — 55 tests across 6 files (file, network, WASM, threads, hot-swap, lifecycle)
- [x] Scheduler CPU Affinity — `SetThreadAffinityMask` (Win) / `sched_setaffinity` (Linux) per-core pinning
- [x] Phase 15: Zero-Copy IPC Ring Buffers — lock-free ring buffer with producer/consumer indices
- [x] Phase 16: Hardware-Enforced Memory Protection (MPK / PKS) — Intel MPK, ARM Memory Domains, 27 tests
- [x] Phase 17: AI KV-Cache & State Compression — FP8/INT4 quantization, ZSTD compression, LRU cache
- [x] Phase 18: Atomic Copy-on-Write (CoW) Persistence — SnapshotManager, RecoveryLog, atomic commit
- [x] Phase 19: IOMMU Support for DMA Isolation — DMA management, page tables, IOMMU domains, 25 tests
- [x] Phase 20: TEE (Trusted Execution Environment) Integration — SGX/TrustZone/SEV, sealing, attestation, enclaves, 28 tests
- [x] Docker: Multi-stage production build — builder + runtime (debian:bookworm-slim), entrypoint aios-tui
- [x] Phase 21: aios-bridge — HTTP/WS API gateway + Intent engine (RU/EN) + Capability enforcement
- [x] Phase 44: 7-tab kernel TUI + `--safe-mode` + GUI AI Studio/Network Settings (v2.8.0)
- [x] Phase 45: AI Console chat persistence + `/preset` templates + streaming (v2.9.0)
- [x] Phase 46: Live USB bootable image — hybrid BIOS+UEFI ISO with Linux kernel, AIOS auto-start TUI, `aios-install` installer, reproducible `live/build.sh` (v2.9.5)
- [x] Phase 47: Virtual File System (`aios-vfs`) + two-panel File Manager (`aios-fm`) with Files tab in TUI and GUI — AIOS:// and HOST:// schemes, capability-gated host access, cancellable async copy/move/delete with progress, AI file preview (v2.10.0)
- [x] Phase 48: Multi-node distributed cluster (`aios-cluster`) — TCP/in-memory transports, discovery via heartbeats, load-aware/round-robin/tier placement, remote spawn/kill/priority, failover respawn, config from env/JSON (v2.11.0)
- [x] Phase 49: `aios-init` — static-musl PID 1 init for the initramfs: core VFS mounts, `/dev/console`, block supervisor with restarts + zombie reaping, rescue-shell fallback, `build_initramfs.sh` cpio/gzip packaging, GRUB/Syslinux `init=/init console=tty0` wiring (v2.12.0)
- [x] Phase 50: `aios-init` hands over to the real kernel TUI — `build_initramfs.sh` builds/stages the static-musl `aios` as `/system/aios-core` (boot straight into the kernel TUI, rescue shell as fallback), adds `--keep-rootfs` + rootfs cleanup guard; `live/build.sh` gains the optional `USE_AIOS_INIT=1` mode with a dedicated GRUB menu (v2.13.0)
- [x] Phase 51: `aios-init` is the default initramfs `/init` in the Live ISO — `live/build.sh` step [4] packs the aios-init initramfs by default (`aios-init` as `/init`, `aios` as `/system/aios-core`, busybox only as rescue shell), step [5] writes the `init=/init console=tty0` GRUB menu; the legacy busybox `switch_root` path is preserved behind `USE_BUSYBOX_INIT=1`, the `USE_AIOS_INIT=1` switch is removed (v2.14.0)

## Backlog

- [x] Wire `aios-init` + `build_initramfs.sh` into `live/build.sh` step [4] as the initramfs `/init` by default (done in v2.14.0; legacy busybox path kept behind `USE_BUSYBOX_INIT=1`)
- [x] Add a `rootfs` cleanup guard / `--keep-rootfs` flag to `build_initramfs.sh` — done (v2.13.0)

## Readiness Assessment (2026-07-29, updated)

**Overall: ~100% ready for real (non-mock) testing.** (up from 90%)

| Component | Status | What's Real |
|-----------|--------|-------------|
| HAL detection | **REAL** | Real OS queries (nvidia-smi, wmic, /proc/cpuinfo) |
| IPC Bus/Channel | **REAL*** | Real mpsc + Arc<Mutex<VecDeque>>, in-process only |
| WASM Sandbox | **REAL** | Real wasmtime compile + execute |
| FileSystem | **REAL** | Real std::fs read/write |
| Scheduler | **REAL** | Real OS thread spawning, cooperative terminate/suspend, CPU affinity pinning |
| BlockRegistry | **REAL** | `load_from_path()` scans .wasm/.bin from disk at boot |
| BlockLoader | **REAL** | `load_from_path_and_execute()` compiles + instantiates WASM |
| Watchdog | **REAL** | Background thread + graduated recovery (kill/dump/shell) |
| TCP Network | **REAL** | Real std::net::TcpListener/TcpStream sockets |
| UDP Network | **REAL** | Real std::net::UdpSocket with broadcast |
| Live Update | **REAL** | WasmLiveUpdateEngine — deploy, swap WASM modules, rollback, memory migration, IPC reroute |
| BlockExecutor | **REAL** | Bridges BlockRegistry + WasmSandbox for full block execution |
| Web Browser | **REAL** | Real HTTP fetch via reqwest, HTML parse, text render, link extraction |
| Web Search | **REAL** | Real HTTP requests to DuckDuckGo/SearXNG/Brave, JSON/HTML parsing, AI summarization via aios-llm |
| Integration Tests | **REAL** | 61 tests with real file I/O, real network loopback, real WASM execution, real OS threads, hot-swap with state |

### All milestone targets achieved ✅

## Priority 0: Advanced Optimization & Hardware Resilience (IMPLEMENTED)

### 0.1 Zero-Copy IPC Ring Buffers ✅
- [x] Replace standard socket/channel IPC for heavy data payloads with shared memory ring buffers
- [x] Implement `shm` + `io_uring` pattern for kernel-bypass data passing
- [x] Ensure O(1) data pass efficiency between AI Orchestrator, Storage Blocks, and Execution Subsystems
- [x] Eliminate kernel-space payload copying for large IPC messages
- [x] Design lock-free ring buffer data structure with producer/consumer indices
- [x] Integration with existing `IpcBus` and `IpcChannel` transports
- [x] Benchmarks: measure latency reduction vs current VecDeque-based bus

### 0.2 Hardware-Enforced Memory Protection (MPK / PKS) ✅ IMPLEMENTED
- [x] Utilize Intel MPK (Memory Protection Keys) / ARM Memory Domains for isolation
- [x] Assign hardware access keys to isolated system blocks
- [x] Prevent cross-block memory reads directly at MMU (Memory Management Unit) hardware level
- [x] Implement runtime capability checking with CPU instruction overhead < 1%
- [x] Per-block access control via PKEY register modifications (x86-64) or DACR (ARM)
- [x] Fallback soft-isolation for unsupported architectures
- [x] Integration with `aios-security` capability tokens
- [x] 27 unit tests covering Intel MPK, ARM domains, and security bridge
- [x] Hardware security bridge: unified MPK/TEE/IOMMU interface

### 0.3 AI KV-Cache & State RAM Compression ✅
- [x] Implement runtime memory quantization (FP8/INT4) for idle AI Orchestrator context buffers
- [x] Compress inactive system state tables in RAM using ZSTD codec
- [x] Minimize memory footprint on low-spec hardware (Tier 3 devices)
- [x] Automatic compression thresholds based on memory pressure detection
- [x] Lazy decompression on access with LRU decompression cache
- [x] Benchmark: measure compression ratio and CPU cost on various state types
- [x] Integration with `aios-context` telemetry store (compressed telemetry)

### 0.4 Atomic Copy-on-Write (CoW) State Persistence ✅
- [x] All block disk operations and live-updates write to Copy-on-Write storage structures
- [x] Ensure instant 1-millisecond atomic rollback snapshots if hardware power lost during live update
- [x] Implement CoW page tables for block state snapshots (via `mmap` + page fault handlers)
- [x] Atomic commit protocol: write to shadow region → flush fsync → atomic rename
- [x] Recovery log for crash resilience during state transfer
- [x] Integration with `aios-live-update` hot-swap engine (CowLiveUpdateEngine)
- [x] Benchmarks: snapshot creation time, rollback latency, disk overhead

### 0.5 Runtime Optimization Engine ✅
- [x] Performance profiler with rolling averages, histograms, percentiles
- [x] Hot-path detection with hit counts and flamegraph output
- [x] Memory layout optimizer for cache-line alignment
- [x] Auto-tuner with grid/random/binary search strategies
- [x] 29 unit tests covering all optimization modules

## Priority 1: Supplementary Specifications (Critical Safety)

### 1.1 AI Watchdog & Emergency Recovery Engine
- [x] Implement `Watchdog` struct with heartbeat tracking
- [x] Add cryptographic heartbeat packets (HMAC-SHA256) from AI Orchestrator to kernel
- [x] Configurable heartbeat interval (N ms) and miss threshold (X consecutive)
- [x] Safe Mode fallback: deterministic CLI Kernel Shell when AI hangs
- [x] State execution log dump on watchdog trigger
- [x] Watchdog integrated into TUI main loop with heartbeat thread
- [x] Unit tests: heartbeat miss, recovery, state dump integrity

### 1.2 Capability-Based Security & Sandboxing (Zero-Trust)
- [x] Implement `CapabilityToken` enum with specific capabilities:
  - `CAP_NET_BIND`, `CAP_NET_CONNECT`
  - `CAP_FS_READ`, `CAP_FS_WRITE`
  - `CAP_HW_ACCESS`, `CAP_MEM_ALLOC`
  - `CAP_SCHED_MODIFY`
- [x] `AccessControlLayer` for token issuance and validation
- [x] Runtime capability checking on every system call
- [x] WebAssembly sandboxing via `wasmtime` for block execution
- [x] Direct memory pointer prohibition enforcement (via IPC-only data exchange)
- [x] Violation intercept: terminate block + notify AI Orchestrator
- [x] Integration with BlockManager: tokens stored per BlockEntry
- [x] Unit tests: capability grant/revoke, violation detection, sandbox isolation
- [x] Multi-Binary Compatibility: POSIX/Win32 translation, dependency healing, sandbox compat (89 tests)

### 1.3 Persistent System Context & Vector Memory Store
- [x] Integrate embedded database (`heed` / `redb` / `sled`)
- [x] `EmbeddedContextStore` struct with typed collections:
  - [x] Telemetry history (CPU/RAM metrics per block config)
  - [x] User workflow patterns (learned priority profiles)
  - [x] Update logs (historical stability scores per block binary)
- [x] Zero-cloud requirement: all persistence local
- [x] Auto-compact on startup if DB exceeds threshold
- [x] Query API: `get_telemetry_range()`, `get_workflow_profile()`, `get_stability_score()`
- [x] Integration with Scheduler for priority auto-tuning based on learned patterns
- [x] Unit tests: write/read/query, compaction, crash recovery

## Priority 2: System Hardening

### 2.1 IPC Bus Improvements
- [x] Bounded queue with backpressure (DropOldest + Reject policies)
- [x] Priority-based queue ordering (send_priority for priority dequeue)
- [x] Message deduplication via packet_id tracking (HashSet-based)
- [x] Bus metrics: sent/received/dropped/deduplicated/peak queue depth/avg latency

### 2.2 Scheduler Enhancements
- [x] Fair scheduling within priority level (weighted round-robin)
- [x] Process aging: prevent starvation of low-priority processes
- [x] Memory pressure notification to AI Orchestrator (callback system)
- [x] Process groups and session management
- [x] Real-time scheduling class for latency-critical blocks

### 2.3 Block Manager Enhancements
- [x] Block dependency graph (topological load/unload order, cycle detection)
- [x] Block versioning with semantic version comparison (parse, ord, compat, bump)
- [x] Hot-reload from file system (watch for new .bin files)
- [x] Block marketplace/repository support

## Priority 3: Hardware & Runtime

### 3.1 Hardware Detection Expansion
- [x] NVIDIA GPU detection via nvidia-smi
- [x] AMD GPU detection via ROCm/SMI
- [x] NPU detection for Intel Meteor Lake, Qualcomm X Elite
- [x] USB/Thunderbolt device enumeration
- [x] Storage device detection (NVMe, SATA)

### 3.2 WebAssembly Runtime Integration
- [x] Wasmtime embedding for block sandboxing
- [x] WASI interface for system call filtering
- [x] Memory limits per WASM block instance
- [x] Shared-nothing isolation between blocks

### 3.3 Real-Time Capabilities
- [x] Deterministic scheduler mode for RT blocks
- [x] Latency measurement infrastructure
- [x] Jitter tracking and reporting
- [x] Priority inheritance protocol

### 3.4 Network Stack
- [x] TCP block (client/server, connection management, send/receive)
- [x] UDP block (bind, send, broadcast, receive)
- [x] Connection tracking and statistics

### 3.5 Core Abstractions
- [x] Filesystem abstraction (Virtual, Local, Overlay)
- [x] File permissions model

## Priority 4: User Interface

### 4.1 TUI Enhancements
- [x] Real-time process tree visualization (table with PID, name, priority, state, RAM, CPU, crashes)
- [x] Block dependency graph visualization
- [x] System metrics charts (RAM gauge, priority distribution, RAM history)
- [x] Interactive block management (load/unload/hot-swap from UI)
- [x] Keyboard-driven process management (j/k navigate, K to kill, 1-4 tabs)

### 4.2 CLI Kernel Shell (Safe Mode)
- [x] Deterministic shell for system recovery
- [x] Basic commands: ps, kill, load, unload, status, logs
- [x] No AI dependency — runs independently
- [x] Accessible when AI Orchestrator is suspended

## Priority 5: Testing & Quality

### 5.1 Expanded Test Coverage
- [x] Property-based testing for IPC protocol (proptest)
- [x] Fuzzing for serialization/deserialization
- [x] Stress tests: 1000+ concurrent blocks (708 total tests)
- [x] Chaos testing: random crashes, memory pressure
- [x] Benchmarks: IPC throughput, scheduler latency

### 5.2 CI/CD
- [x] GitHub Actions pipeline
- [x] Cross-compilation targets (Linux ARM64, Windows x64)
- [x] Clippy + fmt check in CI
- [x] Coverage reporting (cargo-tarpaulin)
- [x] Release automation

## Deferred

- [ ] GUI interface
- [ ] Multi-node distributed scheduling
- [ ] Formal verification of safety properties

## Runtime Transition Checklist (Mock → Real)

**Goal:** Transform all mock/simulated subsystems into real OS-level execution. Target: 90%+ readiness.

### 1. Process Manager & Scheduler — Real Thread Spawning
- [x] `RealThread` struct: `Thread` + `JoinHandle` + `TerminateFlag` + `SuspendFlag`
- [x] `spawn_real_process<F>()`: real OS thread with cooperative termination
- [x] `kill_process()`: set terminate flag, unpark, join handle
- [x] `suspend_process()` / `resume_process()`: park/unpark real threads
- [x] `check_real_threads()`: detect finished threads via `is_finished()`
- [x] Map `ProcessId` → `JoinHandle` in a persistent registry
- [x] CPU affinity: `SetThreadAffinityMask` (Win) / `sched_setaffinity` (Linux) per `aios-hal` tier
- [x] Thread-local storage per process for per-process metrics

### 2. BlockRegistry & BlockLoader — Real File I/O & Execution
- [x] `BlockExecutor`: bridges `BlockRegistry` + `WasmSandbox`, compiles/instantiates/calls
- [x] `deploy_block()`: auto-calls `init`/`start` on WASM blocks
- [x] `BlockRegistry::load_from_path(path)`: scan directory for `.wasm` and `.bin` files, parse manifests, register
- [x] `BlockExecutor::load_from_path_and_execute()`: load + compile WASM from disk in one step
- [x] `BlockLoader::load_from_directory()`: now handles `.wasm` files alongside `.bin`
- [x] Auto-discovery: walk `blocks/` directory on boot, register all valid `.wasm` files
- [x] Manifest parsing from sidecar `.json` files (name, version, capabilities, TTL)

### 3. Active AI Watchdog — Background Supervisor
- [x] `WatchdogRunner`: real background thread with `AtomicBool` stop
- [x] `start()` / `stop()` / `receive_heartbeat()` / `pop_actions()`
- [x] Timeout detection → `WatchdogAction::EnterSafeMode`
- [x] Active recovery: `WatchdogAction::KillProcess(pid)` — severity 4
- [x] Active recovery: `WatchdogAction::DumpState(path)` — severity 5, timestamped
- [x] Active recovery: `WatchdogAction::SafeModeShell` — severity 7
- [x] `escalate()` on runner — triggers context-appropriate recovery actions
- [x] Severity ordering and `is_terminal()` on `WatchdogAction`
- [x] Threshold escalation: warn → suspend → kill → safe mode (graduated response in check_timeout)

### 4. TCP/UDP Network Stack — Real OS Sockets
- [x] `RealTcpBlock`: real `std::net::TcpListener`/`TcpStream` with non-blocking accept
- [x] `start_listening()`, `accept_pending()`, `connect()`, `send()`, `receive()`, `close_connection()`
- [x] `RealUdpBlock`: real `std::net::UdpSocket` with `bind()`, `send_to()`, non-blocking `receive_from()`
- [x] `broadcast()` for UDP broadcast (via `SO_BROADCAST`)
- [x] Socket options: `SO_REUSEADDR`, `SO_KEEPALIVE`, `TCP_NODELAY`
- [x] Bind capability tokens: `CAP_NET_BIND` → `socket.bind()`, `CAP_NET_CONNECT` → `socket.connect()`

### 5. Hot Live-Update Engine — Real WASM Replacement
- [x] `WasmLiveUpdateEngine`: deploy/swap/rollback via `LiveUpdateEngine` + `WasmSandbox`
- [x] `swap_block()`: atomic swap + compile + instantiate new WASM module
- [x] `rollback_block()`: remove active instance + restore from `HotSwapEntry`
- [x] State migration: extract WASM linear memory state before swap, restore to new instance
- [x] IPC channel reroute: atomically redirect pending messages to new block handle
- [x] Zero-downtime swap: ensure in-flight IPC messages are not dropped during transition

### 6. Integration Tests — Real I/O
- [x] `tests/real_file_io.rs`: real file read/write via `aios-core::filesystem`
- [x] `tests/real_network.rs`: TCP loopback send/receive, UDP broadcast
- [x] `tests/real_wasm.rs`: compile + execute WASM blocks end-to-end
- [x] `tests/real_threads.rs`: spawn processes, verify real thread handles
- [x] `tests/real_hot_swap.rs`: deploy block v1, swap to v2, verify function change
- [x] `tests/full_lifecycle.rs`: boot → load blocks → schedule → execute → watchdog → shutdown

- [x] Phase 22: aios-studio Web UI — SPA dashboard with Command Palette, telemetry WebSocket chart, Security Center, capability consent center, auto-reconnect, served from aios-bridge via ServeDir fallback

## Planned

- [x] **Phase 24 sub-item: Backend workflow execution endpoint** — `POST /api/v1/workflow` batch intent executor in `aios-bridge`
- [x] **Phase 23: Multi-Mode AI Engine (`aios-llm`) & Hybrid Intent Router — COMPLETE**
  - [x] `aios-llm` crate with unified trait/enum design: LlmConfig, BackendKind, CloudProvider, LlmRequest/Response
  - [x] Cloud-First mode: HTTP/JSON backend for Groq, OpenRouter, Google AI Studio via `reqwest`
  - [x] Micro-Local mode: Qwen2.5-0.5B-Instruct-GGUF via candle 0.11 (`quantized_qwen2::ModelWeights`, `LogitsProcessor`)
  - [x] Full-Local mode: Qwen2.5-7B-Instruct-GGUF quantized (INT4), same inference pipeline
  - [x] `hf-hub` 1.0 integration: `HFClientSync` for model download from Hugging Face Hub (blocking)
  - [x] Integration with aios-bridge: `POST /api/v1/llm/query` endpoint + `parse_with_llm_fallback()` in intent router
- [x] **Phase 24: EasyLang Engine & No-Code App Builder (`aios-builder`) — COMPLETE**
  - [x] `aios-builder` crate created: Workflow type, AutoManifestGenerator (WASM binary + intent analysis), WorkflowCompiler (WAT gen)
  - [x] In-Memory EasyLang Compiler: declarative text → `.wasm` in milliseconds (WAT→WASM pipeline done)
  - [x] EasyLangParser: line-oriented DSL (`spawn`, `timer`, `load`, `unload`, `kill`, `query`, `compact`, `status`) with optional label prefix, comment support, auto-label generation
  - [x] Auto-Manifest Generator: WASM binary analysis + workflow intent keyword matching
  - [x] Visual Workflow Step Editor in `aios-studio` — palette, add/remove/reorder steps, per-step inline prompt editing, sequential run
  - [x] Workflow persistence — named save/load/delete via localStorage with dropdown selector
- [x] **Phase 25: Secure Web Surfing & Search (`aios-browser` & `aios-search`) — COMPLETE**
  - [x] WASM-based vector HTML/CSS renderer with sandboxed network (HtmlParser, Renderer, BrowserEngine)
  - [x] Anonymous web search via DuckDuckGo / SearXNG / Brave Search APIs (SearchEngine, 3 backends)
  - [x] Local AI TL;DR synthesis via aios-llm (SearchSummarizer)
  - [x] `POST /api/v1/browse` and `POST /api/v1/search` REST endpoints in aios-bridge
- [x] **Phase 26: Atomic Updates & App Store (`aios-updater` & `aios-store`) — COMPLETE**
  - [x] Atomic Dual-Boot (Slot A / Slot B) with 1-second auto-rollback
  - [x] Hot-swapping drivers and apps without reboot (HotSwapEngine)
  - [x] Decentralized WASM registry with Ed25519 signatures (ManifestValidator + StoreRegistry)
  - [x] `GET /api/v1/store/index` and `POST /api/v1/store/register` REST endpoints
- [x] **Phase 27: Debug System & Black Box (`aios-telemetry` & `aios-debug`) — COMPLETE**
  - [x] End-to-end `TraceID` structured tracing (TraceContext)
  - [x] Flight Recorder — ring buffer with kind-based filtering and dump (FlightRecorder)
  - [x] Zero-Knowledge anonymized crash reports (CrashReporter + PanicHandler)
  - [x] Prometheus-compatible `/api/v1/metrics` endpoint
  - [x] `GET /api/v1/traces` and `POST /api/v1/crash-report` REST endpoints

- [x] **Phase 33: Browser Block Out of the Box — COMPLETE**
  - [x] `BrowserBlock` implementing `StatefulBlock` in `aios-browser` (IPC: `browse`, `open_native`, `browser_status`, `HealthCheck`)
  - [x] Kernel (`aios`) registers hal/ipc_bus/scheduler/browser at boot + boot-discovers `AIOS_BLOCKS_DIR` + wires browser handler into `MessageRouter`
  - [x] Kernel TUI `b` hotkey — open any URL in the OS default browser via the browser block
  - [x] Browser works out of the box on a fresh machine (no config, no installed browser, no network needed)

- [x] **Phase 34: Full-Featured Native Browser (`aios-webview`) — COMPLETE**
  - [x] New `aios-webview` crate: native WebView window (wry 0.56 + winit 0.30) with cookies, JavaScript and history out of the box
  - [x] `WebBrowser::open/navigate/back/forward/close` — non-blocking commands via `EventLoopProxy`; browser runs on a background thread
  - [x] Persistent profile via `WebContext` (`AIOS_DATA_DIR`/`aios/webview`) so cookies/storage survive restarts
  - [x] `resolve_target()` omnibox rule shared with the TUI (URL / bare host / DuckDuckGo query)
  - [x] `launcher` module — locate and spawn the `aios-gui` binary (sibling of exe, then PATH)
  - [x] GUI Browser tab (F7) in `aios-gui` — omnibox, Back/Forward, Open/Close, status line
  - [x] TUI hotkey `W` (both `aios-tui` and kernel `aios`) launches the GUI dashboard
  - [ ] Future: embed the webview as an in-window child of the egui tab via `build_as_child` (Windows/macOS/X11), replacing the companion window

- [x] **Phase 35: WHATWG HTML Rendering & Web Tab Navigation in TUI — COMPLETE**
  - [x] `HtmlParser` rebuilt on `scraper`/html5ever — structured text (headings `#`, lists `•`/`1.`, tables `|`, `pre`, `hr`, images `[alt]`), WHATWG-compliant
  - [x] Link resolution against the page base URL + dedupe + non-web scheme filtering + root URL canonicalization (no trailing slash)
  - [x] `Renderer` adapted to the real DOM tree from html5ever
  - [x] `WebState.history` — back navigation in the Web tab (`b`)
  - [x] Page text scroll keys `u`/`d` (±1 line) and `PageUp`/`PageDown` (±20 lines) with a scroll indicator `X–Y`
  - [x] Page pane renders through the visible height with wrapping (no overflow)

- [x] **Phase 36: Responsive Web Tab — Background Fetch, Page Cache, Link Scrolling — COMPLETE**
  - [x] Web fetches moved to background threads (never block the TUI) with a fetch-generation counter that drops stale results
  - [x] Bounded page cache (`WebState.cache`, 20 pages, oldest evicted) — instant `b` back-navigation and revisits
  - [x] Links window scrolls with the selection (6 visible rows, visible range in the title)
  - [x] Page text color-codes structure: headings bold cyan, blank lines dark gray

- [x] **Phase 37: Word-Wrapped Page Text — COMPLETE**
  - [x] `wrap_text()` word-wrap helper: word-boundary split, hard split of over-long words, preserves blank lines and indentation
  - [x] Scroll units equal visual lines — `u`/`d`/`PageUp`/`PageDown` move exactly one/20 visible rows and the bottom of a wrapped page is reachable
  - [x] `WebState.wrap_width` tracked from `crossterm::terminal::size()` and refreshed on `Event::Resize`

- [x] **Phase 38: Web Tab Navigation Sidebar — COMPLETE**
  - [x] Fixed-width history sidebar (`SIDEBAR_WIDTH = 26`) left of the page pane: current page first (marked `▸`), history newest-first, deduplicated
  - [x] Compact URL labels (`compact_url_label`) truncated to the pane width
  - [x] Sidebar focus via `\`: `j`/`k`/`Up`/`Down` move the selection, `Enter`/`o` open it, `Esc` back to links; selection wraps around
  - [x] `web_page_width()` — page wrap width derived from terminal width minus sidebar/borders/prefix; applied at startup and on resize (proportional pane rendering completed)
- [x] Future: bookmarks with persistence in the Web tab — `a` add (name prefilled from page title), `m` open panel, `j`/`k`/`o`/`d`/`Esc` manage, persisted to `AIOS_DATA_DIR/web_bookmarks.json` (v2.14.1)
  - [ ] Future: tabs (multiple open pages) in the Web tab

- [x] **Phase 39: Full Native Browser from the Web Tab — COMPLETE**
  - [x] `B` opens the current page in the full native browser (`aios-webview` WebView2 — JS/CSS/images); the window is reused and auto-recreated, opened on a background thread
  - [x] `n` opens the currently selected link in the native browser
  - [x] Browser handle in a module-level `OnceLock<Mutex<Option<WebBrowser>>>` — kernel untouched
  - [x] `http_client()`: desktop User-Agent + `Accept: text/html` + 15s timeout for text fetches (fewer bot-blocks, no hangs)
  - [ ] Future: embed the webview as an in-window child of the GUI Browser tab via `build_as_child` (Windows/macOS/X11), replacing the companion window
  - [ ] Future: headless render-to-text fallback (chromium/WebDriver) for JS-heavy sites in the TUI text view

- [x] **Phase 40: Block Store — Sources, Catalog, Installer, Update Service — COMPLETE**
  - [x] `aios-store::source`: `StoreSource`/`SourceKind` — GitHub (`github:owner/repo`), local (`local:path`), HTTP update service (`http://host:port`)
  - [x] `aios-store::catalog`: `fetch_index`/`download_block` (async HTTP + local scan of `*.wasm`/`*.bin` + sidecar JSON), `parse_name_version`
  - [x] `aios-store::installer`: `BlockInstaller` — `{name}_{version}.wasm` + sidecar, SHA-256 verification, `list_installed`/`find_installed`/`uninstall`, `backup`/`rollback` (`.bak`), `check_updates`, semantic `cmp_version`
  - [x] `aios-store::manager`: `StoreManager` facade — `search`/`install`/`update` (auto-rollback)/`check_updates`/`parse_source_spec`/`block_on`
  - [x] `aios-net-config` crate: `NetworkConfig`+partial `apply_updates`, `NetworkConfigStore` (atomic JSON), `NetSettingsBlock` (`net_get`/`net_set`/`net_reset`/`net_persist`, StatefulBlock + state roundtrip)
  - [x] Update service in `aios-bridge`: `GET /index.json`, `GET /blocks/{name}.wasm`, `GET /store/index.json`, `GET /store/blocks/{name}.wasm`, `POST /api/v1/store/publish`
  - [x] TUI shell commands: `store list|sources|add-source|search|install|update|uninstall|rollback` and `net get|set|reset`
  - [x] Tests: 32 (`aios-net-config`) + 42 (`aios-store`) unit tests, 2 new integration tests (update flow + net block roundtrip)

- [x] **Phase 41: Net settings block in kernel + store publish — COMPLETE**
  - [x] `net_settings` block registered in the kernel registry at boot (`aios/src/orchestrator.rs`), wired into the `MessageRouter`, id exposed as `OrchestratorState::net_block_id`
  - [x] Kernel TUI hotkey `n`: input mode for `key=value` network-config updates dispatched over IPC (`net_set`) with the result shown in the Events pane
  - [x] `store publish <file.wasm> [name] [version]` in the `aios-tui` shell — SHA-256 + base64 → `POST /api/v1/store/publish` (port from `AIOS_BRIDGE_PORT`)
  - [x] Kernel router tests (4): registry registration + `net_get`/`net_set`/`net_reset` IPC routing

- [x] **Phase 42: Ed25519-signed block manifests with trust enforcement — COMPLETE**
  - [x] Real Ed25519 signing/verification in `aios-store::manifest`: `canonical_bytes()` + `sign_manifest()` + real `verify_strict` in `verify_signature` + `verify_signature_with_keys` (11 tests)
  - [x] Enforcement in `BlockInstaller`: `trusted_keys`, `with_trusted_keys`/`from_env`, `Default` reads `AIOS_TRUSTED_PUBLIC_KEYS`; sidecar persists the full manifest incl. signature (16 tests)
  - [x] Per-source trust policy: `StoreSource.trusted_public_keys`, `StoreManager::verify_source_manifest` in `install()`/`update()`, official GitHub key via `AIOS_OFFICIAL_PUBLIC_KEY` (2 manager tests)
  - [x] TUI shell `store sign <file.wasm> [name] [version] [--key <hex>]` + `store verify <name>`
  - [ ] Future: signed `store publish` (manifest signed before the bridge installs it) and `store trust <source>` command to set a source's trusted keys from the shell

- [x] **Phase 43: AI Console — slash commands, help panel, runtime reconfiguration — COMPLETE (v2.6.0)**
  - [x] Kernel TUI AI Console (tab 3): slash commands `/help /status /clear /history /system /model /backend /key /temp /tokens`
  - [x] Runtime backend/model/key reconfiguration applied to the shared engine (HTTP `/api/v1/llm/query` stays in sync)
  - [x] Built-in help panel (справка) toggled with `h` or `/help`; prompt history (last 50) via `Up`/`Down`
  - [x] Status footer + `/status` report incl. local GGUF model detection; word-wrapped output with prompt/error coloring
  - [x] `aios-llm` config introspection: `LlmEngine::config()`, `provider_name()`, `backend_label()`; 1 new unit test
  - [x] Phase 45 follow-up: chat persistence to disk + `/preset` prompt templates + streaming responses (v2.9.0)

- [x] **Phase 45: AI Console — chat persistence, `/preset` templates, streaming — COMPLETE (v2.9.0)**
  - [x] `aios-llm::LlmEngine::query_stream` streaming API (OpenAI + Google AI Studio SSE deltas, per-token local generation); `extract_stream_delta` + 4 tests
  - [x] AI Console streams responses live (yellow partial line while in flight)
  - [x] Chat persisted as JSON Lines to `AIOS_DATA_DIR/chat.jsonl`; auto-save after each reply + on quit; restored at boot; manual `/save` `/load`
  - [x] `/preset` command family with built-in templates (`assistant`, `code`, `translator`, `explainer`): apply / define / list / delete

- [x] **Phase 45b: GUI AI Studio parity — streaming, persistence, `/preset` — COMPLETE (v2.9.1)**
  - [x] GUI AI Studio streams responses live (yellow partial line) via the same `query_stream` channel; requests deduplicated into one worker slot
  - [x] GUI chat persisted to the shared `AIOS_DATA_DIR/chat.jsonl`; auto-save after each reply + on window close; restored at boot; manual `/save` `/load`
  - [x] GUI `/preset` templates persisted to `AIOS_DATA_DIR/presets.json`; built-in seeds overlaid by saved presets at boot
  - [x] New GUI commands: `/system <text>`, `/history`, `/preset`, `/save`, `/load` + updated help panel and hints

- [x] **Phase 44: 7-tab kernel TUI, safe mode, GUI AI Studio + Network Settings — COMPLETE (v2.8.0)**
  - [x] Kernel TUI (`aios`) restructured to the 7-tab spec: System & HW / Blocks & Svc / AI Console / Studio Bridge / Network & Store / Web / Shell; `1`-`7` + `Alt`+`1`-`7` + `Tab`/`F1`/`?`; header shows AI Tier + version
  - [x] Blocks tab `r`/`k`/`l` restart/unload/load; Web tab full spec keymap (`g j k o u d PageUp PageDown b B n`); Shell tab full command set (`ps blocks kill spawn store list/search/install net get/set status logs restart help clear`) typed inline
  - [x] `--safe-mode` boot flag (skip third-party disk blocks + bridge; `SAFE MODE` header; minimal recoverable shell)
  - [x] GUI restructured to 7 tabs: System Dashboard (overview+metrics+processes merged) / WASM Blocks / AI Studio / App Store / Network Settings / Deps / Native Browser
  - [x] GUI AI Studio: async LLM chat with slash commands, background tokio task, status line
  - [x] GUI Network Settings: form (hostname/port/timeouts/private-access/DNS/user-agent) with Save/Reset + live JSON preview
  - [x] GUI status bar: `HW Tier | IPC: N pkts | F6=Deps F7=Browser` with live IPC packet counter
  - [x] GUI AI Studio parity: streaming + chat/preset persistence (Phase 45b, v2.9.1)

### Readiness Targets
| Milestone | Target Readiness | Key Gap |
|-----------|-----------------|---------|
| Current | **100%** | Distributed scheduling |
| + CPU affinity + state migration | 90%+ | Distributed scheduling |
