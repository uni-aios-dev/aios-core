# Архитектура AIOS

## Обзор системы

AIOS (AI-Native Operating System) — модульная ОС в стиле микроядра, разработанная для AI-ориентированных нагрузок. Система состоит из 24 Rust-крейтов, образующих слоистую архитектуру: базовые типы внизу, абстракция оборудования и управление процессами в середине, системы безопасности/контекста, и пользовательские интерфейсы (TUI/GUI/Единый бинарник) наверху.

Вся коммуникация между крейтами осуществляется через бинарный IPC-протокол. Блоки (модули ядра) поддерживают горячую замену с автоматическим откатом. AI-оркестратор преобразует намерения на естественном языке в системные операции. Крейт `aios` предоставляет единый системный бинарник с обоими режимами — интерактивным TUI и headless-демоном, заменяя отдельные точки входа `aios-tui` и `aios-daemon`.

```
┌──────────────────────────────────────────────────────┐
│              Interface Layer (User-Facing)            │
│  TUI (ratatui)  │  GUI (egui)  │  Единый `aios` bin  │
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

## Слой 1: Фундамент (`aios-core`)

### Обработка ошибок (`error.rs`)

Все операции AIOS возвращают `aios_core::error::Result<T>`, где `T = std::result::Result<T, AIOSException>`.

`AIOSException` содержит 19 вариантов, покрывающих все режимы отказа в системе:

| Вариант | Сценарий использования |
|---------|----------|
| `BlockNotFound(String)` | Блок не найден в реестре |
| `BlockAlreadyRegistered(String)` | Дублирование имени блока |
| `InvalidSignature { expected, actual }` | Несовпадение SHA-256 для бинарного файла |
| `IntegrityCheckFailed(String)` | Общая ошибка целостности |
| `StateExtractionFailed(String)` | Невозможно сериализовать состояние блока |
| `StateRestoreFailed(String)` | Невозможно десериализовать состояние блока |
| `HotSwapFailed(String)` | Атомарная горячая замена не удалась во время операции |
| `RollbackFailed(String)` | Невозможно восстановить предыдущую версию блока |
| `IPCError(String)` | Ошибка транспорта IPC |
| `SchedulerError(String)` | Ошибка планирования процессов |
| `ProcessNotFound(u64)` | PID не найден в планировщике |
| `ProcessAlreadyExists(u64)` | Дублирование PID |
| `PermissionDenied(String)` | Несанкционированная операция |
| `HardwareNotDetected(String)` | Отсутствует аппаратный компонент |
| `InvalidPayload(String)` | Некорректная структура IPC-пакета |
| `Timeout(String)` | Тайм-аут операции |
| `ConfigurationError(String)` | Некорректная конфигурация |
| `SerializationError(String)` | Ошибка bincode |
| `Generic(String)` | Универсальный вариант |

### Типы блоков (`block.rs`)

**`BlockId`** — уникальный 32-битный идентификатор для каждого блока. Реализует `Display` как `"block_{id}"`.

**`BlockManifest`** — метаданные зарегистрированного блока:
- `id: BlockId`, `name: String`, `version: String`, `sha256: [u8; 32]`

**`BlockState`** — состояния жизненного цикла:
```
Unloaded → Loaded → Active ↔ Frozen → Unloaded
                  ↓
                Error
```

**`StatefulBlock` trait** — интерфейс, который должен реализовать каждый блок:
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

### SHA-256 криптография (`crypto.rs`)

Четыре функции для проверки целостности бинарных файлов:
- `compute_sha256(data) -> String` — хеш в шестнадцатеричном представлении
- `compute_sha256_bytes(data) -> [u8; 32]` — сырые 32 байта хеша
- `verify_sha256(data, expected_hex) -> bool` — сравнение строк
- `verify_sha256_bytes(data, expected_bytes) -> bool` — сравнение байтов

### IPC-протокол (`ipc_protocol.rs`)

Бинарный протокол с сериализацией через `bincode`. Каждый пакет самодескрибируемый и проверяемый на целостность.

**`Header`** (фиксированный размер, `#[repr(C)]`):
| Поле | Тип | Описание |
|-------|------|-------------|
| `packet_id` | `u64` | Автоинкрементный уникальный ID (AtomicU64) |
| `source_block` | `u32` | ID блока-отправителя |
| `target_block` | `u32` | ID блока-получателя |
| `command_id` | `u16` | Код операции из перечисления `CommandId` |
| `priority` | `u8` | 0-255, чем выше — тем срочнее |
| `payload_len` | `u32` | Длина сериализованного payload в байтах |
| `checksum` | `[u8; 32]` | SHA-256 от байтов payload |

**`Payload` enum** (15 вариантов):
- `Empty`, `Binary(Vec<u8>)`, `Text(String)`
- Операции с блоками: `RegisterBlock`, `UnloadBlock`, `GetTopology`
- Операции с процессами: `SpawnProcess`, `KillProcess`, `AdjustPriority`
- Системные операции: `HealthCheck`, `ExtractState`, `RestoreState`
- Операции обновления: `HotSwap`, `Rollback`
- AI-операции: `IntentCommand`
- Расширяемые: `Custom(String, Vec<u8>)`

**`CommandId` enum** (13 команд, u16 repr):
- Домен блоков: `0x0001`-`0x0003`
- Домен процессов: `0x0010`-`0x0012`
- Системный домен: `0x0020`-`0x0031`
- Домен обновлений: `0x0040`-`0x0041`
- AI-домен: `0x0050`
- Расширяемый: `0x00FF`

**`Response` enum**: `Success(Payload)`, `Failure { code, message }`, `Timeout`

**Методы `IpcPacket`**:
- `new()` — автоматическая генерация packet_id и SHA-256 контрольной суммы
- `serialize()` / `deserialize()` — бинарное кодирование через bincode
- `verify_checksum()` — проверка целостности
- `response_ok()` / `response_err()` — фабрики ответов

**Производительность**:
- Debug: < 50us на сериализацию+десериализацию одного пакета
- Release: < 1us на сериализацию+десериализацию одного пакета

---

## Слой 2: Абстракция

### IPC-транспорт (`aios-ipc`)

#### Шина (`bus.rs`)

`IpcBus` — шина сообщений на основе VecDeque для коммуникации между блоками:
- **Упорядочение по приоритету** — `send_priority()` извлекает пакеты с наивысшим приоритетом первыми
- FIFO-порядок в пределах одного уровня приоритета
- **Политики обратного давления**: `Reject` (возврат ошибки) или `DropOldest` (удаление самого старого из очереди)
- **Дедупликация сообщений** — `with_dedup()` включает дедупликацию по packet_id через `HashSet<u64>`
- **Метрики шины** — `BusMetrics` отслеживает `total_sent`, `total_received`, `total_dropped`, `total_deduplicated`, `peak_queue_depth`, `avg_send_latency_us`
- **Заморозка/разморозка** для атомарного переноса состояния при горячей замене
- **Замороженная шина отклоняет** новые сообщения (возвращает `SchedulerError`)

`SharedIpcBus` — обёртка `Arc<Mutex<IpcBus>>` для потокобезопасного доступа многопроизводителя/однопотребителя. Реализует `Clone`.

**Протокол заморозки** (используется при горячей замене):
```
1. bus.freeze() → очищает очередь, возвращает Vec<IpcPacket>, устанавливает frozen=true
2. Выполнение операций замены (сообщения не теряются)
3. bus.unfreeze(saved_packets) → восстанавливает пакеты по порядку, устанавливает frozen=false
```

#### Канал (`channel.rs`)

`IpcSender` / `IpcReceiver` — типизированный канал на основе mpsc:
- `IpcSender::send()` — неблокирующий, возвращает `Result<()>`
- `IpcReceiver::receive()` — блокирующий
- `IpcReceiver::try_receive()` — неблокирующий, возвращает `Option<IpcPacket>`
- `IpcSender` реализует `Clone` для многих производителей

