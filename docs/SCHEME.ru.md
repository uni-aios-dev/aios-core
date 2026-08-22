# Схема программы и карта функций AIOS

> Версия: v2.28.1 · Дата: 2026-08-22
> Сопутствующие документы: `docs/AUDIT.ru.md` (полный аудит), `docs/ARCHITECTURE.ru.md` (глубокая архитектура), `docs/INTERFACE.ru.md` (руководство по интерфейсу).
> Этот документ — **карта уровня вызовов**: каждый крейт, его модули и ключевые публичные функции.

## 1. Общая схема системы (слои)

```
                    ПОЛЬЗОВАТЕЛЬСКИЕ ИНТЕРФЕЙСЫ (7 вкладок, паритет TUI/GUI)
 aios (kernel TUI)  │  aios-tui  │  aios-gui (egui)  │  aios-studio (SPA)
                              │  aiosd (aios-daemon, headless)
───────────────────────────────────────────────────────────────────────
        ШЛЮЗ                         │            ЯДРО ИИ
 aios-bridge — axum HTTP/WS,         │   aios-llm — облако (Groq/OpenRouter/
 интенты RU/EN + LLM-fallback,       │   Google) + локальный GGUF (candle),
 проверка полномочий, store API      │   стриминг; aios-builder —
                                     │   EasyLang → WASM-воркфлоу
───────────────────────────────────────────────────────────────────────
                          СИСТЕМНЫЕ СЛУЖБЫ
 aios-store / aios-updater (слоты A/B, доверие Ed25519)  │ aios-cluster (мультиузловой)
 aios-autohal (автоподбор драйверов + hotplug)           │ aios-vfs + aios-fm (файлы)
 aios-browser / aios-search / aios-webview               │ aios-net-config
 aios-telemetry (flight recorder/метрики/трейсы)         │ aios-debug (crash-отчёты)
───────────────────────────────────────────────────────────────────────
              ПОДСИСТЕМЫ ЯДРА (экосистема StatefulBlock)
 aios-block-mgr │ aios-process-mgr │ aios-live-update │ aios-watchdog
 aios-security  │ aios-context     │ aios-wasm        │ aios-net │ exec-compat
───────────────────────────────────────────────────────────────────────
                             ФУНДАМЕНТ
 aios-core (типы/IpcPacket/крипто/ФС/runtime)  │  aios-ipc (шина/канал/ring)
 aios-hal (детект/тиры)                        │  aios-ringbuf · aios-compress
 aios-persistence · aios-optim                 │  защита HW: mpk/iommu/tee
═══════════════════════════════════════════════════════════════════════
 BARE-METAL-ВЕТКА (отдельно, вне воркспейса):
 live ISO → aios-init (статический musl PID 1) → aios-kernel (x86_64-unknown-none,
 вехи M0–M2 готовы; M3 вытеснение, M4 IPC — план) ← aios-kernel-run (QEMU)
```

## 2. Порядок загрузки (бинарник `aios`)

```
main(--daemon|--safe-mode?)
 └─ orchestrator::initialize()
     ├─ hw_probe::probe_system()          CPU/RAM/GPU через sysinfo+HAL, AiTier
     ├─ Scheduler::new(ram_mb)            aging 5с, slice 100мс, макс 5 рестартов
     ├─ BlockRegistry                     регистрация hal/ipc_bus/scheduler/browser/net_settings + зависимости
     ├─ boot_discover(AIOS_BLOCKS_DIR)    сторонние *.wasm/*.bin (пропуск в --safe-mode)
     ├─ MessageRouter                     обработчики BrowserBlock + NetSettingsBlock
     ├─ AccessControlLayer                токены полномочий
     ├─ Watchdog                          монитор сердцебиений (HMAC)
     ├─ LlmEngine                         AI-тир → облачный/локальный бэкенд
     ├─ (bridge, если не safe mode)       HTTP/WS-шлюз на AIOS_BRIDGE_PORT
     └─ AutohalEngine + HotplugMonitor    подбор драйверов, push-события hot-plug
```

## 3. Поток данных IPC

