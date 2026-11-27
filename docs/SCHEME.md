# AIOS Program Scheme & Function Map

> Version: v2.84.0 — Date: 2026-11-26 (post-audit update)
> Companion documents: `docs/AUDIT.md` (full audit), `docs/ARCHITECTURE.md` (deep architecture), `docs/INTERFACE.md` (UI guide).
> Bilingual pair: `docs/SCHEME.ru.md` (Russian). Update both files together.
> This document is the **call-level map**: every crate, its modules, and its key public functions.

## 1. System Scheme (Layer Map)

```
                    USER INTERFACES
  aios (kernel TUI) │ aios-tui │ aios-gui (egui desktop shell) │ aios-studio (SPA)
                              │ aiosd (aios-daemon, headless)
                              │ aios-shell (POSIX-like shell adapter)
─────────────────────────────────────────────────────────────────────────
         GATEWAY / COMMAND LAYER           │            AI CORE
  aios-bridge — axum HTTP/WS, auth,        │   aios-llm — cloud (Groq/OpenRouter/
  RU/EN intent + LLM fallback, capability │   Google) + local GGUF (candle),
  enforcement, store + sys-control API    │   streaming; aios-builder —
  aios-console — unified command core      │   EasyLang → WASM workflows
  (CommandSpec, dispatch, Host/Kernel
  backends, job control, net_text)
─────────────────────────────────────────────────────────────────────────
                          SYSTEM SERVICES
  aios-store / aios-updater (A/B slots, Ed25519 trust) │ aios-cluster (multi-node)
  aios-autohal (+ NIC class/fingerprints, hotplug)    │ aios-vfs + aios-fm (files)
  aios-browser / aios-search / aios-webview           │ aios-net-config
  aios-sys-control (Wi-Fi, DHCP, layout, power, vault)│ aios-telemetry / aios-debug
─────────────────────────────────────────────────────────────────────────
                KERNEL SUBSYSTEMS (StatefulBlock ecosystem)
  aios-block-mgr │ aios-process-mgr (+ fault/recovery) │ aios-live-update │ aios-watchdog
  aios-security  │ aios-context │ aios-wasm │ aios-net │ aios-exec-compat
  aios-core::fault (FaultInfo/FaultKind/RecoveryAction)
─────────────────────────────────────────────────────────────────────────
                             FOUNDATION
  aios-core (types/IpcPacket/crypto/fault/fs/runtime) │ aios-ipc (bus/channel/ring)
  aios-hal (detect/tier) │ aios-ringbuf · aios-compress
  aios-persistence · aios-optim │ hw protection: mpk/iommu/tee
═════════════════════════════════════════════════════════════════════════
  BARE-METAL TRACK — 13 standalone crates, excluded from the workspace:
  aios-init │ aios-kernel │ aios-kernel-run
  boot/aios-boot │ boot/aios-bootinfo │ boot/aios-bootstore │ boot/aios-config
  boot/aios-easylang │ boot/aios-image │ boot/aios-install │ boot/aios-sbctl
  boot/aios-storectl │ boot/aios-wasmhost
  aios-boot → bootinfo → kernel → image/install; sbctl signs the loader;
  aios-kernel-run builds a Limine bootable ISO (BIOS + UEFI) with two kernel
  entries (current + rollback); ISO and USB IMG are marker-verified.
```

Workspace: **41 members** (`Cargo.toml:52-83`), plus the 13 standalone crates above.
`aios-studio` is a non-Rust SPA served from `aios-bridge` via `ServeDir`.

## 2. Boot Flow (`aios` kernel binary)

```
main(--daemon|--safe-mode?)
└─ orchestrator::initialize()
    ├─ hw_probe::probe()                    HwProfile (CPU/RAM/GPU, sysinfo + HAL)
    ├─ Scheduler::new(ram_mb)               aging 5 s, slice 100 ms, max 5 restarts,
    │                                       memory-pressure callbacks, RT deadlines
    ├─ BlockRegistry                        stub ipc_bus + scheduler registered here;
    │                                       blocks::register_base_blocks() creates
    │                                       hal / browser (skipped < 4096 MiB) /
    │                                       net_settings + factory blocks
    ├─ blocks::load_disk_blocks(...)        third-party *.wasm/*.bin (skipped in --safe-mode)
    ├─ BlockExecutor + MessageRouter        router registers every configured base+disk block
    ├─ AccessControlLayer                   capability tokens
    ├─ Watchdog::new(...)                   runner is attached by console_control; heartbeat
    │                                       thread is not started inside initialize()
    ├─ LlmEngine                            deferred background init; bridge owns a second instance
    ├─ DistributedScheduler                 optional cluster node + SchedulerProcessExecutor
    └─ BridgeRuntime::spawn_server()        HTTP/WS gateway on AIOS_BRIDGE_PORT (unless safe mode)

  TUI startup (aios/src/tui/app_state.rs, not part of initialize()):
    ├─ HostBackend / unified console seams
    └─ AutohalEngine + HotplugMonitor via init_hw_engine() (disabled in safe mode)
```

## 3. IPC Data Flow