### Слой абстракции оборудования (`aios-hal`)

#### Обнаружение оборудования (`hardware.rs`)

`HardwareProfile` — полное описание аппаратного обеспечения системы:

```rust
pub struct HardwareProfile {
    pub cpu: CpuInfo,
    pub gpu: Option<GpuInfo>,
    pub npu: Option<NpuInfo>,
    pub memory: MemoryInfo,
    pub pci_devices: Vec<PciDevice>,
}
```

**Методы обнаружения**:
- `HardwareProfile::detect()` — реальное оборудование через API ОС:
  - Windows: команды `wmic`
  - Linux: `/proc/cpuinfo`, `/proc/meminfo`
  - x86: CPUID intrinsics для обнаружения возможностей

**Поля CpuInfo**: cores, threads, model, has_avx512, has_avx2, has_sse42, has_neon, base_freq_mhz, vendor (Intel/AMD/ARM/Apple/Unknown)

**Поля GpuInfo**: name, vram_mb, compute_shaders, vendor, driver_version, cuda_cores, compute_capability

**Методы обнаружения GPU**:
- `detect_gpu_nvidia()` — Windows: запускает `nvidia-smi --query-gpu=name,memory.total,driver_version,compute_cap`
- `estimate_cuda_cores(gpu_name)` — отображение имён GPU на количество CUDA-ядер (RTX 4090→16384, A100→6912, H100→16896)
- `detect_gpu_wmic()` — наследуемый fallback через Windows WMI
- `detect_gpu_amd()` — Linux: парсинг вывода `rocm-smi --showproductname --showmeminfo vram`

**Определение устройств хранения**:
- Структура `StorageDevice`: `name`, `interface`, `capacity_gb`, `model`
- Перечисление `StorageInterface`: `NVMe`, `SATA`, `USB`, `Unknown`
- `detect_storage()` — Windows: `wmic diskdrive` / Linux: `/sys/block`
- `HardwareProfile::storage_devices: Vec<StorageDevice>` — во всех профилях

**Мок-профили** (для тестирования без реального оборудования):
- `mock_legacy()` — Intel i5-3570, 8GB, без GPU/NPU → Tier 2
- `mock_modern()` — AMD Ryzen 9 7950X, 64GB, RTX 4090 (полная информация о GPU), XDNA2 NPU → Tier 1
- `mock_legacy_2012()` — Intel i3-3220, 4GB, Intel HD 2500 → Tier 3
- `mock_nvidia()` — AMD Ryzen 9 7950X3D, 128GB, RTX 4090 (16384 CUDA-ядра, compute capability 8.9) → Tier 1

**`HalBlock`** — абстракция оборудования как `StatefulBlock`:
- Отвечает на IPC-сообщения `HealthCheck` и `Custom("get_hardware_profile")`
- Сериализует/десериализует весь `HardwareProfile` для извлечения состояния
- Проверка работоспособности: true, если количество ядер CPU > 0

#### Классификация AI-уровней (`ai_tier.rs`)

`AiTier` — классификация аппаратных возможностей для AI-нагрузок:

| Уровень | Требования | Макс. модель | Размер батча | Сценарий использования |
|------|-------------|-----------|------------|----------|
| **Tier 1** | NPU + GPU + AVX-512 + ≥16GB RAM | 70 GB | 64 | Локальный вывод LLM |
| **Tier 2** | AVX2/NEON + ≥4GB RAM | 7 GB | 8 | Вывод на edge-устройства |
| **Tier 3** | Все остальные | 0.5 GB | 1 | Облегчённые задачи |

Классификация детерминирована на основе аппаратных флагов. Любое отдельное невыполненное требование снижает уровень на один шаг вниз.

---

## Слой 3: Управление

### Менеджер блоков (`aios-block-mgr`)

#### Реестр (`registry.rs`)

`BlockRegistry` — центральный каталог блоков:
- `register_block(name, version, binary)` → `BlockId` — назначает ID, вычисляет SHA-256, сохраняет как `Loaded`
- `activate_block(id)` — переход состояния в `Active`
- `unload_block(id)` — удаляет запись, возвращает `BlockEntry`
- `topology()` → `Vec<BlockManifest>` — все зарегистрированные блоки
- `verify_signature(id)` — пересчитывает SHA-256 и сравнивает с сохранённым хешем
- `find_by_name(name)` — поиск по имени
- `load_from_path(dir)` — сканирует директорию на `.wasm` и `.bin` файлы, загружает все обнаруженные блоки
- `load_from_path_str(dir_str)` — обёртка для строкового пути
- `boot_discover(root)` — рекурсивный обход всех поддиректорий, обнаруживает и регистрирует `.wasm`/`.bin` файлы, создаёт директорию при её отсутствии

`BlockEntry` хранит: `manifest: BlockManifest`, `state: BlockState`, `binary: Vec<u8>`, `capabilities: Option<CapabilityToken>`

#### Загрузчик (`loader.rs`)

`BlockLoader` — конвейер загрузки высокого уровня:
1. `validate_binary(binary, expected_sha256)` — сравнение SHA-256
2. `load_from_binary(registry, name, version, binary)` — регистрация + валидация + активация за один вызов
3. `load_from_binary_with_capabilities(registry, name, version, binary, token)` — то же, с опциональным назначением `CapabilityToken`
4. `load_from_directory(registry, dir)` — сканирует директорию на `.wasm`/`.bin` файлы, ищет sidecar `.json` манифесты (имя, версия, capabilities, TTL), загружает каждый
5. `unload_block(registry, id)` — предупреждение при выгрузке активного блока

`BlockManifestJson` — структура sidecar манифеста из `.json` файлов:
- `name: Option<String>` — переопределение имени блока (вместо имени файла)
- `version: Option<String>` — переопределение версии
- `capabilities: Option<Vec<String>>` — имена capabilities для назначения (например, `CAP_NET_BIND`)
- `ttl_ms: Option<u64>` — TTL capability-токена (по умолчанию: 3600000мс)

#### Маршрутизатор (`router.rs`)

`MessageRouter` — маршрутизация IPC-пакетов к обработчикам блоков:
- `register_handler(block_id, handler)` — привязка обработчика `Box<dyn FnMut>`
- `add_route(from, to)` — маппинг перенаправления (сообщения блока A перенаправляются в блок B)
- `dispatch(packet)` — разрешение маршрута и вызов обработчика
- `route_target(target)` — возврат цели перенаправления или оригинального назначения

Сигнатура обработчика: `FnMut(&IpcPacket) -> Result<Option<IpcPacket>>`

#### Граф зависимостей (`dependency.rs`)

`DependencyGraph` — управление порядком загрузки/выгрузки блоков:
- `add_block(name)` — регистрация блока без зависимостей
- `add_dependency(block, depends_on)` — объявление зависимости с обнаружением циклов (DFS)
- `load_order()` — топологическая сортировка (алгоритм Кана) для корректной последовательности инициализации
- `unload_order()` — обратная топологическая сортировка для безопасного завершения
- `dependencies_of(block)` / `dependents_of(block)` — запросы графа
- `remove_block(name)` — удаление узла и всех ссылок из других зависимостей

**Обнаружение циклов**: `add_dependency()` проверяет наличие циклов перед добавлением ребра. Возвращает `DependencyError::CircularDependency` с цепочкой цикла.

**Топологическая сортировка**: Зависимости загружаются до зависимых блоков. Независимые узлы могут появиться в любом порядке (порядок итерации HashMap недетерминирован).

#### Семантическое версионирование (`version.rs`)

`SemanticVersion` — управление версиями блоков:
- `parse("1.2.3")` / `parse("v2.0.1")` — поддержка необязательного префикса `v`
- Реализация `Ord`: сравнение major → minor → patch
- `is_compatible_with(base)` — одинаковый major, текущий minor >= базового minor
- `is_newer_than(other)` — self > other
- `bump_major/minor/patch()` — инкремент версии
- `Display` — формат `"1.2.3"`

