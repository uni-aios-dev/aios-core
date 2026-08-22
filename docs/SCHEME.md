# AIOS Program Scheme & Function Map

> Version: v2.28.1 · Date: 2026-08-22
> Companion documents: `docs/AUDIT.md` (full audit), `docs/ARCHITECTURE.md` (deep architecture), `docs/INTERFACE.md` (UI guide).
> This document is the **call-level map**: every crate, its modules, and its key public functions.

## 1. System Scheme (Layer Map)

```
                    USER INTERFACES (7-tab parity TUI/GUI)
 aios (kernel TUI)  │  aios-tui  │  aios-gui (egui)  │  aios-studio (SPA)
                              │  aiosd (aios-daemon, headless)
───────────────────────────────────────────────────────────────────────
        GATEWAY                      │            AI CORE
 aios-bridge — axum HTTP/WS,         │   aios-llm — cloud (Groq/OpenRouter/
 intent RU/EN + LLM fallback,        │   Google) + local GGUF (candle),
 capability enforcement, store API   │   streaming; aios-builder —
                                     │   EasyLang → WASM workflows
───────────────────────────────────────────────────────────────────────
                            SYSTEM SERVICES
 aios-store / aios-updater (A/B slots, Ed25519 trust)  │ aios-cluster (multi-node)
 aios-autohal (driver auto-provisioning + hotplug)     │ aios-vfs + aios-fm (files)
 aios-browser / aios-search / aios-webview             │ aios-net-config
 aios-telemetry (flight recorder/metrics/traces)       │ aios-debug (crash reports)
───────────────────────────────────────────────────────────────────────
                KERNEL SUBSYSTEMS (StatefulBlock ecosystem)
 aios-block-mgr │ aios-process-mgr │ aios-live-update │ aios-watchdog
 aios-security  │ aios-context     │ aios-wasm        │ aios-net │ exec-compat
───────────────────────────────────────────────────────────────────────
                             FOUNDATION
 aios-core (types/IpcPacket/crypto/fs/runtime)  │  aios-ipc (bus/channel/ring)
 aios-hal (detect/tier)                         │  aios-ringbuf · aios-compress
 aios-persistence · aios-optim                  │  hw protection: mpk/iommu/tee
═══════════════════════════════════════════════════════════════════════
 BARE-METAL TRACK (standalone, excluded from workspace):
 live ISO → aios-init (static-musl PID 1) → aios-kernel (x86_64-unknown-none,
 milestones M0–M2 done; M3 preemption, M4 IPC planned) ← aios-kernel-run (QEMU)
```

## 2. Boot Flow (`aios` kernel binary)

```
main(--daemon|--safe-mode?)
 └─ orchestrator::initialize()
     ├─ hw_probe::probe_system()          CPU/RAM/GPU via sysinfo+HAL, AiTier
     ├─ Scheduler::new(ram_mb)            aging 5s, slice 100ms, max 5 restarts
     ├─ BlockRegistry                     register hal/ipc_bus/scheduler/browser/net_settings + dep edges
     ├─ boot_discover(AIOS_BLOCKS_DIR)    third-party *.wasm/*.bin (skipped in --safe-mode)
     ├─ MessageRouter                     BrowserBlock + NetSettingsBlock handlers
     ├─ AccessControlLayer                capability tokens
     ├─ Watchdog                          heartbeat monitor (HMAC)
     ├─ LlmEngine                         AI tier → cloud/local backend
     ├─ (bridge spawn unless safe mode)   HTTP/WS gateway on AIOS_BRIDGE_PORT
     └─ AutohalEngine + HotplugMonitor    driver provisioning, push hot-plug events
```

## 3. IPC Data Flow

```
UI/shell/intent ──► IpcPacket(CommandId, Payload)          [aios-core::ipc_protocol]
      │ bincode + SHA-256 checksum + packet_id + priority
      ▼
SharedIpcBus ──── bounded queue: priority insert, dedup, backpressure
      │            (Reject | DropOldest), metrics           [aios-ipc::bus]
      ├─ payload ≥ 4 KiB ⇒ RingBufferTransport (off-queue) [aios-ipc::ring_transport]
      ▼
MessageRouter.dispatch() ── handler(block_id)             [aios-block-mgr::router]
      ▼
StatefulBlock::handle_message() → Response(ok|err)         [aios-core::block]
      │
      └─ hot-swap path: bus.freeze() → Snapshot → swap → unfreeze/reroute
                                                       [aios-live-update::state_transfer]
```

