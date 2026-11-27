# Схема программы и карта функций AIOS

> Версия: v2.84.0 — Дата: 26.11.2026 (обновление после аудита)
> Сопутствующие документы: `docs/AUDIT.ru.md` (полный аудит), `docs/ARCHITECTURE.ru.md` (глубокая архитектура), `docs/INTERFACE.ru.md` (руководство по интерфейсу).
> Двуязычная пара: `docs/SCHEME.md` (английский). При обновлении карты функций обновляйте оба файла.
> Этот документ — **карта уровня вызовов**: каждый крейт, его модули и ключевые публичные функции.

## 1. Общая схема системы (слои)

```
                    ПОЛЬЗОВАТЕЛЬСКИЕ ИНТЕРФЕЙСЫ
  aios (kernel TUI) │ aios-tui │ aios-gui (egui desktop shell) │ aios-studio (SPA)
                              │ aiosd (aios-daemon, headless)
                              │ aios-shell (POSIX-подобный адаптер shell)
─────────────────────────────────────────────────────────────────────────
        ШЛЮЗ / СЛОЙ КОМАНД                   │            ЯДРО ИИ
  aios-bridge — axum HTTP/WS, авторизация,  │   aios-llm — облако (Groq/OpenRouter/
  интенты RU/EN + LLM-fallback, полномочия, │   Google) + локальный GGUF (candle),
  API магазина и sys-control                │   стриминг; aios-builder —
  aios-console — единое ядро команд          │   EasyLang → WASM-воркфлоу
  (CommandSpec, dispatch, Host/Kernel
  backends, job control, net_text)
─────────────────────────────────────────────────────────────────────────
                          СИСТЕМНЫЕ СЛУЖБЫ
  aios-store / aios-updater (слоты A/B, Ed25519) │ aios-cluster (мультиузловой)
  aios-autohal (+ класс NIC, отпечатки, hotplug) │ aios-vfs + aios-fm (файлы)
  aios-browser / aios-search / aios-webview      │ aios-net-config
  aios-sys-control (Wi-Fi, DHCP, раскладка,      │ aios-telemetry / aios-debug
  питание, хранилище секретов)
─────────────────────────────────────────────────────────────────────────
               ПОДСИСТЕМЫ ЯДРА (экосистема StatefulBlock)
  aios-block-mgr │ aios-process-mgr (+ fault/recovery) │ aios-live-update │ aios-watchdog
  aios-security  │ aios-context │ aios-wasm │ aios-net │ aios-exec-compat
  aios-core::fault (FaultInfo/FaultKind/RecoveryAction)
─────────────────────────────────────────────────────────────────────────
                              ФУНДАМЕНТ
  aios-core (типы/IpcPacket/крипто/fault/ФС/runtime) │ aios-ipc (шина/канал/ring)
  aios-hal (детект/тиры) │ aios-ringbuf · aios-compress
  aios-persistence · aios-optim │ защита HW: mpk/iommu/tee
═════════════════════════════════════════════════════════════════════════
  BARE-METAL-ВЕТКА — 13 отдельных крейтов, вне воркспейса:
  aios-init │ aios-kernel │ aios-kernel-run
  boot/aios-boot │ boot/aios-bootinfo │ boot/aios-bootstore │ boot/aios-config
  boot/aios-easylang │ boot/aios-image │ boot/aios-install │ boot/aios-sbctl
  boot/aios-storectl │ boot/aios-wasmhost
  aios-boot → bootinfo → kernel → image/install; sbctl подписывает загрузчик;
  aios-kernel-run собирает загрузочный ISO Limine (BIOS + UEFI) с двумя
  записями ядра (текущее + откат); ISO и USB IMG проверяются по маркерам.
```

Воркспейс: **41 участник** (`Cargo.toml:52-83`), плюс 13 перечисленных standalone-крейтов.
`aios-studio` — не-Rust SPA, раздаётся `aios-bridge` через `ServeDir`.

## 2. Порядок загрузки (бинарник `aios`)

```
main(--daemon|--safe-mode?)
└─ orchestrator::initialize()
    ├─ hw_probe::probe()                    HwProfile (CPU/RAM/GPU, sysinfo + HAL)
    ├─ Scheduler::new(ram_mb)               aging 5 с, slice 100 мс, макс 5 рестартов,
    │                                       колбэки memory-pressure, RT-дедлайны
    ├─ BlockRegistry                        здесь регистрируются заглушки ipc_bus + scheduler;
    │                                       blocks::register_base_blocks() создаёт
    │                                       hal / browser (пропуск при RAM < 4096 МиБ) /
    │                                       net_settings и блоки из фабрик
    ├─ blocks::load_disk_blocks(...)        сторонние *.wasm/*.bin (пропуск в --safe-mode)
    ├─ BlockExecutor + MessageRouter        роутер регистрирует все base/disk-блоки
    ├─ AccessControlLayer                   токены полномочий
    ├─ Watchdog::new(...)                   раннер подключает console_control; поток
    │                                       сердцебиений не стартует внутри initialize()
    ├─ LlmEngine                            отложенная инициализация в фоне; bridge создаёт
    │                                       собственный экземпляр
    ├─ DistributedScheduler                 опциональный узел кластера + SchedulerProcessExecutor
    └─ BridgeRuntime::spawn_server()        HTTP/WS-шлюз на AIOS_BRIDGE_PORT (кроме safe mode)

  Старт TUI (aios/src/tui/app_state.rs, не часть initialize()):
    ├─ HostBackend / seams единой консоли
    └─ AutohalEngine + HotplugMonitor через init_hw_engine() (отключены в safe mode)
```

## 3. Поток данных IPC

```
UI/shell/intent ──► IpcPacket(CommandId, Payload)          [aios-core::ipc_protocol]
       │ bincode + SHA-256 (по payload) + packet_id + priority
       ▼
SharedIpcBus ──── ограниченная очередь: приоритетная вставка, дедупликация,
       │            backpressure (Reject | DropOldest), метрики [aios-ipc::bus]
       ├─ opt-in: RingBufferTransport::send_via_ring() для payload ≥ 4 КиБ
       │          (не автоматический обход шины)    [aios-ipc::ring_transport]
       ▼
MessageRouter.dispatch() ── handler(block_id)             [aios-block-mgr::router]
       ▼
StatefulBlock::handle_message() → IpcPacket::response_ok/err [aios-core::block]
       │
       ├─ hot-swap: bus.freeze() → Snapshot → swap → unfreeze/reroute
       │                                              [aios-live-update::state_transfer]
       └─ fault: FaultReport 0x0070 / RecoveryDecision 0x0071
                                              [aios-core::fault, aios-process-mgr]
```