#### Горячая перезагрузка (`hot_reload.rs`)

`HotReloader` — мониторинг директории на наличие новых/обновлённых/удалённых файлов блоков:
- `HotReloadConfig`: `watch_dir`, `poll_interval_ms`, `auto_activate`
- `scan_and_reload(registry)` — сканирование на файлы `.bin`/`.aib`, обнаружение изменений через SHA-256
- `TrackedFile`: `path`, `modified`, `sha256`, `loaded_id` — отслеживание каждого файла
- `ReloadEvent` enum: `NewBlock`, `UpdatedBlock`, `RemovedBlock`, `Error`, `NoChange`
- Автосоздание директории при отсутствии; журнал событий для аудита
- При изменении файла: выгрузка старого блока, загрузка нового через `BlockLoader::load_from_binary()`

### Менеджер процессов (`aios-process-mgr`)

#### Типы задач (`task.rs`)

**`Priority`** (5 уровней, упорядочение Ord):
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

**Структура `Process`**:
- `pid: ProcessId`, `name`, `priority`, `state`
- `ram_quota_mb: u64` — зарезервированная RAM
- `cpu_time_ms: u64` — накопленное время CPU
- `crash_count: u32`, `max_restarts: u32` (по умолчанию: 3)
- `parent_pid: Option<ProcessId>` — для дочерних процессов
- `group_id: Option<u64)` — членство в группе процессов

**Структура `ProcessGroup`**:
- `id: u64`, `name: String`, `priority: Priority`
- `member_pids: Vec<ProcessId>` — процессы в этой группе
- `created_at_ms: u64`, `session_id: Option<u64>`

**`ProcessTimer`** — отслеживание временных слайсов:
- `quota_ms` — максимально допустимое время выполнения за слайс
- `quota_exceeded()` — проверка исчерпания временного слайса
- `remaining_ms()` — оставшееся время в текущем слайсе

#### Планировщик (`scheduler.rs`)

`Scheduler` — приоритетный вытесняющий планировщик:

**Структуры данных**:
- `processes: HashMap<ProcessId, Process>` — все процессы
- `priority_queues: BTreeMap<Priority, Vec<ProcessId>>` — очереди готовых по приоритетам
- `current: Option<ProcessId>` — текущий выполняющийся процесс
- `timer: Option<ProcessTimer>` — текущий временной слайс

**Жизненный цикл процесса**:
1. `spawn_process(name, priority, ram_mb)` → `ProcessId` — с ограничением RAM
2. `schedule_next()` → процесс с наивысшим приоритетом из готовых с учётом старения
3. `tick()` — проверка истечения таймера, перепланирование при необходимости
4. `kill_process(pid)` — завершение, освобождение RAM
5. Опционально: `suspend_process()`, `resume_process()`

**Старение процессов** (предотвращение голода):
- `aging_threshold_ms` (по умолчанию: 500мс) — время ожидания перед повышением приоритета
- `schedule_next()` вычисляет эффективный приоритет = базовый + (ожидание / порог), с ограничением +4 уровня
- Процессы с низким приоритетом, ожидающие 4x порога, повышаются до уровня Critical
- Все процессы оцениваются глобально (ранний выход по уровню очереди отсутствует)
- `force_preempt()` — принудительное истечение текущего временного слайса (для тестирования и ручного перепланирования)

**Взвешенный round-robin** (пропорциональные временные слайсы):
- `priority_weight()` отображает: Background=1, Low=2, Normal=3, High=4, Critical=5
- Временной слайс = `default_time_slice_ms * priority_weight` (Critical получает слайс в 5 раз больше Background)
- `round_robin_positions: HashMap<Priority, usize>` отслеживает позицию внутри каждой очереди для справедливости

**Обнаружение недостатка памяти**:
- `memory_pressure_threshold` (по умолчанию: 0.8) — порог использования, вызывающий Critical
- Предупреждение при `threshold * 0.75`, Critical при `threshold`
- `MemoryPressure` enum: `Normal(usage)`, `Warning(usage)`, `Critical(usage)`
- Структура `MemoryPressureEvent`: уровень, соотношение использования, использовано/всего МБ, имена колбэков
- `register_memory_pressure_callback(name)` — регистрация целей уведомлений
- `check_memory_pressure()` → `Option<MemoryPressureEvent>` (None при Normal)

**Группы процессов**:
- `create_group(name, priority)` → `u64` ID группы
- `create_session(name, priority)` → `u64` ID сессии (группа с session_id)
- `add_to_group(pid, group_id)` / `remove_from_group(pid)` — управление членством
- `kill_group(group_id)` — завершение всех участников и удаление группы
- `suspend_group(group_id)` / `resume_group(group_id)` — массовые изменения состояния
- `set_group_priority(group_id, priority)` — смена приоритета для всех участников
- `group_members(group_id)`, `all_groups()`, `group_count()`, `get_group()`

**Привязка к CPU** (`cpu_affinity.rs`):
- `set_cpu_affinity(pid, cores)` — привязывает реальный ОС-поток к конкретным ядрам CPU
- `get_cpu_affinity(pid)` — запрашивает текущую привязку для потока
- `available_cpu_cores()` — возвращает количество доступных ядер
- Платформа: `SetThreadAffinityMask` (Windows), `sched_setaffinity` (Linux), no-op fallback

**Режим планирования реального времени**:
- Перечисление `SchedulingMode`: `Normal` (взвешенный round-robin) и `RealTime` (на основе дедлайнов)
- `set_scheduling_mode(mode)` / `scheduling_mode()` — переключение между режимами
- `set_rt_deadline(pid, deadline_ms)` — назначение абсолютного дедлайна процессу
- `clear_rt_deadline(pid)` — удаление дедлайна у процесса
- RT-планирование: выбор процесса с ранним дедлайном (наименьшее оставшееся время)
- Структура `JitterEntry`: `pid`, `expected_ms`, `actual_ms`, `timestamp` — отслеживание джиттера
- `jitter_log()` / `clear_jitter_log()` — журнал аудита джиттера
- Максимум записей джиттера: 1000 (FIFO-удаление)

**Устойчивость к сбоям**:
- `report_crash(pid)` — инкремент `crash_count`, логирование `CrashEvent` с временной меткой
- `should_restart(pid)` — true, если `crash_count < max_restarts`
- `crash_log: Vec<CrashEvent>` — журнал всех сбоев

**Управление RAM**:
- Общий объём RAM настраивается при создании (по умолчанию от системы)
- Каждый процесс резервирует `ram_quota_mb`
- `spawn_process` завершается с `SchedulerError`, если RAM исчерпана
- `kill_process` освобождает зарезервированную RAM

#### IPC-управление процессами (`process_control.rs`)

`handle_process_command(scheduler, packet)` — IPC-диспетчеризация:
- `SpawnProcess { name, priority, ram_mb }` — создание, возврат PID в виде текста
- `KillProcess { pid }` — завершение, возврат подтверждения
- `AdjustPriority { pid, new_priority }` — изменение, возврат нового приоритета

### Движок live-обновлений (`aios-live-update`)

#### Перенос состояния (`state_transfer.rs`)

`StateTransferManager` — захват и восстановление состояния системы при горячей замене:
- `extract_state(queue, state)` → `Snapshot` — заморозка IPC-шины + захват байтов состояния
- `restore_state(queue, snapshot)` — разморозка шины с сохранёнными пакетами

`Snapshot`: `state: Vec<u8>` + `pending_packets: Vec<IpcPacket>`

#### Движок горячей замены (`engine.rs`)

`LiveUpdateEngine` — атомарная замена блоков с откатом:

**5-шаговая горячая замена** (`perform_swap()`):
1. **Заморозка** — извлечение состояния из старого блока, заморозка IPC-шины
2. **Валидация** — проверка SHA-256 нового бинарного файла
3. **Проверка работоспособности** — опциональное замыкание проверяет новый блок
4. **Сохранение отката** — сохранение старого бинарного файла, состояния и версии как `HotSwapEntry`
5. **Восстановление** — разморозка IPC-шины (сообщения в полёте сохраняются)

**Откат** (`rollback()`):
- Восстановление старого бинарного файла, состояния и версии из `HotSwapEntry`
- Предупреждение при превышении тайм-аута отката (настраивается, по умолчанию: 30с)
- Логирование `SwapRecord` для аудит-журнала

**Аудит-журнал `SwapRecord`**:
- `block_id`, `old_version`, `new_version`, `success`, `rolled_back`, `timestamp`

#### WASM-движок live-обновлений (`wasm_engine.rs`)

`WasmLiveUpdateEngine` — реальная замена WASM-модулей при горячей замене:

**Архитектура**: оборачивает `LiveUpdateEngine` + `WasmSandbox`, поддерживает `active_blocks: HashMap<BlockId, (WasmBlock, Store<StoreState>)>` для маппинга развёрнутых WASM-экземпляров.

**Развёртывание** (`deploy_block()`):
1. Читает бинарник из `BlockRegistry`
2. Компилирует через `WasmBlock::new()` → `create_store()` → `instantiate()`
3. Автоматически вызывает экспорты `init` и `start` (если присутствуют)
4. Сохраняет активную пару `(WasmBlock, Store)` для последующего `call_block_func()`

**Горячая замена** (`swap_block()`):
1. Вызывает `LiveUpdateEngine.perform_swap()` — заморозка IPC, проверка SHA-256, проверка здоровья, сохранение записи отката
2. Компилирует новый WASM-бинарник → инстанцирует → автоматически вызывает `init`
3. Перезаписывает `active_blocks[id]` новым экземпляром
4. Возвращает `SwapResult` со старой/новой версией и вызванными функциями

**Откат** (`rollback_block()`):
1. Удаляет активный WASM-экземпляр из `active_blocks`
2. Вызывает `LiveUpdateEngine.rollback()` — восстанавливает старый бинарник + состояние из `HotSwapEntry`

**Вызов функции** (`call_block_func()`):
- Ищет развёрнутый экземпляр, делегирует в `WasmBlock::call_func()`

**Параметры**: `SwapParams { new_binary, new_version, health_check: Option<HealthCheckFn>, isolation }`

---

## Слой 4: Пользовательский интерфейс (`aios-tui`)

### Движок намерений (`intent_engine.rs`)

`IntentEngine` — преобразование естественного языка в IPC-пакеты:

**Вход**: `"optimize for video editing"` → **Выход**: `TranslatedCommand` с IPC-пакетом

**8 категорий намерений** (сопоставление по ключевым словам, без учёта регистра):

| Намерение | Ключевые слова | Действие |
|--------|----------|--------|
| Оптимизация памяти | "free memory", "clear ram", "reduce memory" | AdjustPriority(pid=0, Background) |
| Оптимизация видео | "optimize video", "video editing", "video rendering" | AdjustPriority(pid=0, Critical) |
| Обновление блока | "update block", "upgrade block" | HotSwap |
| Завершение процесса | "kill", "stop", "terminate" | KillProcess |
| Создание процесса | "start", "spawn", "run" | SpawnProcess(256MB, Normal) |
| Изменение приоритета | "boost", "throttle", "priority" | AdjustPriority |
| Проверка работоспособности | "status", "health", "check" | HealthCheck |
| Топология | "topology", "blocks", "list" | GetTopology |

`IntentContext` предоставляет состояние системы для преобразования: активные процессы, загруженные блоки, текущий уровень, использование RAM.

### Панель управления (`dashboard.rs`)

TUI на базе Ratatui с 7-вкладочной интерактивной компоновкой:

**Зона заголовка**:
- Название проекта "AIOS v1.0.0"
- Текущий AI-уровень с цветовой кодировкой: Tier1=Зелёный, Tier2=Жёлтый, Tier3=Красный
- Состояние watchdog: OK (Зелёный), SUSPENDED (Красный), RECOVERING (Жёлтый), SAFE MODE (Маджента)
- Ядра CPU, использование RAM, количество блоков и процессов

**Зона вкладок**: 7 выбираемых вкладок — Обзор | Процессы | Блоки | Метрики | Зависимости | Web | Shell

**Вкладка 1 — Обзор** (горизонтальное разделение 45/55):
- Слева: панель информации об оборудовании (модель CPU, ядра/потоки, флаги AVX, имя GPU/VRAM, устройства хранения, системные счётчики)
- Справа: журнал активности (последние 20 сообщений, цветовая кодировка: Красный=ошибка, Жёлтый=предупреждение, Зелёный=успех)

**Вкладка 2 — Процессы** (вертикальное разделение):
- Сверху: таблица процессов: PID, Имя, Приоритет, Состояние, RAM, CPU, Сбои
- Выбор строки с индикатором `>>`, цветовая кодировка приоритета/состояния
- Снизу: панель деталей процесса или результат убийства

**Вкладка 3 — Блоки** (вертикальное разделение):
- Сверху: таблица блоков: ID, Имя, Версия, Состояние, Размер — индикатор выбора строки `>>`
- Заголовок показывает горячие клавиши: `j/k: навигация  U: выгрузка  L: загрузка  H: горячая замена`
- Снизу: панель деталей блока с информацией о выбранном блоке и доступных действиях, ИЛИ диалог ввода имени/версии блока

**Вкладка 4 — Метрики** (3-зонная вертикальная):
- Индикатор использования RAM (пороговая окраска Зелёный/Жёлтый/Красный)
- Гистограмма распределения приоритетов процессов (цветные полосы)
- Временной ряд RAM (кольцевой буфер 60 записей, столбчатая диаграмма)

**Вкладка 5 — Зависимости** (вертикальное разделение):
- Сверху: таблица графа зависимостей: #, Блок, Зависит от, Зависимые
- Индикатор выбора строки `>>`, цветовая кодирование зависимостей
- Снизу: панель порядка загрузки и статистики (топологическая сортировка, количество рёбер, блоков)

**Вкладка 6 — Web** (вертикальное разделение):
- Строка ввода URL с индикатором фокуса
- Область отображения текста страницы
- Прокручиваемый список ссылок с выбором (индикатор `>>`)
- Фоновая загрузка через reqwest blocking + HtmlParser из aios-browser
- `WebState`: url_input, current_url, page (PageContent), loading, error, input_focused, scroll
- `PageContent`: url, title, text, links Vec<(String,String)>
- Клавиши: `g`=фокус URL, `Enter`=навигация, `o`=открыть ссылку, `j/k`=навигация, `Esc`=снять фокус

**Вкладка 7 — Shell** (вертикальное разделение):
- Строка ввода команд с индикатором приглашения
- Область вывода (прокручиваемый вывод команд)
- Навигация по истории команд через ↑/↓
- `ShellState`: input_buffer, output (Vec<String>), command_history, history_pos
- Команды: `fetch <url>` (загрузка блока по URL), `search <query>` (веб-поиск DuckDuckGo), `open <url>` (навигация на URL на вкладке Web), `clear` (очистка вывода)
- Поток выполнения: TUI → execute_shell_cmd() → SafeModeShell / fetch / search / open

**Справка F1**:
- Включение по F1 или '?', закрытие по F1/Esc/'?'
- Отображает все горячие клавиши и команды shell во всплывающем окне
- Отрисовывается через draw_help() поверх содержимого текущей вкладки

**Зона подвала**: подсказки горячих клавиш (q=Выход, 1-7=Вкладка, Alt+1-7=Вкладка в любом режиме ввода, j/k=Навигация, K=Убийство, U=Выгрузка, L=Загрузка, H=Горячая замена, F1=Справка, :=Команда, s=Телеметрия, x=Статус, r=Обновление)

