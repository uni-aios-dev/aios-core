# Архитектура AIOS

## Обзор системы

AIOS (AI-Native Operating System) — модульная ОС в стиле микроядра, разработанная для AI-ориентированных нагрузок. Система состоит из 33 Rust-крейтов, образующих слоистую архитектуру: базовые типы внизу, абстракция оборудования и управление процессами в середине, системы безопасности/контекста, и пользовательские интерфейсы (TUI/GUI/Единый бинарник) наверху.

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
- `set_cpu_affinity(pid, cores)` — сохраняет маску привязки на поток (вызов ОС действует на вызывающий поток, поэтому должен выполняться в целевом потоке)
- `get_cpu_affinity(pid)` — запрашивает текущую привязку для потока
- `available_cpu_cores()` — возвращает количество доступных ядер
- `validate_cores(cores)` — предварительная проверка маски перед сохранением
- Платформа: `SetThreadAffinityMask` (Windows), `sched_setaffinity` (Linux), no-op fallback
- Модель применения: порождённый поток процесса сам читает сохранённую маску (`Arc<Mutex<Vec<usize>>>`) и применяет её перед запуском payload, поэтому поток планировщика никогда не перепривязывается

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

## Слой 4: Пользовательский интерфейс (TUI ядра `aios` + `aios-gui`)

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

### Панель управления (`aios/src/tui`)

TUI ядра на базе Ratatui с компоновкой из 7 вкладок по спецификации (бинарник `aios`):

**Зона заголовка**:
- Название проекта "AIOS v2.9.1"
- Обнаруженный AI-уровень с цветовой кодировкой: Tier1=Зелёный, Tier2=Жёлтый, Tier3=Красный
- Бейдж `SAFE MODE` (жёлтый) при загрузке с `--safe-mode`
- Состояние watchdog: OK (Зелёный), SUSPENDED (Красный), RECOVERING (Жёлтый), SAFE MODE (Маджента)
- Ядра CPU, использование RAM, количество блоков и процессов

**Зона вкладок**: 7 вкладок — System & HW | Blocks & Svc | AI Console | Studio Bridge | Network & Store | Web | Shell. Выбор через `1`-`7`, `Alt`+`1`-`7` (работает даже при вводе), `Tab`/`F1` циклирует, `?` включает справку.

**Вкладка 1 — System & HW**: модель CPU, ядра/потоки, флаги AVX, имя GPU/VRAM, хранилище, AI Tier; шкала RAM; журнал активности (цветовая кодировка: Красный=ошибка, Жёлтый=предупреждение, Зелёный=успех)

**Вкладка 2 — Blocks & Svc**: таблица блоков (ID, Имя, Версия, Состояние, Размер) с выбором `j`/`k`; клавиши `r`=перезапуск, `k`=выгрузка, `l`=загрузка с диска (запрос пути); внизу — выбранный блок и список процессов

**Вкладка 3 — AI Console**: чат с LLM, `i` — режим запроса, `Enter` — отправка, `Esc` — выход, `Up`/`Down` — история промптов (последние 50), `h` — панель справки; слэш-команды `/help /status /clear /history /system /model /backend /key /temp /tokens /preset /save /load`; внизу — бэкенд/модель/температура/токены/состояние; вывод переносится по ширине с циановыми промптами, красными ошибками и жёлтой живой строкой стриминга; **ответы стримятся** через `LlmEngine::query_stream`; чат автосохраняется в JSON Lines в `AIOS_DATA_DIR/chat.jsonl` (сохранение после каждого ответа и при выходе, восстановление при старте); `/preset` управляет шаблонами системного промпта; изменения бэкенда/модели/ключа асинхронно пересоздают общий `LlmEngine`

**Вкладка 4 — Studio Bridge**: состояние моста (запущен/выключен), URL, REST/WebSocket эндпоинты

**Вкладка 5 — Network & Store**: редактор сетевой конфигурации (`n` = ввод `key=value`, применяется по IPC `net_set`; `g` = показать JSON конфигурации) плюс список установленных блоков (`s` = обновить); те же операции доступны из Shell как `net get`/`net set`/`store list`/`store search`/`store install`

**Вкладка 6 — Web**: омнибокс (полный URL / голый хост / запрос DuckDuckGo), переносимый по ширине текст страницы, прокручиваемый список ссылок. Клавиши: `g`=фокус омнибокса, `Enter`=навигация, `j/k`=выбор ссылки, `o`/`Enter`=открыть ссылку, `u/d`=прокрутка ±1 строка, `PageUp`/`PageDown`=прокрутка ±20 строк, `b`=назад в истории, `B`=открыть текущую страницу в нативном WebView, `n`=открыть выбранную ссылку нативно, `Esc`=снять фокус. Загрузки идут **в фоне** (никогда не блокируют TUI) со счётчиком поколений, отбрасывающим устаревшие результаты; ограниченный кэш на 20 страниц делает повторные визиты и навигацию назад мгновенными
- **Полноценный браузер по запросу**: `B`/`n` открывают страницу в настоящем окне `aios-webview` (WebView2 — JS/CSS/картинки). Handle живёт в модульном `OnceLock<Mutex<Option<WebBrowser>>>`, поэтому окно переиспользуется, пересоздаётся при закрытии, а открытие идёт в фоновом потоке

