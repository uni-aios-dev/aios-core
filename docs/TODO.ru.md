# Дорожная карта разработки AIOS

## Завершённые

- [x] Фаза 1: Каркас рабочего пространства + IPC бинарный протокол
- [x] Фаза 2: Hardware Abstraction Layer с определением и классификацией tiers
- [x] Фаза 3: Block Manager (реестр, загрузчик, маршрутизатор сообщений)
- [x] Фаза 4: Process Manager (планировщик, устойчивость к сбоям, IPC управление)
- [x] Фаза 5: Live-Update Engine (горячая замена, откат, передача состояния)
- [x] Фаза 6: AI Orchestrator (intent engine) + TUI панель управления
- [x] Фаза 7: Интеграционные тесты (10 тестов полного жизненного цикла)
- [x] Документация: архитектура, журнал изменений, баги, TODO, правила агентов
- [x] Фаза 8: AI Watchdog и Engine аварийного восстановления (heartbeat, safe mode shell)
- [x] Фаза 9: Capability-Based Security и sandboxing (токены, контроль доступа, песочница)
- [x] Фаза 10: Persistent System Context Store (телеметрия, воркфлоу, стабильность)
- [x] Фаза 11: Системная интеграция (watchdog↔TUI, context↔scheduler, старение, очередь приоритетов)
- [x] Фаза 12: Docker + CI/CD инфраструктура
- [x] Фаза 13: Persistent DB (redb), CLI Shell, определение хранилища, RT-планировщик, стресс-тесты
- [x] Фаза 14: Мультибинарная совместимость (`aios-exec-compat`) — POSIX/Win32 трансляция, исцеление зависимостей, совместимость песочницы
- [x] GUI Дашборд (`aios-gui`) — нативное окно egui/eframe, 6 вкладок, тёмная тема, навигация клавиатурой
- [x] Документация интерфейса (`docs/INTERFACE.md`) — полное руководство по GUI/TUI, двуязычное
- [x] Реальные ОС-потоки планировщика — `RealThread`, `TerminateFlag`, `SuspendFlag`, кооперативное завершение/приостановка
- [x] BlockExecutor — мост выполнения WASM-блоков между `BlockRegistry` + `WasmSandbox`
- [x] WatchdogRunner — реальный фоновый поток с `AtomicBool` остановки, обнаружение тайм-аутов, сбор действий
- [x] RealTcpBlock — реальные `std::net::TcpListener`/`TcpStream` сокеты с неблокирующим accept
- [x] WasmLiveUpdateEngine — реальное развёртывание/замена/откат WASM-модулей при live-обновлении
- [x] RealUdpBlock — реальный `std::net::UdpSocket` с broadcast, плюс метод `port()`
- [x] Интеграционные тесты: реальный I/O — 55 тестов в 6 файлах (файлы, сеть, WASM, потоки, горячая замена, жизненный цикл)
- [x] Привязка к CPU в планировщике — `SetThreadAffinityMask` (Win) / `sched_setaffinity` (Linux), привязка по ядрам
- [x] Фаза 15: Zero-Copy IPC Ring Buffers — lock-free кольцевой буфер с producer/consumer индексами
- [x] Фаза 16: Hardware-Enforced Memory Protection (MPK / PKS) — Intel MPK, ARM Memory Domains, 27 тестов
- [x] Фаза 17: AI KV-Cache & State Compression — квантизация FP8/INT4, сжатие ZSTD, LRU кэш
- [x] Фаза 18: Atomic Copy-on-Write (CoW) Persistence — SnapshotManager, RecoveryLog, атомарный коммит
- [x] Фаза 19: IOMMU Support for DMA Isolation — управление DMA, таблицы страниц, домены IOMMU, 25 тестов
- [x] Фаза 20: TEE (Trusted Execution Environment) Integration — SGX/TrustZone/SEV, запечатывание, аттестация, анклавы, 28 тестов
- [x] Docker: Multi-stage production сборка — builder + runtime (debian:bookworm-slim), entrypoint aios-tui
- [x] Фаза 21: aios-bridge — HTTP/WS API шлюз + Intent engine (RU/EN) + контроль capabilities
- [x] Фаза 44: TUI ядра из 7 вкладок + `--safe-mode` + GUI AI Studio/Network Settings (v2.8.0)
- [x] Фаза 45: сохранение чата AI Console + шаблоны `/preset` + стриминг (v2.9.0)
- [x] Фаза 46: загрузочный Live USB образ — гибридный BIOS+UEFI ISO с ядром Linux, автозапуском TUI AIOS, установщиком `aios-install`, воспроизводимой сборкой `live/build.sh` (v2.9.5)
- [x] Фаза 47: Виртуальная файловая система (`aios-vfs`) + двухпанельный файловый менеджер (`aios-fm`) со вкладкой Files в TUI и GUI — схемы AIOS:// и HOST://, доступ к хосту через capability-токены, отменяемые асинхронные copy/move/delete с прогрессом, AI-превью файлов (v2.10.0)
- [x] Фаза 48: Многоузловой распределённый кластер (`aios-cluster`) — TCP/in-memory транспорты, обнаружение через heartbeat, размещение по нагрузке/round-robin/tier, удалённые spawn/kill/приоритет, failover-перезапуск, конфигурация из env/JSON (v2.11.0)
- [x] Фаза 49: `aios-init` — статический musl init PID 1 для initramfs: монтирование базовых VFS, `/dev/console`, супервизор блока с перезапусками и сборкой зомби, запасной спасательный шелл, упаковка `build_initramfs.sh` через cpio/gzip, настройка GRUB/Syslinux `init=/init console=tty0` (v2.12.0)
- [x] Фаза 50: `aios-init` передаёт управление реальному ядерному TUI — `build_initramfs.sh` собирает и размещает статический musl `aios` как `/system/aios-core` (загрузка сразу в ядерный TUI, спасательный шелл — запасной вариант), добавлены `--keep-rootfs` и защита очистки rootfs; `live/build.sh` получает опциональный режим `USE_AIOS_INIT=1` с отдельным GRUB-меню (v2.13.0)
- [x] Фаза 51: `aios-init` — `/init` initramfs по умолчанию в Live ISO — `live/build.sh` шаг [4] упаковывает initramfs на базе `aios-init` по умолчанию (`aios-init` как `/init`, `aios` как `/system/aios-core`, busybox только как спасательный шелл), шаг [5] записывает GRUB-меню `init=/init console=tty0`; прежний путь busybox `switch_root` сохранён за `USE_BUSYBOX_INIT=1`, переключатель `USE_AIOS_INIT=1` удалён (v2.14.0)
- [x] Фаза 52: Управление кластером из Shell ядерного TUI — команды `cluster status/nodes/spawn/kill/migrate` управляют `DistributedScheduler` на реальных потоках `SchedulerProcessExecutor`; `OrchestratorState` удерживает handle планировщика; миграция процессов между узлами (спавн-затем-убийство) (v2.18.0–v2.19.0)
- [x] Фаза 53: Миграция процессов с переносом состояния — `ProcessExecutor::extract_state`/`restore_state`, `Spawn` несёт снимок состояния по сети (`GetState`/`GetStateReply`), `migrate` восстанавливает его на узле назначения до убийства исходника (v2.20.0)
- [x] Фаза 54: Репликация контрольных точек — воркеры рассылают снимок состояния каждого процесса всем пирам каждый heartbeat; `tick()` восстанавливает самую свежую реплицированную контрольную точку при потере узла-хоста и вычищает устаревшие снимки по `checkpoint_ttl` (builder + `AIOS_CLUSTER_CHECKPOINT_TTL_MS`, по умолчанию 15 с) (v2.21.0)