`DashboardState` управляет:
- Снимками процессов/блоков (берутся каждый кадр для согласованного рендеринга)
- Кольцевым буфером истории RAM (60 записей)
- Состоянием выбора (selected_tab, selected_row)
- Отображением результата убийства процесса
- Отображением результата операции с блоком + `BlockInputMode` (None/LoadName/LoadVersion) + буфер ввода
- Снимком графа зависимостей (`DependencySnapshot`) для вкладки Deps
- Буфером логов (ограничен 100 записями)
- Синхронизацией Scheduler + Registry
- Состоянием Web: url_input, current_url, page, loading, error, input_focused, scroll
- Состоянием справки (показана/скрыта)
- Состоянием Shell: input_buffer, output (Vec<String>), command_history, history_pos

### Точка входа (`main.rs`)

Последовательность запуска:
1. Инициализация `env_logger`
2. `HardwareProfile::detect()` — обнаружение реального оборудования
3. `AiTier::from_profile()` — классификация AI-возможностей
4. Создание `BlockRegistry` — регистрация 4 базовых блоков (hal, ipc_bus, scheduler, browser), boot-обнаружение блоков на диске из `AIOS_BLOCKS_DIR`, подключение браузерного блока к `MessageRouter`
5. Создание `Scheduler` — запуск 3 процессов (ai_orchestrator, io_handler, health_monitor)
6. Создание `Watchdog` — запуск потока heartbeat в фоне
7. Создание `EmbeddedContextStore` + `TelemetryStore` — для системной телеметрии
8. Создание `SafeModeShell` — для команд восстановления в безопасном режиме
9. Вход в raw-режим crossterm + альтернативный экран
10. Цикл событий: опрос клавиатуры, перерисовка панели, синхронизация состояния watchdog
11. Восстановление терминала при выходе

Горячие клавиши: `q`=Выход, `1-7`=Вкладка, `Alt+1-7`=Вкладка даже при вводе в Shell/URL, `j/k`=Навигация, `K`=Убийство процесса, `r`=Обновить, `s`=Запись телеметрии, `x`=Статус системы, `F1`/`?`=Справка, `:`=Команда Shell, ↑/↓=История Shell, Вкладка Web: `g`=фокус URL, `o`=Открыть ссылку, `Esc`=снять фокус

---

## Слой 5: Безопасность (`aios-watchdog`, `aios-security`, `aios-context`)

### Watchdog и аварийное восстановление (`aios-watchdog`)

AI-оркестратор никогда не должен стать единственной точкой отказа. Watchdog отслеживает работоспособность оркестратора через криптографические heartbeat-сигналы.

#### Протокол Heartbeat (`heartbeat.rs`)

`Heartbeat` — сигнал работоспособности, аутентифицированный через SHA-256 HMAC:
- `sequence: u64` — монотонный счётчик
- `timestamp_ms: u64` — время создания
- `source_hmac: [u8; 32]` — HMAC от (секрет + последовательность + временная метка)

Проверка: `heartbeat.verify(secret)` пересчитывает HMAC и сравнивает.

#### Watchdog (`watchdog.rs`)

`Watchdog` — мониторинг работоспособности оркестратора с настраиваемыми порогами:

**Состояния:**
```
Monitoring → Suspended → Recovering → Monitoring (при получении heartbeat)
                                         ↓ (тайм-аут)
                                       SafeMode
```

**Конфигурация** (`WatchdogConfig`):
- `heartbeat_interval_ms` — ожидаемая частота heartbeat (по умолчанию: 1000мс)
- `max_missed_heartbeats` — количество пропусков подряд перед приостановкой (по умолчанию: 3)
- `recovery_timeout_ms` — время ожидания восстановления до безопасного режима (по умолчанию: 10с)
- `secret` — секретный ключ HMAC

**Цикл проверки** (`check_timeout()`):
- **Monitoring**: Если возраст heartbeat > интервала, инкремент счётчика пропусков. При максимальных пропусках → `SuspendOrchestrator`
- **Suspended**: Переход в `Recovering`, возврат `AttemptRecovery`
- **Recovering**: Если возраст heartbeat > тайм-аута восстановления → `EnterSafeMode`. Если heartbeat получен → возврат в `Monitoring`
- **SafeMode**: Возврат `InSafeMode`

**Действия восстановления** (`WatchdogAction`):
- `None` — действие не требуется (серьёзность 0)
- `WaitForRecovery` — ожидание heartbeat во время восстановления (серьёзность 1)
- `SuspendOrchestrator` — приостановка выполнения оркестратора (серьёзность 2)
- `AttemptRecovery` — начало последовательности восстановления (серьёзность 3)
- `KillProcess(pid)` — завершение конкретного процесса по PID (серьёзность 4)
- `DumpState(path)` — сериализация состояния системы в файл с таймстампом (серьёзность 5)
- `EnterSafeMode` — переход в безопасный режим (серьёзность 6)
- `SafeModeShell` — порождение детерминированного CLI shell (серьёзность 7)
- `InSafeMode` — уже в безопасном режиме (серьёзность 8)

`is_terminal()` возвращает true для `KillProcess`, `DumpState`, `EnterSafeMode`, `SafeModeShell`, `InSafeMode`.

**Эскалация** (`escalate_actions()`): контекстно-зависимое восстановление на основе текущего состояния:
- **Suspended**: `KillProcess(0)` + `DumpState(с_таймстампом)`
- **Recovering**: `DumpState(с_таймстампом)`
- **SafeMode**: `DumpState(с_таймстампом)` + `SafeModeShell`

**События** (`WatchdogEvent`): аудит-журнал всех переходов состояний с временными метками.

#### Оболочка безопасного режима (`safe_mode.rs`)

`SafeModeShell` — детерминированная CLI для восстановления системы, когда AI-оркестратор приостановлен:

**Команды:** `ps`, `blocks`, `kill <pid>`, `unload <id>`, `status`, `logs`, `restart`, `help`, `exit`

**Ограничение перезапусков:** Настраиваемый `max_restarts` предотвращает бесконечные циклы перезапуска.

---

### Безопасность на основе возможностей (`aios-security`)

Модель нулевого доверия: ни один блок не является доверенным по умолчанию. Все операции требуют явных токенов возможностей.

#### Токены возможностей (`capability.rs`)

`Capability` enum — 15 конкретных разрешений:
- Сеть: `NetBind`, `NetConnect`, `NetListen`
- Файловая система: `FsRead`, `FsWrite`, `FsDelete`
- Оборудование: `HwAccess`
- Память: `MemAlloc`, `MemShare`
- Система: `SchedModify`, `BlockLoad`, `BlockUnload`, `ProcessSpawn`, `ProcessKill`, `SystemConfig`
- Переопределение: `All` (предоставляет все возможности)

`CapabilityToken` — подписанный грант разрешений:
- `block_id: u32` — какой блок владеет этим токеном
- `capabilities: Vec<Capability>` — предоставленные разрешения
- `issued_at_ms / expires_at_ms` — временная привязка действия
- `issuer_signature: [u8; 32]` — SHA-256 HMAC полей токена

#### Слой управления доступом (`access_control.rs`)

`AccessControlLayer` — центральное управление токенами:
- `issue_token(block_id, capabilities)` — создание и сохранение токена
- `check_permission(block_id, required)` — проверка возможности (возвращает `Result`)
- `try_check_permission(block_id, required)` — проверка + запись нарушений
- `revoke_token(block_id)` — удаление токена
- `clean_expired()` — удаление просроченных токенов
- `violations: Vec<Violation>` — аудит-журнал попыток несанкционированного доступа

#### Песочница (`sandbox.rs`)