**Вкладка 7 — Shell** (вертикальное разделение):
- Строка ввода команд с индикатором приглашения
- Область вывода (прокручиваемый вывод команд)
- История команд через ↑/↓; `Esc` очищает текущую строку
- На вкладке Shell все нажатия идут в строку ввода, поэтому `q` выходит только с других вкладок
- `ShellState`: input_buffer, output (Vec<String>), command_history, history_pos
- Команды: `ps`, `blocks`, `kill <pid>`, `spawn <путь-wasm>`, `store list|search|install`, `net get|set`, `cluster status|nodes|spawn|kill|migrate`, `status`, `logs`, `restart`, `help`/`?`, `clear`
- Поток выполнения: TUI → `shell_execute()` → `SafeModeShell::parse_command` / store manager / IPC в блок `net_settings`
- Команды кластера: `cluster status`/`cluster nodes` показывают представление пиров (статус/tier/нагрузка) плюс удалённые и локально размещённые процессы; `cluster spawn <name> [ram_mb] [priority] [target_node]`, `cluster kill <node> <pid>` и `cluster migrate <node> <pid> [target_node]` управляют `DistributedScheduler` напрямую (spawn/kill/migrate блокирующие, до ack-таймаута). Без `AIOS_CLUSTER_PEERS` обработчик отвечает `clustering disabled`

**Справка F1**:
- Включение по F1 или '?', закрытие по F1/Esc/'?'
- Отображает все горячие клавиши и команды shell во всплывающем окне

**Зона подвала**: подсказки горячих клавиш (q=Выход, 1-7=Вкладка, Alt+1-7=Вкладка в любом режиме ввода, W=GUI, Space=Пауза лога, F1=Справка)

`OrchestratorState` управляет:
- Снимками процессов/блоков (берутся каждый кадр для согласованного рендеринга)
- Кольцевым буфером истории RAM (60 записей)
- Состоянием выбора (selected_tab, selected_row)
- Отображением результата операции с блоком + вводом загрузки с диска
- Буфером логов (ограничен 100 записями)
- Синхронизацией Scheduler + Registry
- Состоянием Web: url_input, current_url, page, loading, error, input_focused, scroll, history
- Сетевым состоянием: `net_status` (последний JSON конфигурации от блока `net_settings`)
- Состоянием справки (показана/скрыта)
- Состоянием Shell: input_buffer, output (Vec<String>), command_history, history_pos
- Флагом `safe_mode` из `--safe-mode`
- Handle планировщика кластера (`cluster: Option<Arc<Mutex<DistributedScheduler>>>`), удерживаемый живым при заданном `AIOS_CLUSTER_PEERS`; tick-поток кластера пушит события failover в журнал ядра

### Точка входа (`aios/src/main.rs`)

Последовательность запуска:
1. Инициализация `env_logger`
2. Разбор `--safe-mode` (и `--bridge-port`) в `AppConfig`
3. `HardwareProfile::detect()` — обнаружение реального оборудования
4. `AiTier::from_profile()` — классификация AI-возможностей, уровень логируется при загрузке
5. Создание `BlockRegistry` — регистрация базовых блоков, boot-обнаружение блоков на диске из `AIOS_BLOCKS_DIR` **кроме safe mode**, подключение браузерного блока к `MessageRouter`
6. Создание `Scheduler` — запуск 3 процессов (ai_orchestrator, io_handler, health_monitor)
7. Создание `Watchdog` — запуск потока heartbeat в фоне
8. Создание `EmbeddedContextStore` + `TelemetryStore` — для системной телеметрии
9. Создание `SafeModeShell` — для команд восстановления в безопасном режиме
10. Запуск моста HTTP/WS на заданном порту **кроме safe mode**
11. Вход в raw-режим crossterm + альтернативный экран
12. Цикл событий: опрос клавиатуры, перерисовка панели, синхронизация состояния watchdog
13. Восстановление терминала при выходе

Горячие клавиши: `q`/`Ctrl+C`=Выход, `1-7`/`Alt+1-7`=Вкладка (Alt работает при вводе в Shell/URL/AI/сеть), `Tab`/`F1`/`?`=следующая вкладка/справка, `W`=Запуск дашборда GUI, `Space`=пауза журнала, Blocks: `r`=перезапуск `k`=выгрузка `l`=загрузка, Web: `g`/`Enter`/`j`/`k`/`o`/`u`/`d`/`PageUp`/`PageDown`/`b`/`B`/`n`/`Esc`, Network: `n`=редактор `g`=показать JSON `s`=обновить хранилище, ↑/↓=История Shell

### Safe Mode

`--safe-mode` загружает минимальное восстанавливаемое ядро: сторонние блоки с диска не обнаруживаются, мост не запускается; в шапке показывается `SAFE MODE`. Ядро, планировщик, watchdog, LLM-движок, TUI и Shell остаются доступны.

### Нативный браузер WebView (`aios-webview`)

TUI не может отображать настоящие веб-страницы (нет движка CSS/JS), поэтому полноценный браузер — это **нативное окно** на `wry` (WebView2 на Windows, WebKitGTK на Linux, WKWebView на macOS) поверх событийного цикла `winit`:

- `WebBrowser::open(target)` запускает браузер на выделенном фоновом потоке; вызывающий получает дескриптор и никогда не блокируется
- Команды (`navigate`, `back`, `forward`, `close`) отправляются в событийный цикл браузера через `winit::EventLoopProxy` и применяются асинхронно
- Куки и хранилище сохраняются между перезапусками через `WebContext` с профильным каталогом (`AIOS_DATA_DIR`/`aios/webview` или системный каталог данных)
- `resolve_target()` реализует правило омнибокса, общее с TUI: полный `http(s)`-URL → как есть, голый хост → `https://`, всё остальное → запрос DuckDuckGo (HTML-версия)
- Модуль `launcher` находит бинарник `aios-gui` (рядом с текущим исполняемым файлом, затем PATH) и запускает дашборд GUI