## Бэклог

- [x] Сделать подключение `aios-init` + `build_initramfs.sh` в `live/build.sh` шаг [4] как `/init` initramfs режимом по умолчанию (сделано в v2.14.0; прежний путь busybox сохранён за `USE_BUSYBOX_INIT=1`)
- [x] Защита очистки `rootfs` / флаг `--keep-rootfs` в `build_initramfs.sh` — сделано (v2.13.0)

## Оценка готовности (2026-07-28, обновлено)

**Общая: ~100% готовности к реальным (не mock) тестам.** (рост с 90%)

| Компонент | Статус | Что реально |
|-----------|--------|-------------|
| Определение HAL | **НАСТОЯЩИЙ** | Реальные запросы ОС (nvidia-smi, wmic, /proc/cpuinfo) |
| IPC Шина/Канал | **НАСТОЯЩИЙ*** | Реальный mpsc + Arc<Mutex<VecDeque>>, только внутри процесса |
| WASM Песочница | **НАСТОЯЩАЯ** | Реальная компиляция + выполнение wasmtime |
| Файловая система | **НАСТОЯЩАЯ** | Реальные std::fs read/write |
| Планировщик | **НАСТОЯЩИЙ** | Реальное порождение ОС-потоков, кооперативное завершение/приостановка, привязка к ядрам CPU |
| BlockRegistry | **НАСТОЯЩИЙ** | `load_from_path()` сканирует .wasm/.bin с диска при старте |
| BlockLoader | **НАСТОЯЩИЙ** | `load_from_path_and_execute()` компилирует + инстанцирует WASM |
| Watchdog | **НАСТОЯЩИЙ** | Фоновый поток + ступенчатое восстановление (kill/dump/shell) |
| TCP Сеть | **НАСТОЯЩАЯ** | Реальные std::net::TcpListener/TcpStream сокеты |
| UDP Сеть | **НАСТОЯЩАЯ** | Реальный std::net::UdpSocket с broadcast |
| Live Update | **НАСТОЯЩИЙ** | WasmLiveUpdateEngine — развёртывание, замена WASM-модулей, откат, миграция памяти, IPC-reroute |
| BlockExecutor | **НАСТОЯЩИЙ** | Мост между BlockRegistry + WasmSandbox для полного выполнения блоков |
| Интеграционные тесты | **НАСТОЯЩИЕ** | 61 тест с реальным file I/O, network loopback, WASM-выполнением, ОС-потоками, горячей заменой с состоянием |

### Все целевые показатели вех достигнуты ✅

## Приоритет 0: Продвинутая оптимизация и надёжность оборудования (РЕАЛИЗОВАНО)

### 0.1 Zero-Copy IPC Ring Buffers ✅
- [x] Замена стандартного IPC сокетов/каналов на буферы общей памяти с кольцевой структурой
- [x] Реализация паттерна `shm` + `io_uring` для обхода ядра при передаче данных
- [x] Обеспечение O(1) эффективности передачи данных между AI Orchestrator, Storage Blocks и Execution Subsystems
- [x] Исключение копирования полезной нагрузки в пространство ядра для больших IPC сообщений
- [x] Проектирование lock-free кольцевой буферной структуры данных с индексами producer/consumer
- [x] Интеграция с существующими `IpcBus` и `IpcChannel` транспортами
- [x] Бенчмарки: измерение снижения задержки по сравнению с текущей VecDeque-based шиной

### 0.2 Hardware-Enforced Memory Protection (MPK / PKS) ✅ РЕАЛИЗОВАНО
- [x] Использование Intel MPK (Memory Protection Keys) / ARM Memory Domains для изоляции
- [x] Назначение ключей доступа оборудования изолированным системным блокам
- [x] Предотвращение межблочного доступа к памяти непосредственно на уровне MMU (Memory Management Unit)
- [x] Реализация проверки capabilities во время выполнения с накладными расходами CPU < 1%
- [x] Управление доступом per-block через модификацию PKEY регистра (x86-64) или DACR (ARM)
- [x] Fallback soft-изоляция для неподдерживаемых архитектур
- [x] Интеграция с `aios-security` capability токенами
- [x] 27 unit-тестов, охватывающих Intel MPK, ARM домены и security bridge
- [x] Мост безопасности оборудования: единый интерфейс MPK/TEE/IOMMU