---

## 4. Крейты фундамента

### 4.1 `aios-core` — типы, протокол, крипто, fault, ФС, секреты (11 файлов · ~1,4 тыс. строк · 39 тестов)

Модули: `block`, `crypto`, `error`, `fault`, `filesystem`, `ipc_protocol`, `runtime`, `secrets`.

| Функция / Тип | Назначение |
|---|---|
| трейт `StatefulBlock` | `handle_message`, `extract_state`/`restore_state`, `health_check` |
| `IpcPacket::new(...)` / `with_priority(p)` | Авто packet-id и контрольная сумма (SHA-256 по байтам payload) |
| `IpcPacket::serialize()/deserialize()` | bincode-сериализация туда-обратно |
| `IpcPacket::verify_checksum()` | Проверка подмены payload |
| `IpcPacket::response_ok()/response_err()` | Ответы, связанные с id запроса (также определён DTO `Response`) |
| `Payload::to_bytes()/is_empty()` | Кодирование полезной нагрузки |
| `crypto::compute_sha256()/verify_sha256()` | Целостность (hex + байты) |
| `fault::{FaultInfo,FaultKind,RecoveryAction}` | Контракт сообщений о сбоях для syscall'ов планировщика |
| `FileSystem::local()/virtual_fs()/overlay()` | Бэкенды ФС с пресетами прав |
| `FileSystem::{read,write,list}_{local,virtual}` | Операции с проверкой прав |
| `runtime::block_on_future(fut)` | Безопасный sync→async мост |
| `secrets::{random_secret,data_dir,env_name,resolve_secret,resolve_secret_in,secret_or_ephemeral}` | Общий resolver секретов: `AIOS_<NAME>_SECRET` → `<data_dir>/secrets/<name>.key` → CSPRNG; никогда не выводится из пути |

### 4.2 `aios-ipc` — транспорт (5 файлов · 806 строк · 29 тестов)

Модули: `bus`, `channel`, `ring_transport`.

| Функция / Тип | Назначение |
|---|---|
| `IpcBus::new(max_queue)` (+`with_backpressure`, `with_dedup`) | Конструирование ограниченной очереди |
| `IpcBus::send/send_priority/receive/peek` | Отправка (с политикой) / приём |
| `IpcBus::freeze()/unfreeze(pkts)/reroute(old,new)` | Поддержка переноса состояния при hot-swap |
| `IpcBus::metrics()/reset_metrics()` | sent/dropped/dedup/peak-depth/latency |
| `SharedIpcBus` | Клонируемая обёртка `Arc<Mutex<IpcBus>>` |
| `ipc::channel()` → `(IpcSender, IpcReceiver)` | Пара mpsc с отображением ошибок в `Result` |
| `RingBufferTransport::send_via_ring(&pkt)` | Opt-in путь вне очереди для больших payload |
| `RingBufferTransport::try_receive_from_ring/ring_usage/active_rings` | Доступ к ring-буферам |

### 4.3 `aios-hal` — аппаратная абстракция (3 файла · ~1,9 тыс. строк · 36 тестов)

| Функция / Тип | Назначение |
|---|---|
| `HardwareProfile::detect()` | Пробы CPUID, памяти, nvidia-smi/ROCm/NPU, PCI/storage/USB/TB |
| `AiTier::from_profile(&profile)` | Tier1 локальная LLM / Tier2 квантованная SLM / Tier3 эвристика |
| `AiTier::description()/max_model_size_gb()/recommended_batch_size()` | Подсказки по ёмкости |
| Мок-фабрики: `mock_legacy/modern/intel_meteor_lake/qualcomm_x_elite/nvidia...` | Профили для тестов без привязки к железу |
| `HalBlock::new(id)/with_profile(id,p)/profile()` | `StatefulBlock`, публикующий профиль |

---

## 5. Подсистемы ядра

### 5.1 `aios-block-mgr` — реестр/фабрика/загрузчик/маршрутизатор (9 файлов · ~2,3 тыс. строк · 83 теста)

| Функция / Тип | Назначение |
|---|---|
| `BlockRegistry::register_block(name,ver,binary)` | Выдача `BlockId`, хэш бинарника (SHA-256) |
| `BlockRegistry::{activate,unload,update_state}` | Жизненный цикл (`Unloaded→Loaded→Active→Frozen/Error`) |
| `BlockRegistry::assign_capabilities/check_capability` | Токен-ACL на блок |
| `BlockRegistry::boot_discover(root)/load_from_path(dir)` | Дисковое обнаружение `*_v*.bin|.wasm` |
| `BlockRegistry::topology()/topology_with_state()` | Перечисление манифестов для ответов |
| `BlockFactory` / `register_base_blocks` | Создаёт блоки hal/browser/net_settings при загрузке |
| `BlockLoader::validate_binary(binary,sha)` | Контроль целостности |
| `BlockLoader::load_from_binary(_with_capabilities)/load_from_directory` | Проверенная регистрация |
| `MessageRouter::register_handler/add_route/dispatch/unregister_handler` | Таблица обработчиков + редиректы |
| `DependencyGraph::add_dependency/load_order/unload_order` | Топосортировка Кана, поиск циклов |
| `SemanticVersion::parse/is_newer_than/bump_*/is_compatible_with` | Semver |
| `HotReloader::scan_and_reload(&mut registry)` | Инкрементальная перезагрузка из каталога наблюдения |

### 5.2 `aios-process-mgr` — планировщик, процессы, fault recovery (8 файлов · ~3,1 тыс. строк · 88 тестов)

