# AIOS Architecture

## System Overview

AIOS (AI-Native Operating System) is a modular microkernel-style OS designed for AI-native workloads. It consists of 33 Rust crates forming a layered architecture: foundation types at the bottom, hardware abstraction and process management in the middle, safety/security/context systems, and user-facing interfaces (TUI/GUI/Integrated binary) at the top.

All inter-crate communication flows through a binary IPC protocol. Blocks (kernel modules) are hot-swappable with automatic rollback. An AI Orchestrator translates natural language intents into system operations. The `aios` crate provides a unified system binary with both interactive TUI mode and headless daemon mode, replacing the separate `aios-tui` and `aios-daemon` entry points.

```
┌──────────────────────────────────────────────────────┐
│              Interface Layer (User-Facing)            │
│  TUI (ratatui)  │  GUI (egui)  │  Unified `aios` bin │
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
- `aging_threshold_ms` (default: 500ms) — wait time before priority boost
- `schedule_next()` computes effective priority = base + (wait / threshold), capped at +4 levels
- Low-priority processes waiting 4x threshold are boosted to Critical level
- All processes evaluated globally (no early-break by queue level)
- `force_preempt()` — forcibly expires current time slice (for testing and manual reschedule)

**Weighted round-robin** (proportional time slices):
- `priority_weight()` maps: Background=1, Low=2, Normal=3, High=4, Critical=5
- Time slice = `default_time_slice_ms * priority_weight` (Critical gets 5x Background's slice)
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

## Layer 4: User Interface (`aios-tui`)

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

### Dashboard (`dashboard.rs`)

Ratatui-based TUI with 7-tab interactive layout:

**Header zone**:
- Project title "AIOS v1.0.0"
- Current AI tier with color coding: Tier1=Green, Tier2=Yellow, Tier3=Red
- Watchdog state: OK (Green), SUSPENDED (Red), RECOVERING (Yellow), SAFE MODE (Magenta)
- CPU cores, RAM usage, block count, process count

**Tabs zone**: 7 selectable tabs — Overview | Processes | Blocks | Metrics | Deps | Web | Shell

**Tab 1 — Overview** (45/55 horizontal split):
- Left: System info panel (CPU model, cores/threads, AVX flags, GPU name/VRAM, storage devices, system counts)
- Right: Activity log (last 20 messages, color-coded: Red=error, Yellow=warn, Green=success)

**Tab 2 — Processes** (vertical split):
- Top: Process table with columns: PID, Name, Priority, State, RAM, CPU Time, Crashes
- Row selection with `>>` indicator, color-coded priority/state
- Bottom: Process detail panel or kill result display

**Tab 3 — Blocks** (vertical split):
- Top: Block table with columns: ID, Name, Version, State, Size — row selection with `>>` indicator
- Title bar shows keybindings: `j/k: navigate  U: unload  L: load  H: hot-swap`
- Bottom: Block detail panel with selected block info and available actions, OR block input dialog for name/version entry

**Tab 4 — Metrics** (3-zone vertical):
- RAM usage gauge (Green/Yellow/Red threshold)
- Process priority distribution histogram (colored bars)
- RAM history time-series (60-entry ring buffer, bar chart)

**Tab 5 — Deps** (vertical split):
- Top: Dependency graph table with columns: #, Block, Depends On, Depended By
- Row selection with `>>` indicator, color-coded dependency relationships
- Bottom: Load order & stats panel (topological sort, edge count, block count)

**Tab 6 — Web** (vertical split):
- Omnibox input bar (accepts a full URL, a bare host, or a plain search query — queries run through DuckDuckGo) with focus indicator
- Page text content display area (WHATWG-compliant rendering via html5ever: headings `#` in bold cyan, lists `•`/`1.`, `pre` preserved, tables `|`, `hr`, images as `[alt]`)
- Scrollable links list with selection (`>>` indicator); links resolved against the page base URL, deduplicated, non-web schemes (`javascript:`, `mailto:`, `#anchor`) filtered; the window scrolls with the selection (6 visible rows) and shows the visible range in the title
- Fetches run **in the background** (never block the TUI): `load_url`/`navigate_web` spawn a thread that posts the result to the `page_cache` outbox, picked up by `check_page_cache()` each frame; a fetch-generation counter drops stale results
- Bounded **page cache** (`WebState.cache`, 20 pages keyed by URL, oldest evicted) makes revisits and `b` back-navigation instant
- Page text is **word-wrapped** to the terminal width (`wrap_text()`): each raw line is split at word boundaries (long words hard-split), blank lines and indentation preserved; scroll units equal visual lines; `WebState.wrap_width` is set at startup and updated on terminal resize
- A fixed **navigation sidebar** (`SIDEBAR_WIDTH = 26`) sits left of the page pane and shows the current page (marked `▸`) followed by the visit history newest-first (`web_nav_entries()`), with compact truncated URL labels (`compact_url_label()`); focus toggles with `\`, `j`/`k` move the selection, `Enter`/`o` opens it, `Esc` returns to the links; the wrap width is derived with `web_page_width()` = terminal width minus the sidebar, pane borders and the 2-col line prefix
- **Full browser on demand**: `B` opens the current page (and `n` the selected link) in the real `aios-webview` browser window (WebView2 — JS/CSS/images). The handle lives in a module-level `OnceLock<Mutex<Option<WebBrowser>>>` in `aios-tui`, so the window is reused, auto-recreated on close, and opening happens on a background thread (the TUI never blocks). The text view is unchanged — the native browser is an escape hatch for full rendering
- Text fetches use `http_client()` — desktop Chrome User-Agent, `Accept: text/html` and a 15s timeout — to avoid bot-blocks and hangs
- `WebState`: url_input, current_url, search_query, page (PageContent), loading, error, input_focused, scroll, links_scroll, history (Vec<String>), cache, web_fetch_gen, wrap_width, sidebar_focused, history_sel
- `PageContent` struct: url, title, text, links Vec<(String,String)>
- Keys: `g`=focus omnibox, `Enter`=search/navigate (auto-unfocus), `o`/`Enter`=open selected link, `j/k`=nav, `b`=back in history, `u`/`d`=scroll ±1 line, `PageUp`/`PageDown`=scroll ±20 lines, `Esc`=unfocus

**Tab 7 — Shell** (vertical split):
- Command input line with prompt indicator
- Output display area (scrollable command output)
- Command history navigation with ↑/↓
- `ShellState`: input_buffer, output (Vec<String>), command_history, history_pos
- Available commands: `ps`/`list`, `blocks`/`ls`, `kill <pid>`, `spawn <name> [prio] [ram_mb]`, `load <name> [version]`, `unload <id>`, `status`/`info`, `logs`, `restart`, `help`/`?`, `exit` (SafeModeShell) plus TUI extras `fetch <url>`, `search <query>`, `open <url>`, `clear`
- Execution flow: TUI → execute_shell_cmd() → `SafeModeShell::parse_command` / fetch / search / open

**F1 Help Overlay**:
- Toggled with F1 or '?', dismissed with F1/Esc/'?'
- Shows all keyboard shortcuts and shell commands in a popup window
- Overlay rendered via draw_help() function on top of current tab content

**Footer zone**: Keybind hints (q=Quit, 1-7=Tab, Alt+1-7=Tab everywhere, j/k=Nav, K=Kill, U=Unload, L=Load, H=Hot-swap, F1=Help, :=Cmd, s=Telemetry, x=Status, r=Refresh, W=GUI)

`DashboardState` manages:
- Process/Block snapshots (taken each frame for consistent rendering)
- RAM history ring buffer (60 entries)
- Selection state (selected_tab, selected_row)
- Process kill result display
- Block operation result display + `BlockInputMode` (None/LoadName/LoadVersion) + input buffer
- Dependency snapshot (`DependencySnapshot`) for Deps tab
- Log buffer (capped at 100 entries)
- Scheduler + Registry synchronization
- Web state: url_input, current_url, page, loading, error, input_focused, scroll, history
- Help overlay visibility (shown/hidden)
- Shell state: input_buffer, output (Vec<String>), command_history, history_pos

### Entry Point (`main.rs`)

Startup sequence:
1. Initialize `env_logger`
2. `HardwareProfile::detect()` — detect real hardware
3. `AiTier::from_profile()` — classify AI capability
4. Create `BlockRegistry` — register 4 core blocks (hal, ipc_bus, scheduler, browser), boot-discover disk blocks from `AIOS_BLOCKS_DIR`, wire the browser block into the `MessageRouter`
5. Create `Scheduler` — spawn 3 processes (ai_orchestrator, io_handler, health_monitor)
6. Create `Watchdog` — start heartbeat thread in background
7. Create `EmbeddedContextStore` + `TelemetryStore` — for system telemetry
8. Create `SafeModeShell` — for safe mode recovery commands
9. Enter crossterm raw mode + alternate screen
10. Event loop: poll key events, redraw dashboard, sync watchdog state
11. Restore terminal on exit

Keybindings: `q`=Quit, `1-7`=Tab, `Alt+1-7`=Tab even while typing in Shell/URL input, `j/k`=Navigate, `K`=Kill process, `r`=Refresh, `s`=Record telemetry, `x`=System status, `W`=Launch GUI dashboard, `F1`/`?`=Help, `:`=Shell command, ↑/↓=Shell history, Web tab: `g`=omnibox focus, `o`/`Enter`=Open link, `b`=back in history, `u`/`d`=scroll ±1 line, `PageUp`/`PageDown`=scroll ±20 lines, `Esc`=unfocus

### Native WebView Browser (`aios-webview`)

The TUI cannot render real web pages (no CSS/JS engine), so the full-featured browser is a **native window** powered by `wry` (WebView2 on Windows, WebKitGTK on Linux, WKWebView on macOS) on a `winit` event loop:

- `WebBrowser::open(target)` spawns the browser on a dedicated background thread; the caller receives a handle and never blocks
- Commands (`navigate`, `back`, `forward`, `close`) are posted to the browser's event loop via `winit::EventLoopProxy` and applied asynchronously
- Cookies and storage persist between restarts through a `WebContext` backed by a profile directory (`AIOS_DATA_DIR`/`aios/webview`, or the OS data dir)
- `resolve_target()` implements the omnibox rule shared with the TUI: full `http(s)` URL → as-is, bare host → `https://`, anything else → DuckDuckGo (HTML edition) query
- `launcher` module resolves the `aios-gui` binary (sibling of the current executable, then `PATH`) and spawns the GUI dashboard