### 0.3 AI KV-Cache & State Compression ✅
- [x] Реализация квантизации памяти во время выполнения (FP8/INT4) для неактивных буферов контекста AI Orchestrator
- [x] Сжатие неактивных таблиц состояния системы в RAM с помощью кодека ZSTD
- [x] Минимизация размера памяти на низкоспецифичном оборудовании (Tier 3 устройства)
- [x] Автоматические пороги сжатия на основе обнаружения нехватки памяти
- [x] Ленивая распаковка при доступе с LRU кэшем распаковки
- [x] Бенчмарк: измерение коэффициента сжатия и стоимости CPU на различных типах состояния
- [x] Интеграция с `aios-context` хранилищем телеметрии (сжатая телеметрия)

### 0.4 Atomic Copy-on-Write (CoW) State Persistence ✅
- [x] Все операции дисков блоков и live-updates пишут в структуры хранилища Copy-on-Write
- [x] Обеспечение мгновенного атомарного отката при потере питания
- [x] Реализация CoW таблиц страниц для снимков состояния блоков
- [x] Атомарный протокол коммита: запись в теневой регион → flush fsync → атомарное переименование
- [x] Журнал восстановления для устойчивости к сбоям при передаче состояния
- [x] Интеграция с `aios-live-update` движком горячей замены (CowLiveUpdateEngine)
- [x] Бенчмарки: время создания снимка, задержка отката, накладные расходы диска

### 0.5 Движок оптимизации во время выполнения ✅
- [x] Профилировщик производительности со скользящими средними, гистограммами, перцентилями
- [x] Обнаружение горячих путей с подсчётом попаданий и выводом flamegraph
- [x] Оптимизатор раскладки памяти для выравнивания по кеш-линии
- [x] Авторегулировщик со стратегиями сетки/случайного/бинарного поиска
- [x] 29 модульных тестов для всех модулей оптимизации

## Приоритет 1: Дополнительные спецификации (критическая безопасность)

### 1.1 AI Watchdog и Engine аварийного восстановления
- [x] Реализация структуры `Watchdog` с отслеживанием heartbeat
- [x] Добавление криптографических heartbeat-пакетов (HMAC-SHA256) от AI Orchestrator к ядру
- [x] Настраиваемый интервал heartbeat (N мс) и порог пропусков (X подряд)
- [x] Safe Mode fallback: детерминированный CLI Kernel Shell при зависании AI
- [x] Дамп журнала выполнения состояния при срабатывании watchdog
- [x] Интеграция watchdog в главный цикл TUI с потоком heartbeat
- [x] Unit-тесты: пропуск heartbeat, восстановление, целостность дампа состояния

### 1.2 Capability-Based Security и sandboxing (Zero-Trust)
- [x] Реализация перечисления `CapabilityToken` с конкретными возможностями:
  - `CAP_NET_BIND`, `CAP_NET_CONNECT`
  - `CAP_FS_READ`, `CAP_FS_WRITE`
  - `CAP_HW_ACCESS`, `CAP_MEM_ALLOC`
  - `CAP_SCHED_MODIFY`
- [x] `AccessControlLayer` для выдачи и валидации токенов
- [x] Проверка capabilities во время выполнения каждого системного вызова
- [x] WebAssembly sandboxing через `wasmtime` для выполнения блоков
- [x] Запрет прямых указателей на память (обмен данными только через IPC)
- [x] Перехват нарушений: завершение блока + уведомление AI Orchestrator
- [x] Интеграция с BlockManager: хранение токенов в `BlockEntry`
- [x] Unit-тесты: предоставление/отзыв capabilities, обнаружение нарушений, изоляция песочницы
- [x] Мультибинарная совместимость: POSIX/Win32 трансляция, исцеление зависимостей, совместимость песочницы (89 тестов)

### 1.3 Persistent System Context и Vector Memory Store
- [x] Интеграция встроенной БД (`heed` / `redb` / `sled`)
- [x] Структура `EmbeddedContextStore` с типизированными коллекциями:
  - [x] История телеметрии (метрики CPU/RAM для каждого блока)
  - [x] Паттерны воркфлоу пользователя (изученные профили приоритетов)
  - [x] Журналы обновлений (исторические оценки стабильности по бинарникам блоков)
- [x] Требование zero-cloud: все данные хранятся локально
- [x] Автоматическая компактификация при запуске, если БД превышает порог
- [x] Query API: `get_telemetry_range()`, `get_workflow_profile()`, `get_stability_score()`
- [x] Интеграция с планировщиком для автонастройки приоритетов на основе изученных паттернов
- [x] Unit-тесты: запись/чтение/запросы, компактификация, восстановление после сбоя

## Приоритет 2: Укрепление системы

### 2.1 Улучшения IPC Bus
- [x] Ограниченная очередь с обратным давлением (политики DropOldest и Reject)
- [x] Упорядочение очереди по приоритету (send_priority для извлечения по приоритету)
- [x] Дедупликация сообщений через отслеживание packet_id (на базе HashSet)
- [x] Метрики bus: отправлено/получено/отброшено/дедуплицировано/пиковая глубина очереди/средняя задержка

### 2.2 Улучшения планировщика
- [x] Справедливое планирование внутри уровня приоритета (взвешенный round-robin)
- [x] Старение процессов: предотвращение голода процессов с низким приоритетом
- [x] Уведомление о нехватке памяти AI Orchestrator (система колбэков)
- [x] Группы процессов и управление сессиями
- [x] Режим планирования реального времени для критических по задержке блоков

### 2.3 Улучшения Block Manager
- [x] Граф зависимостей блоков (топологический порядок загрузки/выгрузки, обнаружение циклов)
- [x] Версионирование блоков с семантическим сравнением версий (parse, ord, compat, bump)
- [x] Горячая перезагрузка из файловой системы (мониторинг новых .bin файлов)
- [x] Поддержка маркетплейса/репозитория блоков

## Приоритет 3: Оборудование и runtime

### 3.1 Расширение определения оборудования
- [x] Определение NVIDIA GPU через nvidia-smi
- [x] Определение AMD GPU через ROCm/SMI
- [x] Определение NPU для Intel Meteor Lake, Qualcomm X Elite
- [x] Перечисление устройств USB/Thunderbolt
- [x] Определение устройств хранения (NVMe, SATA)