```
UI/shell/intent ──► IpcPacket(CommandId, Payload)          [aios-core::ipc_protocol]
       │ bincode + SHA-256 checksum (over payload) + packet_id + priority
       ▼
SharedIpcBus ──── bounded queue: priority insert, dedup, backpressure
       │            (Reject | DropOldest), metrics           [aios-ipc::bus]
       ├─ opt-in: RingBufferTransport::send_via_ring() for payload ≥ 4 KiB
       │          (not an automatic bypass of the bus)   [aios-ipc::ring_transport]
       ▼
MessageRouter.dispatch() ── handler(block_id)             [aios-block-mgr::router]
       ▼
StatefulBlock::handle_message() → IpcPacket::response_ok/err [aios-core::block]
       │
       ├─ hot-swap path: bus.freeze() → Snapshot → swap → unfreeze/reroute
       │                                              [aios-live-update::state_transfer]
       └─ fault path: FaultReport 0x0070 / RecoveryDecision 0x0071
                                              [aios-core::fault, aios-process-mgr]
```

---

## 4. Foundation Crates

### 4.1 `aios-core` — types, protocol, crypto, faults, filesystem, secrets (11 files · ~1.4k lines · 39 tests)

Modules: `block`, `crypto`, `error`, `fault`, `filesystem`, `ipc_protocol`, `runtime`, `secrets`.

| Function / Type | Purpose |
|---|---|
| `StatefulBlock` trait | `handle_message`, `extract_state`/`restore_state`, `health_check` |
| `IpcPacket::new(...)` / `with_priority(p)` | Auto packet-id + checksum (SHA-256 over payload bytes) |
| `IpcPacket::serialize()/deserialize()` | bincode round-trip |
| `IpcPacket::verify_checksum()` | Tamper detection on the payload |
| `IpcPacket::response_ok()/response_err()` | Replies linked to request id (`Response` DTO also defined) |
| `Payload::to_bytes()/is_empty()` | Payload encoding |
| `crypto::compute_sha256()/verify_sha256()` | Integrity helpers (hex + bytes) |
| `fault::{FaultInfo,FaultKind,RecoveryAction}` | Crash/fault reporting contract used by scheduler syscalls |
| `FileSystem::local()/virtual_fs()/overlay()` | FS backends with permission presets |
| `FileSystem::{read,write,list}_{local,virtual}` | Permission-checked operations |
| `runtime::block_on_future(fut)` | Safe sync→async bridge |
| `secrets::{random_secret,data_dir,env_name,resolve_secret,resolve_secret_in,secret_or_ephemeral}` | Shared secret resolution: `AIOS_<NAME>_SECRET` → `<data_dir>/secrets/<name>.key` → CSPRNG; never path-derived |

### 4.2 `aios-ipc` — transports (5 files · 806 lines · 29 tests)

Modules: `bus`, `channel`, `ring_transport`.

| Function / Type | Purpose |
|---|---|
| `IpcBus::new(max_queue)` (+`with_backpressure`, `with_dedup`) | Bounded queue construction |
| `IpcBus::send/send_priority/receive/peek` | Enqueue (policy applied) / dequeue |
| `IpcBus::freeze()/unfreeze(pkts)/reroute(old,new)` | Hot-swap state transfer support |
| `IpcBus::metrics()/reset_metrics()` | sent/dropped/dedup/peak-depth/latency |
| `SharedIpcBus` | `Arc<Mutex<IpcBus>>` cloneable handle |
| `ipc::channel()` → `(IpcSender, IpcReceiver)` | mpsc pair with `Result` mapping |
| `RingBufferTransport::send_via_ring(&pkt)` | Opt-in off-queue path for large payloads |
| `RingBufferTransport::try_receive_from_ring/ring_usage/active_rings` | Ring accessors |

### 4.3 `aios-hal` — hardware abstraction (3 files · ~1.9k lines · 36 tests)

| Function / Type | Purpose |
|---|---|
| `HardwareProfile::detect()` | CPUID, memory, nvidia-smi/ROCm/NPU, PCI/storage/USB/TB probes |
| `AiTier::from_profile(&profile)` | Tier1 local LLM / Tier2 quantized SLM / Tier3 heuristic |
| `AiTier::description()/max_model_size_gb()/recommended_batch_size()` | Capacity hints |
| Mock factories: `mock_legacy/modern/intel_meteor_lake/qualcomm_x_elite/nvidia...` | Hardware-independent test profiles |
| `HalBlock::new(id)/with_profile(id,p)/profile()` | `StatefulBlock` exposing the profile |

---

## 5. Kernel Subsystems

### 5.1 `aios-block-mgr` — registry/factory/loader/router (9 files · ~2.3k lines · 83 tests)

| Function / Type | Purpose |
|---|---|
| `BlockRegistry::register_block(name,ver,binary)` | Assign `BlockId`, hash binary (SHA-256) |
| `BlockRegistry::{activate,unload,update_state}` | Lifecycle (`Unloaded→Loaded→Active→Frozen/Error`) |
| `BlockRegistry::assign_capabilities/check_capability` | Token ACL per block |
| `BlockRegistry::boot_discover(root)/load_from_path(dir)` | Disk discovery of `*_v*.bin|.wasm` |
| `BlockRegistry::topology()/topology_with_state()` | Manifest enumeration for replies |
| `BlockFactory` / `register_base_blocks` | Creates hal/browser/net_settings blocks for boot |
| `BlockLoader::validate_binary(binary,sha)` | Integrity gate |
| `BlockLoader::load_from_binary(_with_capabilities)/load_from_directory` | Validated registration |
| `MessageRouter::register_handler/add_route/dispatch/unregister_handler` | Handler table + redirects |
| `DependencyGraph::add_dependency/load_order/unload_order` | Kahn topo-sort, cycle detection |
| `SemanticVersion::parse/is_newer_than/bump_*/is_compatible_with` | Semver |
| `HotReloader::scan_and_reload(&mut registry)` | Incremental reload from a watch dir |