```
UI/shell/intent ──► IpcPacket(CommandId, Payload)          [aios-core::ipc_protocol]
      │ bincode + SHA-256 checksum + packet_id + priority
      ▼
SharedIpcBus ──── ограниченная очередь: приоритетная вставка, дедупликация,
      │            backpressure (Reject | DropOldest), метрики   [aios-ipc::bus]
      ├─ payload ≥ 4 KiB ⇒ RingBufferTransport (вне очереди)     [aios-ipc::ring_transport]
      ▼
MessageRouter.dispatch() ── handler(block_id)                  [aios-block-mgr::router]
      ▼
StatefulBlock::handle_message() → Response(ok|err)             [aios-core::block]
      │
      └─ путь hot-swap: bus.freeze() → Snapshot → swap → unfreeze/reroute
                                                       [aios-live-update::state_transfer]
```

---

## 4. Крейты фундамента

### 4.1 `aios-core` — типы, протокол, криптография, ФС (9 файлов · ~1.1 тыс. строк · 38 тестов)

Модули: `block` (идентификация/жизненный цикл/трейт), `crypto` (SHA-256), `error` (`AIOSException`, `Result<T>`), `filesystem` (local/virtual/overlay + права), `ipc_protocol` (формат сети), `runtime` (синхронно→асинхронный мост).

| Функция / Тип | Назначение |
|---|---|
| трейт `StatefulBlock` | Базовый интерфейс блока: `handle_message`, `extract_state`/`restore_state`, `health_check` |
| `IpcPacket::new(...)` / `with_priority(p)` | Сборка пакета с авто packet-id и контрольной суммой |
| `IpcPacket::serialize()/deserialize()` | bincode-сериализация туда-обратно |
| `IpcPacket::verify_checksum()` | Детекция подмены данных |
| `IpcPacket::response_ok()/response_err()` | Ответы, связанные с id запроса |
| `Payload::to_bytes()/is_empty()` | Кодирование полезной нагрузки |
| `crypto::compute_sha256()/verify_sha256()` | Целостность (hex + байты) |
| `FileSystem::local()/virtual_fs()/overlay()` | Бэкенды ФС с пресетами прав |
| `FileSystem::{read,write,list}_{local,virtual}` | Операции с проверкой прав |
| `runtime::block_on_future(fut)` | Безопасный sync→async мост (без паники вложенного runtime) |

### 4.2 `aios-ipc` — транспорт (5 файлов · ~0.8 тыс. строк · 29 тестов)

Модули: `bus`, `channel`, `ring_transport`.

| Функция / Тип | Назначение |
|---|---|
| `IpcBus::new(max_queue)` (+`with_backpressure`, `with_dedup`) | Конструирование ограниченной очереди |
| `IpcBus::send/send_priority/receive/peek` | Отправка (с политикой) / приём |
| `IpcBus::freeze()/unfreeze(pkts)/reroute(old,new)` | Поддержка переноса состояния при hot-swap |
| `IpcBus::metrics()/reset_metrics()` | sent/dropped/dedup/peak-depth/latency |
| `SharedIpcBus` | Клонируемая обёртка `Arc<Mutex<IpcBus>>` |
| `ipc::channel()` → `(IpcSender, IpcReceiver)` | Пара mpsc с отображением ошибок в `Result` |
| `RingBufferTransport::send_via_ring(&pkt)` | Полезные нагрузки ≥4 КиБ в обход очереди |
| `RingBufferTransport::try_receive_from_ring/ring_usage/active_rings` | Доступ к ring-буферам |

### 4.3 `aios-hal` — аппаратная абстракция (3 файла · ~1.8 тыс. строк · 34 теста)

Модули: `hardware` (детектирование + `HalBlock`), `ai_tier`.

| Функция / Тип | Назначение |
|---|---|
| `HardwareProfile::detect()` | Реальные пробы: флаги CPUID, память, nvidia-smi/ROCm/NPU, PCI/storage/USB/TB |
| `AiTier::from_profile(&profile)` | Tier1 локальная LLM / Tier2 квантованная SLM / Tier3 эвристика |
| `AiTier::description()/max_model_size_gb()/recommended_batch_size()` | Подсказки производительности |
| Мок-фабрики: `mock_legacy/modern/intel_meteor_lake/qualcomm_x_elite/nvidia...` | Профили для тестов без привязки к железу |
| `HalBlock::new(id)/with_profile(id,p)/profile()` | `StatefulBlock`, публикующий профиль |