### 3.2 Интеграция WebAssembly runtime
- [x] Встраивание Wasmtime для sandboxing блоков
- [x] Интерфейс WASI для фильтрации системных вызовов
- [x] Ограничения памяти на экземпляр WASM-блока
- [x] Изоляция между блоками без общего состояния

### 3.3 Возможности реального времени
- [x] Детерминированный режим планировщика для RT-блоков
- [x] Инфраструктура измерения задержки
- [x] Отслеживание и отчётность по джиттеру
- [x] Протокол наследования приоритетов

### 3.4 Сетевой стек
- [x] TCP блок (клиент/сервер, управление соединениями, отправка/получение)
- [x] UDP блок (привязка, отправка, широковещание, получение)
- [x] Отслеживание соединений и статистика

### 3.5 Базовые абстракции
- [x] Абстракция файловой системы (виртуальная, локальная, наложенная)
- [x] Модель прав доступа к файлам

## Приоритет 4: Пользовательский интерфейс

### 4.1 Улучшения TUI
- [x] Визуализация дерева процессов в реальном времени (таблица с PID, именем, приоритетом, состоянием, RAM, CPU, сбоями)
- [x] Визуализация графа зависимостей блоков
- [x] Графики системных метрик (индикатор RAM, распределение приоритетов, история RAM)
- [x] Интерактивное управление блоками (загрузка/выгрузка/горячая замена из UI)
- [x] Управление процессами с клавиатуры (j/k навигация, K для убийства, 1-4 вкладки)

### 4.2 CLI Kernel Shell (Safe Mode)
- [x] Детерминированный shell для восстановления системы
- [x] Базовые команды: ps, kill, load, unload, status, logs
- [x] Без зависимости от AI — работает автономно
- [x] Доступен при приостановке AI Orchestrator

## Приоритет 5: Тестирование и качество

### 5.1 Расширение покрытия тестами
- [x] Property-based тестирование IPC протокола (proptest)
- [x] Fuzzing для сериализации/десериализации
- [x] Нагрузочные тесты: 1000+ параллельных блоков (708 тестов всего)
- [x] Chaos-тестирование: случайные сбои, нехватка памяти
- [x] Бенчмарки: пропускная способность IPC, задержка планировщика

### 5.2 CI/CD
- [x] Pipeline GitHub Actions
- [x] Цели кросс-компиляции (Linux ARM64, Windows x64)
- [x] Clippy + fmt проверка в CI
- [x] Отчёт о покрытии (tarpaulin)
- [x] Автоматизация релизов

## Отложенные

- [ ] GUI интерфейс
- [x] Многоузловое распределённое планирование — кластер построен (v2.11.0) и управляется из Shell ядерного TUI (v2.19.0)
- [ ] Формальная верификация свойств безопасности

## Чеклист перехода runtime (Mock → Real)

**Цель:** трансформация всех mock/симулированных подсистем в реальное выполнение на уровне ОС. Целевой показатель: 90%+ готовности.

### 1. Менеджер процессов и планировщик — реальное порождение потоков
- [x] Структура `RealThread`: `Thread` + `JoinHandle` + `TerminateFlag` + `SuspendFlag`
- [x] `spawn_real_process<F>()`: реальный ОС-поток с кооперативным завершением
- [x] `kill_process()`: установка флага завершения, разблокировка, присоединение handle
- [x] `suspend_process()` / `resume_process()`: приостановка/возобновление реальных потоков
- [x] `check_real_threads()`: обнаружение завершённых потоков через `is_finished()`
- [x] Маппинг `ProcessId` → `JoinHandle` в постоянном реестре
- [x] Привязка к CPU: `SetThreadAffinityMask` (Win) / `sched_setaffinity` (Linux) по tier из `aios-hal`
- [x] Thread-local storage для per-process метрик

### 2. BlockRegistry и BlockLoader — реальный File I/O и выполнение
- [x] `BlockExecutor`: связывает `BlockRegistry` + `WasmSandbox`, компилирует/инстанцирует/вызывает
- [x] `deploy_block()`: автоматически вызывает `init`/`start` на WASM-блоках
- [x] `BlockRegistry::load_from_path(path)`: сканирование директории на `.wasm` и `.bin` файлы, парсинг манифестов, регистрация
- [x] `BlockExecutor::load_from_path_and_execute()`: загрузка + компиляция WASM с диска за один шаг
- [x] `BlockLoader::load_from_directory()`: теперь обрабатывает `.wasm` файлы наряду с `.bin`
- [x] Автообнаружение: обход директории `blocks/` при старте, регистрация всех валидных `.wasm` файлов
- [x] Парсинг манифеста из sidecar `.json` файлов (имя, версия, capabilities, TTL)

### 3. Активный AI Watchdog — фоновый супервайзер
- [x] `WatchdogRunner`: реальный фоновый поток с `AtomicBool` остановки
- [x] `start()` / `stop()` / `receive_heartbeat()` / `pop_actions()`
- [x] Обнаружение тайм-аута → `WatchdogAction::EnterSafeMode`
- [x] Активное восстановление: `WatchdogAction::KillProcess(pid)` — серьёзность 4
- [x] Активное восстановление: `WatchdogAction::DumpState(path)` — серьёзность 5, с таймстампом
- [x] Активное восстановление: `WatchdogAction::SafeModeShell` — серьёзность 7
- [x] `escalate()` на раннере — вызывает контекстно-зависимые действия восстановления
- [x] Упорядочивание по серьёзности и `is_terminal()` для `WatchdogAction`
- [x] Ступенчатая эскалация: предупреждение → приостановка → завершение → safe mode (graduated response в check_timeout)

### 4. Сетевой стек TCP/UDP — реальные OS-сокеты
- [x] `RealTcpBlock`: реальные `std::net::TcpListener`/`TcpStream` с неблокирующим accept
- [x] `start_listening()`, `accept_pending()`, `connect()`, `send()`, `receive()`, `close_connection()`
- [x] `RealUdpBlock`: реальный `std::net::UdpSocket` с `bind()`, `send_to()`, неблокирующим `receive_from()`
- [x] `broadcast()` для UDP broadcast (через `SO_BROADCAST`)
- [x] Опции сокетов: `SO_REUSEADDR`, `SO_KEEPALIVE`, `TCP_NODELAY`
- [x] Привязка capability-токенов: `CAP_NET_BIND` → `socket.bind()`, `CAP_NET_CONNECT` → `socket.connect()`