### 5.2 `aios-process-mgr` — scheduler, processes, fault recovery (8 files · ~3.1k lines · 86 tests)

| Function / Type | Purpose |
|---|---|
| `Scheduler::new(total_ram_mb)` + `with_time_slice/with_aging_threshold/with_max_restarts/with_memory_pressure_threshold` | Tunables |
| `Scheduler::spawn_process/spawn_real_process/spawn_child` | RAM quota admission; host OS threads |
| `Scheduler::schedule_next()/tick()/force_preempt()` | Priority RR + aging + RT deadlines |
| `Scheduler::{kill,suspend,resume}_process/set_priority` | Control |
| `Scheduler::report_crash/should_restart` | Restart policy |
| `Scheduler::poll_faults/take_faults/resolve_fault` | `FaultInfo` queue + `RecoveryAction` (resume/rollback/terminate) |
| `SchedulingMode`, RT jitter log, memory-pressure callbacks | Adaptive scheduling telemetry |
| Group/session ops: `create_group/create_session/kill_group/suspend_group/set_group_priority` | Bulk control |
| `check_real_threads()/get_real_thread_state/set_cpu_affinity/get_cpu_affinity/list_real_threads` | Real-thread liveness, per-core pinning |
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
| `WasmLiveUpdateEngine::{deploy_block,swap_block,rollback_block,call_block_func}` | Wasmtime in-place swap with linear-memory migration |
| `PersistedLiveUpdateEngine::{perform_swap,rollback,recover_from_crash,swap_history,has_rollback,storage_usage}` | CoW storage + recovery log variant |

### 5.4 `aios-watchdog` — liveness supervisor (5 files · ~1.6k lines · 58 tests)

| Function / Type | Purpose |
|---|---|
| `Heartbeat::{verify,compute_hmac,age_ms}` | HMAC-SHA256 signed heartbeats |
| `Watchdog::receive_heartbeat/check_timeout/force_safe_mode/escalate_actions/reset` | Miss counting → graduated actions (warn→suspend→kill→dump→safe mode) |
| `WatchdogAction::{severity,is_terminal}` | Restart/ForceSafeMode/Terminate ordering |
| `SafeModeShell::{parse_command,execute,orchestrator_restarts}` | Deterministic rescue shell (ps/kill/load/status/logs/restart) |
| `WatchdogRunner::{start,stop,pop_actions,from_watchdog,reset}` | Background thread wrapper |

### 5.5 `aios-security` — zero-trust (5 files · 763 lines · 31 tests)

| Function / Type | Purpose |
|---|---|
| `Capability` enum | `CAP_NET_BIND/CONNECT`, `CAP_FS_READ/WRITE`, `CAP_HW_ACCESS`, `CAP_MEM_ALLOC`, `CAP_SCHED_MODIFY`… |
| `AccessControlLayer::{issue_token(_with_ttl),check_permission,revoke_token,clean_expired}` | Issuance/validation/violations log |
| `CapabilityToken::{has_capability,is_expired,verify,compute_signature}` | TTL + HMAC signed tokens |
| `Sandbox::{start,check_syscall,allocate_memory,terminate,from_token}` | Syscall allowlist + memory limits |
| `HardwareSecurityBridge::{assign_mpk_protection,assign_tee_protection,assign_iommu_protection,validate_hardware_access}` | Unified MPK/TEE/IOMMU mapping |

### 5.6 `aios-context` — context store (7 files · 1,092 lines · 36 tests)

| Function / Type | Purpose |
|---|---|
| `TelemetryStore::{query_metric,query_range,query_by_block,average_value,peak_ram}` | Ring-buffer telemetry |
| `CompressedTelemetryStore::{record,compression_ratio}` | ZSTD cold chunks + hot threshold |
| `WorkflowStore::{record,most_used,recently_used}` | Learned usage profiles |
| `StabilityStore::{best_version,record_crash,record_uptime}` | Per-binary-version stability scores |
| `EmbeddedContextStore::{should_compact,compact,export_all,total_entries}` | Unified facade |
| `PersistentStore::{save_all,load_telemetry,save_workflows,save_stability,compact}` | redb-backed persistence |

### 5.7 `aios-wasm` — sandbox runtime (5 files · ~2.0k lines · 62 tests)

| Function / Type | Purpose |
|---|---|
| `WasmSandbox::{compile_module,compile_any,compile_wat}` | Wasmtime engine wrapper |
| `WasmBlock::{new/from_wat,instantiate,call_func}` | Instantiated block instance |
| `WasmBlock::{extract_linear_memory,restore_linear_memory}` | State transfer for hot-swap |
| `SandboxConfig` | Fuel/time/memory limits |
| `IsolationConfig/IsolationLevel` | Per-block shared-nothing isolation matrix |
| `WasiFilter::check_syscall` | WASI syscall allow/deny/log policy |
| `BlockExecutor::execute_block(s)` | Batch execution bridge |

> Note: `Module::deserialize` is used as a fast path for pre-compiled modules; only trusted bytes must reach it.