---

## 5. Подсистемы ядра

### 5.1 `aios-block-mgr` — реестр/загрузчик/маршрутизатор (8 файлов · ~2.1 тыс. строк · 75 тестов)

| Функция / Тип | Назначение |
|---|---|
| `BlockRegistry::register_block(name,ver,binary)` | Выдача `BlockId`, хэш бинарника (SHA-256) |
| `BlockRegistry::{activate,unload,update_state}` | Управление жизненным циклом (`Unloaded→Loaded→Active→Frozen/Error`) |
| `BlockRegistry::assign_capabilities/check_capability` | Токен-ACL на блок |
| `BlockRegistry::boot_discover(root)/load_from_path(dir)` | Дисковое обнаружение `*_v*.bin|.wasm` |
| `BlockRegistry::topology()/topology_with_state()` | Перечисление манифестов для ответов |
| `BlockLoader::validate_binary(binary,sha)` | Контроль целостности перед загрузкой |
| `BlockLoader::load_from_binary(_with_capabilities)/load_from_directory` | Проверенная регистрация |
| `MessageRouter::register_handler/add_route/dispatch` | Таблица обработчиков + редиректы |
| `DependencyGraph::add_dependency/load_order/unload_order` | Топологическая сортировка Кана, поиск циклов |
| `SemanticVersion::parse/is_newer_than/bump_*/is_compatible_with` | Semver |
| `HotReloader::scan_and_reload(&mut registry)` | Инкрементальная перезагрузка из каталога наблюдения |

### 5.2 `aios-process-mgr` — планировщик и процессы (7 файлов · ~2.6 тыс. строк · 73 теста)

| Функция / Тип | Назначение |
|---|---|
| `Scheduler::new(total_ram_mb)` + `with_time_slice/with_aging_threshold/with_max_restarts/with_memory_pressure_threshold` | Настройки |
| `Scheduler::spawn_process/spawn_real_process/spawn_child` | Приём под RAM-квоту; хостинг реальных потоков ОС |
| `Scheduler::schedule_next()/tick()/force_preempt()` | Priority RR + aging + RT-дедлайны |
| `Scheduler::{kill,suspend,resume}_process/set_priority` | Управление |
| `Scheduler::report_crash/should_restart` | Устойчивость к сбоям, политика рестартов |
| Группы/сессии: `create_group/create_session/kill_group/suspend_group/set_group_priority` | Массовое управление |
| `check_real_threads()/get_real_thread_state/set_cpu_affinity` | Живость реальных потоков, привязка к ядрам CPU |
| `ram_usage()/memory_pressure()/check_memory_pressure()` | Телеметрия квот |
| `handle_process_command(scheduler,&packet)` | IPC-фронтенд (Spawn/Kill/AdjustPriority) |
| `process_metrics::{bind_current_thread,record_*}` | TLS-счётчики на процесс |
| `cpu_affinity::{set_thread_affinity,available_cores,validate_cores}` | Привязка Win32/Linux |
| `PriorityInheritance::{acquire_lock,release_lock,...}` | Протокол наследования приоритетов |

### 5.3 `aios-live-update` — атомарный hot-swap (5 файлов · ~1.2 тыс. строк · 23 теста)

| Функция / Тип | Назначение |
|---|---|
| `LiveUpdateEngine::perform_swap(block_id, old…, new…, queue, health_check)` | Атомарный своп из 7 шагов: freeze→проверка SHA→health-гейт→откат-запас→restore очереди |
| `LiveUpdateEngine::rollback(block_id,queue)/expired_rollbacks()/swap_history()` | Управление окном отката |
| `StateTransferManager::{extract_state,restore_state,reroute_snapshot}` | Заморозка шины/снимок/перенаправление |
| `WasmLiveUpdateEngine::{deploy_block,swap_block,rollback_block,call_block_func}` | Настоящий in-place своп Wasmtime с миграцией линейной памяти |
| `PersistedLiveUpdateEngine::{perform_swap,rollback,recover_pending_swaps}` | Вариант на CoW-хранилище + журнал восстановления |