### Графический дашборд (`aios-gui`)

Нативный дашборд на egui/eframe с 8 вкладками: System Dashboard, WASM Blocks, AI Studio, App Store, Network Settings, Deps, Native Browser, Files. Горячая клавиша `W` в обоих TUI запускает дашборд GUI через `aios_webview::launcher::launch_gui()`.

- **System Dashboard (F1)**: карточки статистики (RAM, блоки, процессы, watchdog), панель системы (CPU/GPU/хранилище/HW Tier), спарклайн RAM, распределение приоритетов, таблица процессов (PID, Имя, Приоритет, Состояние, RAM, CPU ms, Сбои) с Обновить/Убить/Приостановить/Возобновить, журнал активности
- **WASM Blocks (F2)**: таблица блоков + Обновить / Загрузить (2-шаговый диалог) / Выгрузить / Горячая замена
- **AI Studio (F3)**: асинхронный чат с LLM — список сообщений, потоковые ответы (живая жёлтая частичная строка), отправка по Enter (фокус сохраняется), слэш-команды `/help /status /clear /history /system /model /backend /key /temp /tokens /preset /save /load`, строка статуса (бэкенд/модель/температура/токены/busy); запросы стримятся через фоновую tokio-задачу, интерфейс остаётся отзывчивым. Чат автосохраняется в общий `AIOS_DATA_DIR/chat.jsonl`, а шаблоны `/preset` — в `AIOS_DATA_DIR/presets.json` (те же файлы, что и у AI Console TUI)
- **App Store (F4)**: каталог с поиском и действиями Установить/Обновить/Удалить
- **Network Settings (F5)**: форма hostname/port/таймауты/private-access/DNS/user-agent с Save (частичное JSON-обновление по IPC в `net_settings`) и Reset, плюс живой JSON-предпросмотр
- **Deps (F6)**: сводка графа зависимостей, цепочка загрузки, таблица зависит/зависят от
- **Native Browser (F7)**: омнибокс, Back/Forward, переключатель Open/Close, управляющие нативным окном `aios-webview`; первая навигация автоматически открывает браузер
- **Files (F8)**: двухпанельный файловый менеджер (`aios-fm`) на `aios-vfs` — панель инструментов (Refresh/Switch/Sort/Up/Mkdir/Rename/View/Copy/Move/Delete, HOST r/w), панели с выбором кликом/двойным кликом, модальный диалог mkdir/rename, сворачиваемое AI-превью, живой прогресс задач и показ capability ACL
- **Строка состояния**: `HW Tier | IPC: N pkts | F6=Deps F7=Browser F8=Files` с живым счётчиком IPC-пакетов

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
- `timeout_ms` применяется как реальное время фоновым тикером `EpochTicker`: он инкрементирует эпоху движка каждые `timeout_ms / 4`, а каждый вызов wasm перевзводит дедлайн store (`EPOCH_TICKS_PER_TIMEOUT = 4`), ограничивая каждый вызов и сохраняя работоспособность долгоживущих store

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

## Виртуальная файловая система (`aios-vfs`) — v2.10.0

- **Крейт**: `aios-vfs` v1.0.0 — асинхронная VFS со схемами адресации, песочница для файлового менеджера.
- **Схемы**: `VfsScheme::{AIOS, HOST}`; `VfsPath` разбирает пути URI-стиля (`AIOS:///sandbox`, `HOST:///C:/...`) и даёт `parent()`, `join()`, `file_name()`, `to_uri()`.
- **Трейт** `VirtualFileSystem` (асинхронный, `tokio::fs`): `list`, `read`, `write`, `create_dir`, `delete`, `rename`, `exists`, `metadata`, `open_seek`. `open_seek` возвращает `Box<dyn AsyncSeekReader + Send + Unpin>`, где `AsyncSeekReader = AsyncRead + AsyncSeek` (используется для AI-превью).
- **Реализации**: `AiosVfs` (песочница в локальной папке с проверкой `canonicalize_inside`) и `HostVfs` (реальные пути хоста, чтение/запись ограничены ACL-токенами `vfs:host:read` / `vfs:host:write`).
- **Операции** (`operations.rs`): `Progress` (атомарные счётчики байтов/файлов, `fraction()`, `pressure_fraction()`), `CancellationToken`, `total_bytes`, `copy_recursive`, `move_item`, `delete_item`, `read_head`, `read_at`.
- **Безопасность** (`security.rs`): `AclContext` — потокобезопасный набор capability-токенов (`Mutex<HashSet>`); `canonicalize_inside(root, path)`.
- **AI-превью** (`ai_preview.rs`): `analyze_file(name, head)` → `AiPreview { title, headline, lines: Vec<(AiLineKind, String)> }`; разбирает WASM name-section, находит паники в логах, даёт подсказки по исходникам.
- 29 модульных тестов (отмена, байты WASM name-section, проверка путей, copy/move/delete, превью).

## Файловый менеджер (`aios-fm`) — v2.10.0