### 5.8 `aios-net` — network blocks (6 files · ~1.7k lines · 57 tests)

| Function / Type | Purpose |
|---|---|
| `RealTcpBlock::{start_listening,connect,send,receive,accept_pending,close_connection}` | Real sockets, SO_REUSEADDR/KEEPALIVE/NODELAY, capability-gated bind/connect |
| `RealUdpBlock::{bind,send_to,receive_from,broadcast,port}` | Real UDP incl. SO_BROADCAST |
| `icmp::{ping,resolve_ipv4,build_echo_request,parse_echo_reply,internet_checksum}` + `IcmpReply` / `PingReport` | Real ICMPv4 echo on Windows (`IcmpSendEcho`) and Unix (ICMP datagram/raw socket), checksum + identifier/sequence correlation, RTT/TTL/loss report |
| `TcpBlock/UdpBlock` (+configs/states) | Simulated transports for deterministic tests |
| `inject_message/inject_packet` | Test hooks |

### 5.9 `aios-exec-compat` — POSIX/Win32 translation (6 files · 1,947 lines · 89 tests)

| Function / Type | Purpose |
|---|---|
| `ExecutableType::{from_bytes,from_extension,required_capabilities}` | ELF/PE/shebang sniffing |
| `PosixSyscall/SyscallRequest/SyscallResponse` + `PosixTranslator` | POSIX syscall translation |
| `Win32Api/Win32Request/Win32Response` + `Win32Translator` | Win32-by-ordinal translation |
| `CompatSandboxManager::{spawn_process,terminate_process,cleanup_terminated,total_memory_used}` | Resource-limited compat processes |
| `DependencyHealer::{scan_dependencies,resolve_missing,heal_dependencies,add_loaded_library}` | Missing DLL/.so healing |

---

## 6. Hardware Protection & Optimization

| Crate | Non-blank lines / tests | Key API |
|---|---|---|
| `aios-mpk` | 816 / 27 | Intel MPK keys, ARM DACR domains, PKRU control, software isolation fallback |
| `aios-iommu` | 528 / 25 | DMA domains, IOVA page tables, device attachment map |
| `aios-tee` | 841 / 28 | SGX/TrustZone/SEV enclaves, sealing, attestation reports |
| `aios-ringbuf` | 653 / 16 (+proptest) | Lock-free SPSC ring: producer/consumer indices, O(1) pass |
| `aios-compress` | 572 / 16 | FP8/INT4 quantization + ZSTD, LRU decompress cache |
| `aios-persistence` | 741 / 13 | `CopyOnWriteStorage` (shadow write→fsync→rename), `RecoveryLog`, `SnapshotManager` |
| `aios-optim` | 964 / 39 | Profiler percentiles, hot-path flamegraph, cache-line optimizer, auto-tuner |

---

## 7. System Services

### 7.1 `aios-autohal` — driver auto-provisioning + NIC (13 files · ~4.8k lines · 79 tests)

Pipeline: detect → DriverStore lookup → fetch/adapt → validate + grant + Wasmtime instantiate → cache/register.

| Function / Type | Purpose |
|---|---|
| `AutohalEngine::provision()/rescan()/remove_device()/provision_blocking()` | Pipeline owner; cached-driver replug |
| `AutohalEngine::{record_failure,rollback_to_generic,set_cap_override,tokens,token,uninstall_driver}` | Self-healing, token exposure, uninstall |
| `extract_fingerprints()/diff_fingerprints()` | USB/PCI/BT/ACPI/NVMe identity diffs |
| `nic::{NicClass,class_from_code,classify,is_nic}` + `SupportedHardware::for_class` + `HardwareFingerprint::nic_class` | PCI NIC class classification for `driver nics` |
| `HotplugMonitor` (+`native.rs`) | udev netlink (Linux) / WM_DEVICECHANGE (Windows) push events + poll fallback |
| `DriverStore/DriverIndex` | Persistent cache at `AIOS://store/drivers/`, failure counters |
| `ui_tui::HardwareInspector` / `ui_gui::HardwarePanel::show` | TUI/GUI parity widgets |

### 7.2 `aios-vfs` + `aios-fm` — virtual FS & file manager (12 files · ~3.5k lines · 53 tests)

| Function / Type | Purpose |
|---|---|
| `VfsPath::parse()/to_uri()/join()` | `AIOS://` and `HOST://` schemes |
| `AiosVfs` / `HostVfs` | Sandboxed roots (`/system`,`/sandbox`,`/store`,`/config`) / ACL-tokened host FS |
| `PathScope::{parse,aios,host,contains}` / `PathAclRule` / `PathAcl::{grant,revoke,check,require,rules}` | Path-scoped `vfs:read` / `vfs:write` rights; most specific scope wins, child revocation overrides parent grant |
| `AiosVfs::with_path_acl()` / `path_acl()` | Shared ACL enforced after `canonicalize_inside` on every read/write operation |
| `VirtualFileSystem::resolve()/canonicalize_inside()` | ACL check + anti-traversal canonicalization |
| `{copy_recursive,move_item,delete_item}` | Cancellable async bulk ops with progress |
| `analyze_file()` | AI preview heuristics |
| `FileManager::new(fs,acl)/send(Command)/snapshot()` | UI-agnostic engine shared by both frontends |
| `ui_tui::{key_to_action,draw} / ui_gui::show` | Two-panel Volkov/Far-style renderers |