### 5. Движок live-обновлений — реальная замена WASM
- [x] `WasmLiveUpdateEngine`: развёртывание/замена/откат через `LiveUpdateEngine` + `WasmSandbox`
- [x] `swap_block()`: атомарная замена + компиляция + инстанцирование нового WASM-модуля
- [x] `rollback_block()`: удаление активного экземпляра + восстановление из `HotSwapEntry`
- [x] Миграция состояния: извлечение состояния WASM linear memory перед заменой, восстановление в новый экземпляр
- [x] Перенаправление IPC-каналов: атомарное переключение ожидающих сообщений на новый handle блока
- [x] Swap без даунтайма: обеспечение непотери IPC-сообщений в полёте во время перехода

### 6. Интеграционные тесты — реальный I/O
- [x] `tests/real_file_io.rs`: реальное чтение/запись файлов через `aios-core::filesystem`
- [x] `tests/real_network.rs`: TCP loopback отправка/получение, UDP broadcast
- [x] `tests/real_wasm.rs`: компиляция + выполнение WASM-блоков end-to-end
- [x] `tests/real_threads.rs`: порождение процессов, проверка реальных thread handles
- [x] `tests/real_hot_swap.rs`: развёртывание блока v1, замена на v2, проверка изменения функции
- [x] `tests/full_lifecycle.rs`: загрузка → загрузка блоков → планирование → выполнение → watchdog → выключение

- [x] Phase 22: aios-studio веб-интерфейс — SPA дашборд с Command Palette, WebSocket-графиком RAM, Security Center, матрицей capability, автоподключением, раздачей из aios-bridge через ServeDir

## Запланировано

- [x] **Подпункт Фазы 24: Backend эндпоинт workflow** — `POST /api/v1/workflow` batch-исполнитель интентов в `aios-bridge`
- [x] **Фаза 23: Многорежимный AI-движок (`aios-llm`) и гибридный маршрутизатор намерений — ЗАВЕРШЕНА**
  - [x] Крейт `aios-llm` с унифицированным trait/enum дизайном: LlmConfig, BackendKind, CloudProvider, LlmRequest/Response
  - [x] Cloud-First: HTTP/JSON бэкенд для Groq, OpenRouter, Google AI Studio через `reqwest`
  - [x] Micro-Local: Qwen2.5-0.5B-Instruct-GGUF через candle 0.11 (`quantized_qwen2::ModelWeights`, `LogitsProcessor`)
  - [x] Full-Local: Qwen2.5-7B-Instruct-GGUF квантованный (INT4), тот же пайплайн инференса
  - [x] Интеграция `hf-hub` 1.0: `HFClientSync` для загрузки моделей с Hugging Face Hub (blocking)
  - [x] Интеграция с aios-bridge: эндпоинт `POST /api/v1/llm/query` + `parse_with_llm_fallback()` в маршрутизаторе интентов
- [x] **Фаза 24: EasyLang Engine и No-Code Builder (`aios-builder`) — ЗАВЕРШЕНА**
  - [x] Крейт `aios-builder` создан: тип Workflow, AutoManifestGenerator (анализ WASM + интентов), WorkflowCompiler (генерация WAT)
  - [x] In-Memory EasyLang Compiler: декларативный текст → `.wasm` за миллисекунды (пайплайн WAT→WASM готов)
  - [x] EasyLangParser: построчный DSL (`spawn`, `timer`, `load`, `unload`, `kill`, `query`, `compact`, `status`) с опциональным префиксом label, поддержка комментариев, авто-генерация label
  - [x] Auto-Manifest Generator: анализ WASM-бинарников + ключевой анализ интентов workflow
  - [x] Визуальный редактор workflow (step-редактор) в `aios-studio` — палитра, добавление/удаление/перестановка шагов, inline редактирование prompt, последовательный запуск
  - [x] Персистентность workflow — именованные сохранение/загрузка/удаление через localStorage с выпадающим списком
- [x] **Фаза 25: Безопасный веб-сёрфинг и поиск (`aios-browser` и `aios-search`) — ЗАВЕРШЕНА**
  - [x] WASM-векторный HTML/CSS рендерер с изолированным сетевым стеком (HtmlParser, Renderer, BrowserEngine)
  - [x] Анонимный веб-поиск через DuckDuckGo / SearXNG / Brave Search API (SearchEngine, 3 бэкенда)
  - [x] Синтез TL;DR локальным ИИ через aios-llm (SearchSummarizer)
  - [x] `POST /api/v1/browse` и `POST /api/v1/search` REST-эндпоинты в aios-bridge
- [x] **Фаза 26: Атомарные обновления и магазин приложений (`aios-updater` и `aios-store`) — ВЫПОЛНЕНО**
  - [x] Atomic Dual-Boot (Slot A / Slot B) с откатом за 1 секунду
  - [x] Горячая замена драйверов и приложений без перезагрузки (HotSwapEngine)
  - [x] Децентрализованный реестр WASM с подписями Ed25519 (ManifestValidator + StoreRegistry)
  - [x] `GET /api/v1/store/index` и `POST /api/v1/store/register` REST-эндпоинты
- [x] **Фаза 27: Система отладки и чёрный ящик (`aios-telemetry` и `aios-debug`) — ВЫПОЛНЕНО**
  - [x] Сквозной `TraceID` structured tracing (TraceContext)
  - [x] Flight Recorder — кольцевой буфер с фильтрацией по типу и дампом (FlightRecorder)
  - [x] Zero-Knowledge анонимизированные отчёты об ошибках (CrashReporter + PanicHandler)
  - [x] Prometheus-совместимый endpoint `/api/v1/metrics`
  - [x] `GET /api/v1/traces` и `POST /api/v1/crash-report` REST-эндпоинты