| Функция / Тип | Назначение |
|---|---|
| `Scheduler::new(total_ram_mb)` + `with_time_slice/with_aging_threshold/with_max_restarts/with_memory_pressure_threshold` | Настройки; ноль слайса/порога ограничен 1 мс, взвешенный слайс насыщается |
| `Scheduler::spawn_process/spawn_real_process/spawn_child` | Приём под RAM-квоту; реальные потоки ОС |
| `Scheduler::schedule_next()/tick()/force_preempt()` | Priority RR + aging + RT-дедлайны |
| `Scheduler::{kill,suspend,resume}_process/set_priority` | Управление |
| `Scheduler::report_crash/should_restart` | Политика рестартов |
| `Scheduler::poll_faults/take_faults/resolve_fault` | Очередь `FaultInfo` + `RecoveryAction` (resume/rollback/terminate) |
| `SchedulingMode`, RT jitter log, memory-pressure callbacks | Адаптивная телеметрия планировщика |
| Группы/сессии: `create_group/create_session/kill_group/suspend_group/set_group_priority` | Массовое управление |
| `check_real_threads()/get_real_thread_state/set_cpu_affinity/get_cpu_affinity/list_real_threads` | Живость реальных потоков, привязка к ядрам |
| `ram_usage()/memory_pressure()/check_memory_pressure()` | Телеметрия квот |
| `handle_process_command(scheduler,&packet)` | IPC-фронтенд (Spawn/Kill/AdjustPriority) |
| `process_metrics::{bind_current_thread,record_*}` | TLS-счётчики на процесс |
| `cpu_affinity::{set_thread_affinity,available_cores,validate_cores}` | Привязка Win32/Linux |
| `PriorityInheritance::{acquire_lock,release_lock,...}` | Наследование приоритетов |

### 5.3 `aios-live-update` — атомарный hot-swap (5 файлов · ~1,2 тыс. строк · 23 теста)

| Функция / Тип | Назначение |
|---|---|
| `LiveUpdateEngine::perform_swap(block_id, old…, new…, queue, health_check)` | Своп из 7 шагов: freeze→SHA→health-гейт→откат-запас→restore очереди |
| `LiveUpdateEngine::rollback(block_id,queue)/expired_rollbacks()/swap_history()` | Управление окном отката |
| `StateTransferManager::{extract_state,restore_state,reroute_snapshot}` | Заморозка шины/снимок/перенаправление |
| `WasmLiveUpdateEngine::{deploy_block,swap_block,rollback_block,call_block_func}` | In-place своп Wasmtime с миграцией линейной памяти |
| `PersistedLiveUpdateEngine::{perform_swap,rollback,recover_from_crash,swap_history,has_rollback,storage_usage}` | Вариант на CoW-хранилище + журнале восстановления |

### 5.4 `aios-watchdog` — супервизор живости (5 файлов · ~1,6 тыс. строк · 58 тестов)

| Функция / Тип | Назначение |
|---|---|
| `Heartbeat::{verify,compute_hmac,age_ms}` | Сердцебиения, подписанные HMAC-SHA256 |
| `Watchdog::receive_heartbeat/check_timeout/force_safe_mode/escalate_actions/reset` | Пропуски → ступенчатые действия (warn→suspend→kill→dump→safe mode) |
| `WatchdogAction::{severity,is_terminal}` | Порядок Restart/ForceSafeMode/Terminate |
| `SafeModeShell::{parse_command,execute,orchestrator_restarts}` | Спасательная оболочка (ps/kill/load/status/logs/restart) |
| `WatchdogRunner::{start,stop,pop_actions,from_watchdog,reset}` | Обёртка фонового потока |

### 5.5 `aios-security` — zero-trust (5 файлов · 763 строки · 31 тест)

| Функция / Тип | Назначение |
|---|---|
| enum `Capability` | `CAP_NET_BIND/CONNECT`, `CAP_FS_READ/WRITE`, `CAP_HW_ACCESS`, `CAP_MEM_ALLOC`, `CAP_SCHED_MODIFY`… |
| `AccessControlLayer::{issue_token(_with_ttl),check_permission,revoke_token,clean_expired}` | Выдача/проверка/журнал нарушений |
| `CapabilityToken::{has_capability,is_expired,verify,compute_signature}` | Токены с TTL и HMAC-подписью |
| `Sandbox::{start,check_syscall,allocate_memory,terminate,from_token}` | Белый список syscall'ов + лимиты памяти |
| `HardwareSecurityBridge::{assign_mpk_protection,assign_tee_protection,assign_iommu_protection,validate_hardware_access}` | Единое отображение MPK/TEE/IOMMU |

### 5.6 `aios-context` — хранилище контекста (7 файлов · 1 092 строки · 36 тестов)

| Функция / Тип | Назначение |
|---|---|
| `TelemetryStore::{query_metric,query_range,query_by_block,average_value,peak_ram}` | Телеметрия в ring-буфере |
| `CompressedTelemetryStore::{record,compression_ratio}` | Холодные ZSTD-блоки + горячий порог |
| `WorkflowStore::{record,most_used,recently_used}` | Изученные профили использования |
| `StabilityStore::{best_version,record_crash,record_uptime}` | Оценки стабильности по версиям бинарников |
| `EmbeddedContextStore::{should_compact,compact,export_all,total_entries}` | Единый фасад |
| `PersistentStore::{save_all,load_telemetry,save_workflows,save_stability,compact}` | Постоянство на redb |

### 5.7 `aios-wasm` — песочница (5 файлов · ~2,0 тыс. строк · 62 теста)

| Функция / Тип | Назначение |
|---|---|
| `WasmSandbox::{compile_module,compile_any,compile_wat}` | Обёртка движка Wasmtime |
| `WasmBlock::{new/from_wat,instantiate,call_func}` | Инстанцированный блок |
| `WasmBlock::{extract_linear_memory,restore_linear_memory}` | Перенос состояния для hot-swap |
| `SandboxConfig` | Лимиты топлива/времени/памяти |
| `IsolationConfig/IsolationLevel` | Матрица shared-nothing-изоляции на блок |
| `WasiFilter::check_syscall` | Политика allow/deny/log для WASI-syscall'ов |
| `BlockExecutor::execute_block(s)` | Пакетное исполнение |

> Примечание: `Module::deserialize` используется как быстрый путь для уже скомпилированных модулей; доверять можно только проверенным байтам.

### 5.8 `aios-net` — сетевые блоки (6 файлов · ~1,7 тыс. строк · 57 тестов)