---

## 4. Foundation Crates

### 4.1 `aios-core` — types, protocol, crypto, filesystem (9 files · ~1.1k lines · 38 tests)

Modules: `block` (identity/lifecycle/trait), `crypto` (SHA-256), `error` (`AIOSException`, `Result<T>`), `filesystem` (local/virtual/overlay + permissions), `ipc_protocol` (wire format), `runtime` (sync→async bridge).

| Function / Type | Purpose |
|---|---|
| `StatefulBlock` trait | Core block interface: `handle_message`, `extract_state`/`restore_state`, `health_check` |
| `IpcPacket::new(...)` / `with_priority(p)` | Build packet with auto packet-id + checksum |
| `IpcPacket::serialize()/deserialize()` | bincode round-trip |
| `IpcPacket::verify_checksum()` | Tamper detection |
| `IpcPacket::response_ok()/response_err()` | Replies linked to request id |
| `Payload::to_bytes()/is_empty()` | Payload encoding |
| `crypto::compute_sha256()/verify_sha256()` | Integrity helpers (hex + bytes) |
| `FileSystem::local()/virtual_fs()/overlay()` | FS backends with permission presets |
| `FileSystem::{read,write,list}_{local,virtual}` | Permission-checked operations |
| `runtime::block_on_future(fut)` | Safe sync→async bridge (no nested-runtime panic) |

### 4.2 `aios-ipc` — transports (5 files · ~0.8k lines · 29 tests)

Modules: `bus`, `channel`, `ring_transport`.

| Function / Type | Purpose |
|---|---|
| `IpcBus::new(max_queue)` (+`with_backpressure`, `with_dedup`) | Bounded queue construction |
| `IpcBus::send/send_priority/receive/peek` | Enqueue (policy applied) / dequeue |
| `IpcBus::freeze()/unfreeze(pkts)/reroute(old,new)` | Hot-swap state transfer support |
| `IpcBus::metrics()/reset_metrics()` | sent/dropped/dedup/peak-depth/latency |
| `SharedIpcBus` | `Arc<Mutex<IpcBus>>` cloneable handle |
| `ipc::channel()` → `(IpcSender, IpcReceiver)` | mpsc pair with `Result` mapping |
| `RingBufferTransport::send_via_ring(&pkt)` | ≥4 KiB payloads bypass the queue |
| `RingBufferTransport::try_receive_from_ring/ring_usage/active_rings` | Ring accessors |

### 4.3 `aios-hal` — hardware abstraction (3 files · ~1.8k lines · 34 tests)

Modules: `hardware` (detection + `HalBlock`), `ai_tier`.

| Function / Type | Purpose |
|---|---|
| `HardwareProfile::detect()` | Real probes: CPUID flags, memory, nvidia-smi/ROCm/NPU, PCI/storage/USB/TB |
| `AiTier::from_profile(&profile)` | Tier1 local LLM / Tier2 quantized SLM / Tier3 heuristic |
| `AiTier::description()/max_model_size_gb()/recommended_batch_size()` | Capacity hints |
| Mock factories: `mock_legacy/modern/intel_meteor_lake/qualcomm_x_elite/nvidia...` | Hardware-independent test profiles |
| `HalBlock::new(id)/with_profile(id,p)/profile()` | `StatefulBlock` exposing the profile |

---

## 5. Kernel Subsystems

### 5.1 `aios-block-mgr` — registry/loader/router (8 files · ~2.1k lines · 75 tests)