- [x] **Фаза 33: Браузерный блок «из коробки» — ЗАВЕРШЕНА**
  - [x] `BrowserBlock`, реализующий `StatefulBlock` в `aios-browser` (IPC: `browse`, `open_native`, `browser_status`, `HealthCheck`)
  - [x] Ядро (`aios`) регистрирует hal/ipc_bus/scheduler/browser при загрузке + boot-обнаружение `AIOS_BLOCKS_DIR` + подключение браузерного обработчика к `MessageRouter`
  - [x] Клавиша `b` в TUI ядра — открытие любого URL в системном браузере через браузерный блок
  - [x] Браузер работает «из коробки» на новом компьютере (без конфига, установленного браузера и сети)

- [x] **Фаза 34: Полноценный нативный браузер (`aios-webview`) — ЗАВЕРШЕНА**
  - [x] Новый крейт `aios-webview`: нативное окно WebView (wry 0.56 + winit 0.30) с куки, JavaScript и историей «из коробки»
  - [x] `WebBrowser::open/navigate/back/forward/close` — неблокирующие команды через `EventLoopProxy`; браузер работает на фоновом потоке
  - [x] Персистентный профиль через `WebContext` (`AIOS_DATA_DIR`/`aios/webview`) — куки и хранилище переживают перезапуск
  - [x] `resolve_target()` — правило омнибокса, общее с TUI (URL / голый хост / запрос DuckDuckGo)
  - [x] Модуль `launcher` — поиск и запуск бинарника `aios-gui` (рядом с exe, затем PATH)
  - [x] Вкладка GUI Browser (F7) в `aios-gui` — омнибокс, Back/Forward, Open/Close, строка статуса
  - [x] Горячая клавиша `W` в TUI (и `aios-tui`, и ядро `aios`) запускает дашборд GUI
  - [ ] Будущее: встроить webview как дочернее окно внутри вкладки egui через `build_as_child` (Windows/macOS/X11), заменив окно-компаньон

- [x] **Фаза 35: WHATWG-рендеринг HTML и навигация вкладки Web в TUI — ЗАВЕРШЕНА**
  - [x] `HtmlParser` перестроен на `scraper`/html5ever — структурированный текст (заголовки `#`, списки `•`/`1.`, таблицы `|`, `pre`, `hr`, изображения `[alt]`), WHATWG-совместимый
  - [x] Резолвинг ссылок относительно базового URL + дедупликация + фильтр не-web-схем + канонизация корневых URL (без завершающего слэша)
  - [x] `Renderer` адаптирован под реальное DOM-дерево html5ever
  - [x] `WebState.history` — навигация назад во вкладке Web (`b`)
  - [x] Клавиши прокрутки текста страницы `u`/`d` (±1 строка) и `PageUp`/`PageDown` (±20 строк) с индикатором прокрутки `X–Y`
  - [x] Панель страницы рендерится по видимой высоте с переносом (без переполнения)

- [x] **Фаза 36: Отзывчивая вкладка Web — фоновая загрузка, кэш страниц, прокрутка ссылок — ЗАВЕРШЕНА**
  - [x] Веб-загрузки вынесены на фоновые потоки (никогда не блокируют TUI) со счётчиком поколений, отбрасывающим устаревшие результаты
  - [x] Ограниченный кэш страниц (`WebState.cache`, 20 страниц, старейшие вытесняются) — мгновенный возврат назад (`b`) и повторные визиты
  - [x] Окно ссылок прокручивается за выбором (6 видимых строк, видимый диапазон в заголовке)
  - [x] Текст страницы раскрашивает структуру: заголовки жирным циановым, пустые строки тёмно-серым

- [x] **Фаза 37: Перенос текста страницы по ширине — ЗАВЕРШЕНА**
  - [x] Хелпер `wrap_text()`: перенос по границам слов, принудительный разрыв длинных слов, сохранение пустых строк и отступов
  - [x] Единицы прокрутки равны визуальным строкам — `u`/`d`/`PageUp`/`PageDown` двигают ровно на одну/20 видимых строк, низ длинной страницы достижим
  - [x] `WebState.wrap_width` отслеживается из `crossterm::terminal::size()` и обновляется на `Event::Resize`