### GUI Dashboard (`aios-gui`)

Native egui/eframe dashboard with 7 tabs: Overview, Processes, Blocks, Marketplace, Metrics, Deps, **Browser**. The **Browser** tab (F7) provides an omnibox, Back/Forward buttons and an Open/Close toggle that drive the `aios-webview` native window; the first navigation auto-opens the browser. Hotkey `W` in either TUI launches the GUI dashboard via `aios_webview::launcher::launch_gui()`.

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

**Commands:** `ps`, `blocks`, `kill <pid>`, `unload <id>`, `status`, `logs`, `restart`, `help`, `exit`

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

## Network Stack (`aios-net`)

- **Crate**: `aios-net` v1.0.0 — TCP/UDP blocks for network communication
- **TCP** (`tcp.rs`): `TcpBlock`, `TcpConfig`, `TcpConnection`, `TcpMessage`, `TcpState` — mock state machine
- **Real TCP** (`real_tcp.rs`): `RealTcpBlock` — real `std::net::TcpListener`/`TcpStream` with non-blocking accept, connection management, send/receive, optional `CapabilityToken` enforcement (`CAP_NET_BIND` for `start_listening()`, `CAP_NET_CONNECT` for `connect()`)
- **UDP** (`udp.rs`): `UdpBlock`, `UdpConfig`, `UdpPacket`, `UdpState` — mock state machine
- **Real UDP** (`real_udp.rs`): `RealUdpBlock` — real `std::net::UdpSocket` with `bind()`, non-blocking `send_to()`/`receive_from()`, broadcast via `SO_BROADCAST`
- Connection tracking, send/receive with channels, broadcast, statistics
- 40 tests covering mock + real TCP/UDP lifecycle

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