| Function / Type | Purpose |
|---|---|
| `BlockRegistry::register_block(name,ver,binary)` | Assign `BlockId`, hash binary (SHA-256) |
| `BlockRegistry::{activate,unload,update_state}` | Lifecycle control (`Unloaded→Loaded→Active→Frozen/Error`) |
| `BlockRegistry::assign_capabilities/check_capability` | Token ACL per block |
| `BlockRegistry::boot_discover(root)/load_from_path(dir)` | Disk discovery of `*_v*.bin|.wasm` |
| `BlockRegistry::topology()/topology_with_state()` | Manifest enumeration for replies |
| `BlockLoader::validate_binary(binary,sha)` | Integrity gate |
| `BlockLoader::load_from_binary(_with_capabilities)/load_from_directory` | Validated registration |
| `MessageRouter::register_handler/add_route/dispatch` | Handler table + redirects |
| `DependencyGraph::add_dependency/load_order/unload_order` | Kahn topo-sort, cycle detection |
| `SemanticVersion::parse/is_newer_than/bump_*/is_compatible_with` | Semver |
| `HotReloader::scan_and_reload(&mut registry)` | Watch-dir incremental reload |

### 5.2 `aios-process-mgr` — scheduler & processes (7 files · ~2.6k lines · 73 tests)

| Function / Type | Purpose |
|---|---|
| `Scheduler::new(total_ram_mb)` + `with_time_slice/with_aging_threshold/with_max_restarts/with_memory_pressure_threshold` | Tunables |
| `Scheduler::spawn_process/spawn_real_process/spawn_child` | Admit under RAM quota; host real OS threads |
| `Scheduler::schedule_next()/tick()/force_preempt()` | Priority RR + aging + RT deadlines |
| `Scheduler::{kill,suspend,resume}_process/set_priority` | Control |
| `Scheduler::report_crash/should_restart` | Crash resilience, restart policy |
| Group/session ops: `create_group/create_session/kill_group/suspend_group/set_group_priority` | Bulk control |
| `check_real_threads()/get_real_thread_state/set_cpu_affinity` | Real-thread liveness, per-core pinning |
| `ram_usage()/memory_pressure()/check_memory_pressure()` | Quota telemetry |
| `handle_process_command(scheduler,&packet)` | IPC front-end (Spawn/Kill/AdjustPriority) |
| `process_metrics::{bind_current_thread,record_*}` | TLS per-process counters |
| `cpu_affinity::{set_thread_affinity,available_cores,validate_cores}` | Win32/Linux pinning |
| `PriorityInheritance::{acquire_lock,release_lock,...}` | Priority-inheritance protocol |

### 5.3 `aios-live-update` — atomic hot-swap (5 files · ~1.2k lines · 23 tests)

| Function / Type | Purpose |
|---|---|
| `LiveUpdateEngine::perform_swap(block_id, old…, new…, queue, health_check)` | 7-step atomic swap: freeze→verify SHA→health gate→stash rollback→restore queue |
| `LiveUpdateEngine::rollback(block_id,queue)/expired_rollbacks()/swap_history()` | Rollback window management |
| `StateTransferManager::{extract_state,restore_state,reroute_snapshot}` | Bus freeze/snapshot/reroute |
| `WasmLiveUpdateEngine::{deploy_block,swap_block,rollback_block,call_block_func}` | Real Wasmtime in-place swap incl. linear-memory migration |
| `PersistedLiveUpdateEngine::{perform_swap,rollback,recover_pending_swaps}` | CoW storage + recovery log variant |

### 5.4 `aios-watchdog` — liveness supervisor (5 files · ~1.2k lines · 47 tests)

| Function / Type | Purpose |
|---|---|
| `Heartbeat::{verify,compute_hmac,age_ms}` | HMAC-SHA256 signed heartbeats |
| `Watchdog::receive_heartbeat/check_timeout/force_safe_mode/escalate_actions/reset` | Miss counting → graduated actions (warn→suspend→kill→dump→safe mode) |
| `WatchdogAction::{severity,is_terminal}` | Restart/ForceSafeMode/Terminate ordering |
| `SafeModeShell::{parse_command,execute,orchestrator_restarts}` | Deterministic rescue shell (ps/kill/load/status/logs/restart) |
| `WatchdogRunner::{start,stop,pop_actions}` | Background thread wrapper |

### 5.5 `aios-security` — zero-trust (5 files · ~0.8k lines · 31 tests)