- [x] **Фаза 38: Навигационный сайдбар вкладки Web — ЗАВЕРШЕНА**
  - [x] Сайдбар истории фиксированной ширины (`SIDEBAR_WIDTH = 26`) слева от панели страницы: текущая страница первая (отмечается `▸`), история от новых к старым, без дублей
  - [x] Компактные ярлыки URL (`compact_url_label`) обрезаются до ширины панели
  - [x] Фокус сайдбара через `\`: `j`/`k`/`Up`/`Down` двигают выбор, `Enter`/`o` открывают запись, `Esc` — назад к ссылкам; выбор зациклен
  - [x] `web_page_width()` — ширина переноса текста из ширины терминала минус сайдбар/рамки/префикс; применяется при старте и на ресайзе (пропорциональная панель завершена)
  - [x] Будущее: закладки с сохранением во вкладке Web — `a` добавить (имя предзаполняется заголовком), `m` открыть панель, `j`/`k`/`o`/`d`/`Esc` управление, сохранение в `AIOS_DATA_DIR/web_bookmarks.json` (v2.14.1)
  - [x] Будущее: вкладки (несколько открытых страниц) во вкладке Web — `t` новая вкладка, `x` закрыть активную, `[`/`]` переключение; состояние страницы/прокрутки/выбора/истории/ошибки на каждую вкладку, фоновые загрузки направляются во вкладку-источник (v2.15.0)

- [x] **Фаза 39: Полноценный нативный браузер из вкладки Web — ЗАВЕРШЕНА**
  - [x] `B` открывает текущую страницу в полноценном нативном браузере (`aios-webview` WebView2 — JS/CSS/картинки); окно переиспользуется и пересоздаётся автоматически, открытие в фоновом потоке
  - [x] `n` открывает выбранную ссылку в нативном браузере
  - [x] Handle браузера в модульном `OnceLock<Mutex<Option<WebBrowser>>>` — ядро не тронуто
  - [x] `http_client()`: десктопный User-Agent + `Accept: text/html` + таймаут 15с для текстовых загрузок (меньше бот-блокировок, нет зависаний)
  - [ ] Будущее: встроить webview как дочернее окно вкладки Browser в GUI через `build_as_child` (Windows/macOS/X11), заменив companion window
  - [x] Будущее: headless render-to-text fallback для JS-тяжёлых сайтов — `aios-browser::headless` дампит DOM в headless-браузере класса Chromium (`msedge`/`chromium`/`google-chrome`, переопределение через `AIOS_HEADLESS_BROWSER`, `--no-sandbox` через `AIOS_HEADLESS_NO_SANDBOX`), когда обычная загрузка не даёт читаемого текста; принимается только если отрендеренный текст заметно богаче (v2.17.0)

- [x] **Фаза 40: Хранилище блоков — источники, каталог, установщик, сервис обновлений — ЗАВЕРШЕНА**
  - [x] `aios-store::source`: `StoreSource`/`SourceKind` — GitHub (`github:owner/repo`), локально (`local:path`), HTTP-сервис обновлений (`http://host:port`)
  - [x] `aios-store::catalog`: `fetch_index`/`download_block` (async HTTP + локальное сканирование `*.wasm`/`*.bin` + sidecar JSON), `parse_name_version`
  - [x] `aios-store::installer`: `BlockInstaller` — `{name}_{version}.wasm` + sidecar, проверка SHA-256, `list_installed`/`find_installed`/`uninstall`, `backup`/`rollback` (`.bak`), `check_updates`, семантический `cmp_version`
  - [x] `aios-store::manager`: фасад `StoreManager` — `search`/`install`/`update` (автооткат)/`check_updates`/`parse_source_spec`/`block_on`
  - [x] Крейт `aios-net-config`: `NetworkConfig` + частичные `apply_updates`, `NetworkConfigStore` (атомарный JSON), `NetSettingsBlock` (`net_get`/`net_set`/`net_reset`/`net_persist`, StatefulBlock + roundtrip состояния)
  - [x] Сервис обновлений в `aios-bridge`: `GET /index.json`, `GET /blocks/{name}.wasm`, `GET /store/index.json`, `GET /store/blocks/{name}.wasm`, `POST /api/v1/store/publish`
  - [x] Команды TUI-шелла: `store list|sources|add-source|search|install|update|uninstall|rollback` и `net get|set|reset`
  - [x] Тесты: 32 (`aios-net-config`) + 42 (`aios-store`) юнит-теста, 2 новых интеграционных теста (поток обновлений + roundtrip net-блока)

- [x] **Фаза 41: Блок сетевых настроек в ядре + store publish — ЗАВЕРШЕНА**
  - [x] Блок `net_settings` регистрируется в реестре ядра при загрузке (`aios/src/orchestrator.rs`), подключается к `MessageRouter`, id доступен как `OrchestratorState::net_block_id`
  - [x] Горячая клавиша `n` в TUI ядра: режим ввода пар `key=value` для обновления сетевой конфигурации через IPC (`net_set`) с выводом результата в панель событий
  - [x] `store publish <file.wasm> [name] [version]` в шелле `aios-tui` — SHA-256 + base64 → `POST /api/v1/store/publish` (порт из `AIOS_BRIDGE_PORT`)
  - [x] Тесты роутера ядра (4): регистрация в реестре + маршрутизация `net_get`/`net_set`/`net_reset` через IPC

- [x] **Фаза 42: Подписанные манифесты блоков Ed25519 с политикой доверия — ЗАВЕРШЕНА**
  - [x] Реальная подпись/проверка Ed25519 в `aios-store::manifest`: `canonical_bytes()` + `sign_manifest()` + реальный `verify_strict` в `verify_signature` + `verify_signature_with_keys` (11 тестов)
  - [x] Enforcement в `BlockInstaller`: `trusted_keys`, `with_trusted_keys`/`from_env`, `Default` читает `AIOS_TRUSTED_PUBLIC_KEYS`; sidecar сохраняет полный манифест включая подпись (16 тестов)
  - [x] Политика доверия по источникам: `StoreSource.trusted_public_keys`, `StoreManager::verify_source_manifest` в `install()`/`update()`, официальный ключ GitHub через `AIOS_OFFICIAL_PUBLIC_KEY` (2 теста менеджера)
  - [x] TUI-шелл `store sign <file.wasm> [name] [version] [--key <hex>]` + `store verify <name>`
  - [x] Будущее: подписанный `store publish` (манифест подписывается до установки мостом) — `store publish --key` подписывает манифест по Ed25519, и мост проверяет подпись (плюс свою локальную политику доверия) перед установкой — и команда `store trust <source> [--key <public_hex>] [--clear]` для задания доверенных ключей источника из шелла, сохраняется через конфиг источников (реализовано, задокументировано в v2.16.0)

- [x] **Фаза 43: AI Console — слэш-команды, панель справки, смена бэкенда на лету — ЗАВЕРШЕНА (v2.6.0)**
  - [x] AI Console ядра (вкладка 3): слэш-команды `/help /status /clear /history /system /model /backend /key /temp /tokens`
  - [x] Смена бэкенда/модели/ключа на лету применяется к общему движку (HTTP `/api/v1/llm/query` остаётся согласованным)
  - [x] Встроенная панель справки по `h` или `/help`; история промптов (последние 50) через `Up`/`Down`
  - [x] Строка состояния + отчёт `/status` включая обнаружение локальных GGUF-моделей; перенос ответов по ширине с подсветкой промптов/ошибок
  - [x] Интроспекция конфигурации в `aios-llm`: `LlmEngine::config()`, `provider_name()`, `backend_label()`; 1 новый unit-тест
  - [x] Продолжение в Фазе 45: сохранение чата на диск + шаблоны `/preset` + потоковые ответы (v2.9.0)

- [x] **Фаза 45: AI Console — сохранение чата, шаблоны `/preset`, стриминг — ЗАВЕРШЕНА (v2.9.0)**
  - [x] Стриминговый API `aios-llm::LlmEngine::query_stream` (SSE-дельты OpenAI + Google AI Studio, локальная генерация по токенам); `extract_stream_delta` + 4 теста
  - [x] AI Console стримит ответы вживую (жёлтая частичная строка во время запроса)
  - [x] Чат сохраняется в JSON Lines в `AIOS_DATA_DIR/chat.jsonl`; автосохранение после каждого ответа и при выходе; восстановление при старте; ручное `/save` `/load`
  - [x] Семейство команд `/preset` со встроенными шаблонами (`assistant`, `code`, `translator`, `explainer`): применить / создать / список / удалить