- **Крейт**: `aios-fm` v1.0.0 — движок двухпанельного (в стиле Volkov/Far) файлового менеджера + рендеры TUI и GUI.
- **Состояние** (`state.rs`): `PanelSide::{Left, Right}`, `PanelState` (путь, курсор, `SortRule::Name/Size/Date/Type`, записи), `human_size`.
- **Команды** (`commands.rs`): `Command` (Navigate/Refresh/Copy/Move/Delete/Mkdir/Rename/View/GrantHostRead/GrantHostWrite/Shutdown) и `Ack` через `tokio::mpsc::unbounded_channel`.
- **Движок** (`engine.rs`): `FileManager::new(fs, acl) -> (FileManager, UnboundedReceiver<Ack>)`; фоновый цикл команд; Copy/Move/Delete запускаются как отменяемые `tokio::spawn`-задачи с `Progress`; `FmSnapshot { panels, active, jobs, acl }`; прямые методы `send`, `snapshot`, `switch_panel`, `set_active`, `set_cursor`, `move_cursor`, `toggle_sort`, `selected`, `default_target`, `acl`, `fs`.
- **TUI-рендер** (`ui_tui.rs`): `draw(frame, area, &FmSnapshot, rows)` (шапка со схемой и ACL, две панели, футер с прогрессом задач и горячими клавишами), `key_to_action`, `progress_bar`.
- **GUI-рендер** (`ui_gui.rs`): `show(ui, &FmSnapshot, &FmTheme) -> Option<FmClick>` (две колонки, выбор кликом/двойным кликом, полосы прогресса, панель ACL).
- 16 модульных тестов (жизненный цикл движка, сортировка/навигация, keymap, GUI-тема).

---

## Многоузловой распределённый кластер (`aios-cluster`) — v2.11.0