### 7.3 `aios-cluster` — multi-node scheduling (8 files · 2,961 lines · 31 tests)

| Function / Type | Purpose |
|---|---|
| `DistributedScheduler::{start(peers),shutdown,tick,set_executor}` | One type = coordinator or worker; executor attached at runtime |
| `DistributedScheduler::{spawn,kill,set_priority,get_state,migrate}` | Remote ops over `ClusterMessage` RPC |
| `TcpClusterTransport / InMemoryClusterTransport` | Length-framed bincode TCP / mpsc registry for tests |
| `ProcessExecutor` trait (+`SchedulerProcessExecutor`, mock) | Node-side spawn/kill/state extraction |
| Checkpoint replication | Heartbeat broadcast of snapshots, TTL pruning, failover restore |
| `ClusterConfig::{from_env,from_json}` | `AIOS_CLUSTER_*` bootstrap |

### 7.4 Web stack

| Crate | Non-blank lines / tests | Key API |
|---|---|---|
| `aios-browser` | 4,339 / 113 | `BrowserEngine::{navigate,evaluate}`, `BrowserSession::{go,back,forward,reload,eval_js,normalize_url}`, embedded boa JS engine, `HtmlParser`, `Renderer`, `dom_to_html`, headless Chromium-class dump fallback; `NetworkClient` SSRF policy (HTTP(S) only, per-hop DNS/IP checks, DNS pinning via `resolve_to_addrs`, manual redirects, streamed body cap) and `BrowserConfig::{allow_private_targets,max_response_bytes,for_untrusted}` |
| `aios-search` | 438 / 7 | `SearchEngine::search` over DuckDuckGo/SearXNG/Brave + `SearchSummarizer` LLM TL;DR |
| `aios-webview` | 456 / 7 | `WebBrowser::{open,navigate,back,forward,close}` on a background thread; persistent profile; `resolve_target()` omnibox rule |
| `aios-net-config` | 942 / 35 | `NetworkConfigStore::{load,load_or,save}`, `NetworkConfig::apply_updates`, validators, `NetSettingsBlock` |
| `aios-sys-control` | 2,609 / 55 | `NetManager` (sim/host Wi-Fi, DHCP, `recheck/adapters/lease`), `host_net::AdapterState`, `LayoutManager` (EN/RU hotkeys), `PowerManager` + 80/70 °C thermal governor, `KeyringVault` (AES-256-GCM redb, TEE-bound) |

### 7.5 Store & updates

| Crate | Non-blank lines / tests | Key API |
|---|---|---|
| `aios-store` | 2,455 / 63 | `StoreManager::{search,install,update,parse_source_spec,trust_source}`, `ManifestValidator` (SHA-256 + Ed25519 `verify_strict`), `BlockInstaller` (sidecars, backup/rollback, `check_updates`) |
| `aios-updater` | 406 / 18 | `DualBootManager::{swap_slot,record_boot_success,should_rollback}` (A/B slots), `RollbackManager::{take_snapshot,rollback_to,auto_rollback_if_needed}`, `HotSwapEngine` |

### 7.6 Observability

| Crate | Non-blank lines / tests | Key API |
|---|---|---|
| `aios-telemetry` | 542 / 17 | `FlightRecorder::{record,dump_since,dump_by_kind}`, `MetricCollector::to_prometheus()`, `TraceContext::{begin_span,end_span,to_json}` |
| `aios-debug` | 322 / 10 | `CrashReporter::generate_report(kind,…,zero_knowledge)` (stack hashing/redaction), `PanicHandler::install()` |

### 7.7 `aios-console` — unified text console core (18 files · ~9.6k lines · 130 tests)

| Function / Type | Purpose |
|---|---|
| `CommandSpec` / `SPECS` / `command registry` | Single declaration of console command vocabulary (text + kernel parity) |
| `CommandBackend` / `backend::dispatch()` | Backend-neutral dispatch used by TUI, bridge and daemon |
| `HostBackend` / `KernelBackend` / `UnavailableBackend` | Host process, kernel console and graceful fallback implementations |
| `CommandOutput` | Structured output shared by all frontends |
| `JobControl` / `FileManagerControl` / `net_text` | Jobs, FM commands and `no_std`-compatible network text parsing |
| `PingControl` / `PingReplyInfo` / `PingReportInfo` / `IcmpPingControl` | Real ICMP echo seam and report shape for `console ping`, mockable in tests |
| `HostBackend::acl_command` / `ping_command` / `set_path_acl` / `set_ping_control` | Path-ACL rules and ICMP echo output on the host, with shared/test seams |

### 7.8 `aios-shell` — shell adapter (1 file · ~1.8k lines · 12 tests)

| Function / Type | Purpose |
|---|---|
| Console-prefix routing, POSIX aliases, `source`, history/env adapters | Legacy bare-shell commands mapped onto `aios-console` |
| Job table adapters | Shared job control with the unified console |
| `sh -c` fallback | Last-resort execution for unmigrated commands |

---

## 8. AI Layer

### 8.1 `aios-llm` (5 files · 761 lines · 13 tests)

| Function / Type | Purpose |
|---|---|
| `LlmEngine::{from_config,query,query_stream,config,backend_label}` | Cloud (Groq/OpenRouter/Google) or candle GGUF local backend |
| `factory::{default_config,config_from_env,from_config,...}` | Config resolution; `CloudEngine`, `LocalEngine`, `LocalModelKind` |
| `LlmStreamSink` | tokio mpsc stream of deltas |
| `extract_stream_delta(payload, google_shape)` | OpenAI + Google SSE delta parsing |
| `download_default_model(kind)/detect_local_models()` | hf-hub download + local `.gguf` scan |