| Function / Type | Purpose |
|---|---|
| `Capability` enum | `CAP_NET_BIND/CONNECT`, `CAP_FS_READ/WRITE`, `CAP_HW_ACCESS`, `CAP_MEM_ALLOC`, `CAP_SCHED_MODIFY`… |
| `AccessControlLayer::{issue_token(_with_ttl),check_permission,revoke_token,clean_expired}` | Issuance/validation/violations log |
| `CapabilityToken::{has_capability,is_expired,verify,compute_signature}` | TTL + HMAC signed tokens |
| `Sandbox::{start,check_syscall,allocate_memory,terminate,from_token}` | Syscall allowlist + memory limits |
| `HardwareSecurityBridge::{assign_mpk_protection,assign_tee_protection,assign_iommu_protection,validate_hardware_access}` | Unified MPK/TEE/IOMMU mapping |

### 5.6 `aios-context` — context store (7 files · ~1.1k lines · 36 tests)

| Function / Type | Purpose |
|---|---|
| `TelemetryStore::{query_metric,query_range,query_by_block,average_value,peak_ram}` | Ring-buffer telemetry |
| `CompressedTelemetryStore::{record,compression_ratio}` | ZSTD cold chunks + hot threshold |
| `WorkflowStore::{record,most_used,recently_used}` | Learned usage profiles (scheduler priority tuning) |
| `StabilityStore::{best_version,record_crash,record_uptime}` | Per-binary-version stability scores |
| `EmbeddedContextStore::{should_compact,compact,export_all,total_entries}` | Unified facade |
| `PersistentStore::{save_all,load_telemetry,save_workflows,save_stability,compact}` | redb-backed persistence |

### 5.7 `aios-wasm` — sandbox runtime (5 files · ~1.6k lines · 56 tests)

| Function / Type | Purpose |
|---|---|
| `WasmSandbox::{compile_module,compile_any,compile_wat}` | Wasmtime engine wrapper |
| `WasmBlock::{new/from_wat,instantiate,call_func}` | Instantiated block instance |
| `WasmBlock::{extract_linear_memory,restore_linear_memory}` | State transfer for hot-swap |
| `SandboxConfig` | fuel/time/memory limits |
| `IsolationConfig/IsolationLevel` | Per-block shared-nothing isolation matrix |
| `WasiFilter::check_syscall` | WASI syscall allow/deny/log policy |
| `BlockExecutor::execute_block(s)` | Batch execution bridge |

### 5.8 `aios-net` — network blocks (5 files · ~1.4k lines · 51 tests)

| Function / Type | Purpose |
|---|---|
| `RealTcpBlock::{start_listening,connect,send,receive,accept_pending,close_connection}` | Real sockets, SO_REUSEADDR/KEEPALIVE/NODELAY, capability-gated bind/connect |
| `RealUdpBlock::{bind,send_to,receive_from,broadcast,port}` | Real UDP incl. SO_BROADCAST |
| `TcpBlock/UdpBlock` (+configs/states) | Simulated transports for deterministic tests |
| `inject_message/inject_packet` | Test hooks |

### 5.9 `aios-exec-compat` — POSIX/Win32 translation (6 files · ~1.9k lines · 89 tests)

| Function / Type | Purpose |
|---|---|
| `ExecutableType::{from_bytes,from_extension,required_capabilities}` | ELF/PE/shebang sniffing |
| `PosixSyscall/SyscallRequest/SyscallResponse` + `PosixTranslator` | POSIX syscall translation (default impl with RAM limits) |
| `Win32Api/Win32Request/Win32Response` + `Win32Translator` | Win32-by-ordinal translation |
| `CompatSandboxManager::{spawn_process,terminate_process,cleanup_terminated,total_memory_used}` | Resource-limited compat processes |
| `DependencyHealer::{scan_dependencies,resolve_missing,heal_dependencies,add_loaded_library}` | Missing DLL/.so healing from search paths |

---

## 6. Hardware Protection & Optimization

| Crate | Lines / Tests | Key API |
|---|---|---|
| `aios-mpk` | 816 / 27 | Intel MPK protection keys, ARM DACR domains, PKRU register control, soft-isolation fallback |
| `aios-iommu` | 528 / 25 | DMA domains, IOVA page tables, device attachment map |
| `aios-tee` | 841 / 28 | SGX/TrustZone/SEV enclaves, sealing, attestation reports |
| `aios-ringbuf` | 653 / 16 (+proptest) | Lock-free SPSC ring: producer/consumer indices, O(1) pass |
| `aios-compress` | 572 / 16 | FP8/INT4 quantization + ZSTD compression, LRU decompress cache |
| `aios-persistence` | 680 / 12 | `CopyOnWriteStorage` (shadow write→fsync→rename), `RecoveryLog`, `SnapshotManager` |
| `aios-optim` | 964 / 39 | Profiler percentiles, hot-path flamegraph, cache-line layout optimizer, grid/random/binary auto-tuner |