## Marketplace (`aios-block-mgr`)

- `BlockMarketplace` — block registry with repository management
- `BlockMetadata` — name, version, description, author, sha256, tags
- `RepositoryEntry` — metadata + status (Available/Installed/UpdateAvailable/Deprecated)
- Publish, search, install, uninstall, check updates
- Multiple repository support with cross-repo search
- 18 unit tests

---

## Testing Architecture

- **Unit tests**: 708 tests embedded in source files under `#[cfg(test)] mod tests`
- **Integration tests**: 28 tests in `tests/integration_test.rs`
- **Stress tests**: 11 tests in `tests/stress_test.rs`
- **Total**: 708 tests, all passing, zero clippy warnings

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
  - `NetworkClient`: configurable user-agent, timeout, redirect limit; capability-based sandboxed network
  - `Renderer`: DOM → markdown-like text output (headings `#`, links `[text](url)`, lists `•`)
  - `Page` type: `url`, `title`, `text_content`, `html`, `links: Vec<Link>`
  - `BrowserConfig`: `user_agent`, `timeout_secs`, `max_redirects`, `sandbox_enabled`
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

### Phase 28: Headless Daemon (`aios-daemon`) — *COMPLETED*
- **`aios-daemon` crate**:
  - `aiosd` binary: headless server performing the same initialization as `aios-tui` without terminal access
  - Loads built-in blocks (hal, ipc_bus, scheduler) and disk blocks from `AIOS_BLOCKS_DIR`
  - Opens persistent store (`redb`) at `AIOS_DATA_DIR/context.redb`
  - Spawns system processes (ai_orchestrator, io_handler, health_monitor)
  - Starts watchdog heartbeat thread
  - Background loop: logs heartbeat (processes, RAM, watchdog state) every 10s, persists telemetry every 60s
  - Minimal dependencies: no ratatui, crossterm, egui, or wasmtime
  - Environment config via `AIOS_DATA_DIR`, `AIOS_BLOCKS_DIR`, `AIOS_MOCK_PROFILE`, `RUST_LOG`