### 8.2 `aios-builder` (5 files · 598 lines · 23 tests)

| Function / Type | Purpose |
|---|---|
| `EasyLangParser::parse(text,name)` | Line DSL (`spawn/timer/load/unload/kill/query/compact/status`) → `Workflow` |
| `WorkflowCompiler::{generate_wat,compile_to_wasm}` | WAT→WASM module (`init/start/step_N` exports) |
| `AutoManifestGenerator::{from_wasm_binary,from_workflow_intents}` | Capability inference + JSON manifest |

### 8.3 `aios-bridge` — API gateway (6 files · ~2.7k lines · 25 unit + 24 integration tests)

`BridgeContext` holds shared subsystem handles: health, watchdog/processes/blocks, store, browser, sys_control, telemetry, auth, live_update, ipc_bus and an `LlmEngine`. `require_auth` middleware is default-deny for `/api/` and `/ws/`; `start_server_with_shutdown()` owns the axum server. Passwords use PBKDF2-HMAC-SHA256 with constant-time checks and 5-attempt/5-minute lockout; registration is opt-in (`with_signup(true)` / `AIOS_BRIDGE_ALLOW_SIGNUP=true`), CORS is fail-closed. `validate_credentials()` is shared by register/login (3–64-byte usernames, 12–1024-byte passwords, checked before hashing) and `failed_logins` is pruned/capped at 10,000 entries. DTO validation caps prompts, workflow batches, `max_tokens` and WASM uploads; `block_binary_path()` confines block binaries to canonicalized paths under `AIOS_BLOCKS_DIR`; poisoned locks are recovered via `lock_or_recover()`. Direct `reqwest` clients outside the browser use operator/env endpoints or fixed defaults; bridge DTOs do not accept URLs except `BrowseRequest.url`, and `browse_handler` uses `BrowserConfig::for_untrusted()` so the native headless fallback and embedded scripts stay disabled.

| Method Path | Handler |
|---|---|
| GET `/api/v1/health` | Health/version/uptime |
| POST `/api/v1/auth/register` · `/api/v1/auth/login` · GET `/api/v1/auth/me` | Registration (opt-in), bearer login (401/429 on failure/lockout), current user |
| GET `/api/v1/system/status` · GET `/api/v1/sys/status` | Watchdog/processes/blocks/RAM; compact sys status |
| GET `/api/v1/sys/wifi/scan` · POST `/api/v1/sys/wifi/connect` · POST `/api/v1/sys/layout` | Wi-Fi scan/connect and EN/RU layout control |
| POST `/api/v1/intent` | NL intent execute (LLM fallback, ACL checks, prompt limit) |
| POST `/api/v1/workflow` | Batch prompt sequence (batch and prompt limits) |
| POST `/api/v1/llm/query` | Direct LLM query (prompt/system/token/temperature limits) |
| POST `/api/v1/browse` · `/api/v1/search` | Browser/search proxies |
| GET `/api/v1/store/index` · POST `/api/v1/store/register` · POST `/api/v1/store/publish` | Store service (publish verifies SHA-256 + optional Ed25519, bounded base64/WASM size) |
| GET `/index.json` · `/store/index.json` · `/blocks/{name}.wasm` · `/store/blocks/{name}.wasm` | Update-service catalog/download |
| GET `/api/v1/metrics` · `/api/v1/traces` · POST `/api/v1/crash-report` | Prometheus text / spans / crash report |
| WS `/ws/telemetry` | 100 ms RAM/process push; bearer token required on every upgrade |

Core types: `BridgeContext` (builders `with_blocks_dir`, `with_signup`, `with_auth`), `IntentParser` (RU/EN keywords + `parse_with_llm_fallback`), `UserIntent`, `AuthStore`, ~30 DTOs, `BridgeError` (incl. `Unauthorized` / `TooManyRequests`), `block_binary_path()`, `lock_or_recover()`, DTO validators.

---

## 9. Interfaces & Binaries

| Binary / Crate | Purpose | Notes |
|---|---|---|
| `aios` (kernel TUI, 8 files · ~5.8k lines · 56 tests) | Unified system binary | Tabs: System&HW / Blocks&Svc / AI Console / Studio Bridge / Network&Store / Web / Shell; `--safe-mode`, `--daemon`; hotkeys `b/B`, `n`, `K`, `l/L`, `W` (GUI notice), `F10` (re-probe HW); cluster shell commands |
| `aios-tui` (10 files · ~6.1k lines · 81 tests) | Standalone dashboard | `fetch/search/open`, `net get/set/reset`, full `store` family, watchdog shell fallback |
| `aios-gui` (22 files · ~5.7k lines · 34 tests) | egui desktop shell | Start menu, windows, taskbar, dark theme, voice/program launcher (`desktop.rs`, `voice.rs`); legacy reference tooling since v2.81 |
| `aios-daemon` (`aiosd`, 1 file · 169 lines) | Headless Docker/server mode | Boot blocks, heartbeat thread, periodic redb persistence |
| `aios-studio` (SPA) | Web dashboard served by bridge | Command palette, WS telemetry chart, security center |
| `aios-init` (standalone, musl static) | PID 1 for initramfs | VFS mounts, block supervisor (3 restarts), zombie reaping, rescue shell |
| Native UEFI image (`aios-image` → `aios-install`) | UEFI/GPT/ESP disk image | `aios-sbctl` signs `BOOTX64.EFI`; QEMU/OVMF verified |