---

## 7. System Services

### 7.1 `aios-autohal` — driver auto-provisioning (12 files · ~4.4k lines · 73 tests)

Pipeline (5 steps): detect → DriverStore lookup → fetch/adapt → validate+grant+Wasmtime instantiate → cache/register.

| Function / Type | Purpose |
|---|---|
| `AutohalEngine::provision()/rescan()/remove_device()` | Pipeline owner; cached-driver replug |
| `AutohalEngine::{record_failure,rollback_to_generic,set_cap_override}` | Self-healing after 3 failures → Generic Fallback |
| `extract_fingerprints()/diff_fingerprints()` | USB/PCI/BT/ACPI/NVMe identity diffs |
| `HotplugMonitor` (+`native.rs`) | udev netlink (Linux) / WM_DEVICECHANGE (Windows) push events + poll net |
| `DriverStore/DriverIndex` | Persistent cache at `AIOS://store/drivers/`, failure counters |
| `ui_tui::HardwareInspector / ui_gui::show_panel` | TUI/GUI parity widgets |

### 7.2 `aios-vfs` + `aios-fm` — virtual filesystem & file manager (11 files · ~3.2k lines · 45 tests)

| Function / Type | Purpose |
|---|---|
| `VfsPath::parse()/to_uri()/join()` | `AIOS://` and `HOST://` schemes |
| `AiosVfs` / `HostVfs` | Sandboxed roots (`/system`,`/sandbox`,`/store`,`/config`) / ACL-tokened host FS |
| `VirtualFileSystem::resolve()` | ACL check + anti-traversal canonicalization |
| `{copy_recursive,move_item,delete_item}` | Cancellable async bulk ops with progress |
| `analyze_file()` | AI preview heuristics |
| `FileManager::new(fs,acl)/send(Command)/snapshot()` | UI-agnostic engine shared by both frontends |
| `ui_tui::{key_to_action,draw} / ui_gui::show` | Two-panel Volkov/Far-style renderers |

### 7.3 `aios-cluster` — multi-node scheduling (8 files · ~3.0k lines · 31 tests)

| Function / Type | Purpose |
|---|---|
| `DistributedScheduler::{start(peers),shutdown,tick}` | One type = coordinator or worker (executor attached?) |
| `DistributedScheduler::{spawn,kill,set_priority,get_state,migrate}` | Remote ops over `ClusterMessage` RPC |
| `TcpClusterTransport / InMemoryClusterTransport` | Length-framed bincode TCP / mpsc registry for tests |
| `ProcessExecutor` trait (+`SchedulerProcessExecutor`, mock) | Node-side spawn/kill/state extraction |
| Checkpoint replication | Heartbeat broadcast of snapshots, TTL pruning, failover restore |
| `ClusterConfig::{from_env,from_json}` | `AIOS_CLUSTER_*` bootstrap |

### 7.4 Web stack

| Crate | Key API |
|---|---|
| `aios-browser` (8 files · 1.4k ln · 36 t) | `BrowserEngine::navigate(url)`, `HtmlParser::{parse,extract_text,extract_links,extract_title}`, `Renderer::{render_page,to_text}`, headless Chromium-class dump fallback, `BrowserBlock` |
| `aios-search` (5 files · 0.4k ln · 7 t) | `SearchEngine::{search}` over DuckDuckGo/SearXNG/Brave + `SearchSummarizer` LLM TL;DR |
| `aios-webview` (2 files · 0.3k ln · 7 t) | `WebBrowser::{open,navigate,back,forward,close}` on background thread via event-loop proxy; persistent profile; `resolve_target()` omnibox rule |
| `aios-net-config` (5 files · 0.9k ln · 32 t) | `NetworkConfigStore::{load,load_or,save}`, `NetworkConfig::apply_updates`, validators, `NetSettingsBlock` |

### 7.5 Store & updates