### 5.4 `aios-watchdog` — супервизор живости (5 файлов · ~1.2 тыс. строк · 47 тестов)

| Функция / Тип | Назначение |
|---|---|
| `Heartbeat::{verify,compute_hmac,age_ms}` | Сердцебиения, подписанные HMAC-SHA256 |
| `Watchdog::receive_heartbeat/check_timeout/force_safe_mode/escalate_actions/reset` | Пропуски → ступенчатые действия (warn→suspend→kill→dump→safe mode) |
| `WatchdogAction::{severity,is_terminal}` | Порядок Restart/ForceSafeMode/Terminate |
| `SafeModeShell::{parse_command,execute,orchestrator_restarts}` | Детерминированная спасательная оболочка (ps/kill/load/status/logs/restart) |
| `WatchdogRunner::{start,stop,pop_actions}` | Обёртка фонового потока |

### 5.5 `aios-security` — zero-trust (5 файлов · ~0.8 тыс. строк · 31 тест)

| Функция / Тип | Назначение |
|---|---|
| enum `Capability` | `CAP_NET_BIND/CONNECT`, `CAP_FS_READ/WRITE`, `CAP_HW_ACCESS`, `CAP_MEM_ALLOC`, `CAP_SCHED_MODIFY`… |
| `AccessControlLayer::{issue_token(_with_ttl),check_permission,revoke_token,clean_expired}` | Выдача/проверка/журнал нарушений |
| `CapabilityToken::{has_capability,is_expired,verify,compute_signature}` | Токены с TTL и HMAC-подписью |
| `Sandbox::{start,check_syscall,allocate_memory,terminate,from_token}` | Белый список syscall'ов + лимиты памяти |
| `HardwareSecurityBridge::{assign_mpk_protection,assign_tee_protection,assign_iommu_protection,validate_hardware_access}` | Единое отображение MPK/TEE/IOMMU |

### 5.6 `aios-context` — хранилище контекста (7 файлов · ~1.1 тыс. строк · 36 тестов)

| Функция / Тип | Назначение |
|---|---|
| `TelemetryStore::{query_metric,query_range,query_by_block,average_value,peak_ram}` | Телеметрия в ring-буфере |
| `CompressedTelemetryStore::{record,compression_ratio}` | Холодные ZSTD-блоки + горячий порог |
| `WorkflowStore::{record,most_used,recently_used}` | Изученные профили использования (подстройка приоритетов планировщика) |
| `StabilityStore::{best_version,record_crash,record_uptime}` | Оценки стабильности по версиям бинарников |
| `EmbeddedContextStore::{should_compact,compact,export_all,total_entries}` | Единый фасад |
| `PersistentStore::{save_all,load_telemetry,save_workflows,save_stability,compact}` | Постоянство на redb |

### 5.7 `aios-wasm` — песочница (5 файлов · ~1.6 тыс. строк · 56 тестов)

| Функция / Тип | Назначение |
|---|---|
| `WasmSandbox::{compile_module,compile_any,compile_wat}` | Обёртка движка Wasmtime |
| `WasmBlock::{new/from_wat,instantiate,call_func}` | Инстанцированный блок |
| `WasmBlock::{extract_linear_memory,restore_linear_memory}` | Перенос состояния для hot-swap |
| `SandboxConfig` | Лимиты топлива/времени/памяти |
| `IsolationConfig/IsolationLevel` | Матрица shared-nothing-изоляции на блок |
| `WasiFilter::check_syscall` | Политика allow/deny/log для WASI-syscall'ов |
| `BlockExecutor::execute_block(s)` | Пакетное исполнение |

### 5.8 `aios-net` — сетевые блоки (5 файлов · ~1.4 тыс. строк · 51 тест)