| Функция / Тип | Назначение |
|---|---|
| `RealTcpBlock::{start_listening,connect,send,receive,accept_pending,close_connection}` | Реальные сокеты, SO_REUSEADDR/KEEPALIVE/NODELAY, bind/connect через токены |
| `RealUdpBlock::{bind,send_to,receive_from,broadcast,port}` | Реальный UDP, включая SO_BROADCAST |
| `icmp::{ping,resolve_ipv4,build_echo_request,parse_echo_reply,internet_checksum}` + `IcmpReply` / `PingReport` | Реальный ICMPv4 echo в Windows (`IcmpSendEcho`) и Unix (ICMP-датграмный/raw-сокет), контрольная сумма и сопоставление identifier/sequence, отчёт RTT/TTL/потерь |
| `TcpBlock/UdpBlock` (+конфиги/состояния) | Симулируемые транспорты для детерминированных тестов |
| `inject_message/inject_packet` | Тестовые хуки |

### 5.9 `aios-exec-compat` — трансляция POSIX/Win32 (6 файлов · 1 947 строк · 89 тестов)

| Функция / Тип | Назначение |
|---|---|
| `ExecutableType::{from_bytes,from_extension,required_capabilities}` | Распознавание ELF/PE/shebang |
| `PosixSyscall/SyscallRequest/SyscallResponse` + `PosixTranslator` | Трансляция POSIX-syscall'ов |
| `Win32Api/Win32Request/Win32Response` + `Win32Translator` | Трансляция Win32 по ординалам |
| `CompatSandboxManager::{spawn_process,terminate_process,cleanup_terminated,total_memory_used}` | Процессы совместимости с лимитами ресурсов |
| `DependencyHealer::{scan_dependencies,resolve_missing,heal_dependencies,add_loaded_library}` | Поиск недостающих DLL/.so |

---

## 6. Защита на уровне железа и оптимизация

| Крейт | Непустые строки / тесты | Ключевой API |
|---|---|---|
| `aios-mpk` | 816 / 27 | Ключи Intel MPK, домены ARM DACR, регистр PKRU, программная изоляция как fallback |
| `aios-iommu` | 528 / 25 | DMA-домены, IOVA-таблицы страниц, карта устройств |
| `aios-tee` | 841 / 28 | Анклавы SGX/TrustZone/SEV, запечатывание, аттестация |
| `aios-ringbuf` | 653 / 16 (+proptest) | Lock-free SPSC ring: индексы производителя/потребителя, O(1) |
| `aios-compress` | 572 / 16 | Квантование FP8/INT4 + ZSTD, LRU-кэш декомпрессии |
| `aios-persistence` | 741 / 13 | `CopyOnWriteStorage` (теневая запись→fsync→rename), `RecoveryLog`, `SnapshotManager` |
| `aios-optim` | 964 / 39 | Процентили профайлера, flamegraph горячих путей, оптимизатор cache-line, авто-тюнер |

---

## 7. Системные службы

### 7.1 `aios-autohal` — автоподбор драйверов и NIC (13 файлов · ~4,8 тыс. строк · 79 тестов)

Конвейер: обнаружение → поиск в DriverStore → загрузка/адаптация → проверка+выдача полномочий+Wasmtime → кэш/регистрация.

| Функция / Тип | Назначение |
|---|---|
| `AutohalEngine::provision()/rescan()/remove_device()/provision_blocking()` | Владелец конвейера; replug кэшированного драйвера |
| `AutohalEngine::{record_failure,rollback_to_generic,set_cap_override,tokens,token,uninstall_driver}` | Самолечение, токены, удаление драйвера |
| `extract_fingerprints()/diff_fingerprints()` | Различия идентификаторов USB/PCI/BT/ACPI/NVMe |
| `nic::{NicClass,class_from_code,classify,is_nic}` + `SupportedHardware::for_class` + `HardwareFingerprint::nic_class` | Классификация PCI-сетевых карт для `driver nics` |
| `HotplugMonitor` (+`native.rs`) | udev netlink (Linux) / WM_DEVICECHANGE (Windows) + polling |
| `DriverStore/DriverIndex` | Постоянный кэш в `AIOS://store/drivers/`, счётчики отказов |
| `ui_tui::HardwareInspector` / `ui_gui::HardwarePanel::show` | Виджеты с паритетом TUI/GUI |

### 7.2 `aios-vfs` + `aios-fm` — виртуальная ФС и файловый менеджер (12 файлов · ~3,5 тыс. строк · 53 теста)

| Функция / Тип | Назначение |
|---|---|
| `VfsPath::parse()/to_uri()/join()` | Схемы `AIOS://` и `HOST://` |
| `AiosVfs` / `HostVfs` | Изолированные корни (`/system`,`/sandbox`,`/store`,`/config`) / хост-ФС за ACL-токенами |
| `PathScope::{parse,aios,host,contains}` / `PathAclRule` / `PathAcl::{grant,revoke,check,require,rules}` | Права `vfs:read` / `vfs:write` по путям; побеждает самая специфичная область, запрет на потомке перекрывает разрешение на родителе |
| `AiosVfs::with_path_acl()` / `path_acl()` | Общий ACL, проверяемый после `canonicalize_inside` при каждой операции чтения/записи |
| `VirtualFileSystem::resolve()/canonicalize_inside()` | Проверка ACL + каноникализация против traversal |
| `{copy_recursive,move_item,delete_item}` | Отменяемые асинхронные операции с прогрессом |
| `analyze_file()` | Эвристики «умного» предпросмотра |
| `FileManager::new(fs,acl)/send(Command)/snapshot()` | UI-независимый движок для обоих фронтендов |
| `ui_tui::{key_to_action,draw} / ui_gui::show` | Двухпанельные рендеры в стиле Волкова/Far |

### 7.3 `aios-cluster` — мультиузловое планирование (8 файлов · 2 961 строка · 33 теста)

| Функция / Тип | Назначение |
|---|---|
| `DistributedScheduler::{start(peers),shutdown,tick,set_executor}` | Один тип = координатор или воркер; executor подключается в рантайме |
| `DistributedScheduler::{spawn,kill,set_priority,get_state,migrate}` | Удалённые операции через RPC `ClusterMessage` |
| `TcpClusterTransport / InMemoryClusterTransport` | TCP bincode с длиной кадра / mpsc-реестр для тестов |
| трейт `ProcessExecutor` (+`SchedulerProcessExecutor`, mock) | Спавн/убийство/извлечение состояния на узле; все общие блокировки восстанавливаются после отравления |
| Репликация контрольных точек | Broadcast снапшотов, TTL-чистка, failover-восстановление |
| `ClusterConfig::{from_env,from_json}` | Загрузка через `AIOS_CLUSTER_*` |

### 7.4 Веб-стек