| Crate | Key API |
|---|---|
| `aios-store` (8 files · 2.1k ln · 58 t) | `StoreManager::{search,install,update,parse_source_spec,trust_source}`, `ManifestValidator` (SHA-256 + Ed25519 `verify_strict`, trusted keys), `BlockInstaller` (sidecars, backup/rollback, `check_updates`) |
| `aios-updater` (4 files · 0.4k ln · 18 t) | `DualBootManager::{swap_slot,record_boot_success,should_rollback}` (Slot A/B), `RollbackManager::{take_snapshot,rollback_to,auto_rollback_if_needed}`, `HotSwapEngine` |

### 7.6 Observability

| Crate | Key API |
|---|---|
| `aios-telemetry` (4 files · 0.5k ln · 17 t) | `FlightRecorder::{record,dump_since,dump_by_kind}`, `MetricCollector::to_prometheus()`, `TraceContext::{begin_span,end_span,to_json}` |
| `aios-debug` (3 files · 0.3k ln · 10 t) | `CrashReporter::generate_report(kind,…,zero_knowledge)` (stack hashing/redaction), `PanicHandler::install()` global hook |

---

## 8. AI Layer

### 8.1 `aios-llm` (5 files · ~0.7k lines · 13 tests)

| Function / Type | Purpose |
|---|---|
| `LlmEngine::{from_config,query,query_stream,config,backend_label}` | Cloud (Groq/OpenRouter/Google) or candle GGUF local (Qwen2.5-0.5B/7B INT4) |
| `LlmStreamSink` | tokio mpsc stream of deltas |
| `extract_stream_delta(payload, google_shape)` | OpenAI + Google SSE delta parsing |
| `download_default_model(kind)/detect_local_models()` | hf-hub download + local `.gguf` scan |

### 8.2 `aios-builder` (5 files · ~0.6k lines · 23 tests)

| Function / Type | Purpose |
|---|---|
| `EasyLangParser::parse(text,name)` | Line DSL (`spawn/timer/load/unload/kill/query/compact/status`) → `Workflow` |
| `WorkflowCompiler::{generate_wat,compile_to_wasm}` | WAT→WASM module (`init/start/step_N` exports) |
| `AutoManifestGenerator::{from_wasm_binary,from_workflow_intents}` | Capability inference (15-entry table) + JSON manifest |

### 8.3 `aios-bridge` — API gateway (5 files · ~1.5k lines · covered by 24 integration tests)

Endpoints (`server.rs`):

| Method Path | Handler |
|---|---|
| GET `/api/v1/health` | Health/version/uptime |
| GET `/api/v1/system/status` | Watchdog + processes + blocks + RAM |
| POST `/api/v1/intent` | NL intent execute (LLM fallback, ACL checks) |
| POST `/api/v1/workflow` | Batch prompt sequence |
| POST `/api/v1/llm/query` | Direct LLM query |
| POST `/api/v1/browse` · `/api/v1/search` | Browser/search proxies |
| GET `/api/v1/store/index` · POST `/api/v1/store/register` · POST `/api/v1/store/publish` | Store service (publish verifies SHA-256 + optional Ed25519) |
| GET `/index.json` · `/store/index.json` · `/blocks/{name}.wasm` · `/store/blocks/{name}.wasm` | Update-service catalog/download |
| GET `/api/v1/metrics` · `/api/v1/traces` · POST `/api/v1/crash-report` | Prometheus text / spans / crash report |
| WS `/ws/telemetry` | 100 ms RAM/process push |

Core types: `BridgeContext` (shared subsystem handles), `IntentParser` (RU/EN keywords + `parse_with_llm_fallback`), `UserIntent` enum, ~30 DTOs, `BridgeError`.

---

## 9. Interfaces & Binaries