- [x] **Фаза 45b: паритет GUI AI Studio — стриминг, персистентность, `/preset` — ЗАВЕРШЕНО (v2.9.1)**
  - [x] GUI AI Studio стримит ответы вживую (жёлтая частичная строка) через тот же канал `query_stream`; запросы дедуплицируются в один рабочий слот
  - [x] Чат GUI сохраняется в общий `AIOS_DATA_DIR/chat.jsonl`; автосохранение после каждого ответа и при закрытии окна; восстановление при старте; ручное `/save` `/load`
  - [x] Шаблоны `/preset` GUI сохраняются в `AIOS_DATA_DIR/presets.json`; встроенные шаблоны при старте перекрываются сохранёнными
  - [x] Новые команды GUI: `/system <text>`, `/history`, `/preset`, `/save`, `/load` + обновлённая справка и подсказки

- [x] **Фаза 44: TUI ядра из 7 вкладок, safe mode, GUI AI Studio + Network Settings — ЗАВЕРШЕНО (v2.8.0)**
  - [x] TUI ядра (`aios`) переструктурирован под спецификацию из 7 вкладок: System & HW / Blocks & Svc / AI Console / Studio Bridge / Network & Store / Web / Shell; `1`-`7` + `Alt`+`1`-`7` + `Tab`/`F1`/`?`; в шапке AI Tier + версия
  - [x] Вкладка Blocks `r`/`k`/`l` перезапуск/выгрузка/загрузка; вкладка Web полный набор клавиш (`g j k o u d PageUp PageDown b B n`); вкладка Shell полный набор команд (`ps blocks kill spawn store list/search/install net get/set status logs restart help clear`) с вводом инлайном
  - [x] Флаг загрузки `--safe-mode` (пропуск сторонних блоков с диска + мост; шапка `SAFE MODE`; минимальная восстанавливаемая оболочка)
  - [x] GUI переструктурирован в 7 вкладок: System Dashboard (объединённые overview+metrics+processes) / WASM Blocks / AI Studio / App Store / Network Settings / Deps / Native Browser
  - [x] GUI AI Studio: асинхронный чат с LLM со слэш-командами, фоновая tokio-задача, строка статуса
  - [x] GUI Network Settings: форма (hostname/port/таймауты/private-access/DNS/user-agent) с Save/Reset + живой JSON-предпросмотр
  - [x] Строка состояния GUI: `HW Tier | IPC: N pkts | F6=Deps F7=Browser` с живым счётчиком IPC-пакетов
  - [x] Паритет GUI AI Studio: стриминг + персистентность чата/шаблонов (Фаза 45b, v2.9.1)

- [ ] **Фаза 46: aios-autohal — автоматическое обеспечение оборудованием и хранилище драйверов (Master Brief)**
  - **Роль**: автоматическое определение подключённого оборудования по слепку; поиск, скачивание и адаптация открытых драйверов в изолированные `.wasm`-модули; безопасный запуск в WASM-песочнице с выдачей Capability-токенов; локальное кэширование со 100% паритетом TUI/GUI.
  - [x] `fingerprint.rs` — `HardwareFingerprint`/`BusType` (USB/PCI/Bluetooth/ACPI/NVMe), извлечение из `aios-hal::HardwareProfile`
  - [x] `manifest.rs` — `DriverManifest` (id, name, version, supported_hardware, required_capabilities, hash_sha256, entry_point), JSON-схема + валидация, `DriverSource` (Redox Tree / Linux Core / Custom Store / Builtin / Generic)
  - [x] `catalog.rs` — офлайн-каталог встроенных драйверов + generic fallback `GENERIC_WAT`
  - [x] `fetcher.rs` — конвейер `DriverFetcher`: builtin → реестр custom store → Redox tree → зеркало Linux Core; WASM или исходники C/Rust, проверка хэша SHA-256
  - [x] `adapter.rs` — `SourceAdapter`: переписывание вызовов `inb/outb/readl/writel/ioread*` на host-импорты `hal_*`, компиляция C/Rust в `wasm32-wasi`
  - [x] `registry.rs` — `DriverStore`/`DriverIndex`: кэш `AIOS://store/drivers/`, маппинг fingerprint→driver, счётчики сбоев, override прав
  - [x] `engine.rs` — асинхронный конвейер из 5 шагов: детекция (HAL event loop) → локальный поиск в store → сетевой поиск/адаптация → проверка SHA-256 + выдача прав (`CapabilityToken`) + инстанцирование в Wasmtime → кэширование и регистрация
  - [x] `engine.rs` self-healing: после 3 сбоев подряд автопереход на Generic Fallback Driver с предупреждением в UI
  - [x] `ui_tui.rs` — ratatui-виджет Hardware Inspector: дерево устройств по шинам (USB/PCI/NVMe), бейджи статуса ([Active]/[Downloading...]/[Fallback/Generic]), отображение прав, hot-plug тосты `[Hardware] Detected USB 046D:0825 -> Fetching WASM Driver... [OK]`
  - [x] `ui_gui.rs` — egui-панель Hardware & Drivers: таблица устройств с иконками, VID/PID, источником драйвера; прогресс скачивания/компиляции; интерактивная матрица прав (checkbox'ы); кнопки [Update Driver]/[Rollback to Generic]/[Uninstall]
  - [x] Тесты: unit-тесты каждого модуля (всего 57); speed-тест с двойными порогами debug/release (debug 50 мкс / release 8 мкс на операцию fingerprint)
  - [x] Доки: ARCHITECTURE/CHANGELOG/INTERFACE (EN + RU)

### Целевые показатели готовности
| Веха | Целевая готовность | Ключевой разрыв |
|------|-------------------|-----------------|
| Текущее | **100%** | Распределённое планирование |
| + Привязка к CPU + миграция состояния | 90%+ | Распределённое планирование |