| Крейт | Непустые строки / тесты | Ключевой API |
|---|---|---|
| `aios-browser` | 4 339 / 114 | `BrowserEngine::{navigate,evaluate}`, `BrowserSession::{go,back,forward,reload,eval_js,normalize_url}`, встроенный boa JS с `MAX_SCRIPT_ARRAY_ITEMS`, `HtmlParser`, `Renderer`, `dom_to_html`, headless-фолбэк; SSRF-политика `NetworkClient` (только HTTP(S), проверка DNS/IP на каждом hop, привязка DNS через `resolve_to_addrs`, ручные редиректы, лимит тела), `BrowserConfig::{allow_private_targets,max_response_bytes,for_untrusted}` |
| `aios-search` | 438 / 7 | `SearchEngine::search` через DuckDuckGo/SearXNG/Brave + `SearchSummarizer` LLM TL;DR |
| `aios-webview` | 456 / 7 | `WebBrowser::{open,navigate,back,forward,close}` в фоновом потоке; постоянный профиль; `resolve_target()` |
| `aios-net-config` | 942 / 35 | `NetworkConfigStore::{load,load_or,save}`, `NetworkConfig::apply_updates`, валидаторы, `NetSettingsBlock` |
| `aios-sys-control` | 2 609 / 55 | `NetManager` (sim/host Wi-Fi, DHCP, `recheck/adapters/lease`), `host_net::AdapterState`, `LayoutManager` (EN/RU), `PowerManager` + гувернёр 80/70 °C, `KeyringVault` (AES-256-GCM redb, TEE) |

### 7.5 Магазин и обновления