`Sandbox` — изолированная среда выполнения для каждого блока:
- `check_syscall(name, required_cap)` — валидация каждого системного вызова относительно разрешённых возможностей
- `allocate_memory(bytes)` — обеспечение ограничений памяти
- `max_syscalls` — ограничение количества системных вызовов для предотвращения бесконечных циклов
- Состояния: `Created → Running → Terminated` или `→ Violated`

При нарушении: песочница завершает блок и уведомляет AI-оркестратор для изоляции/отката.

---

### Персистентный системный контекст (`aios-context`)

Локальное встроенное хранилище для исторической осведомлённости системы. 100% zero-cloud, работает полностью на устройстве.

#### Хранилище контекста (`store.rs`)

`EmbeddedContextStore` — единый доступ ко всем коллекциям данных:
- `telemetry()` / `telemetry_mut()` — история метрик CPU/RAM
- `workflows()` / `workflows_mut()` — изученные пользовательские паттерны
- `stability()` / `stability_mut()` — оценки надёжности блоков

#### Телеметрия (`telemetry.rs`)

`TelemetryStore` — временные ряды метрик с переполнением FIFO (по умолчанию: 10 000 записей):
- `record(entry)` — сохранение метрики с временной меткой, опциональными block_id и process_name
- `query_metric(name)` — фильтрация по имени метрики
- `query_range(start_ms, end_ms)` — запрос по временному диапазону
- `query_by_block(block_id)` — метрики по блоку
- `average_value(name)` — вычисление среднего
- `peak_ram()` — зафиксированный максимальный объём использования RAM

#### Паттерны рабочих процессов (`workflow.rs`)

`WorkflowStore` — изученные профили приоритетов:
- `record(name, trigger_blocks)` — отслеживание паттернов использования
- `most_used()` — наиболее часто используемый рабочий процесс
- `WorkflowProfile.set_priority(process, priority)` — рекомендации приоритетов на основе обучения

#### Оценки стабильности (`stability.rs`)

`StabilityStore` — отслеживание исторической надёжности для каждого бинарного файла блока:
- `record(score)` — upsert по (block_name, version)
- `best_version(block_name)` — наивысшая оценка стабильности для решений об откате
- `record_crash()` — снижение оценки на 0.1 (минимум: 0.0)
- `record_uptime(ms)` — повышение оценки на 0.01 (максимум: 1.0)
- `is_healthy()` — оценка >= 0.5

---

## Потоки данных между крейтами

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

Весь обмен данными между блоками осуществляется через `IpcPacket` посредством `IpcBus`. Между блоками не передаются прямые указатели на память. Сериализация состояния использует `Vec<u8>` для максимальной портируемости.

---

## Мультибинарная совместимость (`aios-exec-compat`)

### Парсер заголовков (`format.rs`)

`ExecutableType` — идентификация формата бинарника по magic bytes:
- `from_bytes(data: &[u8])` — `MZ`→PE, `\x7fELF`→ELF, `AIOS`→нативный
- `from_extension(path)` — .exe/.dll→PE, .so/.elf→ELF, .aib→AIOS

`BinaryHeader::parse(data)` — извлечение: entry_point_offset, is_64bit, machine_arch, subsystem

**Возможности по ExecutableType**:
- `AiosNative`: без ограниченных capabilities (нативное выполнение)
- `LinuxElf`: FilesystemRead/Write, ProcessCreate, NetworkAccess
- `WindowsPe`: все LinuxElf + RegistryAccess, WinApiCompat

### POSIX подсистема (`posix.rs`)

`PosixTranslator` — трансляция Linux syscall в IPC-пакеты AIOS:
- 18 вариантов syscall: файловый I/O, процессы, память, сеть
- `translate(request)` → `SyscallResponse` с result/errno/out_data
- `translate_to_ipc(request)` → `IpcPacket` с `Payload::Custom`

### Win32 подсистема (`win32.rs`)

`Win32Translator` — маппинг Win32/NT API на маршруты ядра AIOS:
- 16 вариантов API: файл, процесс, память, синхронизация
- Диспатч по ordinal (стандартные SSN Windows)
- Регистрация DLL для отслеживания зависимостей

### Исцелитель зависимостей (`dependency_healer.rs`)

- `scan_dependencies()` — сканирование импортируемых символов по путям поиска
- `heal_dependencies()` — комбинированный пайплайн сканирования + автозагрузки
- Кэш резолюции, настраиваемые пути поиска, автоматическая загрузка в sandbox

### Совместимость песочницы (`sandbox_compat.rs`)

- `CompatSandboxConfig` — лимиты по типу: память, файлы, потоки, capabilities
- `CompatProcess` — проверка capabilities, ограничения ресурсов, блокировка syscall
- `CompatSandboxManager` — управление жизненным циклом с лимитом процессов

---

## WebAssembly рантайм (`aios-wasm`)

### Песочница (`sandbox.rs`)

`WasmSandbox` — движок песочницы на базе Wasmtime:
- Потребление топлива и эпохальное прерывание для ограничения ресурсов
- `SandboxConfig`: лимиты страниц памяти, топлива, максимальные экземпляры, тайм-аут

### Жизненный цикл блока (`block.rs`)

`WasmBlock` — управление жизненным циклом WASM-блоков:
- Компиляция из сырых байтов или WAT-текста
- Инстанцирование с `SandboxConfig`
- Вызов экспортируемых функций с типизированными параметрами
- `MemoryStats`: лимиты памяти/топлива и статус инстанцирования

### Фильтрация WASI (`wasi_filter.rs`)

`WasiFilter` — фильтрация WASI-системных вызовов с политиками для каждого вызова:
- `WasiPolicy`: `Allow`, `Deny`, `Log` для каждого syscall
- Предустановленные конфигурации: `permissive()`, `restrictive()`, `no_network()`

### Изоляция (`isolation.rs`)

`IsolationConfig` — изоляция «без общего контента» с уровнями:
- `None`, `Process`, `Memory`, `Network`, `Full`
- `ResourceLimits`: лимиты памяти, CPU-времени, хранилища, сети и файлов на блок
- `IsolationBoundary`: реестр изоляции по блокам с управлением межблочной коммуникацией

---

## Сетевой стек (`aios-net`)

- **Крейт**: `aios-net` v1.0.0 — TCP/UDP блоки для сетевого взаимодействия
- **TCP** (`tcp.rs`): `TcpBlock`, `TcpConfig`, `TcpConnection`, `TcpMessage`, `TcpState` — mock state machine
- **Real TCP** (`real_tcp.rs`): `RealTcpBlock` — реальные `std::net::TcpListener`/`TcpStream` с неблокирующим accept, управлением соединениями, отправкой/получением, опциональным enforcement `CapabilityToken` (`CAP_NET_BIND` для `start_listening()`, `CAP_NET_CONNECT` для `connect()`)
- **UDP** (`udp.rs`): `UdpBlock`, `UdpConfig`, `UdpPacket`, `UdpState` — mock state machine
- **Real UDP** (`real_udp.rs`): `RealUdpBlock` — реальный `std::net::UdpSocket` с `bind()`, неблокирующим `send_to()`/`receive_from()`, broadcast через `SO_BROADCAST`
- Отслеживание соединений, отправка/получение через каналы, широковещание, статистика
- 40 тестов для mock + реального жизненного цикла TCP/UDP

---

## Абстракция файловой системы (`aios-core`)

- `FileSystem` — единый слой доступа к файлам (виртуальная, локальная, наложенная)
- `FilePermissions` — чтение/запись/выполнение
- `FileEntry` — путь, размер, is_dir, права
- Виртуальная: хранилище в памяти с проверкой прав
- Локальная: доступ к файлам через корневой путь
- Наложенная: виртуальный слой поверх локальной
- 20 модульных тестов

---

## Маркетплейс (`aios-block-mgr`)

- `BlockMarketplace` — реестр блоков с управлением репозиториями
- `BlockMetadata` — имя, версия, описание, автор, sha256, теги
- `RepositoryEntry` — метаданные + статус (Available/Installed/UpdateAvailable/Deprecated)
- Публикация, поиск, установка, удаление, проверка обновлений
- 18 модульных тестов