| Функция / Тип | Назначение |
|---|---|
| `RealTcpBlock::{start_listening,connect,send,receive,accept_pending,close_connection}` | Реальные сокеты, SO_REUSEADDR/KEEPALIVE/NODELAY, bind/connect через токены полномочий |
| `RealUdpBlock::{bind,send_to,receive_from,broadcast,port}` | Реальный UDP, включая SO_BROADCAST |
| `TcpBlock/UdpBlock` (+конфиги/состояния) | Симулируемые транспорты для детерминированных тестов |
| `inject_message/inject_packet` | Тестовые хуки |

### 5.9 `aios-exec-compat` — трансляция POSIX/Win32 (6 файлов · ~1.9 тыс. строк · 89 тестов)

| Функция / Тип | Назначение |
|---|---|
| `ExecutableType::{from_bytes,from_extension,required_capabilities}` | Распознавание ELF/PE/shebang |
| `PosixSyscall/SyscallRequest/SyscallResponse` + `PosixTranslator` | Трансляция POSIX-syscall'ов (дефолтная реализация с лимитами RAM) |
| `Win32Api/Win32Request/Win32Response` + `Win32Translator` | Трансляция Win32 по ординалам |
| `CompatSandboxManager::{spawn_process,terminate_process,cleanup_terminated,total_memory_used}` | Процессы совместимости с лимитами ресурсов |
| `DependencyHealer::{scan_dependencies,resolve_missing,heal_dependencies,add_loaded_library}` | Поиск недостающих DLL/.so по путям поиска |

---

## 6. Защита на уровне железа и оптимизация

| Крейт | Строки / тесты | Ключевой API |
|---|---|---|
| `aios-mpk` | 816 / 27 | Ключи защиты Intel MPK, домены ARM DACR, регистр PKRU, программная изоляция как fallback |
| `aios-iommu` | 528 / 25 | DMA-домены, IOVA-таблицы страниц, карта подключения устройств |
| `aios-tee` | 841 / 28 | Анклавы SGX/TrustZone/SEV, запечатывание, отчёты аттестации |
| `aios-ringbuf` | 653 / 16 (+proptest) | Lock-free SPSC ring: индексы производителя/потребителя, передача за O(1) |
| `aios-compress` | 572 / 16 | Квантование FP8/INT4 + сжатие ZSTD, LRU-кэш декомпрессии |
| `aios-persistence` | 680 / 12 | `CopyOnWriteStorage` (теневая запись→fsync→rename), `RecoveryLog`, `SnapshotManager` |
| `aios-optim` | 964 / 39 | Процентили профайлера, flamegraph горячих путей, оптимизатор cache-line, авто-тюнер grid/random/binary |

---

## 7. Системные службы

### 7.1 `aios-autohal` — автоподбор драйверов (12 файлов · ~4.4 тыс. строк · 73 теста)

Конвейер (5 шагов): обнаружение → поиск в DriverStore → загрузка/адаптация → проверка+выдача полномочий+инстанциирование Wasmtime → кэш/регистрация.

| Функция / Тип | Назначение |
|---|---|
| `AutohalEngine::provision()/rescan()/remove_device()` | Владелец конвейера; replug с кэшированным драйвером |
| `AutohalEngine::{record_failure,rollback_to_generic,set_cap_override}` | Самолечение после 3 сбоев → Generic Fallback |
| `extract_fingerprints()/diff_fingerprints()` | Различия идентификаторов USB/PCI/BT/ACPI/NVMe |
| `HotplugMonitor` (+`native.rs`) | udev netlink (Linux) / WM_DEVICECHANGE (Windows) push-события + поллинг |
| `DriverStore/DriverIndex` | Постоянный кэш в `AIOS://store/drivers/`, счётчики отказов |
| `ui_tui::HardwareInspector / ui_gui::show_panel` | Виджеты с паритетом TUI/GUI |

### 7.2 `aios-vfs` + `aios-fm` — виртуальная ФС и файловый менеджер (11 файлов · ~3.2 тыс. строк · 45 тестов)