- **Крейт**: `aios-cluster` v1.0.0 — распределённое планирование поверх `aios-process-mgr`. Узел запускает `DistributedScheduler` за `Arc<Mutex<...>>`; подключённый `ProcessExecutor` делает его **воркером** (может размещать удалённые процессы), узел без executor'а — чистый **координатор**.
- **Типы** (`types.rs`): `NodeId` (u64), `NodeStatus {Unknown, Online, Offline, Leaving}`, `NodeMetrics` (доля CPU, используемая/общая RAM, число процессов, `load_fraction()`), `NodeInfo` (id, имя, адрес, аппаратный `tier` 1–3), `RemoteProcessId { node, pid }` (глобально уникальная удалённая идентичность), `RemoteProcessSpec` (приоритет 0–4, квота RAM, опциональные block id / init payload / фильтры `[min_tier..=max_tier]`), `RemoteProcessStatus`, `PlacementStrategy {RoundRobin, LeastLoaded, ByTier}`.
- **Сетевой протокол** (`protocol.rs`): enum `ClusterMessage`, сериализуемый bincode и упакованный в кадры `[u32 LE длина][payload]`. Запросы несут `request_id` + `from`, чтобы ответы (SpawnAck/KillAck/SetPriorityAck/GetStateReply) можно было сопоставить с ожидающей операцией. `Spawn` опционально несёт снимок состояния процесса (`state: Option<Vec<u8>>`), восстанавливаемый на узле назначения после спавна; `GetState`/`GetStateReply` забирают этот снимок для миграции с переносом состояния.
- **Транспорты** (`transport.rs`): трейт `ClusterTransport` (`addr`, `send`, `start`, `shutdown`). `TcpClusterTransport` — реальный `std::net::TcpListener` на узел, подключается к пирам по требованию, один кадр на поток. `InMemoryClusterTransport` + `MemoryRegistry` маршрутизируют сообщения внутри процесса (детерминированные тесты / несколько планировщиков на одной машине).
- **Планировщик** (`scheduler.rs`): `DistributedScheduler` — фоновый **heartbeat-поток** рассылает `Hello(self_info)` пирам с заданным интервалом; пиры отвечают `Metrics`, так что каждый узел сходится к актуальной картине кластера. Живость: `last_contact` на узел; узел, молчащий дольше `failover_threshold`, переходит в `Offline` внутри `tick()`. Размещение фильтрует онлайн-узлы по диапазону tier, затем применяет стратегию (LeastLoaded по `load_fraction`, при равенстве — младший id). `spawn`/`kill`/`set_priority`/`get_state` — блокирующие вызовы, которые дренируют inbox до подходящего ack или `ack_timeout`. `migrate` перемещает отслеживаемый процесс на другой узел с переносом состояния: через `get_state` забирает снимок исходника, спавнит копию на назначении (явный узел или стратегия размещения, никогда не исходный) с восстановленным снимком и лишь затем убивает оригинал, поэтому при неудаче получения состояния или спавна исходник остаётся нетронутым; перенос обратно на исходный узел отклоняется, а лишняя копия повторно убивается. **Репликация контрольных точек**: каждый heartbeat-период воркер извлекает снимок каждого локально размещённого процесса и рассылает всем пирам fire-and-forget `Checkpoint { from, rid, state }`, так что любой координатор может восстановить состояние при failover. При потере узла `tick()` перезапускает отслеживаемые процессы узла в другом месте (`failover_respawn`), восстанавливая самый свежий реплицированный снимок. Полученные контрольные точки получают отметку времени и вычищаются по `checkpoint_ttl` внутри `tick()`, чтобы устаревшие снимки давно молчащего узла нельзя было случайно воскресить. **Авторитет метрик**: нагрузка известного узла обновляется только из отдельного сообщения `Metrics` — снимок из `Hello` используется только при первом появлении узла, поэтому устаревший idle-снимок не затирает живую нагрузку. Журнал событий ограничен (`events()`, последние 100).
- **Исполнители** (`executor.rs`): трейт `ProcessExecutor` (spawn/kill/set_priority/status/metrics/**extract_state**/**restore_state**). `MockProcessExecutor` — детерминированный, моделирует 16 GiB RAM для осмысленных долей нагрузки, засевает снимок состояния каждого процесса из `spec.payload`. `SchedulerProcessExecutor` — адаптер поверх реального `aios-process-mgr::scheduler::Scheduler`. Снимки состояния — непрозрачные байты; исполнители хранят их по процессам, heartbeat-поток реплицирует их как контрольные точки, и `migrate` переносит их между узлами.
- **Конфигурация** (`config.rs`): `ClusterConfig` из переменных окружения `AIOS_CLUSTER_*` или JSON (`node_id`, `node_name`, `addr`, `tier`, `peers`, `heartbeat_ms`, `failover_threshold_ms`, `failover_respawn`, `strategy`, `checkpoint_ttl_ms`). Возвращает `None`, если кластеризация не запрошена.
- **Тесты**: 21 unit (протокол 6, транспорт 2, исполнитель 2, планировщик 9, конфиг 2) + 10 интеграционных (`tests/scheduling.rs`: двухузловой spawn/kill, round-robin, least-loaded, TCP loopback, failover-перезапуск, управление приоритетом, миграция процессов, миграция с переносом снимка состояния, пути ошибок миграции, ошибки неизвестного узла/без пиров) + 1 doc-тест.

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
  - `HtmlParser`: построен на `scraper`/html5ever (WHATWG-совместимый) — извлекает текст, ссылки, заголовки; структурирует вывод с заголовками `#`/`###`, списками `•`/`1.`, `pre`/`br` без потерь, строками таблиц `|`, `hr`, изображениями как `[alt]`; удаляет `<script>`, `<style>`, `<head>`, `<iframe>` и скрытые элементы; ссылки резолвятся относительно базового URL и дедуплицируются, не-web-схемы отфильтрованы
  - `NetworkClient`: настраиваемые user-agent, таймаут, лимит редиректов; изолированный сетевой доступ
  - `Renderer`: DOM → markdown-подобный текст (заголовки `#`, ссылки `[text](url)`, списки `•`)
  - `Page`: `url`, `title`, `text_content`, `html`, `links: Vec<Link>`
  - `BrowserConfig`: `user_agent`, `timeout_secs`, `max_redirects`, `sandbox_enabled`, `headless_fallback` (включён по умолчанию)
  - **Headless render-to-text fallback** (модуль `headless`, v2.17.0): когда обычная загрузка не даёт читаемого текста (`looks_like_js_shell`, < 80 непробельных символов), движок запускает headless-браузер класса Chromium (`msedge`/`chromium`/`google-chrome`/`brave-browser`, переопределение `AIOS_HEADLESS_BROWSER`, `--no-sandbox` через `AIOS_HEADLESS_NO_SANDBOX`) с флагами `--headless --dump-dom --virtual-time-budget=5000`, лимит 4 МиБ, выполнение на блокирующем потоке с таймаутом 30 с; отрендеренный DOM принимается только когда `has_more_content` находит на 60+ непробельных символов больше, чем обычная загрузка, иначе исходный HTML остаётся авторитетным
  - **28 unit-тестов**: извлечение текста, парсинг ссылок, заголовков, URL-резолвинг, удаление head/комментариев, структура макета
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
  - `StoreSource` / `SourceKind`: три источника блоков — GitHub (`github:owner/repo`), локальная папка (`local:path`), HTTP-сервис обновлений (`http://host:port`)
  - `BlockInstaller`: установка на диск `{name}_{version}.wasm` + sidecar JSON в `AIOS_BLOCKS_DIR`; проверка SHA-256, `backup`/`rollback` (`.bak`), `check_updates`, семантический `cmp_version`
  - `StoreManager`: фасад над источниками и установщиком — `search`, `install`, `update` (автооткат при ошибке), `uninstall`, `rollback`, `parse_source_spec`, `block_on` (синхронные контексты)
  - **42 unit-теста**: URL источников, сканирование каталога, установщик, откат, сценарии менеджера
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
- **Эндпоинты сервиса обновлений** (Фаза 40, `aios-bridge`):
  - `GET /index.json`, `GET /store/index.json` — «сырой» каталог блоков с диска
  - `GET /blocks/{name}.wasm`, `GET /store/blocks/{name}.wasm` — скачивание бинарника блока
  - `POST /api/v1/store/publish` — публикация пользовательского блока (base64 wasm + SHA-256 + манифест); роль локального сервиса обновлений
- **BridgeContext** расширен: `StoreRegistry`, `MetricCollector`, `FlightRecorder`, `TraceContext`, `CrashReporter`, `PanicHandler`, `blocks_dir` (`AIOS_BLOCKS_DIR`)

### Блок сетевых настроек (`aios-net-config`, Фаза 40)
- `NetworkConfig` / `ProxyConfig` / `DnsConfig` / `InterfaceConfig` / `ProxyProtocol` — полная сетевая конфигурация с JSON-сериализацией и частичными обновлениями (`apply_updates` с валидацией: порты 1–65535, синтаксис IP, разбор URL прокси)
- `NetworkConfigStore` — атомарное сохранение JSON (временный файл + rename) в `AIOS_DATA_DIR`/`network.json`
- `NetSettingsBlock` — `StatefulBlock` на IPC-шине: `net_get`, `net_set <json>`, `net_reset`, `net_persist`; извлечение/восстановление состояния через bincode
- **32 unit-теста** по config/validation/store/block