---

## Архитектура тестирования

- **Модульные тесты**: 708 тестов, встроенных в исходные файлы под `#[cfg(test)] mod tests`
- **Интеграционные тесты**: 28 тестов в `tests/integration_test.rs`
- **Стресс-тесты**: 11 тестов в `tests/stress_test.rs`
- **Итого**: 708 тестов, все проходят, ноль предупреждений clippy

**Пороги скорости**:
- Debug-режим: < 50us (без оптимизации)
- Release-режим: < 1us (с оптимизацией)

**Мок-профили оборудования**: Все тесты используют мок-профили и никогда не требуют реального оборудования.

---

## Модель безопасности (текущая)

- Контрольные суммы SHA-256 на всех бинарных файлах блоков (проверка целостности)
- Конечный автомат блока предотвращает некорректные переходы
- Ограничение квоты RAM предотвращает исчерпание памяти
- Ограничение количества сбоев предотвращает бесконечные циклы перезапуска
- Заморозка IPC-шины предотвращает потерю сообщений при горячей замене
- **Обратное давление IPC-шины** предотвращает неограниченный рост очереди (Reject или DropOldest)
- **Дедупликация IPC-шины** предотвращает обработку дублирующихся сообщений
- **Метрики IPC-шины** для операционной видимости
- **Watchdog** мониторит работоспособность AI-оркестратора через HMAC heartbeat; входит в безопасный режим при сбое
- **Токены возможностей** с временной привязкой действия и HMAC-подписями
- **Слой управления доступа** проверяет каждый системный вызов относительно возможностей токена
- **Песочница** обеспечивает ограничения памяти, подсчёт системных вызовов и проверку возможностей для каждого блока
- **Перехват нарушений** завершает блоки при несанкционированном доступе
- **Граф зависимостей блоков** предотвращает загрузку блоков до их зависимостей
- **Семантическое версионирование** обеспечивает совместимость обновлений блоков
- **Обнаружение недостатка памяти** оповещает при превышении порогов использования RAM

**Ещё не реализовано**: полная интеграция WebAssembly runtime. См. `docs/TODO.md`.

---

## Дорожная карта разработки (Фазы 22–27)

### Фаза 24: EasyLang Engine и No-Code App Builder (`aios-builder`) — *ЗАВЕРШЕНО*
- **Крейт `aios-builder`**: тип Workflow (JSON-сериализуемый), AutoManifestGenerator (анализ WASM-бинарников + ключевой анализ интентов workflow для вывода capability), WorkflowCompiler (генерация WAT-текста → компиляция в WASM через `wat`)
- **EasyLangParser**: построчный DSL (`spawn`, `timer`, `load`, `unload`, `kill`, `query`, `compact`, `status`)
- **18 unit-тестов**: парсинг WASM, обнаружение capability, генерация JSON-манифеста, компиляция workflow, парсинг DSL

### Подпункт Фазы 24: Backend выполнение workflow
- **`POST /api/v1/workflow`**: Эндпоинт пакетного выполнения интентов — принимает `{prompts: [...]}`, парсит и выполняет каждый шаг последовательно, возвращает результаты с проверкой capability
- **Интеграция с Builder**: `runWorkflow()` отправляет один batch-запрос вместо N отдельных интентов

### Фаза 22: Универсальный Web и Desktop UI (`aios-studio`) — *ЗАВЕРШЕНО*
- **Smart Command Palette:** Поле ввода (`Ctrl+K`) с автодополнением намерений, отправка `POST /api/v1/intent`
- **Дашборд телеметрии:** WebSocket Canvas-графики RAM, таблица процессов, карточки здоровья
- **Capability Consent Center:** Визуальный список блоков с индикацией прав и кнопками быстрых действий
- **Вкладка Easy Builder:** Визуальный step-редактор workflow — палитра блоков (триггеры/действия), добавление/удаление/перестановка, последовательное выполнение как интенты
- **Раздача статики:** `tower-http::ServeDir` fallback из `aios-bridge` по `/`

### Фаза 23: Многорежимный AI-движок (`aios-llm`) и гибридный маршрутизатор намерений
Три адаптивных режима в зависимости от аппаратных ресурсов:

- **Cloud-First (Zero-Resource) — для ПК 2–4 ГБ RAM**
  - 0 МБ нагрузки на диск/RAM для локальных моделей
  - Анонимизация запросов: локальное ядро → внешние AI-провайдеры (Groq, OpenRouter, Google AI Studio)
  - Вырезка geo-маркеров и удаление персональных ID

- **Micro-Local (Гибрид) — для ПК 4–8 ГБ RAM**
  - Локальная микро-модель (SmolLM/Qwen-0.5B, ~300 МБ RAM) для офлайн-парсинга команд
  - Облачный fallback для тяжёлых задач
  - Автоматическое переключение режимов по доступности сети

- **Full-Local (Автономный) — для ПК 8+ ГБ RAM**
  - Квантованные локальные модели 3B–7B (GGUF INT4/FP8) с заморозкой KV-Cache
  - Сжатие ZSTD холодного KV-Cache (~300–500 МБ)
  - Фоновый прогрев и сжатие кэша

### Фаза 24: EasyLang Engine и No-Code Builder (`aios-builder`)
- **In-Memory EasyLang Compiler:** Микро-компилятор в ядре, переводящий декларативный текст (RU/EN, ~10 ключевых фраз) в `.wasm` за миллисекунды
- **Auto-Manifest Generator:** Автоматический анализ и генерация минимальных `CapabilityToken`
- **Визуальный редактор workflow:** Встроен в `aios-studio` — «Когда событие X → Выполни действие Y» drag-and-drop

### Фаза 25: Безопасный веб-сёрфинг и поиск (`aios-browser` и `aios-search`) — *ЗАВЕРШЕНО*
- **Крейт `aios-browser`**: `BrowserEngine` с `navigate(url)` → HTTP-запрос через `reqwest`, парсинг HTML через `HtmlParser`, рендеринг в текст через `Renderer`
  - `HtmlParser`: извлекает текст, ссылки, заголовки; удаляет `<script>`, `<style>`, HTML-комментарии
  - `NetworkClient`: настраиваемые user-agent, таймаут, лимит редиректов; изолированный сетевой доступ
  - `Renderer`: DOM → markdown-подобный текст (заголовки `#`, ссылки `[text](url)`, списки `•`)
  - `Page`: `url`, `title`, `text_content`, `html`, `links: Vec<Link>`
  - `BrowserConfig`: `user_agent`, `timeout_secs`, `max_redirects`, `sandbox_enabled`
  - **11 unit-тестов**: извлечение текста, парсинг ссылок, заголовков, URL-резолвинг, удаление head/комментариев
- **`BrowserBlock` (интеграция блока в ядро)**: `BrowserBlock` реализует `StatefulBlock` в `aios-browser/src/block.rs` и регистрируется при загрузке во всех бинарниках (`aios`, `aios-tui`, `aiosd`)
  - IPC-команды: `browse` (загрузка и парсинг страницы, возвращает bincode-сериализованный `Page`), `open_native` (открыть URL в системном браузере через крейт `open`), `browser_status` (конфиг + состояние в JSON); поддерживается `HealthCheck`
  - Не хранит постоянный рантайм — каждая навигация выполняется на выделенном однониточном Tokio-рантайме, безопасно и из sync-, и из async-контекста (без паники вложенного рантайма)
  - Извлечение/восстановление состояния через bincode (`BrowserConfig` + `BlockState`)