- **`aios-tui` headless mode**:
  - `--headless` CLI flag and `AIOS_HEADLESS=1` env var: skips TUI initialization, runs background loop
- **Docker**:
  - Dockerfile builds only `aios-daemon` (~2min build), uses `aiosd` as default CMD
  - `docker-compose.yml` has headless daemon by default, `interactive` profile for TUI
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
- Shell command `store publish <file.wasm> [name] [version]` (`aios-tui`) computes the file SHA-256, base64-encodes the wasm and posts a `StorePublishRequest` to `POST /api/v1/store/publish` (bridge port from `AIOS_BRIDGE_PORT`, default `8080`); name defaults to the file stem, version to `1.0.0`
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
- `orchestrator.rs` — Async initialization of IPC, Scheduler, BlockRegistry, AccessControl, Watchdog, LLM, WASM, Bridge
- `tui/` — Ratatui interactive dashboard with 4 tabs and event log

### Binary Modes
- `aios` — Interactive TUI mode (default)
- `aios --daemon` — Headless daemon mode (background server)

### TUI Hotkeys
| Key | Action |
|-----|--------|
| Tab / F1 | Next tab |
| 1-4 | Direct tab select |
| Alt+1-4 | Direct tab select even while the browser URL prompt or AI query line is active |
| q | Quit |
| g | Open bridge URL in browser |
| b | Open a URL in the native browser (URL input mode, dispatched to browser block via MessageRouter) |
| n | Edit network settings (`key=value` input mode, `net_set` over IPC) |
| r | Reprobe hardware |
| Space | Pause/resume log scroll |

### AI Console (Tab 3, Phase 43 / v2.6.0)
- Interactive LLM chat: `i` enters query mode, `Enter` sends; each query re-applies the current console `LlmConfig` to the shared `BridgeContext.llm` engine, so console settings and the HTTP `/api/v1/llm/query` endpoint stay consistent
- Slash-command system handled in `TuiApp::handle_ai_command`: `/help /status /clear /history /system /model /backend /key /temp /tokens`; backend/model/key changes rebuild the engine asynchronously via `apply_config_async`
- Built-in help panel (справка) toggled with `h` or `/help`, styled reference of keys + commands; prompt history (last 50) navigable with `Up`/`Down`
- Status footer `backend | model | temp | tokens | state` (`thinking...` / `done: Nms` / error); `/status` reports config + detected local GGUF models via `aios_llm::local::detect_local_models`
- `aios-llm` exposes `LlmEngine::config()`, `provider_name()`, `backend_label()` for config introspection

### Startup Sequence
1. Hardware probe (CPU, RAM, GPU, OS)
2. Initialize IPC bus (SharedIpcBus)
3. Create Scheduler with RAM-aware config
4. Initialize BlockRegistry — register core blocks (hal, ipc_bus, scheduler, browser), boot-discover `AIOS_BLOCKS_DIR` (default `./blocks`), register the browser IPC handler in the `MessageRouter`
5. Setup AccessControl + Watchdog
6. Initialize LLM Engine (cloud backend by default)
7. Initialize WASM Executor (BlockExecutor)
8. Create BridgeContext with all subsystems
9. Spawn Bridge HTTP server (axum, port 8080)
10. Start TUI event loop (or enter daemon loop)

The browser works out of the box on a fresh machine: no config file, no installed browser, and no network are required to start — the block is active in the topology, dispatchable over IPC, and the `b` hotkey opens any URL in the OS default browser.