### Команды TUI-шелла для Store и сети (`aios-tui`, Фаза 40)
- `store list | sources | add-source <spec> | search <q> [--source N] | install <name> [--source N] | update [name] [--source N] | uninstall <name> | rollback <name>`
- `net get | net set key=value ... | net reset` — чтение/запись сетевой конфигурации через `NetSettingsBlock` (сохранение через `NetworkConfigStore`)

### Подписи Ed25519 и политика доверия (`aios-store`, Фаза 42 / v2.5.0)
- **Модель подписи** — каждый манифест несёт опциональный `SignatureInfo`; канонические байты: `aios-manifest-v1\n` + name + version + description + author + отсортированные capabilities + размер + `wasm_sha256`; `sign_manifest(manifest, &SigningKey) -> SignatureInfo` подписывает их по Ed25519 (`ed25519-dalek` v2, фича `rand_core`)
- **Проверка** — `ManifestValidator::verify_signature` выполняет реальную проверку `verify_strict` по встроенному ключу; `verify_signature_with_keys(manifest, &[String])` принимает любой доверенный публичный ключ из списка
- **Enforcement в установщике** — `BlockInstaller.trusted_keys: Vec<String>`: если список не пуст, `install_from_bytes` отклоняет неподписанные манифесты и любые манифесты, не подписанные одним из доверенных ключей. Конструкторы `with_trusted_keys(dir, keys)` / `from_env(dir)`; `Default` читает `AIOS_TRUSTED_PUBLIC_KEYS` (разделители `,`/`;`). Sidecar установщика теперь сохраняет полный `ManifestInfo` включая подпись, поэтому подписанные установки остаются проверяемыми
- **Политика доверия по источникам** — `StoreSource.trusted_public_keys` (`#[serde(default)]`); `StoreManager::verify_source_manifest(source, manifest)` применяется в `install()` и `update()`. Источник GitHub по умолчанию наследует официальный ключ из `AIOS_OFFICIAL_PUBLIC_KEY` через `official_public_key()`. Если ключи не заданы, подпись всё равно проверяется по встроенному ключу (неподписанные установки разрешены)
- **TUI-шелл** — `store sign <file.wasm> [name] [version] [--key <secret_hex>]` подписывает локальный wasm (ключ из `AIOS_STORE_SIGNING_KEY`, если `--key` опущен) и пишет подписанный sidecar; `store verify <name>` проверяет SHA-256 + Ed25519 установленного блока
- **Подписанная публикация** — `store publish ... [--key <secret_hex>]` строит манифест, подписывает его по Ed25519 и включает `SignatureInfo` в `StorePublishRequest`; мост проверяет подпись через `ManifestValidator::verify_signature` перед `install_from_bytes` (установщик создаётся через `from_env`, поэтому к подписанным публикациям применяется и локальная политика `AIOS_TRUSTED_PUBLIC_KEYS`; неподписанные публикации остаются разрешёнными, пока не настроены ключи)
- **`store trust <source> [--key <public_hex>] [--clear]`** — задаёт/очищает `StoreSource.trusted_public_keys` из шелла `aios-tui` (hex-ключ валидируется как настоящий публичный ключ Ed25519), сохраняется через `StoreManager::save_config` в конфиг источников; `store sources` показывает число доверенных ключей на источник

### Интеграция `net_settings` в ядро (Фаза 41)
- `net_settings` регистрируется в реестре блоков ядра при загрузке (`aios/src/orchestrator.rs`), обработчик подключается к `MessageRouter`; итоговый `BlockId` доступен как `OrchestratorState::net_block_id`
- Горячая клавиша `n` в TUI ядра (`aios`) открывает режим ввода пар `key=value`; по `Enter` токены преобразуются в частичное JSON-обновление и уходят как `net_set` через IPC, возвращённый JSON конфигурации выводится в панель событий
- Команда `store publish <file.wasm> [name] [version]` (`aios-tui`) вычисляет SHA-256 файла, кодирует wasm в base64 и отправляет `StorePublishRequest` в `POST /api/v1/store/publish` (порт моста из `AIOS_BRIDGE_PORT`, по умолчанию `8080`); имя по умолчанию — имя файла без расширения, версия — `1.0.0`
- `StorePublishRequest` / `StorePublishResponse` в `aios-bridge::dto` оба `Serialize + Deserialize` для клиентских round-trip

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
- `orchestrator.rs` — Асинхронная инициализация IPC, Scheduler, BlockRegistry, AccessControl, Watchdog, LLM, WASM, Bridge (в safe mode без блоков с диска и моста)
- `tui/` — Интерактивная TUI-панель на Ratatui с 7 вкладками (System/Blocks/AI Console/Bridge/Network & Store/Web/Shell) и журналом событий

### Режимы бинарника
- `aios` — Интерактивный TUI-режим (по умолчанию)
- `aios --daemon` — Headless-режим демона (фоновый сервер)
- `aios --safe-mode` — Минимальное восстанавливаемое ядро (без блоков с диска и моста, бейдж `SAFE MODE`)