| Функция / Тип | Назначение |
|---|---|
| `VfsPath::parse()/to_uri()/join()` | Схемы `AIOS://` и `HOST://` |
| `AiosVfs` / `HostVfs` | Изолированные корни (`/system`,`/sandbox`,`/store`,`/config`) / хост-ФС за ACL-токенами |
| `VirtualFileSystem::resolve()` | Проверка ACL + каноникализация против traversal |
| `{copy_recursive,move_item,delete_item}` | Отменяемые асинхронные операции с прогрессом |
| `analyze_file()` | Эвристики «умного» предпросмотра |
| `FileManager::new(fs,acl)/send(Command)/snapshot()` | UI-независимый движок, общий для обоих фронтендов |
| `ui_tui::{key_to_action,draw} / ui_gui::show` | Рендеры двух панелей в стиле Волкова/Far |

### 7.3 `aios-cluster` — мультиузловое планирование (8 файлов · ~3.0 тыс. строк · 31 тест)

| Функция / Тип | Назначение |
|---|---|
| `DistributedScheduler::{start(peers),shutdown,tick}` | Один тип = координатор или воркер (есть ли executor) |
| `DistributedScheduler::{spawn,kill,set_priority,get_state,migrate}` | Удалённые операции через RPC `ClusterMessage` |
| `TcpClusterTransport / InMemoryClusterTransport` | TCP bincode с длиной кадра / mpsc-реестр для тестов |
| трейт `ProcessExecutor` (+`SchedulerProcessExecutor`, mock) | Спавн/убийство/извлечение состояния на узле |
| Репликация контрольных точек | Broadcast снапшотов с сердцебиением, TTL-чистка, failover-восстановление |
| `ClusterConfig::{from_env,from_json}` | Загрузка через `AIOS_CLUSTER_*` |

### 7.4 Веб-стек

| Крейт | Ключевой API |
|---|---|
| `aios-browser` (8 файлов · 1.4 тыс. · 36 т) | `BrowserEngine::navigate(url)`, `HtmlParser::{parse,extract_text,extract_links,extract_title}`, `Renderer::{render_page,to_text}`, headless-фолбэк браузерного движка, `BrowserBlock` |
| `aios-search` (5 файлов · 0.4 тыс. · 7 т) | `SearchEngine::search` через DuckDuckGo/SearXNG/Brave + LLM TL;DR в `SearchSummarizer` |
| `aios-webview` (2 файла · 0.3 тыс. · 7 т) | `WebBrowser::{open,navigate,back,forward,close}` в фоновом потоке через event-loop proxy; постоянный профиль; правило адресной строки `resolve_target()` |
| `aios-net-config` (5 файлов · 0.9 тыс. · 32 т) | `NetworkConfigStore::{load,load_or,save}`, `NetworkConfig::apply_updates`, валидаторы, `NetSettingsBlock` |

### 7.5 Магазин и обновления