`aios-webview` is now a compatibility helper for opening external browser windows; the GUI itself is driven by `aios-gui`.

## 10. Bare-Metal Microkernel Track

`aios-kernel` (`no_std`, `x86_64-unknown-none`, nightly; 39 files · ~26.5k non-blank lines) boots via Limine (BIOS + UEFI). `aios-kernel-run` builds the hybrid Limine ISO/USB image (dual kernel, `v<major>.<minor>+` current + rollback labels) and runs QEMU smoke tests. Since v2.84.0 the displayed release has one source of truth: `aios-kernel/src/version.rs` defines `SEMVER = env!("CARGO_PKG_VERSION")` and `KERNEL = concat!("AIOS kernel v", ...)`, with `2.84.0` in `Cargo.toml`; boot banner/config line, panic/fatal reports, TUI status/About/`ver`, GUI About/titles/Files preview all read those constants.

| Milestone | Status | Content |
|---|---|---|
| M0 (v2.26.0) | ✅ | QEMU boot, serial COM1 + VGA console, physical-memory mapping |
| M1 (v2.27.0) | ✅ | GDT/TSS, 256-entry IDT, PIC remap, PIT 100 Hz, PS/2 keyboard |
| M2 (v2.28.0) | ✅ | Page-table walker, map/unmap + frame allocator, 2 MiB heap |
| M3 (v2.29.0) | ✅ | PIT-tick round-robin preemption, frame-copy context switch, ring-0 worker + ring-3 programs |
| M4 (v2.29.0) | ✅ | IPC mailboxes behind `int 0x80`; header mirrors `aios_core::ipc_protocol` |
| M5 (v2.79.0) | ✅ | Limine handoff + GOP framebuffer, signed-loader path (`aios-sbctl`) |
| M6 (v2.81–2.83) | ✅ | Native desktop GUI, Start menu, in-kernel apps (Store, Calculator, Files, Editor, Terminal) |
| M7 (v2.84.0) | ✅ | NIC boot line + DHCP DORA, fault/recovery syscalls, `ring3-demo` |

> Post-audit hardening (v2.84.0, 2026-11-26):
> - `config.rs` adds `KeyPolicy { Effective, Informational }`: only `blocks.<name>.rights` / `blocks.<name>.fuel` are Effective; `system.*`, `boot.*`, `net.*` and unknown keys are Informational — validated and logged with their source, not applied as runtime knobs. Templates in `boot/assets/aios/config/{system,net,blocks}.conf` have matching copies under `boot/out/assets/aios/config/` (identical SHA-256).
> - `memory::translate_user(virt, len, write)` is the only `int 0x80` user-buffer gate (higher-half, unmapped/non-USER, read-only writes, null and `addr + len` wrap are rejected); `user_read`/`user_write` return `Option`, `write_user_string` is overflow-safe, `SYS_GUI CREATE` request/buffer/title and `draw_client()` are revalidated, invalid socket buffers return `u64::MAX`.
> - `scripts/flash-usb.ps1`: elevation with redirected output, volume lock/dismount via `DeviceIoControl`, `FileMode.Open`/`FileShare.ReadWrite`, 512-byte sector-aligned padded writes, `Flush($true)`, bounded `Update-Disk` + `pnputil /rescan-disks`, bounded disk-ready wait (no `ERROR_NOT_READY` hang), full-image SHA-256 read-back and MBR `55 AA` check.
> - `aios-kernel-run` parses the kernel version from `[package]` in `Cargo.toml`, stages main + fallback ELFs, writes a two-entry Limine menu (main `v2.84+`, fallback default `v2.79`; `AIOS_FALLBACK_VERSION` override), deletes stale ISOs before rebuild and checks MBR `55 AA` + ISO9660 `CD001` on ISO/USB. Clean clones must supply a real fallback ELF via `AIOS_FALLBACK_KERNEL` (default local ignored `out/aios-kernel-v279.elf`); no fake ELF is committed.
> - Kernel GUI text uses `console::TEXT_ADVANCE = GLYPH_W + SCALE` as the single glyph step; `tui::set_text_clip` / `clear_text_clip` and `gui::set_surface_clip` bound every `draw_text` to one surface, while `fit_label` / `short_label` ellipsize task bar and Start labels. Five `#[cfg(test)]` layout tests live in `aios-kernel/src/tui.rs` (`glyph_layout_tests`) and `aios-kernel/src/gui.rs` (`layout_tests`).
> - `interrupts::fatal_fmt` renders ring-0 `#PF` diagnostics (`addr/ip/err/rsp`, classified by `pf_kind`) on the fatal screen; five `#[cfg(test)]` error-code classification tests live in `aios-kernel/src/interrupts.rs`.