### Горячие клавиши TUI
| Клавиша | Действие |
|---------|----------|
| Tab / F1 / ? | Следующая вкладка / оверлей справки |
| 1-8 | Прямой выбор вкладки |
| Alt+1-8 | Прямой выбор вкладки даже при вводе в Shell / URL браузера / AI-запросе / сетевой строке |
| q / Ctrl+C | Выход |
| W | Запуск дашборда AIOS GUI (`aios-gui`) |
| Space | Пауза/возобновление прокрутки логов |
| r / k / l (Blocks) | Перезапуск / выгрузка / загрузка выбранного блока |
| g / j / k / o / u / d / b / B / n (Web) | Омнибокс / выбор ссылки / открыть / прокрутка / назад / нативный просмотрщик |
| n / g / s (Network & Store) | Редактор сети / показать JSON конфигурации / обновить список хранилища |
| Files (вкладка 8): Tab/↑/↓, Enter, Backspace, F2-F3, F5-F9, g/w, r | Переключение панелей / навигация, открыть папку или AI-превью, родительская папка, переименовать, просмотр, копировать, переместить, создать папку, удалить, сортировка, выдать host read/write, обновить |

### AI Console (вкладка 3, Фаза 43 / v2.6.0, Фаза 45 / v2.9.0)
- Интерактивный чат с LLM: `i` включает режим запроса, `Enter` отправляет; каждый запрос повторно применяет текущий `LlmConfig` консоли к общему движку `BridgeContext.llm`, поэтому настройки консоли и HTTP-эндпоинт `/api/v1/llm/query` остаются согласованными
- Система слэш-команд в `TuiApp::handle_ai_command`: `/help /status /clear /history /system /model /backend /key /temp /tokens /preset /save /load`; смена бэкенда/модели/ключа асинхронно пересоздаёт движок через `apply_config_async`
- Встроенная панель справки открывается по `h` или `/help` — стилизованный справочник клавиш и команд; история промптов (последние 50) листается клавишами `Up`/`Down`
- Строка состояния `backend | model | temp | tokens | state` (`streaming...` / `done: Nms` / ошибка); `/status` выводит конфигурацию и найденные локальные GGUF-модели через `aios_llm::local::detect_local_models`
- **Стриминг (Фаза 45)**: `submit_ai_query` запускает `LlmEngine::query_stream(&req, tx)` в tokio-задаче; дельты накапливаются в `TuiApp.ai_stream` (рендерятся вживую жёлтым), итоговый текст добавляется в ленту. `aios-llm` стримит SSE-дельты для облачных бэкендов (`extract_stream_delta`, форматы OpenAI и Google AI Studio) и дельты по токенам для локальных (`generate_tokens` с колбэком)
- **Сохранение чата (Фаза 45)**: лента хранится в `TuiApp.ai_log` (`Vec<AiMessage>`, где `AiMessage { role, text }`), автосохраняется в JSON Lines в `AIOS_DATA_DIR/chat.jsonl` (по умолчанию `aios_data/chat.jsonl`) после каждого завершённого ответа и при выходе через `save_chat`, восстанавливается при старте через `load_chat`; ручное управление `/save` / `/load`
- **Шаблоны промптов (Фаза 45)**: `TuiApp.ai_presets` (`BTreeMap<имя, текст>`) с предустановками `assistant`/`code`/`translator`/`explainer`; `/preset <имя>` применяет шаблон как системный промпт, `/preset <имя> <текст>` задаёт шаблон, `/preset list` / `/preset del <имя>` управляют набором
- `aios-llm` получил `LlmEngine::config()`, `provider_name()`, `backend_label()` для интроспекции конфигурации

### Последовательность запуска
1. Определение оборудования (CPU, RAM, GPU, ОС)
2. Инициализация IPC-шины (SharedIpcBus)
3. Создание Scheduler с RAM-ориентированной конфигурацией
4. Инициализация BlockRegistry — регистрация базовых блоков (hal, ipc_bus, scheduler, browser), boot-обнаружение `AIOS_BLOCKS_DIR` (по умолчанию `./blocks`; пропуск в safe mode), регистрация IPC-обработчика браузерного блока в `MessageRouter`
5. Настройка AccessControl + Watchdog
6. Инициализация LLM Engine (облачный бэкенд по умолчанию)
7. Инициализация WASM Executor (BlockExecutor)
8. Создание BridgeContext со всеми подсистемами
9. Запуск Bridge HTTP-сервера (axum, порт из `--bridge-port`, по умолчанию 8080) — пропуск в safe mode
10. Запуск цикла событий TUI (или цикла демона)

Браузер работает «из коробки» на новом компьютере: для запуска не нужны ни конфиг-файлы, ни установленный браузер, ни сеть — блок активен в топологии, доступен по IPC, а вкладка Web (`B`/`n`/омнибокс) открывает любой URL в нативном WebView.

## Слой 7: Live USB развёртывание (`live/`)

### Обзор
Каталог `live/` собирает гибридный (BIOS+UEFI) ISO-образ, который грузится сразу в TUI `aios` на Linux — без Windows и без предустановленной системы. Образ воспроизводимо собирается в Docker через `live/build.sh` и записывается на флешку.