| Крейт | Непустые строки / тесты | Ключевой API |
|---|---|---|
| `aios-store` | 2 455 / 63 | `StoreManager::{search,install,update,parse_source_spec,trust_source}`, `ManifestValidator` (SHA-256 + Ed25519 `verify_strict`), `BlockInstaller` (sidecar'ы, backup/rollback, `check_updates`) |
| `aios-updater` | 406 / 18 | `DualBootManager::{swap_slot,record_boot_success,should_rollback}` (слоты A/B), `RollbackManager::{take_snapshot,rollback_to,auto_rollback_if_needed}`, `HotSwapEngine` |

### 7.6 Наблюдаемость

| Крейт | Непустые строки / тесты | Ключевой API |
|---|---|---|
| `aios-telemetry` | 542 / 17 | `FlightRecorder::{record,dump_since,dump_by_kind}`, `MetricCollector::to_prometheus()`, `TraceContext::{begin_span,end_span,to_json}` |
| `aios-debug` | 322 / 10 | `CrashReporter::generate_report(kind,…,zero_knowledge)` (хэширование стека/редакция), `PanicHandler::install()` |

### 7.7 `aios-console` — единое ядро текстовой консоли (18 файлов · ~9,6 тыс. строк · 130 тестов)

| Функция / Тип | Назначение |
|---|---|
| `CommandSpec` / `SPECS` / registry команд | Единое описание словаря команд (паритет текста и ядра) |
| `CommandBackend` / `backend::dispatch()` | Независимый от бэкенда dispatch для TUI, bridge и демона |
| `HostBackend` / `KernelBackend` / `UnavailableBackend` | Хост-процессы, ядерная консоль и корректный фолбэк |
| `CommandOutput` | Структурированный вывод, общий для фронтендов |
| `JobControl` / `FileManagerControl` / `net_text` | Задачи, команды ФМ и совместимый с `no_std` разбор сетевых команд |
| `PingControl` / `PingReplyInfo` / `PingReportInfo` / `IcmpPingControl` | Реальный ICMP-echo адаптер и форма отчёта для `console ping`, подменяемый в тестах |
| `HostBackend::acl_command` / `ping_command` / `set_path_acl` / `set_ping_control` | Правила path ACL и вывод ICMP echo на хосте с общими/тестовыми точками внедрения |

### 7.8 `aios-shell` — адаптер shell (1 файл · ~1,8 тыс. строк · 12 тестов)

| Функция / Тип | Назначение |
|---|---|
| Префиксы консоли, POSIX-алиасы, `source`, history/env-адаптеры | Старые bare-shell команды, сведённые к `aios-console` |
| Адаптеры таблицы задач | Общий job control с единой консолью |
| Фолбэк `sh -c` | Последняя инстанция для ещё не перенесённых команд |

---

## 8. Слой ИИ

### 8.1 `aios-llm` (5 файлов · 761 строка · 13 тестов)

| Функция / Тип | Назначение |
|---|---|
| `LlmEngine::{from_config,query,query_stream,config,backend_label}` | Облако (Groq/OpenRouter/Google) или локальный бэкенд candle GGUF |
| `factory::{default_config,config_from_env,from_config,...}` | Разрешение конфигурации; `CloudEngine`, `LocalEngine`, `LocalModelKind` |
| `LlmStreamSink` | Поток дельт через tokio mpsc |
| `extract_stream_delta(payload, google_shape)` | Разбор SSE-дельт OpenAI + Google |
| `download_default_model(kind)/detect_local_models()` | Загрузка hf-hub + скан локальных `.gguf` |

### 8.2 `aios-builder` (5 файлов · 598 строк · 23 теста)

| Функция / Тип | Назначение |
|---|---|
| `EasyLangParser::parse(text,name)` | Построчный DSL (`spawn/timer/load/unload/kill/query/compact/status`) → `Workflow` |
| `WorkflowCompiler::{generate_wat,compile_to_wasm}` | WAT→WASM-модуль (экспорты `init/start/step_N`) |
| `AutoManifestGenerator::{from_wasm_binary,from_workflow_intents}` | Вывод полномочий + JSON-манифест |

### 8.3 `aios-bridge` — API-шлюз (6 файлов · ~2,7 тыс. строк · 25 unit + 24 integration-теста)

`BridgeContext` хранит общие дескрипторы подсистем: health, watchdog/процессы/блоки, магазин, браузер, sys_control, телеметрия, auth, live_update, ipc_bus и `LlmEngine`. middleware `require_auth` по умолчанию закрывает `/api/` и `/ws/`; `start_server_with_shutdown()` владеет axum-сервером. Пароли хешируются PBKDF2-HMAC-SHA256 с constant-time проверками и блокировкой после 5 попыток на 5 минут; регистрация включается явно (`with_signup(true)` / `AIOS_BRIDGE_ALLOW_SIGNUP=true`), CORS работает по fail-closed. `validate_credentials()` общий для register/login (имена 3–64 байта, пароли 12–1024 байта проверяются до хеширования), а `failed_logins` очищается и ограничен 10 000 записей. Валидация DTO ограничивает промпты, пакеты воркфлоу, `max_tokens` и загрузки WASM; `block_binary_path()` ограничивает бинарники блоков каноническими путями внутри `AIOS_BLOCKS_DIR`; отравленные блокировки восстанавливаются через `lock_or_recover()`. Прямые `reqwest`-клиенты вне браузера используют эндпоинты оператора/окружения или фиксированные значения; DTO моста не принимают URL, кроме `BrowseRequest.url`, а `browse_handler` применяет `BrowserConfig::for_untrusted()`, поэтому нативный headless-fallback и встроенные скрипты остаются отключёнными.

| Метод Путь | Обработчик |
|---|---|
| GET `/api/v1/health` | Здоровье/версия/uptime |
| POST `/api/v1/auth/register` · `/api/v1/auth/login` · GET `/api/v1/auth/me` | Регистрация (по включению), bearer-логин (401/429 при ошибке/блокировке), текущий пользователь |
| GET `/api/v1/system/status` · GET `/api/v1/sys/status` | Watchdog/процессы/блоки/RAM; компактный sys status |
| GET `/api/v1/sys/wifi/scan` · POST `/api/v1/sys/wifi/connect` · POST `/api/v1/sys/layout` | Сканирование/подключение Wi-Fi и раскладка EN/RU |
| POST `/api/v1/intent` | Исполнение NL-интента (LLM fallback, проверки ACL, лимит промпта) |
| POST `/api/v1/workflow` | Последовательность промптов (лимиты пакета и промптов) |
| POST `/api/v1/llm/query` | Прямой LLM-запрос (лимиты prompt/system/токенов/температуры) |
| POST `/api/v1/browse` · `/api/v1/search` | Прокси браузера/поиска |
| GET `/api/v1/store/index` · POST `/api/v1/store/register` · POST `/api/v1/store/publish` | Магазин (publish проверяет SHA-256 + опционально Ed25519, ограничения base64/WASM) |
| GET `/index.json` · `/store/index.json` · `/blocks/{name}.wasm` · `/store/blocks/{name}.wasm` | Каталог/загрузка службы обновлений |
| GET `/api/v1/metrics` · `/api/v1/traces` · POST `/api/v1/crash-report` | Prometheus / спаны / crash-отчёт |
| WS `/ws/telemetry` | Push RAM/процессов каждые 100 мс; bearer-токен обязателен при каждом повышении |

Ключевые типы: `BridgeContext` (билдеры `with_blocks_dir`, `with_signup`, `with_auth`), `IntentParser` (RU/EN-ключевые слова + `parse_with_llm_fallback`), `UserIntent`, `AuthStore`, ~30 DTO, `BridgeError` (вкл. `Unauthorized` / `TooManyRequests`), `block_binary_path()`, `lock_or_recover()`, валидаторы DTO.

---

## 9. Интерфейсы и бинарники

| Бинарник / крейт | Назначение | Примечания |
|---|---|---|
| `aios` (kernel TUI, 8 файлов · ~5,8 тыс. строк · 56 тестов) | Единый системный бинарник | Вкладки: System&HW / Blocks&Svc / AI Console / Studio Bridge / Network&Store / Web / Shell; `--safe-mode`, `--daemon`; хоткеи `b/B`, `n`, `K`, `l/L`, `W` (уведомление о GUI), `F10` (повторный probes HW); shell-команды кластера |
| `aios-tui` (10 файлов · ~6,1 тыс. строк · 81 тест) | Отдельная панель управления | `fetch/search/open`, `net get/set/reset`, полный набор `store`, watchdog-shell |
| `aios-gui` (22 файла · ~5,7 тыс. строк · 34 теста) | egui desktop shell | Start menu, окна, taskbar, тёмная тема, голосовой запуск программ (`desktop.rs`, `voice.rs`); с v2.81 — legacy reference tooling |
| `aios-daemon` (`aiosd`, 1 файл · 169 строк) | Headless Docker/сервер | Загрузочные блоки, поток сердцебиений, периодическая запись в redb |
| `aios-studio` (SPA) | Веб-панель через bridge | Командная палитра, WS-график телеметрии, центр безопасности |
| `aios-init` (отдельный, статический musl) | PID 1 для initramfs | Монтирование VFS, супервизор блоков (3 рестарта), зомби, спасательная оболочка |
| Нативный UEFI-образ (`aios-image` → `aios-install`) | Образ GPT/ESP | `aios-sbctl` подписывает `BOOTX64.EFI`; проверено в QEMU/OVMF |

`aios-webview` теперь — совместимый хелпер для открытия внешних окон браузера; сам GUI управляется через `aios-gui`.

## 10. Bare-metal-ветка микроядра

`aios-kernel` (`no_std`, `x86_64-unknown-none`, nightly; 39 файлов · ~26,5 тыс. непустых строк) загружается через Limine (BIOS + UEFI). `aios-kernel-run` собирает гибридный образ Limine ISO/USB (два ядра, метки `v<major>.<minor>+` для текущего и отката) и запускает QEMU smoke-тесты. С v2.84.0 отображаемая версия имеет единственный источник: `aios-kernel/src/version.rs` определяет `SEMVER = env!("CARGO_PKG_VERSION")` и `KERNEL = concat!("AIOS kernel v", ...)`, а `2.84.0` находится в `Cargo.toml`; boot-баннер/строка конфига, panic/fatal-отчёты, статус TUI/About/`ver`, заголовки GUI About/окон и превью Files читают эти константы.

| Веха | Статус | Содержание |
|---|---|---|
| M0 (v2.26.0) | ✅ | Загрузка в QEMU, serial COM1 + VGA, отображение физической памяти |
| M1 (v2.27.0) | ✅ | GDT/TSS, IDT на 256 вентилей, ремап PIC, PIT 100 Гц, клавиатура PS/2 |
| M2 (v2.28.0) | ✅ | Обход таблиц страниц, map/unmap + аллокатор кадров, куча 2 МиБ |
| M3 (v2.29.0) | ✅ | Вытеснение по тикам PIT, frame-copy переключение контекста, ring-0 worker + ring-3 программы |
| M4 (v2.29.0) | ✅ | IPC-почтовые ящики за `int 0x80`; заголовок зеркалит `aios_core::ipc_protocol` |
| M5 (v2.79.0) | ✅ | Limine handoff + GOP-фреймбуфер, путь подписанного загрузчика (`aios-sbctl`) |
| M6 (v2.81–2.83) | ✅ | Нативный desktop GUI, Start menu, в-ядерные приложения (Store, Calculator, Files, Editor, Terminal) |
| M7 (v2.84.0) | ✅ | NIC-строка при загрузке + DHCP DORA, fault/recovery syscalls, `ring3-demo` |

> Укрепление после аудита (v2.84.0, 26.11.2026):
> - `config.rs` добавляет `KeyPolicy { Effective, Informational }`: эффективны только `blocks.<name>.rights` / `blocks.<name>.fuel`; `system.*`, `boot.*`, `net.*` и неизвестные ключи информационные — проверяются и логируются с источником, но не применяются как рантайм-настройки. Шаблоны в `boot/assets/aios/config/{system,net,blocks}.conf` имеют соответствующие копии в `boot/out/assets/aios/config/` (идентичный SHA-256).
> - `memory::translate_user(virt, len, write)` — единственный шлюз пользовательских буферов для `int 0x80` (отвергаются upper-half, неотображённые/не-USER страницы, запись в read-only, null и переполнение `addr + len`); `user_read`/`user_write` возвращают `Option`, `write_user_string` устойчив к переполнению, запрос/буфер/заголовок `SYS_GUI CREATE` и `draw_client()` проверяются повторно, некорректные буферы сокета возвращают `u64::MAX`.
> - `scripts/flash-usb.ps1`: elevate с перенаправлением вывода, блокировка/размонтирование томов через `DeviceIoControl`, `FileMode.Open`/`FileShare.ReadWrite`, запись выровненных по 512 байтам дополненных блоков, `Flush($true)`, ограниченные `Update-Disk` + `pnputil /rescan-disks`, ожидание готовности диска без зависания на `ERROR_NOT_READY`, полный SHA-256 прочитанного образа и проверка MBR `55 AA`.
> - `aios-kernel-run` разбирает версию ядра из `[package]` в `Cargo.toml`, размещает основной и резервный ELF, пишет двухпунктовое меню Limine (основной `v2.84+`, резервный по умолчанию `v2.79`; переопределение через `AIOS_FALLBACK_VERSION`), удаляет устаревший ISO перед пересборкой и проверяет MBR `55 AA` + ISO9660 `CD001` в ISO/USB. В чистых клонах нужен настоящий резервный ELF через `AIOS_FALLBACK_KERNEL` (по умолчанию локальный игнорируемый `out/aios-kernel-v279.elf`); поддельный ELF в репозиторий не коммитится.
> - Текст GUI ядра использует `console::TEXT_ADVANCE = GLYPH_W + SCALE` как единый шаг глифа; `tui::set_text_clip` / `clear_text_clip` и `gui::set_surface_clip` ограничивают каждый `draw_text` одной поверхностью, а `fit_label` / `short_label` добавляют `...` к подписям таскбара и «Пуска». Пять тестов `#[cfg(test)]` лежат в `aios-kernel/src/tui.rs` (`glyph_layout_tests`) и `aios-kernel/src/gui.rs` (`layout_tests`).
> - `interrupts::fatal_fmt` рисует диагностику `#PF` из ring 0 (`addr/ip/err/rsp`, класс определяет `pf_kind`) на экране сбоя; пять тестов `#[cfg(test)]` классификации кодов ошибок лежат в `aios-kernel/src/interrupts.rs`.

Модули: `main`, `boot`, `gdt`, `idt`, `interrupts`, `memory` (`USER_SPACE_LIMIT`, `user_range`, `translate_user`, `walk_user_leaf`), `heap`, `framebuffer`, `font8x8`, `psf`, `serial`, `port`, `syscalls` (почтовые ящики + `int 0x80`; `user_read`, `user_write`, `write_user_string`), `sched`, `user`, `gui`, `console`, `console_backend`, `acpi`, `ahci`, `blocks`, `config` (`KeyPolicy`, `policy`, `validate_informational`, `Summary.invalid_values`), `ec`, `fault`, `lapic`, `lid`, `net`, `nic`, `nvme`, `pci`, `ps2`, `sock`, `thermal`, `vector`, `version` (`SEMVER`, `KERNEL`), `wifi`, `xhci`, `build.rs` (asm-заглушки).

Хост-хелперы из пунктов после аудита: `aios-kernel-run` — `kernel_version`, `parse_package_version`, `release_train`, `fallback_kernel`, `fallback_version`, `stage`, `menu_text`, `create_iso`, `create_usb_image`, `verify_hybrid_image`; `scripts/flash-usb.ps1` — `Lock-And-DismountVolumes`, `Open-RawDisk`, `Invoke-BoundedRecovery`, `Wait-ForDiskReady`.

## 11. Интеграционные тесты (корневой `tests/`, 15 файлов · 194 теста)

| Файл | Тесты | Покрытие |
|---|---|---|
| `integration_test.rs` | 30 | Полный жизненный цикл, скорость IPC, параллельные спавны, hot-swap+IPC |
| `bridge_tests.rs` | 24 | Разбор интентов EN/RU |
| `chaos_test.rs` | 18 | Повреждённые пакеты, переполнение шины, циклы сбоев |
| `real_file_io.rs` | 12 | Снапшоты/COW на реальном диске |
| `real_network.rs` | 11 | Реальные TCP listen/accept/мультиклиент |
| `stress_test.rs` | 11 | ×1000 спавнов, RT ×500, реестр ×500 (двойные пороги) |
| `real_threads.rs` | 10 | Реальные потоки: terminate/suspend/resume |
| `real_wasm.rs` | 8 | Wasmtime end-to-end, изоляция мультиблоков |
| `browser_search_tests.rs` | 8 | HTML-парсер и конфигурация untrusted-браузера |
| `full_lifecycle.rs` | 7 | Boot→deploy→swap→watchdog→shutdown |
| `real_hot_swap.rs` | 7 | Hot-swap WASM при смене версии |
| `e2e_pipeline_test.rs` | 6 | Цепочка HW→тир→LLM intent→EasyLang→WASM; мост поднят с `with_signup(true)` |
| `fuzz_test.rs` | 6 | Fuzzing случайных пакетов |
| `sys_control_tests.rs` | 32 | Wi-Fi-симуляция, wire-формат DHCP, раскладки, термоциклы, keyring |
| `stress_fault_tolerance.rs` | 5 | 50 параллельных WASM-блоков, штормы сбоев |

## 12. Статистика кодовой базы (срез аудита v2.84.0, 26.11.2026)

- **358 `.rs`-файлов**, **~125,5 тыс. непустых строк** (~136,9 тыс. вместе с пустыми): **41 участник воркспейса + 13 standalone-крейтов**.
- **1 824 теста пройдено в 100 наборах** (unit + integration + doc-tests) при `cargo test --workspace`; насчитано 2 011 атрибутов тестов (`#[test]` 1 941 + `#[tokio::test]` 70) в 232 файлах.
- `cargo fmt --all -- --check`: чисто. `cargo clippy --workspace --all-targets -- -D warnings`: **0 предупреждений**.
- Карта усилений после аудита (27.11.2026): новые `aios-core/src/secrets.rs` и
  `scripts/check_unsafe_safety.py`; `aios-bridge/src/auth.rs` (PBKDF2, блокировки),
  валидаторы `dto.rs`, `server.rs::{block_binary_path,lock_or_recover,with_auth}`;
  SSRF-политика `aios-browser/src/network.rs` с привязкой DNS через
  `resolve_to_addrs`; 201 восстановление отравленных
  блокировок в 21 production-файле; CI-шаг `Require SAFETY comments on new unsafe blocks`.
  После усилений `cargo test --workspace`: **1 865 тестов пройдено / 0 провалено в 100 наборах**;
  `scripts/check_unsafe_safety.py HEAD` не находит новых unsafe-блоков без комментария.
- Отдельные проверки: check/clippy `aios-kernel` (настройки по умолчанию и `--features limine`), тесты `aios-kernel-run` 2/2, проверка синтаксиса PowerShell. Свежая пересборка гибридного ISO/IMG здесь **не** выполнялась: бинарники `xorriso`/Limine отсутствуют, каталог `iso/` пуст — использовались только логика проверки маркеров и осмотр существующих образов.
- Пять тестов `aios-kernel` на компоновку текста — это исходные проверки `#[cfg(test)]` для freestanding-бинарика: они не входят в приведённый выше счёт `cargo test --workspace`, потому что в bare-metal-цели нет crates `test`. Проверка в рантайме выполнена на релизном ELF, записанном в LBA 32 ISO копии образа (`aios-kernel-usb-limine-v284-gui.img`): Legacy BIOS QEMU дошёл до `Limine handoff` / GOP `1280x800` / DHCP bound, клавишами `F4`/`F5`/`F9` открыл App Store, Network и окно оборудования, а screendump показал, что ни один текстовый участок не шире ячейки глифа 16 px. Затем образ записан и полностью прочитан назад с Disk 3 (`\\.\PhysicalDrive3`, серийник `EE05DB505AE9`): SHA-256, хеш ELF в LBA 32, размер записи `AIOS_KER.;1` и MBR `55 AA` совпали (`boot/out/flash-gui-disk3.log`).

Крупнейшие крейты по непустым строкам: `aios-kernel` 26,5 тыс. · `boot/` 13,0 тыс. · `aios-console` 9,6 тыс. · `aios-tui` 6,1 тыс. · `aios-gui` 5,7 тыс. · `aios` 5,8 тыс. · `aios-autohal` 4,8 тыс. · `tests/` 4,3 тыс. · `aios-browser` 4,3 тыс.

## 13. Голосовой конвейер (2026-11-27)

| Область | Файлы | Точки входа |
|---|---|---|
| Захват/STT | `aios-voice/src/capture.rs`, `stt.rs`, `pipeline.rs` | `AudioSource`, `MicrophoneSource`, `MockAudioSource`, `SttEngine`, `LocalWhisperEngine`, `CloudSttEngine`, `VoicePipeline` |
| Генерация | `aios-builder/src/script.rs`, `workflow.rs`, `compiler.rs`, `manifest.rs` | `ScriptLlm`, `LlmScriptLlm`, `MockScriptLlm`, `OfflineScriptLlm`, `ScriptGenerator`, `SystemCommand`, `WorkflowCompiler`, `AutoManifestGenerator` |
| Песочница | `aios-wasm/src/sandbox.rs` | `HostPolicy`, `instantiate_with_policy`, защищённые host-импорты `aios.net_check` / `browse` / `console_cmd` / `notify` |
| Мост | `aios-bridge/src/voice.rs`, `dto.rs`, `server.rs` | `VoiceRuntime`, `run_voice_worker`, обработчики голоса, `/ws/voice` |
| UI | `aios-studio/*`, `aios/src/tui/*` | Кнопка микрофона Studio + `Ctrl+Shift+M`; TUI `Ctrl+Shift+M` с маркерами `[LISTEN]/[STT]/[THINKING]/[EXEC]` |

Поток: `Idle -> Listening{level} -> Transcribing -> Transcribed{text} -> Thinking -> ScriptReady` либо `AwaitingConfirmation -> Executing -> Completed|Failed`. Рабочий поток моста реагирует на `Transcribed`, генерирует строгий EasyLang (только код, ограниченные повторы, офлайн-фолбэк), компилирует в WASM, выводит возможности через `AutoManifestGenerator`, проверяет их по токену сессии в `AccessControlLayer` и выполняет с соответствующим `HostPolicy`. Привилегированные скрипты ожидают `POST /api/v1/voice/execute-text` с `confirm:true`.

Эндпоинты: `POST /api/v1/voice/start`, `/stop`, `/transcribe` (WAV или s16le, максимум 10 МиБ), `/execute-text`, `GET /api/v1/voice/status`, `GET /ws/voice?token=...`. Все API-маршруты требуют bearer-авторизацию; WebSocket проверяют токен самостоятельно. `/ws/telemetry` дополнительно передаёт `voice_phase`.

Успешные скрипты сохраняются в `<AIOS_WORKFLOWS_DIR|workflows>` как `<stem>.easy`, `<stem>.wasm` и `<stem>.meta.json` с исходной фразой. Тестовое покрытие: `tests/voice_pipeline_test.rs` (мок аудио -> мок LLM -> выполнение WASM), а также модульные тесты `aios-voice` (11), `aios-builder` (29), `aios-wasm` (64) и `aios-bridge` (26).