- **Крейт `aios-search`**: `SearchEngine` с мульти-бэкендным анонимным поиском + AI-суммаризация
  - `DuckDuckGoBackend`: POST на `html.duckduckgo.com/html/`, парсинг HTML-ответа
  - `SearXngBackend`: GET с `format=json`, парсинг JSON-ответа
  - `BraveBackend`: GET на `api.search.brave.com`, API-ключ в `X-Subscription-Token`, парсинг JSON
  - `SearchSummarizer`: интеграция с `aios-llm` для TL;DR (2-3 предложения по топ-5 результатам)
  - `SearchConfig`: `backend`, `api_key`, `api_url`, `max_results`, `enable_summary`
  - **3 unit-теста**: конфиг по умолчанию, создание движка, URL бэкендов
- **REST-эндпоинты aios-bridge**:
  - `POST /api/v1/browse` — `{"url": "..."}` → title, text_content, links
  - `POST /api/v1/search` — `{"query":"...","backend":"...","max_results":N,"enable_summary":bool}` → результаты + AI-краткое содержание

### Фаза 28: Headless Daemon (`aios-daemon`) — *ЗАВЕРШЕНО*
- **Крейт `aios-daemon`**:
  - Бинарник `aiosd`: headless-сервер с той же инициализацией, что и `aios-tui`, без терминала
  - Загружает встроенные блоки (hal, ipc_bus, scheduler) и дисковые блоки из `AIOS_BLOCKS_DIR`
  - Открывает persistent store (`redb`) в `AIOS_DATA_DIR/context.redb`
  - Запускает системные процессы (ai_orchestrator, io_handler, health_monitor)
  - Запускает поток heartbeat watchdog
  - Фоновый цикл: heartbeat (процессы, RAM, watchdog) каждые 10с, сохранение телеметрии каждые 60с
  - Минимальные зависимости: без ratatui, crossterm, egui, wasmtime
  - Конфигурация через `AIOS_DATA_DIR`, `AIOS_BLOCKS_DIR`, `AIOS_MOCK_PROFILE`, `RUST_LOG`
- **Headless-режим `aios-tui`**:
  - Флаг `--headless` и `AIOS_HEADLESS=1`: пропускает TUI-инициализацию, работает в фоне
- **Docker**:
  - Dockerfile собирает только `aios-daemon` (~2мин), CMD — `aiosd`
  - `docker-compose.yml`: daemon по умолчанию, профиль `interactive` для TUI
  - Размер образа уменьшен с ~800MB до ~120MB

### Фаза 26+27: Атомарные обновления, магазин, телеметрия и отладка (`aios-updater`, `aios-store`, `aios-telemetry`, `aios-debug`) — *ЗАВЕРШЕНО*
- **Крейт `aios-updater`**:
  - `DualBootManager`: Управление слотами A/B с `swap()`, `boot_success()`, `detect_active_slot()`, информацией о слотах
  - `HotSwapEngine`: Отслеживание горячей замены по BlockId со счётчиком; обёртка над aios-live-update
  - `RollbackManager`: Откат на основе снимков с настраиваемым таймаутом (по умолчанию 1с автооткат), очистка снимков
  - **12 unit-тестов**: создание слотов, переключение, успешная загрузка, горячая замена, откат
- **Крейт `aios-store`**:
  - `ManifestInfo`: name, version, description, author, capabilities (HashSet), wasm_sha256, signature (Ed25519), store_url
  - `ManifestValidator`: Валидация SHA-256, проверка подписи Ed25519, белый список capability
  - `StoreRegistry`: HashMap по ключу name@version с `register()`, `get()`, `find_all()`, `list()`, `unregister()`
  - `StoreClient`: HTTP-клиент с `fetch_index()` и `download_block()` для удалённого магазина
  - **9 unit-тестов**: валидация SHA-256, валидация capability, CRUD реестра
- **Крейт `aios-telemetry`**:
  - `TraceContext`: Дерево спанов с `begin_span()`, `end_span()`, `set_tag()`, `set_status()`, `to_json()` (JSON-экспорт)
  - `FlightRecorder`: Кольцевой буфер с фильтрацией по типу, настраиваемыми max_events + retention_secs, `dump()` и `dump_by_kind()`
  - `MetricCollector`: Счётчики, датчики, гистограммы с `snapshot()` (MetricSnapshot) и `to_prometheus()` (формат Prometheus)
  - **17 unit-тестов**: вложенность спанов, статус ошибки, экспорт JSON, запись/дамп/очистка регистратора, все типы метрик
- **Крейт `aios-debug`**:
  - `CrashReporter`: Генерирует отчёты с опциональным zero-knowledge режимом (хеширование, без данных полёта)
  - `CrashKind`: Panic, WatchdogTimeout, OOM, BlockCrash, Unknown
  - `PanicHandler`: Кастомный хук паники через std::panic::set_hook, направляет информацию в CrashReporter
  - **6 unit-тестов**: генерация отчёта, zero-knowledge, экспорт JSON, последний/все отчёты
- **REST-эндпоинты aios-bridge**:
  - `GET /api/v1/store/index` — список всех манифестов
  - `POST /api/v1/store/register` — регистрация манифеста
  - `GET /api/v1/metrics` — метрики в формате Prometheus
  - `GET /api/v1/traces` — текущий TraceContext в JSON
  - `POST /api/v1/crash-report` — создание отчёта об аварии
- **BridgeContext** расширен: `StoreRegistry`, `MetricCollector`, `FlightRecorder`, `TraceContext`, `CrashReporter`, `PanicHandler`

---

## Слой 6: Интегрированный бинарник (`aios/`)

### Обзор
Крейт `aios` — это единый системный бинарник, объединяющий все 17+ крейтов рабочего пространства в один исполняемый файл. Предоставляет:
- Интерактивную TUI-панель (ratatui) для мониторинга и управления системой
- Headless-режим демона для развёртывания в Docker/фоне
- Реальное определение оборудования при запуске
- Централизованную оркестрацию всех подсистем

### Модули
- `hw_probe.rs` — Реальное обнаружение оборудования через sysinfo + платформенные API
- `orchestrator.rs` — Асинхронная инициализация IPC, Scheduler, BlockRegistry, AccessControl, Watchdog, LLM, WASM, Bridge
- `tui/` — Интерактивная TUI-панель на Ratatui с 4 вкладками и журналом событий

### Режимы бинарника
- `aios` — Интерактивный TUI-режим (по умолчанию)
- `aios --daemon` — Headless-режим демона (фоновый сервер)

### Горячие клавиши TUI
| Клавиша | Действие |
|---------|----------|
| Tab / F1 | Следующая вкладка |
| 1-4 | Прямой выбор вкладки |
| Alt+1-4 | Прямой выбор вкладки даже при активной строке URL браузера или строке AI-запроса |
| q | Выход |
| g | Открыть URL моста в браузере |
| b | Открыть URL в системном браузере (режим ввода URL, команда уходит в браузерный блок через MessageRouter) |
| r | Переопределить оборудование |
| Space | Пауза/возобновление прокрутки логов |

### Последовательность запуска
1. Определение оборудования (CPU, RAM, GPU, ОС)
2. Инициализация IPC-шины (SharedIpcBus)
3. Создание Scheduler с RAM-ориентированной конфигурацией
4. Инициализация BlockRegistry — регистрация базовых блоков (hal, ipc_bus, scheduler, browser), boot-обнаружение `AIOS_BLOCKS_DIR` (по умолчанию `./blocks`), регистрация IPC-обработчика браузерного блока в `MessageRouter`
5. Настройка AccessControl + Watchdog
6. Инициализация LLM Engine (облачный бэкенд по умолчанию)
7. Инициализация WASM Executor (BlockExecutor)
8. Создание BridgeContext со всеми подсистемами
9. Запуск Bridge HTTP-сервера (axum, порт 8080)
10. Запуск цикла событий TUI (или цикла демона)

Браузер работает «из коробки» на новом компьютере: для запуска не нужны ни конфиг-файлы, ни установленный браузер, ни сеть — блок активен в топологии, доступен по IPC, а клавиша `b` открывает любой URL в системном браузере.