| Крейт | Ключевой API |
|---|---|
| `aios-store` (8 файлов · 2.1 тыс. · 58 т) | `StoreManager::{search,install,update,parse_source_spec,trust_source}`, `ManifestValidator` (SHA-256 + Ed25519 `verify_strict`, доверенные ключи), `BlockInstaller` (sidecar'ы, backup/rollback, `check_updates`) |
| `aios-updater` (4 файла · 0.4 тыс. · 18 т) | `DualBootManager::{swap_slot,record_boot_success,should_rollback}` (слоты A/B), `RollbackManager::{take_snapshot,rollback_to,auto_rollback_if_needed}`, `HotSwapEngine` |

### 7.6 Наблюдаемость

| Крейт | Ключевой API |
|---|---|
| `aios-telemetry` (4 файла · 0.5 тыс. · 17 т) | `FlightRecorder::{record,dump_since,dump_by_kind}`, `MetricCollector::to_prometheus()`, `TraceContext::{begin_span,end_span,to_json}` |
| `aios-debug` (3 файла · 0.3 тыс. · 10 т) | `CrashReporter::generate_report(kind,…,zero_knowledge)` (хэширование стека/редакция), глобальный хук `PanicHandler::install()` |

---

## 8. Слой ИИ

### 8.1 `aios-llm` (5 файлов · ~0.7 тыс. строк · 13 тестов)

| Функция / Тип | Назначение |
|---|---|
| `LlmEngine::{from_config,query,query_stream,config,backend_label}` | Облако (Groq/OpenRouter/Google) или локальный GGUF через candle (Qwen2.5-0.5B/7B INT4) |
| `LlmStreamSink` | Поток дельт через tokio mpsc |
| `extract_stream_delta(payload, google_shape)` | Разбор SSE-дельт OpenAI + Google |
| `download_default_model(kind)/detect_local_models()` | Загрузка hf-hub + скан локальных `.gguf` |

### 8.2 `aios-builder` (5 файлов · ~0.6 тыс. строк · 23 теста)

| Функция / Тип | Назначение |
|---|---|
| `EasyLangParser::parse(text,name)` | Построчный DSL (`spawn/timer/load/unload/kill/query/compact/status`) → `Workflow` |
| `WorkflowCompiler::{generate_wat,compile_to_wasm}` | WAT→WASM-модуль (экспорты `init/start/step_N`) |
| `AutoManifestGenerator::{from_wasm_binary,from_workflow_intents}` | Вывод полномочий (таблица на 15 позиций) + JSON-манифест |

### 8.3 `aios-bridge` — API-шлюз (5 файлов · ~1.5 тыс. строк · покрыт 24 интеграционными тестами)

Эндпоинты (`server.rs`):

| Метод Путь | Обработчик |
|---|---|
| GET `/api/v1/health` | Здоровье/версия/uptime |
| GET `/api/v1/system/status` | Watchdog + процессы + блоки + RAM |
| POST `/api/v1/intent` | Исполнение NL-интента (LLM fallback, проверки ACL) |
| POST `/api/v1/workflow` | Последовательность промптов |
| POST `/api/v1/llm/query` | Прямой LLM-запрос |
| POST `/api/v1/browse` · `/api/v1/search` | Прокси браузера/поиска |
| GET `/api/v1/store/index` · POST `/api/v1/store/register` · POST `/api/v1/store/publish` | Служба магазина (publish проверяет SHA-256 + опционально Ed25519) |
| GET `/index.json` · `/store/index.json` · `/blocks/{name}.wasm` · `/store/blocks/{name}.wasm` | Каталог/загрузка службы обновлений |
| GET `/api/v1/metrics` · `/api/v1/traces` · POST `/api/v1/crash-report` | Текст Prometheus / спаны / crash-отчёт |
| WS `/ws/telemetry` | Push RAM/процессов каждые 100 мс |

Ключевые типы: `BridgeContext` (общие подсистемы), `IntentParser` (ключевые слова RU/EN + `parse_with_llm_fallback`), enum `UserIntent`, ~30 DTO, `BridgeError`.

---

## 9. Интерфейсы и бинарники

| Бинарник / крейт | Назначение | Примечания |
|---|---|---|
| `aios` (kernel TUI, 6 файлов · 3.6 тыс. · 9 т) | Единый системный бинарник | 7 вкладок: System&HW / Blocks&Svc / AI Console / Studio Bridge / Network&Store / Web / Shell; `--safe-mode`, `--daemon`; хоткеи `b B n N W F9 F10`; shell-команды cluster |
| `aios-tui` (4 файла · 4.3 тыс. · 40 т) | Отдельная панель управления | `fetch/search/open`, `net get/set/reset`, полный набор `store`, фолбэк на watchdog-shell |
| `aios-gui` (18 файлов · 3.3 тыс. · 10 т) | egui-панель 1200×800, тёмная тема | Вкладки: Dashboard / Blocks / AI Studio / Store / Network / Deps / Browser / Files / Hardware |
| `aios-daemon` (`aiosd`, 1 файл · 183 строки) | Headless-режим Docker/сервера | Загрузочные блоки, поток сердцебиений, периодическая запись в redb |
| `aios-studio` (SPA) | Веб-панель, обслуживается bridge | Командная палитра, WS-график телеметрии, центр безопасности |
| `aios-init` (отдельный, статический musl) | PID 1 для initramfs | Монтирование VFS, супервизор блоков (3 рестарта), сбор зомби, спасательная оболочка, передача в `/system/aios-core` |
| Live ISO (`live/build.sh`) | Гибридный BIOS+UEFI образ | aios-init — `/init` по умолчанию; legacy busybox за `USE_BUSYBOX_INIT=1` |

## 10. Bare-metal-ветка микроядра

`aios-kernel` (`no_std`, `x86_64-unknown-none`, nightly; 10 файлов · ~1.3 тыс. строк) + `aios-kernel-run` (QEMU BIOS-раннер).

| Веха | Статус | Содержание |
|---|---|---|
| M0 (v2.26.0) | ✅ | Загрузка в QEMU, serial COM1 + VGA-консоль, отображение физической памяти |
| M1 (v2.27.0) | ✅ | GDT/TSS (double-fault IST), IDT на 256 вентилей, ремап PIC, PIT 100 Гц, клавиатура PS/2 |
| M2 (v2.28.0) | ✅ | Обход таблиц страниц, map/unmap + аллокатор кадров, куча 2 МиБ со списком свободных блоков (`Box/Vec/String`) |
| M3 | ⬜ план | Вытеснение: планировщик по таймеру, переключение контекста, ring 0/3 |
| M4 | ⬜ план | IPC на стороне ядра с переиспользованием `aios_core::ipc_protocol` |

Модули: `main` (точка входа/стеки/idle-цикл) · `gdt` (GDT+TSS) · `idt` (256 вентилей) · `interrupts` (PIC/PIT/клавиатура + сгенерированные заглушки) · `memory` (translate/map/unmap/bump-аллокатор) · `heap` (free-list GlobalAlloc) · `vga` (писатель 80×25) · `serial` (COM1) · `port` (inb/outb) · `build.rs` (256 asm-заглушек векторов).

## 11. Интеграционные тесты (корневой `tests/`, 14 файлов · 162 теста)

| Файл | Тесты | Покрытие |
|---|---|---|
| `integration_test.rs` | 30 | Полный жизненный цикл, скорость IPC, параллельные спавны, hot-swap+IPC |
| `bridge_tests.rs` | 24 | Разбор интентов EN/RU |
| `chaos_test.rs` | 18 | Повреждённые пакеты, переполнение шины, циклы сбоев |
| `real_file_io.rs` | 12 | Снапшоты/COW-постоянство на реальном диске |
| `real_network.rs` | 11 | Реальные TCP listen/accept/мультиклиент |
| `stress_test.rs` | 11 | ×1000 спавнов, RT ×500, реестр ×500 (двойные пороги debug/release) |
| `real_threads.rs` | 10 | Реальные потоки: terminate/suspend/resume |
| `real_wasm.rs` | 8 | Wasmtime end-to-end, изоляция мультиблоков |
| `browser_search_tests.rs` | 7 | HTML-парсер |
| `full_lifecycle.rs` | 7 | Boot→deploy→swap→watchdog→shutdown |
| `real_hot_swap.rs` | 7 | Hot-swap WASM при смене версии |
| `e2e_pipeline_test.rs` | 6 | Цепочка HW→тир→LLM intent→EasyLang→WASM |
| `fuzz_test.rs` | 6 | Fuzzing случайных пакетов |
| `stress_fault_tolerance.rs` | 5 | 50 параллельных WASM-блоков, штормы сбоев |

## 12. Статистика кодовой базы (срез аудита v2.28.1)

- **244 исходника Rust**, **~59 400 строк** в 39 крейтах воркспейса + 3 отдельных крейта.
- **1338 тестов зелёные** в 91 наборе (unit + integration + doc-tests), `cargo clippy --workspace --all-targets`: **0 предупреждений**, `cargo fmt --check`: чисто.

Крупнейшие крейты: `aios-autohal` 4.4 тыс. · `aios-tui` 4.3 тыс. · `aios` 3.6 тыс. · `aios-gui` 3.3 тыс. · `aios-cluster` 3.0 тыс. · `aios-process-mgr` 2.6 тыс. · `aios-block-mgr` 2.1 тыс. · `aios-store` 2.1 тыс. · `tests/` 3.9 тыс.