Modules: `main`, `boot`, `gdt`, `idt`, `interrupts`, `memory` (`USER_SPACE_LIMIT`, `user_range`, `translate_user`, `walk_user_leaf`), `heap`, `framebuffer`, `font8x8`, `psf`, `serial`, `port`, `syscalls` (mailboxes + `int 0x80`; `user_read`, `user_write`, `write_user_string`), `sched`, `user`, `gui`, `console`, `console_backend`, `acpi`, `ahci`, `blocks`, `config` (`KeyPolicy`, `policy`, `validate_informational`, `Summary.invalid_values`), `ec`, `fault`, `lapic`, `lid`, `net`, `nic`, `nvme`, `pci`, `ps2`, `sock`, `thermal`, `vector`, `version` (`SEMVER`, `KERNEL`), `wifi`, `xhci`, `build.rs` (asm stubs).

Host-side helpers named by the post-audit bullets: `aios-kernel-run` — `kernel_version`, `parse_package_version`, `release_train`, `fallback_kernel`, `fallback_version`, `stage`, `menu_text`, `create_iso`, `create_usb_image`, `verify_hybrid_image`; `scripts/flash-usb.ps1` — `Lock-And-DismountVolumes`, `Open-RawDisk`, `Invoke-BoundedRecovery`, `Wait-ForDiskReady`.

## 11. Integration Tests (root `tests/`, 15 files · 194 tests)

| File | Tests | Coverage |
|---|---|---|
| `integration_test.rs` | 30 | Full lifecycle, IPC speed, concurrent spawns, hot-swap+IPC |
| `bridge_tests.rs` | 24 | EN/RU intent parsing |
| `chaos_test.rs` | 18 | Corrupted packets, bus overflow, crash loops |
| `real_file_io.rs` | 12 | Snapshot/COW persistence on real disk |
| `real_network.rs` | 11 | Real TCP listen/accept/multi-client |
| `stress_test.rs` | 11 | ×1000 spawn, RT ×500, registry ×500 (dual thresholds) |
| `real_threads.rs` | 10 | Real threads: terminate/suspend/resume |
| `real_wasm.rs` | 8 | Wasmtime end-to-end, multi-block isolation |
| `browser_search_tests.rs` | 8 | HTML parser and untrusted browser config |
| `full_lifecycle.rs` | 7 | Boot→deploy→swap→watchdog→shutdown |
| `real_hot_swap.rs` | 7 | WASM version-change hot swap |
| `e2e_pipeline_test.rs` | 6 | HW→tier→LLM intent→EasyLang→WASM chain; bridge started with `with_signup(true)` |
| `fuzz_test.rs` | 6 | Randomized packet fuzzing |
| `sys_control_tests.rs` | 32 | Wi-Fi sim, DHCP wire format, layout switching, thermal cycles, keyring lifecycle |
| `stress_fault_tolerance.rs` | 5 | 50 parallel WASM blocks, crash storms |

## 12. Codebase Statistics (v2.84.0 audit snapshot, 2026-11-26)

- **358 `.rs` files**, **~125.5k non-blank lines** (~136.9k with blank lines): **41 workspace members + 13 standalone crates**.
- **1,824 tests passed in 100 suites** (unit + integration + doc-tests) by `cargo test --workspace`; 2,011 test attributes counted (`#[test]` 1,941 + `#[tokio::test]` 70) across 232 files.
- `cargo fmt --all -- --check`: clean. `cargo clippy --workspace --all-targets -- -D warnings`: **0 warnings**.
- Post-audit hardening map (2026-11-27): new `aios-core/src/secrets.rs` and
  `scripts/check_unsafe_safety.py`; `aios-bridge/src/auth.rs` (PBKDF2, lockout),
  `dto.rs` validators, `server.rs::block_binary_path`/`lock_or_recover`/`with_auth`;
  `aios-browser/src/network.rs` SSRF policy with DNS pinning via
  `resolve_to_addrs`; 201 poisoned-lock recoveries in 21
  production files; CI step `Require SAFETY comments on new unsafe blocks`.
  Post-hardening `cargo test --workspace`: **1,860 tests passed / 0 failed in 100 suites**;
  `scripts/check_unsafe_safety.py HEAD~1` reports no undocumented new unsafe blocks.
- Standalone/tooling gates: `aios-kernel` check/clippy (default and `--features limine`), `aios-kernel-run` tests 2/2, PowerShell parser check. A fresh hybrid ISO/IMG rebuild was **not** run here: `xorriso`/Limine binaries are missing and `iso/` is empty — only marker/check logic and existing image inspection were used.
- The five `aios-kernel` GUI text-layout tests are source-level `#[cfg(test)]` checks for the freestanding binary: they are not included in the host `cargo test --workspace` count above because the bare-metal target has no `test` crate. Runtime verification used a release ELF patched into ISO LBA 32 of a copied image (`aios-kernel-usb-limine-v284-gui.img`): Legacy BIOS QEMU reached `Limine handoff` / GOP `1280x800` / DHCP bound, opened App Store, Network and hardware windows with `F4`/`F5`/`F9`, and a screendump showed no text run wider than the 16 px glyph cell. The image was then written to and fully read back from Disk 3 (`\\.\PhysicalDrive3`, serial `EE05DB505AE9`) with matching SHA-256, LBA 32 ELF hash, `AIOS_KER.;1` record size and MBR `55 AA` (`boot/out/flash-gui-disk3.log`).

Top crates by non-blank lines: `aios-kernel` 26.5k · `boot/` 13.0k · `aios-console` 9.6k · `aios-tui` 6.1k · `aios-gui` 5.7k · `aios` 5.8k · `aios-autohal` 4.8k · `tests/` 4.3k · `aios-browser` 4.3k.