| Binary / Crate | Purpose | Notes |
|---|---|---|
| `aios` (kernel TUI, 6 files · 3.6k ln · 9 t) | Unified system binary | 7 tabs: System&HW / Blocks&Svc / AI Console / Studio Bridge / Network&Store / Web / Shell; `--safe-mode`, `--daemon`; hotkeys `b B n N W F9 F10`; cluster shell commands |
| `aios-tui` (4 files · 4.3k ln · 40 t) | Standalone dashboard | `fetch/search/open`, `net get/set/reset`, full `store` command family, watchdog shell fallback |
| `aios-gui` (18 files · 3.3k ln · 10 t) | egui dashboard 1200×800 dark | Tabs: Dashboard / Blocks / AI Studio / Store / Network / Deps / Browser / Files / Hardware |
| `aios-daemon` (`aiosd`, 1 file · 183 ln) | Headless Docker/server mode | Boot blocks, heartbeat thread, periodic redb persistence |
| `aios-studio` (SPA) | Web dashboard served by bridge | Command palette, WS telemetry chart, security center |
| `aios-init` (standalone, musl static) | PID 1 for initramfs | VFS mounts, block supervisor (3 restarts), zombie reaping, rescue shell, handover to `/system/aios-core` |
| Live ISO (`live/build.sh`) | Hybrid BIOS+UEFI image | aios-init default `/init`; busybox legacy behind `USE_BUSYBOX_INIT=1` |

## 10. Bare-Metal Microkernel Track

`aios-kernel` (`no_std`, `x86_64-unknown-none`, nightly; 10 files · ~1.3k lines) + `aios-kernel-run` (QEMU BIOS runner).

| Milestone | Status | Content |
|---|---|---|
| M0 (v2.26.0) | ✅ | QEMU boot, serial COM1 + VGA console, physical-memory mapping |
| M1 (v2.27.0) | ✅ | GDT/TSS (double-fault IST), 256-entry IDT, PIC remap, PIT 100 Hz, PS/2 keyboard |
| M2 (v2.28.0) | ✅ | Page-table walker, map/unmap + frame allocator, 2 MiB free-list heap (`Box/Vec/String`) |
| M3 | ⬜ plan | Preemption: timer scheduler, context switch, ring 0/3 |
| M4 | ⬜ plan | Kernel-side IPC reusing `aios_core::ipc_protocol` |

Modules: `main` (entry/stacks/idle loop) · `gdt` (GDT+TSS) · `idt` (256 gates) · `interrupts` (PIC/PIT/keyboard + generated stubs) · `memory` (translate/map/unmap/bump allocator) · `heap` (free-list GlobalAlloc) · `vga` (80×25 writer) · `serial` (COM1) · `port` (inb/outb) · `build.rs` (256 asm vector stubs).

## 11. Integration Tests (root `tests/`, 14 files · 162 tests)

| File | Tests | Coverage |
|---|---|---|
| `integration_test.rs` | 30 | Full lifecycle, IPC speed, concurrent spawns, hot-swap+IPC |
| `bridge_tests.rs` | 24 | EN/RU intent parsing |
| `chaos_test.rs` | 18 | Corrupted packets, bus overflow, crash loops |
| `real_file_io.rs` | 12 | Snapshot/COW persistence on real disk |
| `real_network.rs` | 11 | Real TCP listen/accept/multi-client |
| `stress_test.rs` | 11 | ×1000 spawn, RT ×500, registry ×500 (dual thresholds debug/release) |
| `real_threads.rs` | 10 | Real threads: terminate/suspend/resume |
| `real_wasm.rs` | 8 | Wasmtime end-to-end, multi-block isolation |
| `browser_search_tests.rs` | 7 | HTML parser |
| `full_lifecycle.rs` | 7 | Boot→deploy→swap→watchdog→shutdown |
| `real_hot_swap.rs` | 7 | WASM version-change hot swap |
| `e2e_pipeline_test.rs` | 6 | HW→tier→LLM intent→EasyLang→WASM chain |
| `fuzz_test.rs` | 6 | Randomized packet fuzzing |
| `stress_fault_tolerance.rs` | 5 | 50 parallel WASM blocks, crash storms |

## 12. Codebase Statistics (v2.28.1 audit snapshot)

- **244 Rust source files**, **~59,400 lines** across 39 workspace crates + 3 standalone crates.
- **1,338 tests green** in 91 suites (unit + integration + doc-tests), `cargo clippy --workspace --all-targets`: **0 warnings**, `cargo fmt --check`: clean.

Top crates by size: `aios-autohal` 4.4k · `aios-tui` 4.3k · `aios` 3.6k · `aios-gui` 3.3k · `aios-cluster` 3.0k · `aios-process-mgr` 2.6k · `aios-block-mgr` 2.1k · `aios-store` 2.1k · `tests/` 3.9k.