### Структура и цепочка загрузки
- `live/build.sh` — сборка в Docker: Alpine 3.24 minirootfs (распаковка, `chroot` apk install), static-musl release сборка `aios` (офлайн-крейты через монтирование registry из `CARGO_HOME`, сборка в `/tmp/target` во избежание I/O-ошибок NTFS bind-mount), squashfs из rootfs, кастомный initramfs, GRUB2
- `live/init.rs` — init busybox: сканирует блочные устройства, монтирует `/dev/aioslivedata` (iso9660) или `/dev/aiosliveiso` (vfat), loop-mount `boot/aios.squashfs`, `switch_root` в него, запуск `rcS`
- `live/rcS` — mount proc/sys/dev, DHCP-сеть на всех ethernet/wifi-интерфейсах, запуск TUI AIOS на `tty1`
- `live/aios-launch` — запускает `aios` на `tty1`, перезапуск при падении, откат в шелл
- `live/aios-install` — интерактивный установщик: список дисков, выбор цели (например `sda`), разметка GPT (512 МБ EFI + ext4 root), копирование системы, установка GRUB
- `live/grub.cfg` — меню GRUB: **AIOS Live**, **AIOS Live (verbose)**, **AIOS Installer**; 10 с по умолчанию
- `live/inittab` — без getty: `aios-launch` на tty1, askhell на tty2

### Жизненный цикл
- Загрузка: BIOS/UEFI → GRUB → initramfs init → squashfs root (только чтение; `/tmp`, `/run`, `/var/log` на tmpfs) → TUI `aios` → `Esc`/`q` в шелл `#` → `aios-install` для постоянной установки на диск
- Флаги сборки: `aios` собирается с `--no-default-features` для Live-образа (без webview) — см. feature `webview` в `Cargo.toml` (v2.9.4)

## Слой 8: `aios-init` и автономный initramfs (`aios-init/`, `build_initramfs.sh`)

### Обзор
`aios-init` — это выделенный Rust `/init` для initramfs AIOS: статически скомпилированный (`x86_64-unknown-linux-musl`) супервизор PID 1, который монтирует базовые VFS, передаёт управление блоку AIOS и никогда не паникует — тем самым устраняя `Kernel panic: No working init found`.

### Обязанности (порядок загрузки)
1. Установка обработчиков `sigaction`: SIGTERM/SIGINT/SIGHUP выставляют флаг завершения; SIGCHLD (`SA_NOCLDSTOP`) будит цикл сборки зомби; SIGPIPE игнорируется.
2. Монтирование базовых VFS: `/proc` (proc), `/sys` (sysfs), `/dev` (devtmpfs; если недоступна — `mknod` для `/dev/console` 5:1, `/dev/null` 1:3, `/dev/tty` 5:0), `/tmp` (tmpfs).
3. Открытие `/dev/console` и `dup2` в fd 0/1/2, чтобы все журналы загрузки шли на консоль.
4. Запуск и супервизия `/system/aios-core` (запасной `/installer`), до 3 перезапусков (задержка 300 мс) при падении.
5. Сборка каждого потомка через `waitpid(-1, WNOHANG)`, чтобы осиротевшие «внуки» не становились вечными зомби.
6. По SIGTERM/SIGINT: передача сигнала блоку, ожидание до 5 с, затем SIGKILL.
7. Аварийный запасной вариант: если блок отсутствует или перезапуски исчерпаны — запуск спасательного шелла (`/bin/sh` → `/bin/busybox sh` → `/bin/ash`); если шелла нет — idle-цикл со сборкой зомби, без паники ядра.

### Сборка initramfs
```
rustup target add x86_64-unknown-linux-musl
./build_initramfs.sh                     # initramfs.cpio.gz (ядерный TUI + init)
./build_initramfs.sh --keep-rootfs       # оставить стейджинг-каталог rootfs/
./build_initramfs.sh --no-aios-core      # без ядерного бинарника aios (только спасательный шелл)
BUSYBOX_PATH=/usr/bin/busybox.static ./build_initramfs.sh   # + спасательный шелл
```
Скрипт выполняет `cargo build --release --target x86_64-unknown-linux-musl` для `aios-init` и (если не задан `--no-aios-core`/`SKIP_AIOS_CORE=1`) `cargo build -p aios --release --target x86_64-unknown-linux-musl --no-default-features` для реального ядерного TUI. Он формирует структуру в `rootfs/`, копирует `aios-init` в `/init` и `aios` в `/system/aios-core`, затем упаковывает `find . | cpio --null -ov --format=newc | gzip -9`. Защита очистки отказывается удалять путь за пределами каталога скрипта; `--keep-rootfs` сохраняет стейджинг-каталог. Когда присутствует `/system/aios-core`, `aios-init` сразу загружает полный ядерный TUI; спасательный шелл остаётся только запасным вариантом (v2.13.0).

### Вариант Live-образа (aios-init по умолчанию)
Шаг [4] в `live/build.sh` по умолчанию упаковывает aios-init-initramfs: `aios-init` как `/init`, бинарник `aios` как `/system/aios-core`, busybox только как спасательный шелл — ядро грузится сразу в ядерный TUI без корня squashfs; шаг [5] записывает отдельное GRUB-меню с записями `init=/init console=tty0`. Прежний вариант busybox-initramfs (монтирование squashfs + `switch_root`, `init.rs`) сохранён за флагом-отключением `USE_BUSYBOX_INIT=1` (v2.14.0; в v2.13.0 aios-init включался опционально через `USE_AIOS_INIT=1`).

### Параметры ядра Linux
- GRUB: `menuentry "AIOS" { linux /boot/vmlinuz init=/init console=tty0 quiet; initrd /boot/initramfs.cpio.gz; }`
- Syslinux: `LABEL aios\n KERNEL /boot/vmlinuz\n APPEND init=/init console=tty0 quiet\n INITRD /boot/initramfs.cpio.gz`
- `init=/init` указывает ядру запускать этот бинарник вместо `/sbin/init`; `console=tty0` направляет вывод ядра и init на основную консоль.
