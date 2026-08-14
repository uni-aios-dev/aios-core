# Журнал разработки AIOS

## v2.25.0 — Нативные push-уведомления hot-plug (2026-08-14)

### `native.rs` (aios-autohal)
- Новый нативный push-слушатель `NativeHotplugMonitor`: ядро сообщает AIOS *сразу*, когда дерево устройств движется, поэтому полный `HardwareProfile::detect()` + диф отпечатков запускается ровно тогда, когда это гарантированно полезно, а не по истечении следующего тика сигнала/опроса (каденция v2.24.1, по умолчанию 250 мс).
- **Windows**: скрытое message-only окно + `RegisterDeviceNotificationW` (все классы device-interface). `WM_DEVICECHANGE` с `DBT_DEVICEARRIVAL` / `DBT_DEVICEREMOVECOMPLETE` будит выделенный поток message-pump, который передаёт монитору грубое `NativeEvent` (`added` + `BusHint`).
- **Linux**: сокет `NETLINK_KOBJECT_UEVENT` (multicast-группа 1) разбирает uevents `add`/`bind`/`change`/`remove`/`unbind`; имя подсистемы классифицируется в `BusHint`. Чистый FFI на `libc`, завязан на `cfg(target_os = "linux")`.
- `BusHint` (Usb/Pci/Nvme/Storage/Other) позволяет монитору игнорировать посторонний шум (дисплеи, звук, HID) без оплаты полного сканирования. Нативный слой осознанно **не** строит отпечатки сам — он лишь вовремя запускает авторитетный `HardwareProfile::detect()`.
- `HotplugConfig` получает `native_enabled` (по умолчанию `true`); `false` форсирует чистый путь опроса/дешёвого сигнала (например, окружения без нативного источника). `HotplugMonitor::start` дренирует нативные события каждый тик; любое релевантное событие сразу запускает полное пере-определение, дешёвый сигнал и страховочная сетка `poll_ms` остаются запасным вариантом.
- Чистое завершение: `HWND` публикуется сразу после создания окна, поэтому `Drop` шлёт `WM_CLOSE` (pump выходит, поток джойнится); ограниченное ожидание в `Drop` убирает стартовую гонку, где `WM_CLOSE` мог бы потеряться, а ограниченный join утекает «заклинивший» слушатель вместо зависания вызывающего.
- Тесты: +3 в `hotplug.rs` (`native_monitor_starts_and_stops_cleanly` — реальный PnP-слушатель на Windows и проверка появления скрытого окна, `disabled_native_falls_back_to_polling`, обновлённый `config_defaults`) плюс юнит-тесты нативного модуля (фильтр релевантности, классификация Windows-интерфейсов, разбор Linux-uevent). Workspace: 1332 теста проходят, clippy — ноль предупреждений, `cargo fmt` чист.
- Файлы: `aios-autohal/src/{lib,native,hotplug}.rs`, `aios-autohal/Cargo.toml` (libc для linux-таргета), `docs/*`.

## v2.24.1 — Адаптивное пере-определение hot-plug по дешёвому сигналу дерева устройств (2026-08-14)

### `hotplug.rs` (aios-autohal)
- Монитор больше не запускает дорогой `HardwareProfile::detect()` безусловно каждый опрос. На Linux дешёвый сигнал изменения (`cheap_signal` → `dir_signal_hash`) хэширует mtime каталогов `/sys/bus/{usb,pci,nvme}/devices` — они меняются ровно при подключении/отключении устройства — поэтому полное определение запускается в момент реального движения дерева устройств (задержка ≤ `signal_poll_ms`, по умолчанию 250 мс), а не по истечении фиксированного интервала.
- `HotplugConfig` получает `signal_poll_ms` (по умолчанию 250) — частоту проверки дешёвого сигнала; `poll_ms` (по умолчанию 1000) остаётся страховочной сеткой полного сканирования, гарантирующей свежее определение как минимум с этой частотой даже при отсутствии движения.
- Платформы без такого сигнала (например Windows) сохраняют фиксированную частоту полного сканирования — поведение не изменилось относительно v2.24.0; отсутствующее/нечитаемое дерево `/sys` деградирует так же (нет сигнала → фиксированная частота).
- Чистая реализация только на std, как принято в репозитории (`aios-hal` на Linux читает sysfs, без FFI-крейтов). Поток монитора по-прежнему лишь выдаёт события отпечатков; изменение движка остаётся в потоке UI.
- Тесты: +5 (стабильность/различимость/пропуск отсутствующих путей у `dir_signal_hash`, на не-Linux нет дешёвого сигнала, дефолты конфига). Workspace: 1328 тестов проходят, clippy — ноль предупреждений, `cargo fmt` чист.
- Файлы: `aios-autohal/src/hotplug.rs`, `docs/*`.

## v2.24.0 — Живой hot-plug цикл событий для обеспечения оборудованием (2026-08-14)

### Hot-plug демон (aios-autohal + kernel TUI + GUI)
- Новый `hotplug.rs` — фоновый `HotplugMonitor`: отдельный поток периодически пере-определяет подключённый `HardwareProfile` (интервал `HotplugConfig::poll_ms`, по умолчанию 1000 мс), извлекает набор отпечатков (`extract_fingerprints`) и сравнивает его с предыдущим опросом, отдавая `HotplugEvent::Added/Removed(HardwareFingerprint)` через канал `mpsc`. Первый опрос лишь фиксирует базовую линию, чтобы старт не выглядел как массовое подключение. Монитор корректно останавливается в `Drop` (общий флаг `AtomicBool` + `join`).
- `engine.rs` — новый `AutohalEngine::remove_device(&HardwareFingerprint)`: выгружает WASM-инстанс и запись `DeviceDriver` для физически удалённого устройства, но сохраняет закэшированный драйвер в `DriverStore` (запись индекса удаляется, индекс персистится), поэтому повторное подключение обеспечивается мгновенно без сетевого запроса; добавляет info-тост (`[Hardware] USB 046D:0825 removed -> driver cached, re-provisions on replug`).
- Kernel TUI (`aios`): `TuiApp` запускает монитор вместе с движком (`hw_hotplug`, инертен в safe mode) и разгребает его каждый тик (`hw_poll_hotplug` перед `hw_refresh` в цикле `run`): `Added` → `provision_blocking`, `Removed` → `remove_device`. Периодический пере-скан по `F10` остаётся ручным полным rescan.
- GUI (`aios-gui`): `AiosApp` зеркально запускает тот же монитор (`hw_hotplug` вместе с движком) и разгребает его в каждом кадре `update`, так что вкладка Hardware & Drivers отражает живое отключение/подключение без ручных действий.
- Поток монитора никогда не трогает движок напрямую (`AutohalEngine` владеет не-`Send` инстансами Wasmtime); он лишь выдаёт события отпечатков, которые применяет поток UI.
- Тесты: unit-тесты `hotplug.rs` (`diff_fingerprints` добавление/удаление, отчёт при первом скане, пропуск warm-up, неизменный набор, хелперы событий) + тест `engine::remove_device` (устройство выпадает, тост, повторное подключение из кэша). Workspace: 1323 теста проходят, clippy — ноль предупреждений, `cargo fmt` чист.
- Файлы: `aios-autohal/src/{lib,hotplug,engine}.rs`, `aios/src/tui/{app_state,mod}.rs`, `aios-gui/src/app.rs`, `docs/*`.

## v2.23.0 — Живая интеграция aios-autohal в kernel TUI и GUI (2026-08-14)

### Живая интеграция (паритет TUI/GUI по Master Brief)
- `aios-gui` получает вкладку **Hardware & Drivers (F9)**: `AiosApp` создаёт `AutohalEngine` при старте (`hw_init`), запускает первичный проход обеспечения по обнаруженному `HardwareProfile`, а пока вкладка открыта — обновляет снимки `DeviceView`/тостов (`hw_refresh`). Действия панели — `Rescan`, `Update`, `Rollback to Generic`, `Uninstall`, переопределения прав — возвращаются в движок через `apply_hw_actions` (`tabs/hardware.rs` рендерит общую панель `HardwarePanel`).
- Kernel TUI (`aios`) встраивает тот же **Hardware Inspector** во вкладку System & HW (`draw_system_tab`): `TuiApp` теперь хранит `hw_engine`/`hw_views`/`hw_toasts`, инициализируется в `new()` через `init_hw_engine` (инертен в safe mode), обновляется каждый тик (`hw_refresh`) и пере-скан при `F10` (`refresh_hw` использует `HardwareProfile::detect()`).
- Паритет интерфейсов — на уровне данных: обе поверхности рендерят одни и те же снимки `DeviceView` и ленту тостов из единого движка.
- Тесты: состав workspace-набора не изменился, но бинарники `aios` и `aios-gui` теперь собираются с общим движком; clippy — ноль предупреждений; `cargo fmt` чист.
- Гигиена clippy для `--all-targets`: исправлено предварительно существовавшее предупреждение в тестах `aios-browser` (`headless_fallback` перенесён в литерал `BrowserConfig`), три среза `&[key.clone()]` в тестах `aios-store` (теперь `std::slice::from_ref`) и сравнение `== false` в тестах `aios-vfs` (теперь `!is_root()`).
- Файлы: `aios-gui/Cargo.toml`, `aios-gui/src/app.rs`, `aios-gui/src/tabs/hardware.rs`, `aios-gui/src/tabs/mod.rs`, `aios/Cargo.toml`, `aios/src/tui/app_state.rs`, `aios/src/tui/ui.rs`, `aios/src/tui/mod.rs`, `docs/*`.

## v2.22.0 — Автоматическое обеспечение оборудованием и хранилище драйверов (aios-autohal) (2026-08-14)

### Новый крате `aios-autohal`
- Полный конвейер автоматического обеспечения по Master Brief: определение оборудования (`extract_fingerprints` из `aios-hal::HardwareProfile` — USB/PCI/NVMe/Bluetooth/ACPI), поиск/скачивание драйвера (`DriverFetcher`: builtin-каталог → реестр custom store → Redox Tree → зеркало Linux Core, WASM или исходники C/Rust), адаптация исходников (`SourceAdapter` переписывает вызовы `inb/outb/readl/writel/ioread*` на host-импорты `hal_*` и компилирует в `wasm32-wasi`), проверка SHA-256, инстанцирование с выдачей прав и локальное кэширование в `DriverStore` в `AIOS_DATA_DIR/drivers`.
- `engine.rs` — `AutohalEngine`: асинхронный конвейер из 5 шагов (`rescan`/`provision`/`provision_blocking`/`provision_dedicated`) + self-healing: после 3 сбоев подряд устройство автоматически переходит на Generic Fallback Driver (`GENERIC_FALLBACK_ID`) с предупреждающим тостом; поддержаны явные `rollback_to_generic`, `uninstall_driver` (generic защищён) и override прав на устройство (`set_cap_override`).
- `ui_tui.rs` — ratatui-виджет `HardwareInspector`: таблица устройств по шинам (USB/PCI/NVMe/Bluetooth/ACPI), бейджи статуса ([Active]/[Downloading...]/[Compiling]/[Generic]/[Failed]/[Rolled Back]), сводка прав и лента hot-plug тостов (`[Hardware] Detected USB 046D:0825 -> Fetching WASM Driver... [OK]`).
- `ui_gui.rs` — egui-панель `HardwarePanel` со 100% паритетом данных: таблица устройств (VID/PID, источник драйвера, цветные статусы), прогресс-бары скачивания/компиляции, интерактивная матрица прав безопасности (checkbox'ы) и кнопки [Update Driver]/[Rollback to Generic]/[Uninstall]/[Rescan].
- `manifest.rs` — JSON-схема `DriverManifest` с `required_capabilities` (через имена `Capability`) + валидация; `registry.rs` — `DriverStore`/`DriverIndex` персистентно хранят маппинг fingerprint→driver, счётчики сбоев и override прав (bincode/serde).
- Исправления при доведении крате до чистой сборки: `rewrite_register_access` смешивал типы `&str`/`String` (переписан на `Vec<(&str, String)>`, сигнатура `rewrite_idents` обновлена), `?` над `Option` в функциях, возвращающих `Result<Option<_>>` (`fetch_from_registry`/`fetch_from_catalog` теперь возвращают `Ok(None)` при промахе), частичный перенос `fetched` в `engine.rs` (состояние источника зафиксировано через `matches!` до match), неиспользуемый импорт `Deserialize` в `manifest.rs` и лишний `&` у `ProgressBar` в `ui_gui.rs`.
- 57 unit-тестов (fingerprint, manifest, fetcher, registry, engine, ui_tui, ui_gui), включая speed-тест с двойными порогами (debug 50 мкс / release 8 мкс на операцию extract+key+driver_id) — проходят в debug и release; clippy без предупреждений; `cargo fmt` чист.
- Файлы: `aios-autohal/src/{lib,adapter,catalog,engine,fetcher,fingerprint,manifest,registry,ui_tui,ui_gui}.rs`, `aios-autohal/Cargo.toml`, `aios-autohal/src/fetcher.rs`, `Cargo.toml` (член workspace), `docs/*`.

## v2.21.0 — Репликация контрольных точек в распределённом кластере (2026-08-09)

### `aios-cluster`
- Воркеры теперь реплицируют состояние автоматически: каждый heartbeat-период узел-воркер извлекает снимок каждого локально размещённого процесса и рассылает всем пирам fire-and-forget `Checkpoint { from, rid, state }`, так что каждый координатор постоянно хранит актуальное состояние отслеживаемых процессов без явного round-trip `GetState`.
- Восстановление при failover стало автоматическим: при потере узла `tick()` перезапускает его отслеживаемые процессы в другом месте и восстанавливает на новом хосте самую свежую реплицированную контрольную точку, после чего новый хост сам реплицирует свои снимки (инъекция состояния со стороны координатора не нужна).
- Полученные контрольные точки получают отметку времени и вычищаются по новому `checkpoint_ttl` (builder `.with_checkpoint_ttl`, по умолчанию 15 с) внутри `tick()`, чтобы устаревшие снимки давно молчащего узла нельзя было случайно воскресить. Аксессор `checkpoints()` отдаёт реплицированные снимки (сортировка, при записи побеждает самый свежий снимок процесса).
- `ClusterConfig` получает `checkpoint_ttl_ms` (serde default + переменная окружения `AIOS_CLUSTER_CHECKPOINT_TTL_MS`); оркестратор `aios` пробрасывает его в builder планировщика.
- Новые тесты: unit `test_checkpoint_replicated_and_restored_on_failover` (кластер из 3 узлов: spawn с payload, ожидание репликации, остановка хоста, проверка, что перезапущенный процесс встаёт на выживший узел с восстановленным и повторно реплицированным снимком) и `test_checkpoint_pruned_when_stale` (нулевой TTL, tick удаляет снимок). Круговой тест контрольной точки в протоколе уже был. (21 unit + 10 integration + 1 doc).
- Файлы: `aios-cluster/src/scheduler.rs`, `aios-cluster/src/config.rs`, `aios/src/orchestrator.rs`, `docs/*`.

## v2.20.0 — Миграция процессов с переносом состояния в распределённом кластере (2026-08-09)

### `aios-cluster`
- Снимки состояния процесса стали полноправной частью: `ProcessExecutor` получает `extract_state(pid)` / `restore_state(pid, bytes)`; спавн засевает снимок из `spec.payload`, так что тесты могут внедрять состояние через спек спавна. `MockProcessExecutor` и `SchedulerProcessExecutor` хранят непрозрачные по-процессные снимки.
- Сетевой протокол: `Spawn` несёт опциональный `state: Option<Vec<u8>>`, который узел назначения восстанавливает сразу после спавна; новые сообщения `GetState` / `GetStateReply` забирают снимок с узла, размещающего процесс.
- `DistributedScheduler::migrate` теперь переносит состояние: через `get_state` забирает снимок исходника, спавнит копию на назначении с восстановленным снимком и лишь затем убивает исходник. При неудаче получения состояния или спавна исходник остаётся нетронутым и отслеживаемым.
- Новые тесты: unit `mock_state_roundtrip` (жизненный цикл состояния исполнителя: засев, восстановление, ошибки, сброс при kill) и `test_get_state_roundtrip` (круговая сериализация протокола); интеграционный `migrate_carries_process_state` (кластер из 3 узлов, spawn с payload на узел b, migrate на узел c, проверка, что снимок восстановлен на c и удалён на b). (18 unit + 10 integration + 1 doc).
- Файлы: `aios-cluster/src/protocol.rs`, `aios-cluster/src/executor.rs`, `aios-cluster/src/scheduler.rs`, `aios-cluster/tests/scheduling.rs`, `docs/*`.

## v2.19.0 — Управление кластером из Shell ядерного TUI (2026-08-09)

### Ядерный TUI (`aios`)
- `OrchestratorState` теперь удерживает планировщик кластера живым (`cluster: Option<Arc<Mutex<DistributedScheduler>>>`); раньше узел кластера создавался и сразу отбрасывался, поэтому управлять им из TUI было невозможно.
- Новые команды Shell: `cluster status` (собственный узел + пиры со статусом/tier/нагрузкой, плюс удалённые и локально размещённые процессы), `cluster nodes`, `cluster spawn <name> [ram_mb] [priority] [target_node]`, `cluster kill <node> <pid>` и `cluster migrate <node> <pid> [target_node]`. spawn/kill/migrate переиспользуют блокирующее API `DistributedScheduler` и запускаются на реальных потоках планировщика `aios-process-mgr` через `SchedulerProcessExecutor`. Без `AIOS_CLUSTER_PEERS` обработчик отвечает `clustering disabled`.
- Новые unit-тесты в `aios/src/tui/mod.rs`: `cluster_disabled_prints_hint` и `cluster_shell_spawn_kill_migrate` (кластер из 3 узлов на in-memory транспорте через shell-обработчик: spawn на узел 2, migrate на узел 3, kill). Крейт `aios` теперь имеет 6 unit-тестов.
- Файлы: `aios/src/orchestrator.rs`, `aios/src/tui/mod.rs`, `docs/*`.

## v2.18.0 — Миграция процессов в распределённом кластере (2026-08-09)

### `aios-cluster`
- Новый `DistributedScheduler::migrate(rid, target)` перемещает отслеживаемый удалённый процесс на другой узел: копия спавнится на назначении (явный узел `target` или активная стратегия размещения, никогда не исходный узел) и лишь затем оригинал убивается, так что при неудаче спавна исходник остаётся нетронутым и отслеживаемым.
- Обработаны крайние случаи: перенос на исходный узел отклоняется, перенос неотслеживаемого/неизвестного процесса завершается ошибкой на ранней стадии, а если стратегия выбрала тот же узел, лишняя копия убивается до возврата ошибки.
- Новые интеграционные тесты в `tests/scheduling.rs`: `migrate_moves_process_between_nodes` (явный перенос узла проверяет смену хоста на исполнителях плюс перенос по стратегии, который никогда не возвращает исходный узел) и `migrate_rejects_same_node_or_unknown` (пути ошибок, процесс остаётся целым после отклонённых попыток). 2 новых интеграционных теста (16 unit + 9 integration + 1 doc).
- Файлы: `aios-cluster/src/scheduler.rs`, `aios-cluster/tests/scheduling.rs`, `docs/*`.

## v2.17.0 — Headless-фолбэк рендера в текст для JS-тяжёлых сайтов (2026-08-09)

### `aios-browser`
- Новый модуль `headless`: когда обычный HTTP-запрос не возвращает читаемого текста (признак SPA-страницы, рендерящейся на клиенте), движок запускает headless-браузер класса Chromium с `--dump-dom` и повторно разбирает полностью отрендеренный DOM, так что текстовый вид TUI теперь показывает реальный контент на JS-сайтах.
- Поиск браузера перебирает `msedge`, `microsoft-edge`, `chromium`, `chromium-browser`, `google-chrome`, `google-chrome-stable`, `chrome`, `brave-browser` плюс известные пути Windows (Edge/Chrome в `Program Files`) и macOS; бинарник можно переопределить через `AIOS_HEADLESS_BROWSER`, добавить `--no-sandbox` — через `AIOS_HEADLESS_NO_SANDBOX=1`.
- Дамп использует `--virtual-time-budget=5000` (ускорение виртуального времени, чтобы скрипты выполнялись быстро), ограничен 4 МиБ, выполняется на блокирующем потоке с таймаутом 30 с и принимается только если отрендеренный текст заметно богаче обычной загрузки (`has_more_content`, +60 непробельных символов); иначе исходный HTML остаётся авторитетным.
- Управляется флагом `BrowserConfig::headless_fallback` (включён по умолчанию); новые unit-тесты покрывают построение CLI, эвристики «оболочка/богаче контент», путь ошибки при отсутствии бинарника и решения фолбэка в движке (5 новых тестов).
- Файлы: `aios-browser/src/headless.rs` (новый), `aios-browser/src/engine.rs`, `aios-browser/src/types.rs`, `aios-browser/src/lib.rs`, `docs/*`.

## v2.16.0 — Задокументированы подписанный `store publish` и `store trust` (2026-08-09)

### Инструменты доверия Block Store
- Рабочий процесс подписанного `store publish` и `store trust` (код появился ещё в v2.11.0) теперь полностью задокументирован: `store publish <file.wasm> [name] [version] [--key <secret_hex>]` подписывает манифест по Ed25519 до загрузки; мост проверяет подпись через `ManifestValidator::verify_signature` и применяет свою локальную политику доверия (`AIOS_TRUSTED_PUBLIC_KEYS` через `BlockInstaller::from_env`) перед установкой.
- `store trust <source> [--key <public_hex>] [--clear]` управляет `StoreSource.trusted_public_keys` из шелла `aios-tui`: `--key` добавляет проверенный 32-байтовый публичный ключ Ed25519, `--clear` удаляет все, без флагов — печатает текущие ключи; сохраняется через `StoreManager::save_config`.
- Обновлены доки: `docs/INTERFACE.md`, `docs/ARCHITECTURE.md`, `docs/TODO.md`; исправлена устаревшая заметка в BUGS о том, что мост использует `BlockInstaller::new`, — теперь корректно `from_env`.

## v2.15.0 — Вкладки Web-браузера (несколько открытых страниц) (2026-08-09)

### Ядерный TUI (`aios`), вкладка Web
- Текстовый браузер теперь поддерживает несколько открытых страниц-вкладок: `t` — новая вкладка, `x` — закрыть активную вкладку (последнюю закрыть нельзя), `[`/`]` — переключиться на предыдущую/следующую вкладку (с зацикливанием).
- Каждая вкладка хранит свой URL, загруженную страницу, смещение прокрутки, выбранную ссылку, состояние ошибки и историю «назад»; при переключении состояние восстанавливается мгновенно без повторной загрузки.
- Пока страница грузится в фоновой вкладке, активная вкладка продолжает работать — результат загрузки направляется во вкладку, которая его запустила (`FetchOut` теперь несёт индекс целевой вкладки), а `web_poll` применяет его только там.
- Панель контента рисует строку вкладок (активная подсвечена жёлтым), когда открыто больше одной вкладки; заголовок панели становится `Text Browser — Tab N/M`.
- Нижняя строка-подсказка теперь перечисляет `t tab x close [ ] switch`.
- Файлы: `aios/src/tui/app_state.rs`, `aios/src/tui/mod.rs`, `aios/src/tui/ui.rs`, `docs/*`.

## v2.14.1 — Закладки Web-вкладки с персистентностью (2026-08-09)

### Ядерный TUI (`aios`), вкладка Web
- Новая функция закладок на вкладке Web (таб 6): `a` добавляет текущую страницу в закладки (имя предзаполняется заголовком страницы), `m` открывает панель закладок; внутри панели `j`/`k` — выбор, `o`/`Enter` — открыть закладку, `d` — удалить, `Esc` — закрыть.
- Закладки сохраняются как JSON-массив в `AIOS_DATA_DIR/web_bookmarks.json` (та же директория, что и `chat.jsonl`/`presets.json`), пишутся при добавлении/удалении/переименовании и на выходе, восстанавливаются при старте.
- Панель закладок заменяет список ссылок, пока открыта; под загруженной страницей показывается строка-подсказка `'a' bookmark  'm' bookmarks (N)`.
- Файлы: `aios/src/tui/app_state.rs`, `aios/src/tui/mod.rs`, `aios/src/tui/ui.rs`, `docs/*`.

## v2.14.0 — `aios-init` становится `/init` initramfs по умолчанию в Live ISO (2026-08-09)

### `live/build.sh` — режим aios-init теперь по умолчанию
- Шаг [4] теперь упаковывает initramfs на базе `aios-init` по умолчанию: `aios-init` как `/init`, бинарник `aios` как `/system/aios-core`, busybox только как спасательный шелл — ядро грузится сразу в ядерный TUI без `switch_root` в squashfs.
- Прежний путь busybox-initramfs (`init.rs` + squashfs-корень + `switch_root`) сохранён за флагом-отключением `USE_BUSYBOX_INIT=1`.
- Шаг [5] записывает отдельное GRUB-меню с параметрами `init=/init console=tty0` по умолчанию («AIOS (aios-init kernel TUI)» / «AIOS (verbose)»); при `USE_BUSYBOX_INIT=1` используется прежний `/work/grub.cfg`.
- Переключатель `USE_AIOS_INIT=1` удалён — aios-init теперь единственный и всегда активный режим по умолчанию.
- Файлы: `live/build.sh`, `docs/*`.

## v2.13.0 — `aios-init` передаёт управление реальному ядерному TUI как `/system/aios-core` (2026-08-09)

### `build_initramfs.sh` — размещение полноценного ядерного TUI в initramfs
- Initramfs теперь собирает и упаковывает реальный ядерный бинарник `aios` (статический musl, `cargo build -p aios --release --target x86_64-unknown-linux-musl --no-default-features`) как `/system/aios-core`, поэтому `aios-init` сразу загружает полноценный ядерный TUI (7 вкладок, AI Console, Watchdog, кластер) вместо перехода в спасательный шелл. Спасательный шелл остаётся запасным вариантом, когда сборка aios пропущена или завершилась ошибкой.
- Флаг `--no-aios-core` / переменная окружения `SKIP_AIOS_CORE=1`: пропустить сборку и размещение aios (получается прежний initramfs только со спасательным шеллом).
- Флаг `--keep-rootfs`: не удалять стейджинг-каталог `rootfs/` после упаковки (удобно для изучения структуры).
- Защита очистки rootfs: скрипт отказывается удалять путь за пределами `SCRIPT_DIR` (`${ROOTFS}` должен начинаться с `${SCRIPT_DIR}/`) — страховка от `rm -rf` по неверному пути.

### `live/build.sh` — опциональный переключатель `USE_AIOS_INIT=1`
- Новый режим сборки `USE_AIOS_INIT=1` для шага [4]: собирает `aios-init` (rust в Alpine уже musl), размещает бинарник `aios` как `/system/aios-core` и `aios-init` как `/init`, а busybox остаётся только спасательным шеллом — ядро грузится сразу в ядерный TUI AIOS без `switch_root` в squashfs.
- В этом режиме шаг [5] записывает отдельное GRUB-меню с параметрами `init=/init console=tty0` («AIOS (aios-init kernel TUI)» / «AIOS (verbose)»).
- Обычный путь (busybox `init.rs` + корень squashfs) не меняется; переключатель опционален и включается переменной окружения.
- Файлы: `build_initramfs.sh`, `live/build.sh`, `docs/*`.

## v2.12.0 — `aios-init`: статический musl init-процесс PID 1 для initramfs (2026-08-08)

### `aios-init` (новый автономный крейт, вне workspace)
- Новый Rust `/init` для initramfs AIOS — минимальный, статически слинкованный (`x86_64-unknown-linux-musl`) процесс PID 1, который полностью устраняет `Kernel panic: No working init found`: он никогда не паникует и всегда приземляется в шелл или в idle-цикл.
- Единственная зависимость — `libc`; release-профиль использует `panic = "abort"`, `lto`, `opt-level = "z"` и `strip` для крошечного статического бинарника.
- Последовательность загрузки: монтирование `/proc` (proc), `/sys` (sysfs), `/dev` (devtmpfs с запасным `mknod` для `/dev/console`, `/dev/null`, `/dev/tty`), `/tmp` (tmpfs) → открытие `/dev/console` и перенаправление stdin/stdout/stderr → супервизор блока AIOS.
- Передача управления блоку: запуск и супервизия `/system/aios-core` (затем `/installer`), до 3 автоматических перезапусков с задержкой 300 мс; при чистом выходе код возврата попадает в журнал загрузки.
- Соответствие PID 1: обработчики `sigaction` для SIGTERM/SIGINT/SIGHUP (передаются потомку, SIGTERM → ожидание 5 с → SIGKILL) и SIGCHLD (`SA_NOCLDSTOP`); непрерывная сборка зомби через `waitpid(-1, WNOHANG)`, поэтому осиротевшие «внуки» не накапливаются.
- Аварийный запасной вариант: если блок не найден или все перезапуски исчерпаны, запускается спасательный шелл (`/bin/sh`, затем `/bin/busybox sh`, затем `/bin/ash`); если шелла нет — init остаётся в idle-цикле со сборкой зомби вместо паники.
- Верификация: `cargo check` и `cargo clippy` (ноль предупреждений) для `x86_64-unknown-linux-musl`, `cargo fmt` чистый.

### `build_initramfs.sh` + загрузочная обвязка
- Новый `build_initramfs.sh` собирает initramfs: компилирует бинарник под musl-таргет, создаёт структуру `/bin /dev /proc /sys /tmp /system`, копирует его в `/init`, `chmod +x` и упаковывает `initramfs.cpio.gz` через `find | cpio --format=newc | gzip -9`. Опциональный `BUSYBOX_PATH=/usr/bin/busybox.static` добавляет busybox-шелл для аварийного режима.
- Интеграция с workspace: `aios-init` исключён из корневого workspace (unix-only системный код), поэтому `cargo build --workspace` на Windows не затрагивается.
- Параметры ядра (GRUB): `linux /boot/vmlinuz init=/init console=tty0`; Syslinux: `APPEND init=/init console=tty0 quiet`. См. `docs/ARCHITECTURE.md`, слой 8.
- Файлы: `aios-init/*` (новое), `build_initramfs.sh` (новое), `Cargo.toml`, `docs/*`.

## v2.11.0 — Многоузловой распределённый кластер (`aios-cluster`) (2026-08-08)

### `aios-cluster` (новый крейт) — распределённое планирование
- Новый крейт workspace: узлы обнаруживают друг друга через сменный транспорт, обмениваются снимками нагрузки на heartbeat'ах, размещают процессы по стратегии и выполняют failover, когда узел замолкает.
- `types.rs`: `NodeId`, `NodeStatus {Unknown, Online, Offline, Leaving}`, `NodeMetrics` (CPU/RAM/число процессов + `load_fraction()`), `NodeInfo` (id, имя, адрес, аппаратный `tier`), `RemoteProcessId` (`node:pid`), `RemoteProcessSpec` (приоритет 0–4, квота RAM, опциональные block id / payload / диапазон tier), `RemoteProcessStatus`, `PlacementStrategy {RoundRobin, LeastLoaded, ByTier}`, `now_ms()`.
- `protocol.rs`: enum `ClusterMessage` (`Hello`, `Metrics`, `Spawn`/`SpawnAck`, `Kill`/`KillAck`, `SetPriority`/`SetPriorityAck`, `StatusRequest`/`StatusReply`), bincode `encode`/`decode_frame` с фреймингом `[u32 LE len]`; 4 unit-теста.
- `transport.rs`: трейт `ClusterTransport`; `InMemoryClusterTransport` + общий `MemoryRegistry` (однопроцессный, детерминированный); `TcpClusterTransport` (настоящий loopback-listener, кадры с длиной); 2 unit-теста.
- `executor.rs`: трейт `ProcessExecutor`; `MockProcessExecutor` (детерминированный, модель RAM 16 GiB для осмысленной нагрузки); `SchedulerProcessExecutor` — мост к реальному планировщику `aios-process-mgr`; 1 unit-тест.
- `scheduler.rs`: `DistributedScheduler` — роли координатора и воркера; обнаружение через heartbeat Hello, отслеживание живости с `failover_threshold`, размещение (`LeastLoaded`/`RoundRobin`/`ByTier` + фильтры по tier), блокирующие `spawn`/`kill`/`set_priority` с таймаутом подтверждения, `tick()` — детект отказа и перезапуск процессов упавшего узла, ограниченный журнал событий. Метрики известного узла берутся только из отдельного сообщения `Metrics` (снимок в Hello перезаписывал бы живую нагрузку устаревшим idle). 8 unit-тестов.
- `config.rs`: `ClusterConfig` из окружения (`AIOS_CLUSTER_*`) или JSON; 2 unit-теста.
- Интеграционные тесты (`tests/scheduling.rs`, 7): discovery/spawn/kill двух узлов, чередование round-robin, размещение по наименьшей загрузке, failover-перезапуск на выживший узел, удалённая смена приоритета, реальный TCP loopback spawn/kill, пути ошибок (неизвестный узел / нет пиров).
- Верификация: `cargo build --workspace`, clippy и fmt чистые; `aios-cluster` — 16 unit + 7 integration + 1 doc-тест, все проходят.
- Файлы: `aios-cluster/*` (новое), `Cargo.toml`, `docs/*`.

## v2.10.0 — Виртуальная файловая система + двухпанельный файловый менеджер (2026-08-07)

### `aios-vfs` (новый крейт) — слой виртуальной файловой системы
- Новый крейт workspace с путями через схемы и асинхронным вводом-выводом:
  - `VfsScheme::{AIOS, HOST}`, `VfsPath` (URI-стиль: `AIOS:///sandbox`, `HOST:///C:/...`), `VfsEntry`, `VfsMetadata`.
  - Трейт `VirtualFileSystem`: `list`, `read`, `write`, `create_dir`, `delete`, `rename`, `exists`, `metadata`, `open_seek` (возвращает `Box<dyn AsyncSeekReader + Send + Unpin>`, где `AsyncSeekReader = AsyncRead + AsyncSeek`).
  - `AiosVfs` (песочница в локальной папке) и `HostVfs` (реальные пути хоста, доступ через capability-токены).
  - `operations.rs`: `Progress` (атомарные счётчики, `fraction()`/`pressure_fraction()`), `CancellationToken`, асинхронные `copy_recursive` / `move_item` / `delete_item` / `total_bytes` / `read_head` / `read_at`.
  - `security.rs`: `AclContext` (capability-токены `vfs:host:read`, `vfs:host:write`) + проверка `canonicalize_inside` на вхождение пути в песочницу.
  - `ai_preview.rs`: `analyze_file` + `AiPreview`/`AiLineKind` — эвристическое AI-превью файлов (разбор WASM name-section, детектор паник в логах, подсказки по исходникам).
- 29 unit-тестов (в т.ч. отмена копирования, байты секции имён WASM-модуля, проверка canonicalize).

### `aios-fm` (новый крейт) — двухпанельный файловый менеджер (стиль Volkov/Far)
- `state.rs`: `PanelState` (курсор, `SortRule`, записи), `human_size`.
- `commands.rs`: `Command` / `Ack` через `tokio::mpsc::unbounded_channel`.
- `engine.rs`: `FileManager` с фоновым циклом команд; Copy/Move/Delete выполняются как отменяемые `tokio::spawn`-задачи с `Progress` + `JobInfo`; `FmSnapshot { panels, active, jobs, acl }`; помощники `set_cursor` / `set_active`.
- `ui_tui.rs`: `draw` (шапка + две панели + футер с горячими клавишами), `key_to_action`, `progress_bar`.
- `ui_gui.rs`: `show` (две колонки, выбор кликом/двойным кликом через `FmClick`, полосы прогресса, панель ACL).
- 16 unit-тестов (engine, state, keymap, GUI-тема).

### TUI: вкладка Files (8)
- Новая вкладка `Files` рендерится через `aios_fm::ui_tui::draw`; `FileManager` + `AclContext` запускаются на tokio-рантайме при старте (песочница = `AIOS_DATA_DIR/vfs_sandbox`).
- Клавиши: Tab / стрелки — навигация, Enter — открыть папку или AI-превью файла, Backspace — родительская папка, F3 — просмотр, F5 — копировать, F6 — переместить, F7 — создать папку, F8 — удалить, F2 — переименовать, F9 — сортировка, `g`/`w` — выдача host read/write, `r` — обновить, Esc — закрыть превью.
- Канал Ack опрашивается на каждой итерации цикла; превью и логи обновляются вживую.

### GUI: вкладка Files (8)
- Новая вкладка `Files` (`tabs/files.rs`): панель инструментов (Refresh/Switch/Sort/Up/Mkdir/Rename/View/Copy/Move/Delete, HOST r/w), две панели, модальный диалог mkdir/rename, сворачиваемое AI-превью, живой прогресс задач; одинарный клик — выбор, двойной — открытие.
- `AiosApp::fm_init()` запускает движок на отдельном tokio-рантайме; Ack опрашиваются каждый кадр; F3/F5–F9 и стрелки/Enter/Tab/Backspace сопоставлены FM-действиям внутри вкладки.

### Верификация
- `cargo build --workspace`, clippy и fmt чистые; тесты: `aios-vfs` 29, `aios-fm` 16, `aios-tui` 40, `aios-gui` 10 — все проходят.
- Файлы: `aios-vfs/*` (новое), `aios-fm/*` (новое), `Cargo.toml`, `aios-tui/src/{main,dashboard}.rs`, `aios-gui/src/{app,main}.rs`, `aios-gui/src/tabs/files.rs`, `aios-gui/src/tabs/mod.rs`, `docs/*`.

## v2.9.5 — загрузочный Live USB образ + исправления для Linux (2026-08-06)

### Live USB (загрузочная флешка AIOS)
- Собран полноценный **гибридный ISO** (BIOS + UEFI) с меткой `AIOS-LIVE` (~1.24 ГБ), который грузит ядро Linux и автоматически запускает ядро TUI AIOS.
- Состав: меню GRUB (AIOS Live / подробная консоль / пункт установщика), `/boot/vmlinuz` (Alpine `linux-lts` 6.18), `/boot/initramfs.gz` (кастомный busybox init: находит и монтирует squashfs в loop), `/boot/aios.squashfs` (сжатый rootfs со статическим musl-бинарником `aios` и установщиком).
- Rootfs — минимальный Alpine 3.24 minirootfs; система грузится прямо в TUI AIOS на `tty1`; сеть (DHCP) поднимается при загрузке; `aios-install` ставит AIOS на локальный диск (GPT: EFI + ext4, загрузчик GRUB).
- Сборка воспроизводима через `live/build.sh` в Docker (`alpine:latest`, оффлайн-crates из локального registry): статический musl-бинарник без `webview`, alpine minirootfs, squashfs, initramfs, `grub-mkrescue`.
- Записан на флешку 1.9 ГБ (PS2) и проверен: запись байт-в-байт, SHA-256 флешки совпадает с ISO (`67596162...`).

### `aios`: исправления сборки под Linux (`hw_probe.rs`)
- В GPU-пробе для `#[cfg(target_os = "linux")]` вызывался `Command::output()`, но забыто `.ok()?.stdout` перед `String::from_utf8(...)` (два места), из-за чего крейт никогда не компилировался под Linux. Исправлены оба Linux-места и эквивалентное macOS-место.
- Проверено: Linux static-musl release сборка теперь компилируется; Windows-сборка и тесты не изменились.
- Файлы: `aios/src/hw_probe.rs`, `live/*` (новые скрипты сборки), `docs/CHANGELOG.md` (+ `.ru`)

## v2.9.4 — опциональная feature `webview` для headless/Live сборок (2026-08-06)

### `aios`: cargo feature `webview`
- Встроенный браузер ядра TUI (клавиша `W`, а также `B`/`n` на вкладке Web) зависит от `aios-webview` → `wry` → WebKitGTK, что тяжело для Linux. `aios-webview` теперь опциональная зависимость за feature `webview` (включена по умолчанию).
- `cargo build -p aios --no-default-features` собирает ядро без GTK/WebKit-линковки — используется для статической musl-сборки Live USB (см. Live USB в v2.9.4 ниже).
- Проверено: обе конфигурации (`default` и `--no-default-features`) собираются чисто, clippy чист в обеих, все 4 теста `aios` проходят.
- Файлы: `aios/Cargo.toml`, `aios/src/tui/mod.rs`

## v2.9.3 — GUI: исправлена утечка светлой системной темы (2026-08-06)

### `aios-gui`: невидимые поля TextEdit при светлой системной теме
- Сообщённая проблема: поля на вкладке Network Settings (и другие текстовые поля) отображались белым по белому — eframe 0.31 стартует с системной темой ОС (`egui_winit::State::new(..., event_loop.system_theme(), ...)`), которая на этой машине светлая, поэтому `visuals.extreme_bg_color` оставался белым `#FFFFFF`, а `theme.apply()` переопределял только `dark_mode` и часть палитры виджетов. Фон TextEdit рисуется из `extreme_bg_color`, поэтому светлый текст лежал на белом поле.
- `AiosTheme::apply` теперь задаёт все поля поверхности `Visuals` явно: `override_text_color = None`, `hyperlink_color = accent`, `faint_bg_color = TRANSPARENT`, `extreme_bg_color = #1E1E2A`, `code_bg_color = #242432`, `warn_fg_color = warning`, `error_fg_color = danger`, `panel_fill`/`window_fill = surface`, `window_stroke = border`, `bg_stroke = border` для всех пяти состояний виджетов, плюс ранее отсутствующее состояние `open` (`bg_fill`/`weak_bg_fill`/`fg_stroke`).
- Проверено по пикселям на запущенном GUI: фон TextEdit теперь `#1E1E2A` с ярким текстом `#D4D4DF` и рамкой `#3A3A4A`; секции, элементы DragValue и кнопки отображаются в тёмной палитре.
- Файлы: `aios-gui/src/theme.rs`, `docs/INTERFACE.md`, `docs/INTERFACE.ru.md`

## v2.9.2 — GUI: исправлена читаемость текста кнопок (2026-08-06)

### `aios-gui`: различимость кнопок
- Сообщённая проблема: цвет текста кнопок и цвет их фона почти совпадали, кнопки плохо читались. Кнопки egui по умолчанию рисуются цветом `visuals.weak_bg_fill` (не задан → нейтральный серый `gray(60)`), а текст вторичных кнопок использовал `muted` (#78788C) — контраст около 2.5:1.
- Добавлен отдельный `button_bg` (#2E2E3E) в `AiosTheme` и подключён в `widgets.inactive/hovered/active.weak_bg_fill` и `bg_fill`, поэтому у каждой кнопки теперь единый, чуть более светлый фон, чем у карточек/секций; состояния hover/active дополнительно осветляются.
- Осветлён `muted` (#78788C → #A5A5B9) и `text_dim` (#8C8CA0 → #9B9BAF), чтобы затемнённый текст и подписи оставались читаемыми.
- Явные заливки кнопок переведены с `surface_alt` на `button_bg` (Quick Actions в сайдбаре `app.rs`, кнопка Send в AI Studio); две кнопки Cancel на вкладке Blocks переведены с `muted` на `text_dim`.
- Файлы: `aios-gui/src/theme.rs`, `aios-gui/src/app.rs`, `aios-gui/src/tabs/ai_studio.rs`, `aios-gui/src/tabs/blocks.rs`, `docs/INTERFACE.md`, `docs/INTERFACE.ru.md`

## v2.9.1 — GUI AI Studio: стриминг, `/preset`, паритет персистентности (2026-08-05)

### `aios-gui`: AI Studio повторяет функциональность AI Console TUI
- Ответы чата теперь **стримятся**: дельты приходят по unbounded-каналу из фоновой задачи `tokio` и отображаются вживую (жёлтая строка), пока идёт запрос. Запросы дедуплицируются в единственный рабочий слот (`pending_ai`), поэтому одновременная отправка не портит ленту.
- Лог чата сохраняется в JSON Lines в общий `AIOS_DATA_DIR/chat.jsonl` (та же схема, что и в TUI): автосохранение после каждого завершённого ответа и при закрытии окна, восстановление при старте через `ai_load_persisted`; ручное управление через `/save` и `/load`.
- Шаблоны промптов сохраняются в `AIOS_DATA_DIR/presets.json` (pretty JSON, тот же формат, что и в TUI): `/preset <name>` применяет шаблон как системный промпт, `/preset <name> <text>` создаёт шаблон, `/preset list` показывает список, `/preset del <name>` удаляет шаблон. Встроенные шаблоны (`assistant`/`code`/`translator`/`explainer`) при старте перекрываются сохранёнными.
- Новые слэш-команды: `/system <text>`, `/history`, `/preset`, `/save`, `/load` (добавлены в панель справки AI Studio); подсказки интерфейса обновлены.
- Версия в шапке TUI ядра обновлена до `AIOS v2.9.1`.
- Файлы: `aios-gui/src/app.rs`, `aios-gui/src/main.rs`, `aios-gui/src/tabs/ai_studio.rs`, `aios/src/tui/ui.rs`

### `aios-hal`: исправлено падение при старте (BUG-039)
- `HardwareProfile::detect` паниковал на машинах, где `wmic memorychip /format:csv` выдаёт короткие строки — `detect_memory` обращался к `parts[2]` после проверки только `parts.len() >= 2`. Вынос разбора в чистый помощник `parse_wmic_memory_csv` (требующий `parts.len() >= 3`) делает загрузку GUI и TUI стабильной. 2 новых unit-теста.
- Файлы: `aios-hal/src/hardware.rs`

## v2.9.0 — AI Console: персистентность чата, шаблоны `/preset`, стриминг (2026-08-05)

### `aios-llm`: стриминговые запросы
- Новый `LlmEngine::query_stream(&LlmRequest, LlmStreamSink)` отправляет дельты текста через unbounded-канал `tokio` вместо полного ответа.
- Облачный бэкенд (`cloud.rs`) читает тело HTTP-ответа как поток байтов, разбивает SSE-строки `data:` и извлекает дельты из формата OpenAI (`choices[0].delta.content` / устаревший `choices[0].text`) и Google AI Studio (`candidates[0].content.parts[0].text`) через новый помощник `extract_stream_delta`.
- Локальный бэкенд (`local.rs`) рефакторингован: `query` и `query_stream` используют общий цикл `generate_tokens` с колбэком `on_delta` на каждый декодированный токен, поэтому локальные модели тоже стримятся по токенам.
- 4 новых unit-теста для `extract_stream_delta`.
- Файлы: `aios-llm/src/types.rs`, `aios-llm/src/cloud.rs`, `aios-llm/src/local.rs`, `aios-llm/src/factory.rs`, `aios-llm/Cargo.toml`

### `aios`: персистентность и шаблоны промптов AI Console
- Ответы теперь **стримятся** в AI Console: дельты накапливаются в `ai_stream` и отображаются вживую жёлтым цветом, пока идёт запрос; по завершении итоговый текст добавляется в ленту. `/help` документирует стриминговое поведение.
- Лог чата сохраняется в JSON Lines в `AIOS_DATA_DIR/chat.jsonl` (по умолчанию `aios_data/chat.jsonl`): автосохранение после каждого завершённого ответа и при выходе, восстановление в ленту при старте; ручное управление через `/save` и `/load`.
- Новое семейство команд `/preset` с четырьмя встроенными шаблонами (`assistant`, `code`, `translator`, `explainer`): `/preset <name>` применяет шаблон как системный промпт, `/preset <name> <text>` создаёт/перезаписывает шаблон, `/preset list` показывает список, `/preset del <name>` удаляет шаблон.
- Файлы: `aios/src/tui/mod.rs`, `aios/src/tui/app_state.rs`, `aios/src/tui/ui.rs`

## v2.8.0 — TUI ядра из 7 вкладок, safe mode, GUI AI Studio и Network Settings (2026-08-05)

### `aios`: TUI ядра приведён к спецификации из 7 вкладок
- TUI ядра `aios` (ratatui) теперь содержит 7 вкладок: **System & HW**, **Blocks & Svc**, **AI Console**, **Studio Bridge**, **Network & Store**, **Web**, **Shell**. Прямой выбор через `1`-`7`, `Alt`+`1`-`7`, `Tab`/`F1`, оверлей справки через `?`. В шапке показывается обнаруженный **AI Tier** и версия приложения (`AIOS v2.8.0`).
- Вкладка Blocks получила клавиши `r`/`k`/`l` (перезапуск / выгрузка / загрузка с диска) в дополнение к выбору. Вкладка Web реализует полный набор клавиш спецификации (`g` омнибокс, `j/k` выбор ссылки, `o`/`Enter` открыть, `u/d` и `PageUp`/`PageDown` прокрутка текста, `b` назад, `B` нативный просмотрщик, `n` открыть выбранную ссылку нативно). Вкладка Shell реализует `ps`, `blocks`, `kill`, `spawn`, `store list/search/install`, `net get/set`, `status`, `logs`, `restart`, `help`, `clear` и полностью вводится инлайном (каждое нажатие идёт в строку ввода; `q` выходит только с других вкладок).
- Клавиши `n`/`g` перенесены на вкладку Network & Store; удалены старые горячие клавиши ядра `b`/`r` и хак `dispatch_open_url`.
- Файлы: `aios/src/tui/mod.rs`, `aios/src/tui/ui.rs`, `aios/src/tui/app_state.rs`

### `aios`: флаг загрузки `--safe-mode`
- Новый CLI-флаг `--safe-mode`: AIOS загружается только с минимальным ядром — сторонние блоки с диска не обнаруживаются, HTTP/WebSocket мост выключен, в шапке показывается `SAFE MODE`. Ядро, планировщик, watchdog, LLM-движок, TUI и Shell остаются доступны.
- Файлы: `aios/src/main.rs`, `aios/src/orchestrator.rs`

### `aios-gui`: вкладки AI Studio и Network Settings
- GUI переструктурирован из 6-табовой спецификации в 7 вкладок: **System Dashboard** (объединённые overview + metrics + processes), **WASM Blocks**, **AI Studio**, **App Store**, **Network Settings**, **Deps**, **Native Browser**. Модули вкладок `processes` и `metrics` удалены.
- AI Studio: асинхронный чат с LLM со слэш-командами (`/help /backend /model /key /temp /tokens /clear /history`), строка статуса, отправка по Enter с сохранением фокуса; запросы выполняются в фоновой tokio-задаче, интерфейс остаётся отзывчивым.
- Network Settings: форма hostname/port/таймауты/private-access/DNS/user-agent с кнопками Save (частичное JSON-обновление по IPC в `net_settings`) и Reset, плюс живой JSON-предпросмотр.
- Строка состояния теперь показывает `HW Tier | IPC: N pkts | F6=Deps F7=Browser` с живым счётчиком IPC-пакетов.
- Файлы: `aios-gui/src/app.rs`, `aios-gui/src/tabs/mod.rs`, `aios-gui/src/tabs/ai_studio.rs`, `aios-gui/src/tabs/network.rs`, `aios-gui/src/tabs/overview.rs`, `aios-gui/Cargo.toml`

## v2.7.0 — Проход по багам: корректность, надёжность, правки UI (2026-08-04)

### `aios-browser`: `extract_text` возвращал пустой текст на страницах с `<!DOCTYPE html>`
- **BUG-021 (ВЫСОКИЙ)** — `HtmlParser::extract_text` обрабатывал только собственный текст body, поэтому корень документа вида `<!DOCTYPE html><html>...</html>` давал пустой результат. Парсер теперь обходит дочерние элементы корня документа и возвращает видимый текст независимо от doctype.
- Новый регрессионный тест `test_extract_text_with_doctype`.
- Файлы: `aios-browser/src/html_parser.rs`

### `aios-ipc`: политика `DropOldest` удаляла самый критичный пакет
- **BUG-022 (СРЕДНИЙ)** — `IpcBus` `DropOldest` извлекал элемент из начала очереди, но очередь упорядочена по приоритету (наивысший первым), поэтому переполнение выбрасывало самый важный пакет и сохраняло наименее важный. Теперь удаляется с конца (наименьший приоритет).
- Новый тест `test_drop_oldest_keeps_highest_priority`.
- Файлы: `aios-ipc/src/bus.rs`

### `aios-bridge`: список процессов не показывал самый новый; метрики состояния всегда были 0
- **BUG-023 (СРЕДНИЙ)** — `status_handler` перебирал PID `0..process_count`, но идентификаторы процессов начинаются с 1, поэтому самый новый процесс никогда не отображался. Теперь используется `scheduler.all_processes()`.
- **BUG-024 (СРЕДНИЙ)** — ветка `MetricType::All` читала `process_count` после удаления планировщика, фиксируя `0`. Счётчик теперь читается до удаления.
- Файлы: `aios-bridge/src/server.rs`

### `aios-tui`: возврат назад в Web «пинг-понгом» вечно крутился
- **BUG-025 (СРЕДНИЙ)** — нажатие `b` для возврата назад снова добавляло текущую страницу в историю, поэтому возврат к A и повторное `b` возвращали к B (цикл A↔B). `load_url` теперь принимает `push_history: bool`; возврат назад вынимает из истории без повторного добавления. Обновлены все точки вызова (навигация, клик по ссылке, открытие из сайдбара).
- **BUG-026 (СРЕДНИЙ)** — быстрые нажатия `B` могли открыть второе окно нативного браузера. Атомарный флаг `WEB_BROWSER_SPAWNING` теперь разрешает только один запуск.
- Файлы: `aios-tui/src/main.rs`

### `aios-gui`: открытие браузера блокировало UI до 45 с и могло открыться дважды
- **BUG-027 (СРЕДНИЙ)** — открытие WebView происходило синхронно в потоке egui. Теперь оно запускается в фоновом потоке с ячейками `pending_browser`/`pending_browser_error` и флагом `browser_opening`; `poll_browser_open` подхватывает результат каждый кадр.
- Файлы: `aios-gui/src/app.rs`

### `aios-search`: redirect-URL DuckDuckGo `uddg` не раскрывался
- **BUG-028 (СРЕДНИЙ)** — URL результатов, указывающие на `/l/?uddg=...`, возвращались как есть. `resolve_duckduckgo_url` теперь извлекает параметр `uddg` (пропуская значения, не начинающиеся с http/s).
- `aios-search` добавляет зависимость `url`; 4 новых теста.
- Файлы: `aios-search/src/backends.rs`, `aios-search/Cargo.toml`

### `aios-context`: пакеты телеметрии и сжатия затирали предыдущие данные
- **BUG-029 (СРЕДНИЙ)** — `save_telemetry` писал каждый пакет под один и тот же ключ, поэтому более поздние пакеты затирали ранние. Ключи теперь берутся из монотонного счётчика `TELEMETRY_NEXT_KEY` в `META_TABLE`.
- **BUG-030 (СРЕДНИЙ)** — ключи чанков сжатой телеметрии строились от ключа метрики/времени, который совпадал в каждом раунде, так что каждое сжатие затирало предыдущий чанк. Чанки теперь используют монотонный `next_chunk_id`.
- Новые тесты `test_save_telemetry_does_not_clobber_previous_batches` и `test_multiple_compression_rounds_do_not_collide`.
- Файлы: `aios-context/src/persistence.rs`, `aios-context/src/compressed_telemetry.rs`

### `aios-core`: `response_err` терял текст ошибки
- **BUG-031 (СРЕДНИЙ)** — `response_err` возвращал `Payload::Empty`, выбрасывая текст ошибки. Сообщение теперь передаётся как `Payload::Text(msg)`.
- Новый тест `test_response_err_carries_message`.
- Файлы: `aios-core/src/ipc_protocol.rs`

### `aios-security`: `remaining_ms` у capability был инвертирован
- **BUG-032 (НИЗКИЙ)** — `remaining_ms()` вычислял `now − expires`, возвращая почти ноль для долгоживущих capability. Теперь `expires_at_ms.saturating_sub(now_ms())`.
- Новый тест с будущим сроком истечения.
- Файлы: `aios-security/src/capability.rs`

### `aios-process-mgr`: счётчик наследования никогда не считался
- **BUG-033 (НИЗКИЙ)** — `total_inheritances` был объявлен, но не инкрементировался, поэтому всегда показывал 0. Теперь он растёт в обоих путях повышения приоритета (при взятии блокировки и при запросе ресурса) и доступен через `state()`.
- Новые тесты.
- Файлы: `aios-process-mgr/src/priority_inheritance.rs`

### `aios-wasm`: восстановление линейной памяти молча обрезало лишние данные
- **BUG-034 (НИЗКИЙ)** — `restore_linear_memory` копировал `min(data, memory)`, молча теряя байты. Теперь при превышении размера памяти возвращается явная ошибка; `aios-live-update` логирует предупреждение при неудачном восстановлении.
- Новый тест `test_restore_linear_memory_rejects_oversized_data`.
- Файлы: `aios-wasm/src/sandbox.rs`, `aios-live-update/src/wasm_engine.rs`

### `aios-process-mgr`: CPU affinity применялся к потоку планировщика
- **BUG-035 (НИЗКИЙ)** — вызов affinity ОС действует на вызывающий поток, поэтому маска применялась к потоку планировщика, а не к потоку процесса. Маска теперь хранится на поток и применяется самим порождённым потоком перед выполнением payload; `validate_cores` предварительно проверяет маску.
- Файлы: `aios-process-mgr/src/cpu_affinity.rs`, `aios-process-mgr/src/scheduler.rs`

### `aios`: инверсия порядка блокировок TUI/моста
- **BUG-036 (НИЗКИЙ)** — вкладка блоков в TUI брала блокировки `scheduler → registry`, а мост — `registry → scheduler`, классическая предпосылка взаимоблокировки. Теперь везде порядок `scheduler → registry`; список процессов строится через `all_processes()` вместо жёстко заданных PID 1..5.
- Файлы: `aios/src/tui/ui.rs`

### `aios`: 32-битное переполнение `AdapterRAM` в WMIC
- **BUG-037 (НИЗКИЙ)** — значение `AdapterRAM` = 0xFFFFFFFF (VRAM более 4 ГБ) показывалось как мнимые ~4 ГБ вместо неизвестного. Такие значения теперь считаются неизвестными (0).
- Файлы: `aios/src/hw_probe.rs`

### `aios-wasm`: `timeout_ms` теперь применяется через тикер эпохи
- **BUG-038 (НИЗКИЙ)** — `timeout_ms` никогда не применялся как реальное время: ни один поток не вызывал `Engine::increment_epoch()`, поэтому дедлайн эпохи был недостижим, и только топливо ограничивало выполнение. Фоновый тикер на движок (`EpochTicker`) теперь инкрементирует эпоху каждые `timeout_ms / 4`; store взводятся на `EPOCH_TICKS_PER_TIMEOUT = 4` тика, а `call_func`/`instantiate` (плюс `init`/`start` в executor) перевзводят дедлайн перед каждым вызовом wasm, так что каждый вызов ограничен `timeout_ms`, а долгоживущие store продолжают работать.
- Новые тесты `test_epoch_timeout_interrupts_runaway_wasm` и `test_epoch_deadline_rearmed_between_calls`; всего в aios-wasm теперь 56 unit-тестов.
- Файлы: `aios-wasm/src/sandbox.rs`, `aios-wasm/src/executor.rs`

### Тесты и верификация
- Набор workspace: 82 test-цели, 0 падений в debug. `cargo clippy --workspace --all-targets -- -D warnings` — 0 предупреждений; `cargo fmt --all --check` — чисто.
- 17 новых тестов на каждый фикс (включая два теста на тайм-аут эпохи).

## v2.6.0 — AI Console: слэш-команды, панель справки, смена бэкенда на лету (2026-08-04)

### TUI ядра (`aios`) — AI Console (вкладка 3): переработка
- **Система слэш-команд** в строке ввода ИИ: `/help`, `/status`, `/clear`, `/history`, `/system <промпт>`, `/model <имя>`, `/backend <groq|openrouter|google|micro|full>`, `/key <api-ключ>`, `/temp <0.0-2.0>`, `/tokens <1-8192>`
- **Смена бэкенда на лету**: `/backend`, `/model` и `/key` асинхронно пересоздают общий `LlmEngine` внутри `BridgeContext`, поэтому HTTP-эндпоинт `POST /api/v1/llm/query` использует ту же конфигурацию, что и консоль; каждый запрос также повторно применяет текущую конфигурацию консоли перед выполнением
- **Панель справки**: встроенный стилизованный справочник клавиш и слэш-команд во вкладке AI, открывается по `h` или `/help`, закрывается `Esc`/`h`/`q`
- **История промптов**: `Up`/`Down` листают последние 50 промптов во время ввода; `/history` выводит их списком; `/clear` очищает чат
- **Строка состояния**: живая строка `backend | model | temp | tokens | state` (`thinking...` / `done: Nms[, N tokens]` / ошибка); `/status` печатает полный отчёт, включая найденные локальные GGUF-модели
- **Полировка отрисовки**: длинные ответы переносятся по ширине панели, промпты пользователя (`>`) подсвечиваются цианом, строки `[error]` — красным, буфер вывода увеличен (200 записей)
- `TuiApp` получил `ai_system_prompt`, `ai_config`, `ai_history`/`ai_history_index`, `ai_show_help`, `ai_status`; новые хелперы `submit_ai_query`, `handle_ai_command`, `apply_config_async`, `push_ai_line`

### `aios-llm`: доступ к конфигурации
- `LlmEngine::config() -> LlmConfig` плюс аксессоры `config()` в `CloudEngine`/`LocalEngine`; новый хелпер `provider_name(&CloudProvider)` и `LlmEngine::backend_label()`
- 1 новый unit-тест `test_engine_config_accessor` (всего в aios-llm: 9)

### Тесты и верификация
- Полный набор workspace вырос до 1149 тестов, все проходят в debug и release (изменённые крейты)
- `cargo clippy --workspace` — 0 предупреждений, `cargo fmt --all` — чисто

## v2.3.0 — Хранилище блоков: сервис обновлений + сетевые настройки (2026-08-03)

### `aios-store`: источники, каталог, установщик, менеджер
- Новый модуль `aios-store::source`: `StoreSource` / `SourceKind` — три источника блоков: `github:owner/repo`, `local:path`, `http://host:port` (сервис обновлений)
- Новый модуль `aios-store::catalog`: `fetch_index` / `download_block` (async HTTP + локальное сканирование), `parse_name_version`
- Новый модуль `aios-store::installer`: `BlockInstaller` — установка `{name}_{version}.wasm` + sidecar JSON, проверка SHA-256, `list_installed` / `find_installed` / `uninstall`, `backup` / `rollback` (`.bak`), `check_updates`, семантический `cmp_version`
- Новый модуль `aios-store::manager`: фасад `StoreManager` — `search`, `install`, `update` (автооткат при ошибке), `check_updates`, `parse_source_spec`, `block_on` для синхронных контекстов
- Исправление: `rollback` теперь удаляет текущий (битый/новый) файл версии перед восстановлением бэкапа, поэтому `find_installed` возвращает откаченную версию

### Новый крейт `aios-net-config`
- `NetworkConfig` / `ProxyConfig` / `DnsConfig` / `InterfaceConfig` / `ProxyProtocol` с JSON-сериализацией и частичными обновлениями (`apply_updates` с валидацией)
- `NetworkConfigStore`: атомарное сохранение JSON (временный файл + rename) в `AIOS_DATA_DIR`
- `NetSettingsBlock`: `StatefulBlock` поверх IPC-шины с командами `net_get`, `net_set`, `net_reset`, `net_persist`; извлечение/восстановление состояния через bincode

### `aios-bridge`: эндпоинты сервиса обновлений
- `GET /index.json` и `GET /store/index.json` — «сырой» каталог блоков с диска
- `GET /blocks/{name}.wasm` и `GET /store/blocks/{name}.wasm` — скачивание бинарника блока
- `POST /api/v1/store/publish` — публикация пользовательского блока (base64 wasm + SHA-256 + манифест); роль локального сервиса обновлений
- `BridgeContext` получил поле `blocks_dir` (из `AIOS_BLOCKS_DIR`, по умолчанию `./blocks`)

### `aios-tui`: команды шелла
- `store list | sources | add-source <spec> | search <q> [--source N] | install <name> [--source N] | update [name] [--source N] | uninstall <name> | rollback <name>`
- `net get | net set key=value ... | net reset` — просмотр/изменение/сохранение сетевой конфигурации через `NetSettingsBlock`

### Тесты и верификация
- `aios-net-config`: 32 юнит-теста (валидация, JSON roundtrip, блок IPC, roundtrip состояния)
- `aios-store`: 42 юнит-теста (URL источников, сканирование каталога, установщик, откат, менеджер)
- Интеграция: `test_block_store_update_flow` (поиск → установка → отклонение подделки → обновление → откат) и `test_net_settings_block_roundtrip`; всего в интеграционном наборе 30 тестов
- Полная сборка workspace, `cargo test --workspace`, `cargo clippy --workspace` (0 предупреждений), `cargo fmt --all` — всё проходит

## v2.2.9 — Полноценный нативный браузер из вкладки Web (2026-08-02)

### `aios-tui`: открытие любой страницы в настоящем браузере
- `B` во вкладке Web открывает текущую страницу в **полноценном нативном браузере** (`aios-webview`: WebView2 — JavaScript, CSS, картинки, реальный рендеринг). Окно переиспользуется между нажатиями и автоматически пересоздаётся, если закрылось; открытие идёт в фоновом потоке, так что TUI не зависает
- `n` открывает выбранную ссылку в нативном браузере (дополняет `o`/`Enter`, которые открывают её в текстовом виде)
- Handle браузера живёт в модульном `OnceLock<Mutex<Option<WebBrowser>>>` — ядро, реестр блоков и планировщик не менялись
- Текстовые загрузки теперь шлют десктопный User-Agent + заголовок `Accept: text/html` и используют таймаут 15с (`http_client()`), поэтому больше сайтов отвечают вместо бот-блокировки, а зависший хост не может повесить загрузку

## v2.2.8 — Навигационный сайдбар с историей во вкладке Web (2026-08-02)

### `aios-tui`: сайдбар истории во вкладке Web
- Новый сайдбар фиксированной ширины (`SIDEBAR_WIDTH = 26`) слева от панели страницы: текущая страница первая (отмечается `▸`), затем история посещений от новых к старым, без дублей
- Ярлыки сайдбара — компактные URL (`https://www.example.com/deep/path` → `example.com/deep/path`), обрезаются с `…` до ширины панели
- Фокус переключается клавишей `\` (как `g` для омнибокса): `j`/`k`/`Up`/`Down` двигают выбор, `Enter`/`o` открывают выделенную запись (перезагружает текущую страницу, если выбрана она), `Esc` возвращает к списку ссылок; выбор зацикливается
- Ширина текста страницы теперь учитывает сайдбар: `web_page_width()` вычисляет ширину переноса из ширины терминала минус сайдбар, рамки и префикс строки; `wrap_width` считается так при старте и на каждом `Event::Resize` (завершает follow-up Фазы 37 «пропорциональная панель»)
- Новые хелперы `web_nav_entries()`, `compact_url_label()`, `web_page_width()`; 8 новых unit-тестов

## v2.2.7 — Перенос текста страницы по ширине во вкладке Web (2026-08-01)

### `aios-tui`: единицы прокрутки теперь совпадают с визуальными строками
- Новый хелпер `wrap_text()` переноса слов (без новых зависимостей): переносит каждую строку страницы по ширине терминала, принудительно разбивает слишком длинные слова и сохраняет пустые строки и ведущие отступы (вложенные списки/таблицы не теряют структуру)
- `draw_web` рендерит предварительно перенесённые строки вместо ratatui-переноса, поэтому «строка страницы» всегда равна одной строке терминала; индикатор прокрутки и клавиши `u`/`d`/`PageUp`/`PageDown` теперь работают по **визуальным** строкам — нажатие `d` двигает ровно на одну видимую строку, а низ длинной страницы достижим
- `WebState.wrap_width` отслеживает ширину терминала: инициализируется из `crossterm::terminal::size()` при старте и обновляется на каждом `Event::Resize`
- `web_scroll` ограничивает прокрутку по количеству перенесённых строк; 4 новых unit-теста для `wrap_text` (перенос по границе слов, принудительный разрыв длинных слов, сохранение отступов/пустых строк)

## v2.2.6 — Отзывчивая вкладка Web: фоновая загрузка, кэш страниц, прокрутка ссылок (2026-08-01)

### `aios-tui`: неблокирующие веб-запросы
- `load_url` / `navigate_web` больше не блокируют TUI: загрузка страниц и поиск выполняются на фоновых потоках, а результат подхватывается `check_page_cache()` каждый кадр (ранее неиспользуемый исходящий канал `page_cache` теперь задействован)
- Монотонный счётчик поколений загрузки (`WebState.web_fetch_gen`) отбрасывает устаревшие результаты — медленный старый запрос не может перезаписать более новую навигацию
- Панель «Loading...» остаётся активной, пока страница загружается

### `aios-tui`: ограниченный кэш страниц
- `WebState.cache` хранит до `WEB_CACHE_CAP = 20` недавно загруженных страниц по ключу URL (старейшие вытесняются); повторное посещение или возврат назад (`b`) по закэшированному URL рендерится мгновенно без сетевого запроса
- 2 unit-теста: вставка/поиск/дедупликация кэша и вытеснение по лимиту

### `aios-tui`: прокрутка списка ссылок и цвета заголовков
- Окно ссылок прокручивается вместе с выбором (`WebState.links_scroll`): при более чем `LINKS_VIEW_ROWS = 6` ссылках окно следует за выбранной строкой, а в заголовке виден диапазон (`3–8 / 23`)
- Текст страницы теперь раскрашивает структуру: заголовки (`#`) — жирным циановым, пустые строки — тёмно-серым
- 3 новых unit-теста: ограничение прокрутки ссылок, применение результата загрузки, отбрасывание устаревшего поколения

## v2.2.5 — WHATWG-совместимый рендеринг HTML во вкладке Web TUI (2026-08-01)

### `aios-browser`: HtmlParser перестроен на `scraper`/html5ever
- Прежний regex-парсер HTML заменён на **WHATWG-совместимый** конвейер `html5ever` (`scraper` 0.21 + `ego-tree` 0.9; большинство зависимостей уже были в Cargo.lock через `wry` → `dom_query`, поэтому прирост веса минимален)
- Извлечение текста теперь **структурировано**: заголовки становятся `#`/`###`, списки `•`/`1.`, `pre`/`br` сохраняют форматирование, строки таблиц — через `|`, `hr` рисуется как разделитель, изображения — как `[alt]`; `<script>`, `<style>`, `<head>`, `<iframe>` и скрытые элементы пропускаются
- Извлечение ссылок резолвит каждый `href` относительно базового URL страницы (теперь работают protocol-relative и относительные ссылки), дедуплицирует и отфильтровывает не-web-схемы (`javascript:`, `mailto:`, `tel:`, `#якорь`); корневые URL канонизируются без завершающего слэша
- **28 unit-тестов** (было 21): извлечение текста, ссылки, заголовки, удаление скриптов и структура макета

### `aios-tui`: навигация и рендеринг вкладки Web
- `WebState` получает `history: Vec<String>` — предыдущий URL запоминается перед каждой навигацией
- Новые клавиши вкладки Web: `b` = назад в истории, `u`/`d` = прокрутка текста страницы ±1 строка, `PageUp`/`PageDown` = ±20 строк
- Область текста страницы рендерится по видимой высоте окна с переносом (`Wrap { trim: false }`) и индикатором прокрутки `X–Y` в заголовке; заголовок окна ссылок перечисляет полный набор клавиш
- `draw_web` больше не переполняет панель страницы; справка F1 перечисляет новые клавиши

## v2.2.4 — Полноценный нативный браузер (WebView) + горячая клавиша GUI (2026-08-01)

### Новый крейт: `aios-webview` — настоящий браузерный движок
- Полноценный браузер в терминале невозможен (терминал не рендерит CSS/JS) — теперь это **нативное окно WebView** (WebView2 на Windows, WebKitGTK на Linux, WKWebView на macOS) с куки, JavaScript и историей «из коробки»
- `WebBrowser::open(target)` запускает браузер на выделенном фоновом потоке (событийный цикл winit 0.30 + webview wry 0.56), поэтому вызывающий код никогда не блокируется; `navigate`/`back`/`forward`/`close` — неблокирующие команды через `EventLoopProxy`
- Персистентный профиль: куки и хранилище переживают перезапуск через `WebContext` в `AIOS_DATA_DIR`/`aios/webview` (каталог данных ОС, если не задано), с учётом переменной `AIOS_DATA_DIR`
- `resolve_target()` — логика омнибокса: полный URL → как есть, голый хост → `https://`, обычный запрос → DuckDuckGo (HTML-версия); **5 unit-тестов**
- Модуль `launcher`: находит бинарник `aios-gui` (рядом с текущим исполняемым файлом, затем PATH) и запускает его; **2 unit-теста**
- Добавлен в workspace; безопасен для headless (`cargo test` не открывает окон)

### `aios-gui`: Вкладка Browser (7-я) с нативным webview
- Новая вкладка **Browser** (F7, «🌐 Browser» в сайдбаре): омнибокс (URL или поисковый запрос), кнопки Back/Forward, переключатель Open/Close, строка статуса
- Первая навигация автоматически открывает окно браузера; вкладка управляет нативным окном — куки, JS и история живут в движке
- Нижняя панель обновлена: `... F6=Deps F7=Browser`; в `AiosApp` добавлены поля `browser`/`browser_addr`/`browser_status` и методы open/navigate/back/forward/close
- Новый unit-тест для веток ошибок закрытого браузера

### `aios-tui` и ядро `aios`: горячая клавиша GUI `W`
- Нажатие **`W`** запускает дашборд AIOS GUI из обоих TUI; сбой (бинарник не найден) логируется, а не роняет программу
- Справка F1 перечисляет новую клавишу

## v2.2.3 — Веб-омнибокс и непрозрачная справка F1 (2026-07-31)

### `aios-tui`: Омнибокс вкладки Web
- Строка URL стала **омнибоксом**: можно вводить полный URL (`https://...`), голый хост (`example.com`, автоматически получает `https://`) или обычный поисковый запрос (`как работает AIOS`, ищется через DuckDuckGo и отображается как страница)
- После `Enter` омнибокс **автоматически снимает фокус**, поэтому ввод больше не «залипает» — сразу можно перемещаться по результатам (`j`/`k`) и открывать ссылку
- `Enter` теперь открывает выбранную ссылку (как и `o`), когда омнибокс не в фокусе
- Новое поле `search_query` в `WebState`; в строке показывается `search: <запрос>` для страниц поиска
- Новые unit-тесты для определения «URL или запрос» (`is_url_input`, 4 теста)

### `aios-tui`: Справка F1
- Справка стала **полноэкранной непрозрачной панелью**: сначала рисуется `Clear`, а контент добивается до высоты экрана, поэтому фон дашборда больше не просвечивает сквозь текст справки (раньше остаточные ячейки под текстом оставались видны, и всё сливалось)

### Ядро `aios`: Клавиша браузера (`b`)
- `dispatch_open_url` теперь нормализует ввод: голые хосты становятся URL `https://`, обычные запросы — ссылками на поиск DuckDuckGo, открываются в системном браузере; подпись ввода — `URL/query:`

## v2.2.2 — Исправлена оболочка безопасного режима в aios-tui (2026-07-31)

### `aios-tui` и `aios-watchdog`
- **BUG-020 исправлен:** все команды SafeModeShell (`ps`, `blocks`, `kill`, `spawn`, `load`, `unload`, `status`, `logs`, `restart`, `help`, `exit`) раньше возвращали `Error: Unknown command` на вкладке Shell — `execute_shell_cmd` отправлял всё, кроме `fetch`/`search`/`open`/`clear`, в `ShellCommand::Unknown`, минуя `SafeModeShell::parse_command`
- Команды теперь идут через единый парсер `SafeModeShell::parse_command`, полностью восстанавливая набор команд безопасного режима в TUI
- `help`/`?` теперь дополнительно перечисляют TUI-специфичные команды (`fetch`, `search`, `open`, `clear`)
- Вывод `blocks` приведён в порядок: состояние блока печатается как `Active` вместо `Some(Active)` через `registry.topology_with_state()`

## v2.2.1 — Переключение вкладок Alt+цифра (2026-07-31)

### `aios-tui` и ядро `aios`
- Новые горячие клавиши `Alt+1`-`Alt+7` переключают вкладки в `aios-tui` даже при активной командной строке Shell, строке URL Web, окнах загрузки блоков или поверх справки F1 — раньше цифры перехватывались активным полем ввода, и переключить вкладку со вкладки Shell было невозможно
- Ядро `aios` получает `Alt+1`-`Alt+4` переключение вкладок, работающее и при активной строке URL браузера (`b`), и при активной строке AI-запроса; переключение выходит из `browser_mode`/`ai_mode`
- Обычное цифровое переключение вкладок в ядре `aios` больше не похищает цифры, вводимые в строку AI-запроса
- Семь цифровых веток в `aios-tui` вынесены в общий хелпер `switch_tab`

## v2.2.0 — Фаза 33: Браузерный блок «из коробки» (2026-07-31)

### `aios-browser`: Полноценный блок ядра (`BrowserBlock`)
- Новый `BrowserBlock`, реализующий `StatefulBlock` (`aios-browser/src/block.rs`), экспортируется как `aios_browser::BrowserBlock`
- IPC-команды: `browse` (загрузка и парсинг страницы, возвращает bincode-сериализованный `Page`), `open_native` (открыть URL в системном браузере через крейт `open`), `browser_status` (конфиг + состояние в JSON); поддерживается `HealthCheck`
- Нет постоянного рантайма в поле — каждая навигация выполняется на выделенном однониточном Tokio-рантайме, безопасно и из sync-, и из async-контекста (исправлено падение при drop рантайма внутри async)
- Извлечение/восстановление состояния через bincode (`BrowserConfig` + `BlockState`)
- **7 новых unit-тестов** для `BrowserBlock`

### Ядро (`aios`) — регистрация блоков при загрузке
- Исправлено: ранее ядро запускалось с **пустым реестром блоков** (противоречие `docs/ARCHITECTURE.md`)
- При загрузке регистрируются 4 базовых блока (hal, ipc_bus, scheduler, browser), boot-обнаружение блоков на диске из `AIOS_BLOCKS_DIR` (по умолчанию `./blocks`), браузерный блок подключён к `MessageRouter` (`OrchestratorState` получил поля `router` + `browser_block_id`)
- Браузер работает «из коробки» на новом компьютере: не нужны ни конфиг, ни установленный браузер, ни сеть для запуска

### TUI ядра — клавиша браузера
- Новая клавиша `b`: режим ввода URL, `Enter` отправляет команду `open_native` в браузерный блок через `MessageRouter`, результат пишется в журнал событий
- Добавлена строка ввода URL над строкой подсказок; строка подсказок обновлена (`[b] browse`)

### `aios-tui` и `aiosd`
- Оба бинарника теперь регистрируют браузерный блок при загрузке вместе с hal/ipc_bus/scheduler

### Исправлены пред-существующие падения тестов
- `tests/browser_search_tests.rs` `test_html_parser_extract_text` падал — `HtmlParser::extract_text` включал текст `<head>`/`<title>`; теперь вырезается `<head>...</head>` (только текст тела страницы), как в реальном браузере
- `tests/browser_search_tests.rs` `test_duckduckgo_parse_results` падал — `DuckDuckGoBackend::parse_html_response` использовал смещение `+7` после `href="` (6 символов), теряя начальную `h` у каждого URL; исправлено на `+6`
- Добавлен unit-тест `test_extract_text_strips_head` (всего в aios-browser: 18)
- `tests/chaos_test.rs` `test_chaos_reporter_rapid_fire` падал — ассертил плейнтекст `event #0` для отчёта, заред актированного через zero-knowledge (чётные индексы); ассерты теперь проверяют редактирование (`event #0` отсутствует, `event #1`/`event #99` присутствуют, `"redacted":true` присутствует)
- `tests/e2e_pipeline_test.rs` `test_e2e_easylang_wasm_pipeline` падал — `WorkflowCompiler::generate_wat` генерил `init`/`start` с `(result i32)`, а `BlockExecutor::execute_block` вызывает их с пустым буфером результатов, поэтому вызовы падали и `functions_called` не содержал `init`/`start`; `init`/`start` теперь экспортируются без результата (в соответствии с контрактом исполнителя и его unit-фикстурами)
- `tests/e2e_pipeline_test.rs` `test_e2e_bridge_http_endpoints` падал — `MetricCollector` моста нигде не заполнялся, поэтому `/api/v1/metrics` всегда возвращал пустой Prometheus-текст (без строк `# HELP`); добавлен axum-middleware, записывающий счётчик `http_requests_total`, gauge `http_last_latency_ms` и гистограмму `http_request_latency_ms` для каждого запроса
- `tests/stress_fault_tolerance.rs` `test_fault_tolerance_scheduler_survives_crash` падал — ассертил, что high-priority replacement запланируется сразу, но шедулер продолжает текущий процесс до истечения кванта (time-slicing, без вытеснения на середине кванта — тот же контракт, что в `test_priority_scheduling`); тест теперь планирует после спавна replacement



### Сквозные интеграционные тесты (`tests/e2e_pipeline_test.rs`)
- HW & Core Probe: проверка профиля mock_modern (модель CPU, ядра, RAM, AI tier, сериализация)
- LLM & Intent Routing: тесты IntentParser для команд show/kill/list/check (EN/RU)
- EasyLang & WASM Pipeline: цепочка EasyLangParser → WorkflowCompiler → wasm → BlockLoader → BlockExecutor
- IPC & Context Store: IpcBus send/receive, EmbeddedContextStore telemetry, RingBuffer zero-copy, Crypto hash, PersistentStore redb
- Bridge HTTP Gateway: axum-сервер на эфемерном порту, endpoints /api/v1/health, /status, /workflow, /metrics, /intent

### Стресс-тесты и устойчивость к сбоям (`tests/stress_fault_tolerance.rs`)
- 50 параллельных WASM-блоков: регистрация, выполнение, проверка identity-функции, IPC-обмен
- IPC пропускная способность: 500 пакетов через 50 блоков с таймингами (<2s отправка, <2s получение)
- Изоляция паники блока: CrashReporter генерирует BlockCrash-отчёт, остальные 9 блоков продолжают работу
- Выживаемость планировщика: 20 процессов → убийство одного → 19 выживших + замена
- Серийные сбои: 10 процессов, убито 5, планировщик продолжает планирование

### Кроссплатформенные скрипты установки (`scripts/`)
- `scripts/install.sh` (Linux/macOS): проверка зависимостей (git, curl, cargo), автоустановка rustup, `cargo build --release`, копирование в /usr/local/bin или ~/.local/bin, создание `~/.aios/{models,blocks,logs}`, загрузка модели Qwen2.5-0.5B GGUF
- `scripts/install.ps1` (Windows): аналогичная логика для PowerShell, обновление PATH пользователя, структура каталогов Windows

### Сопровождение
- Исправлены ошибки компиляции в `chaos_test.rs` (use of moved value) и `browser_search_tests.rs` (разделитель raw string, неверный путь импорта BrowserEngine)
- Добавлены `aios-llm`, `aios-builder`, `tokio`, `serde_json`, `portpicker`, `reqwest` в dev-зависимости интеграционных тестов

## v2.0.0 — Фаза 31: Единый бинарник `aios` (2026-07-30)

### Новый крейт: `aios` — единый системный бинарник
- Новый крейт `aios` объединяет все 17+ крейтов рабочего пространства в один исполняемый файл
- Режимы: `aios` (интерактивный TUI) и `aios --daemon` (headless-сервер)
- Член рабочего пространства в корневом Cargo.toml

### Обнаружение оборудования (`hw_probe.rs`)
- Реальное обнаружение CPU: название бренда, физические/логические ядра, архитектура x86_64/ARM64, флаги инструкций (AVX2, AVX-512, SSE4.2, AES-NI, NEON)
- Реальное обнаружение RAM: всего/использовано/свободно в байтах и ГБ через sysinfo
- Реальное обнаружение GPU: модель + VRAM через nvidia-smi (Linux), wmic (Windows), system_profiler (macOS)
- Классификация AI Tier: Tier1 (AVX-512/AVX2+16GB+GPU) / Tier2 (AVX2+4GB) / Tier3 (запасной)

### Асинхронный оркестратор (`orchestrator.rs`)
- Асинхронная инициализация всех подсистем: IPC шина, Scheduler, BlockRegistry, AccessControl, Watchdog, LLM Engine, WASM Executor, Telemetry (TraceContext/FlightRecorder/MetricCollector)
- Bridge HTTP-сервер (axum) на порту 8080 с поддержкой корректного завершения
- Конвейер логов через Arc<Mutex<Vec<String>>>, общий с TUI

### Интерактивная TUI-панель (`src/tui/`)
- Заголовок: версия, статус, аптайм, CPU, RAM
- 4 вкладки навигации через Tab/F1/1-4
- Вкладка 1: Система и оборудование — CPU, индикатор RAM, GPU, ОС, AI Tier, статус подсистем
- Вкладка 2: Блоки и процессы — содержимое BlockRegistry, список процессов Scheduler
- Вкладка 3: AI Консоль — интерактивная консоль запросов LLM с реальным выводом
- Вкладка 4: Studio GUI Bridge — URL моста, API-эндпоинты, статус
- Подвал: поток событий (3 видимые строки) с цветовой кодировкой (ERROR=красный, WARN=жёлтый, Bridge=голубой)
- Горячие клавиши: q=выход, g=открыть браузер, r=переопределить HW, Space=пауза логов, Tab/F1=следующая вкладка, 1-4=перейти на вкладку

### Зависимости
- Clap для парсинга CLI
- ratatui + crossterm для TUI
- sysinfo для обнаружения оборудования
- open для запуска браузера

## v1.3.0 — Фаза 30: Оболочка и справка F1 в TUI (2026-07-30)

### aios-tui: Вкладка оболочки (Вкладка 7) и справка F1
- Новая вкладка Shell (вкладка 7, клавиша '7') с интерактивной командной строкой
- Ввод команд в приглашении, Enter для выполнения, ↑/↓ для истории команд
- Справка F1: переключение по F1 или '?', закрытие по F1/Esc/'?'
- Новые команды: `fetch <url>` загрузка блока по URL, `search <query>` веб-поиск через DuckDuckGo, `open <url>` навигация на URL в веб-вкладке, `clear` очистка вывода
- ShellState: input_buffer, output (Vec<String>), command_history, history_pos
- Новые функции: draw_shell(), draw_help(), execute_shell_cmd()
- Подвал обновлён: 1-7, F1=Help, :=Cmd (убраны g/o, теперь в справке)

## v1.3.0 — Фаза 29: Вкладка веб-браузера в TUI (2026-07-30)

### aios-tui: Вкладка веб-браузера (Вкладка 6)
- Новая вкладка Web (вкладка 6, клавиша '6') в TUI-дашборде для веб-сёрфинга с клавиатуры
- `g` — Фокус на строку URL, ввод URL и нажатие `Enter` для навигации
- `o` — Открыть выбранную ссылку
- `j/k` — Перемещение выбора ссылок вверх/вниз
- `Esc` — Снять фокус со строки URL
- Фоновая загрузка через reqwest blocking + HtmlParser из aios-browser
- WebState: url_input, current_url, page (PageContent), loading, error, input_focused, scroll
- PageContent: url, title, text, links Vec<(String,String)>
- Двуязычные обновления документации (CHANGELOG, INTERFACE, ARCHITECTURE)

## v1.2.0 — Фаза 26+27: Атомарные обновления, магазин, телеметрия и отладка (2026-07-29)

### aios-updater — Новый крейт: Атомарный Dual-Boot и Hot-Swap
- Новый крейт `aios-updater` — атомарные обновления с dual-boot слотами, движком горячей замены и откатом по таймеру
- `DualBootManager` — управление слотами A/B с `swap()`, `boot_success()`, `detect_active_slot()`, информацией о слотах
- `HotSwapEngine` — обёртка над aios-live-update для отслеживания горячей замены блоков по ID со счётчиком
- `RollbackManager` — откат на основе снимков с настраиваемым таймаутом (по умолчанию 1с автооткат), очистка снимков
- 12 unit-тестов: создание слотов, переключение, успешная загрузка, горячая замена, откат успех/таймаут/очистка

### aios-store — Новый крейт: Децентрализованный WASM-реестр
- Новый крейт `aios-store` — WASM-реестр блоков с валидацией SHA-256, подписями Ed25519 и реестром магазина
- `ManifestInfo` — name, version, description, author, capabilities, wasm_sha256, signature, store_url
- `ManifestValidator` — валидация содержимого SHA-256, проверка подписей Ed25519, белый список capability
- `StoreRegistry` — карта ключей name@version с `register()`, `get()`, `find_all()`, `list()`, `unregister()`
- `StoreClient` — HTTP-клиент для получения индекса магазина и загрузки WASM-блоков
- 9 unit-тестов: валидация SHA-256 (успех/неудача), валидация capability (верные/неверные), CRUD реестра

### aios-telemetry — Новый крейт: Структурированная трассировка и метрики
- Новый крейт `aios-telemetry` — сквозная структурированная трассировка, регистратор полёта, метрики Prometheus
- `TraceContext` — дерево спанов с `begin_span()`, `end_span()`, `set_tag()`, `set_status()`, экспорт `to_json()`
- `FlightRecorder` — кольцевой буфер с фильтрацией по типу, настраиваемым макс. событий + хранением, дамп по типу
- `MetricCollector` — счётчики, датчики, гистограммы с `snapshot()`, `to_prometheus()` (формат Prometheus text)
- 17 unit-тестов: вложенность спанов, статус ошибки, экспорт JSON, запись/дамп/очистка регистратора, все типы метрик

### aios-debug — Новый крейт: Отчёты об авариях и обработчик паник
- Новый крейт `aios-debug` — отчёты об авариях с нулевым знанием и кастомный обработчик паник
- `CrashReporter` — генерирует отчёты с опциональным режимом zero-knowledge (хеширование, без данных полёта)
- `CrashKind` — Panic, WatchdogTimeout, OOM, BlockCrash, Unknown
- `PanicHandler` — кастомный хук паники, направляющий информацию в CrashReporter
- 6 unit-тестов: генерация отчёта, режим zero-knowledge, экспорт JSON, последний/все отчёты

### aios-bridge: REST-эндпоинты магазина, метрик, трасс и отчётов об авариях
- `GET /api/v1/store/index` — список всех зарегистрированных манифестов в магазине
- `POST /api/v1/store/register` — регистрация нового манифеста
- `GET /api/v1/metrics` — метрики в формате Prometheus из MetricCollector
- `GET /api/v1/traces` — текущий TraceContext в формате JSON
- `POST /api/v1/crash-report` — создание отчёта об аварии (для отладки), возвращает JSON

### BridgeContext расширен
- Добавлены `StoreRegistry`, `MetricCollector`, `FlightRecorder`, `TraceContext`, `CrashReporter`, `PanicHandler` в BridgeContext
- Все новые экземпляры инициализируются с разумными параметрами по умолчанию в `BridgeContext::new()`

## v1.1.0 — Фаза 25: Безопасный веб-сёрфинг и поиск (2026-07-29)

### aios-browser — Новый крейт: Веб-браузер на WASM
- Новый крейт `aios-browser` — изолированный веб-браузер с HTML-парсером, текстовым рендерером и сетевым доступом через capability-токены
- `BrowserEngine` — основной struct с методом `navigate(url)` для загрузки и рендеринга веб-страниц
- `HtmlParser` — извлекает текст, ссылки, заголовки из HTML; удаляет скрипты, стили, комментарии
- `NetworkClient` — HTTP-клиент через `reqwest` с настраиваемым таймаутом, user-agent, лимитом редиректов
- `Renderer` — конвертирует DOM в markdown-подобный текст с заголовками, ссылками, списками
- `Page` — `url`, `title`, `text_content`, `html`, `links` для структурированных данных страницы
- `BrowserConfig` — `user_agent`, `timeout_secs`, `max_redirects`, `sandbox_enabled`
- 10 unit-тестов: извлечение текста, парсинг ссылок, заголовков, URL-резолвинг, удаление комментариев

### aios-search — Новый крейт: Анонимный веб-поиск
- Новый крейт `aios-search` — мульти-бэкендный анонимный поиск с AI-суммаризацией (TL;DR)
- `SearchEngine` — направляет запросы в настраиваемые бэкенды: DuckDuckGo, SearXNG, Brave
- `DuckDuckGoBackend` — POST через `html.duckduckgo.com/html/`, парсит HTML-ответ
- `SearXngBackend` — GET с `format=json`, парсит JSON-ответ
- `BraveBackend` — GET через `api.search.brave.com`, требует API-ключ в заголовке `X-Subscription-Token`
- `SearchSummarizer` — интеграция с `aios-llm` для AI-суммаризации результатов поиска
- `SearchConfig` — `backend`, `api_key`, `api_url`, `max_results`, `enable_summary`
- 3 unit-теста: конфиг по умолчанию, создание движка, URL бэкендов

### aios-bridge: REST-эндпоинты браузера и поиска
- `POST /api/v1/browse` — принимает `{ "url": "..." }`, возвращает title, text_content, links
- `POST /api/v1/search` — принимает `{ "query": "...", "backend": "...", "max_results": N, "enable_summary": bool }`, возвращает результаты с опциональным AI-кратким содержанием

## v1.0.0 — Фаза 23: Многорежимный AI-движок + Гибридный маршрутизатор намерений (2026-07-29)

### aios-llm: Реальный GGUF-инференс (Micro-Local и Full-Local)
- `LocalEngine` переписан: реальный GGUF-инференс через `candle-core` 0.11 + `candle-transformers` 0.11
- Поддержка Qwen2.5 GGUF: `quantized_qwen2::ModelWeights::from_gguf()`, пошаговая генерация через `LogitsProcessor`
- Micro-Local: Qwen2.5-0.5B-Instruct-GGUF (~300 МБ RAM, INT4)
- Full-Local: Qwen2.5-7B-Instruct-GGUF (~4-8 ГБ RAM, INT4)
- Интеграция `hf-hub` 1.0: `HFClientSync` (blocking) для автоматической загрузки моделей с Hugging Face Hub
- Варианты бэкенда `LocalModelKind::Micro` / `LocalModelKind::Full`
- `detect_local_models()` сканирует `AIOS_MODELS_DIR` или `models/` на наличие `.gguf` файлов
- `download_default_model()` загружает Qwen2.5 GGUF + tokenizer.json через HF Hub
- `LlmEngine::from_config()` теперь диспетчеризирует в `MicroLocal`/`FullLocal` движки
- `factory.rs` обновлён: `BackendKind::MicroLocal` и `BackendKind::FullLocal`

### aios-bridge: LLM Fallback в маршрутизаторе намерений
- `IntentParser::parse_with_llm_fallback()` — когда rule-based парсер возвращает `UserIntent::Unknown`, вызывает LLM для классификации
- LLM получает структурированный system prompt с доступными типами намерений (ProcessControl, BlockManagement, SystemQuery, MemoryCompaction)
- Ответ парсится из JSON обратно в `UserIntent` через `parse_llm_response()`
- `intent_handler` и `workflow_handler` обновлены для использования LLM fallback
- 8 unit-тестов: конфиг по умолчанию, serde round-trip, провайдеры по умолчанию, диспетчеризация, 3x from_config, detect_local_models

### aios-builder: Новый крейт — EasyLang Engine и Auto-Manifest Generator
- Новый крейт `aios-builder` — EasyLang компилятор, движок workflow и авто-генератор манифестов
- Тип `Workflow` — JSON-сериализуемый workflow с именованными шагами, валидацией и serde round-trip
- `AutoManifestGenerator` — анализ WASM-бинарников через `wasmparser`: определение capability из имён экспортов/импортов; ключевой анализ интентов workflow для вывода capability; генерация sidecar `BlockManifestJson` (`name`, `version`, `capabilities`)
- `WorkflowCompiler` — пайплайн компиляции Workflow→WASM: генерация WAT-текста, компиляция WAT→WASM через `wat`
- 8 unit-тестов: обнаружение capability из экспортов/импортов WASM, генерация JSON-манифеста, анализ интентов workflow, компиляция пустого/с шагами, WAT-вывод

#### EasyLang Parser — текстовый DSL → Workflow
- `EasyLangParser` — построчный декларативный DSL: `spawn "browser"`, `timer 5000`, `load "network"`, `query "memory"`, `compact`, `status`
- Автоматическая генерация label из текста команды; опциональный префикс `label:` для кастомных имён
- Комментарии: строки `//` и `#`; пустые строки игнорируются
- 10 unit-тестов: парсинг пустого/комментариев, одна/много команд, кастомные label, ошибка пробела в label, unicode-метки, JSON round-trip

### aios-llm: Новый крейт — Multi-Mode AI Engine
- Новый крейт `aios-llm` — унифицированный LLM-интерфейс с бэкендами Cloud, Micro-Local, Full-Local
- `LlmConfig` — сериализуемая конфигурация: тип бэкенда, модель, API ключ/URL, max tokens, temperature
- `CloudEngine` — HTTP/JSON бэкенд для Groq, OpenRouter, Google AI Studio (OpenAI-совместимый API)
- `LocalEngine` — заглушка для будущего GGUF/ONNX локального инференса (Micro-Local / Full-Local)
- `LlmEngine` enum с `from_config()` фабрикой и `async query()` диспетчеризацией
- 7 unit-тестов: конфиг по умолчанию, serde round-trip, провайдеры по умолчанию, диспетчеризация, локальная недоступность

### aios-bridge: Эндпоинт выполнения workflow
- `POST /api/v1/workflow` — новый эндпоинт, принимающий `{prompts: [string, ...]}` для пакетного выполнения интентов
- Последовательный парсинг и выполнение каждого prompt, возврат результатов по каждому шагу
- Проверка capability для каждого шага индивидуально
- `runWorkflow()` в Builder переведён на единый batch-запрос вместо N отдельных запросов

### aios-studio: Вкладка Easy Builder
- Новая вкладка "Builder" в боковой панели с визуальным step-редактором workflow
- Палитра блоков (Триггеры: Timer, Event; Действия: Spawn, Kill, Load, Unload, Compact, Query)
- Добавление/удаление/перестановка шагов; редактирование prompt для каждого шага (inline input)
- Сохранение/загрузка именованных workflow через localStorage с выпадающим списком и удалением
- Кнопка "Run Workflow" отправляет каждый шаг через `POST /api/v1/intent` и отображает результаты
- Toast-уведомления об операциях сохранения/загрузки/удаления

### aios-studio: SPA Веб-Дашборд
- Новая директория `aios-studio/` — самодостаточное HTML/CSS/JS SPA-приложение
- Дашборд телеметрии в реальном времени: график RAM на Canvas, таблица процессов, карточки здоровья
- Smart Command Palette (Ctrl+K) — отправка намерений на естественном языке через `POST /api/v1/intent`
- Вкладка Security Center — список блоков, матрица capability-токенов, кнопки быстрых действий
- Автопереподключение WebSocket с экспоненциальной задержкой и визуальным индикатором
- Тёмная тема, минимальные зависимости (ноль npm-пакетов), работает в любом современном браузере

### aios-bridge: Раздача Статики
- `tower-http` расширен фичей `fs` для `ServeDir`
- Fallback-роутер на `aios-studio/` — SPA по `/`, CSS по `/style.css`, JS по `/app.js`
- API-маршруты (`/api/v1/*`, `/ws/*`) имеют приоритет; все остальные пути уходят в статику

## v1.0.0 — Bridge & Intent Engine (2026-07-28)

### aios-bridge: HTTP/WebSocket API Gateway
- Новый крейт `aios-bridge` — внешний API-шлюз для GUI/Web-клиентов
- `GET /api/v1/health` — проверка работоспособности с версией и аптаймом
- `GET /api/v1/system/status` — полный снимок системы (процессы, блоки, watchdog, RAM)
- `POST /api/v1/intent` — обработка намерений на естественном языке с проверкой capabilities
- `GET /ws/telemetry` — WebSocket-эндпоинт с потоковой передачей метрик (100ms интервал)
- CorsLayer permissive для кросс-доменных Web-клиентов
- Асинхронный сервер на Axum 0.7 с tokio runtime

### aios-bridge/intent_engine: Правила-ориентированный парсер намерений
- `IntentParser` с двуязычным (RU/EN) сопоставлением правил
- `UserIntent` enum: ProcessControl, BlockManagement, SystemQuery, MemoryCompaction, WorkflowExecution
- Действия процессов: List, Kill, Spawn, AdjustPriority
- Действия блоков: List, Load, Unload, HotSwap
- `ExecutionPlan` DAG с маппингом требований `CapabilityToken`
- 25 unit-тестов, покрывающих все типы намерений на двух языках
- Graceful `Unknown` fallback с подсказками

### aios-bridge/security: Контроль прав доступа
- Каждое исполнение намерения проверяет `AccessControlLayer` перед системными вызовами
- Недостающая capability возвращает HTTP 403 Forbidden с описанием
- Bridge работает со своим `bridge_block_id` для идентификации в ACL
## v2.5.0 — Подписанные манифесты блоков Ed25519 с политикой доверия (2026-08-04)

### `aios-store`: реальная подпись и проверка Ed25519
- `manifest::canonical_bytes()` — детерминированная каноническая сериализация `aios-manifest-v1\n` + name/version/description/author/отсортированные capabilities/размер/`wasm_sha256`
- `manifest::sign_manifest(manifest, &SigningKey) -> SignatureInfo` — подпись Ed25519 (`ed25519-dalek` v2, фича `rand_core`) по каноническим байтам; `verify_signature` теперь выполняет реальную проверку `verify_strict`; `verify_signature_with_keys(manifest, &[String])` проверяет по списку доверенных публичных ключей
- Корневой `Cargo.toml` получил `ed25519-dalek = { version = "2", features = ["rand_core"] }`; `aios-store` добавил dep `ed25519-dalek` и dev-dep `rand_core` (для `OsRng` в тестах)
- 11 тестов манифеста: roundtrip подпись/проверка, изменение wasm/capabilities, чужой ключ, принятие/отклонение доверенным ключом, ошибка отсутствия подписи, неверный алгоритм

### `aios-store`: enforcement подписей в `BlockInstaller`
- `BlockInstaller.trusted_keys: Vec<String>` — если список не пуст, `install_from_bytes` отклоняет неподписанные манифесты и любые манифесты, не подписанные одним из доверенных ключей
- Новые конструкторы `with_trusted_keys(dir, keys)` и `from_env(dir)`; `Default` читает `AIOS_TRUSTED_PUBLIC_KEYS` (разделители `,`/`;`); без доверенных ключей подпись всё равно проверяется по встроенному ключу
- Sidecar теперь сохраняет полный `ManifestInfo` (включая подпись), поэтому подписанные установки остаются проверяемыми через `store verify`
- 16 тестов установщика: отклонение неподписанного/чужого ключа, принятие корректной подписи, отклонение изменённого манифеста, парсинг env, сохранение подписи в sidecar

### `aios-store`: политика доверия по источникам
- `StoreSource.trusted_public_keys: Vec<String>` (`#[serde(default)]`); `StoreManager::verify_source_manifest(source, manifest)` отклоняет манифест, не подписанный одним из доверенных ключей источника; применяется в `install()` и `update()`
- `github_default` наследует официальный ключ из `AIOS_OFFICIAL_PUBLIC_KEY` через `official_public_key()`; `StoreManager::new`/`with_sources` теперь используют `BlockInstaller::from_env`
- 2 теста менеджера: отклонение недоверенной / принятие доверенной подписи от источника

### Шелл `aios-tui`: `store sign` / `store verify`
- `store sign <file.wasm> [name] [version] [--key <secret_hex>]` — вычисляет SHA-256, строит манифест, подписывает его по Ed25519 (ключ из `AIOS_STORE_SIGNING_KEY`, если `--key` опущен), пишет подписанный sidecar JSON рядом с файлом и печатает публичный ключ
- `store verify <name>` — проверяет установленный блок: SHA-256 бинарника + подпись Ed25519 манифеста из sidecar
- `aios-tui` теперь зависит от `ed25519-dalek`

### Тесты и верификация
- `aios-store` вырос до 56 unit-тестов; всего в workspace 1148 тестов, все проходят
- Полная сборка workspace, `cargo test --workspace`, `cargo clippy --workspace` (0 предупреждений), `cargo fmt --all` — всё проходит

## v2.4.0 — Блок сетевых настроек в ядре + store publish (2026-08-03)

### Ядро `aios`: сетевые настройки через IPC
- Блок `net_settings` регистрируется при загрузке в реестре ядра и подключается к `MessageRouter` (`aios/src/orchestrator.rs`); его `BlockId` доступен как `OrchestratorState::net_block_id`
- Новая клавиша `n` в TUI ядра (`aios`): режим ввода пар `key=value` для частичного обновления сетевой конфигурации, уходит в блок по IPC (команда `net_set` через `MessageRouter`); возвращённый JSON конфигурации выводится в панель событий; `Esc` отменяет ввод
- Обработка событий TUI: `TuiApp` получил поля `net_input` / `net_mode`; `ui.rs` рисует строку-подсказку сети и обновлённую справку; переключение вкладок Alt+цифра также сбрасывает net-режим

### Шелл `aios-tui`: `store publish`
- Новая команда `store publish <file.wasm> [name] [version]` — читает файл, вычисляет SHA-256, кодирует wasm в base64 и отправляет `StorePublishRequest` в `POST /api/v1/store/publish` локального сервиса обновлений (порт моста из `AIOS_BRIDGE_PORT`, по умолчанию `8080`); имя по умолчанию — имя файла без расширения, версия — `1.0.0`
- `StorePublishRequest` / `StorePublishResponse` в `aios-bridge::dto` теперь оба `Serialize + Deserialize`, чтобы клиент мог их сериализовать и разбирать
- `aios-tui` теперь зависит от `aios-bridge`, `sha2`, `hex`, `base64`

### Тесты и верификация
- Новые тесты ядра в `aios/src/orchestrator.rs` (4): регистрация `net_settings` в реестре, маршрутизация `net_get` / `net_set` / `net_reset` через IPC по `MessageRouter`
- Полная сборка workspace, `cargo test --workspace`, `cargo clippy --workspace` (0 предупреждений), `cargo fmt --all` — всё проходит


## v1.0.0 — Автообнаружение, парсинг манифестов, enforcement capabilities (2026-07-28)

### Block Registry: автообнаружение
- `BlockRegistry::boot_discover(root)` — рекурсивный обход директории, обнаруживает все `.wasm` и `.bin` файлы во вложенных поддиректориях и регистрирует их
- Создаёт корневую директорию блоков, если она не существует
- Исправлен баг, когда `walk_recursive` создавал внутренний реестр вместо регистрации в `self`
- 3 новых теста: создание директории, обход поддиректорий, пропуск не-блочных файлов

### Block Loader: парсинг sidecar JSON-манифестов
- `BlockLoader::load_from_directory()` теперь ищет sidecar `.json` файлы рядом с `.wasm`/`.bin` файлами (например, `mynet_1.0.0.json` для `mynet_1.0.0.wasm`)
- Структура `BlockManifestJson`: парсит `name`, `version`, `capabilities`, `ttl_ms` из JSON
- Capabilities парсятся из строковых имён (`CAP_NET_BIND`, `CAP_NET_CONNECT` и т.д.) в `CapabilityToken`
- Если файл манифеста существует, его значения переопределяют дефолты из имени файла и автоматически назначают `CapabilityToken` записи блока
- Обратная совместимость: при отсутствии `.json` sidecar используется парсинг из имени файла
- `BlockLoader::load_from_binary_with_capabilities()` — новый метод загрузки с опциональным назначением capabilities
- 5 новых тестов: парсинг capabilities, пустые caps, from_file, с sidecar, без sidecar (fallback)

### RealTcpBlock: enforcement capability-токенов
- `RealTcpBlock` теперь хранит опциональный `CapabilityToken` через `set_capability()`
- `start_listening()` проверяет `CAP_NET_BIND` перед привязкой
- `connect()` проверяет `CAP_NET_CONNECT` перед исходящим соединением
- Без токена — разрешено всё (обратная совместимость)
- Просроченные токены отклоняются
- `Capability::All` предоставляет все capabilities
- Добавлена зависимость `aios-security` в крейт `aios-net`
- 7 новых тестов: нет токена — разрешено всё, grant/deny bind, grant/deny connect, отказ при истечении, All предоставляет всё
- 605 юнит-тестов проходят, ноль clippy-предупреждений, fmt чист

## v1.0.0 — Релиз: полная интеграция и продакшен-качество (2026-07-27)

### Документация интерфейса
- Добавлены `docs/INTERFACE.md` + `docs/INTERFACE.ru.md` — полное руководство по использованию GUI/TUI
- Включает: схемы компоновки, горячие клавиши, действия мышью, все 6 вкладок GUI, тему
- Раздел TUI: 5 вкладок, 11 горячих клавиш, совместимость с терминалами
- Раздел GUI: 6 вкладок (Обзор, Процессы, Блоки, Маркетплейс, Метрики, Зависимости), навигация F1-F6, мышь, справочник цветов тёмной темы
- Обновлён `AGENTS.md`: новое правило #5 — INTERFACE.md обязана обновляться при любом изменении пользовательского интерфейса

### Переход runtime: Mock → Real (65% → 75%)
- **Загрузка BlockRegistry с диска**: `load_from_path(dir)` сканирует директорию на `.wasm` и `.bin` файлы, парсит версию из имени файла, регистрирует и активирует все обнаруженные блоки
- **BlockExecutor загрузка+выполнение**: `load_from_path_and_execute()` выполняет одноразовую загрузку + компиляцию WASM + инстанцирование + `init`/`start` из директории
- **BlockLoader поддержка .wasm**: `load_from_directory()` теперь обрабатывает `.wasm` файлы наряду с `.bin`
- **Активное восстановление Watchdog**: ступенчатая эскалация — действия `KillProcess(pid)`, `DumpState(path)`, `SafeModeShell` с упорядочиванием по серьёзности
- **Escalation WatchdogRunner**: `escalate()` вызывает контекстно-зависимые действия восстановления на основе текущего состояния
- **RealUdpBlock**: реальный `std::net::UdpSocket` с `bind()`, `send_to()`, неблокирующим `receive_from()`, broadcast и метриками per-socket
- **Чеклист перехода runtime в TODO**: полный 6-секционный чеклист с целевыми показателями готовности по вехам
- 766 тестов всего, ноль clippy-предупреждений

### Интеграционные тесты: реальный I/O (75% → 85%)
- Добавлены 6 новых файлов интеграционных тестов с **реальным** аппаратным I/O, заменяющих mock-данные:
  - `tests/real_file_io.rs` — 10 тестов: SnapshotManager, CopyOnWriteStorage, RecoveryLog, загрузка BlockRegistry с диска, большие полезные нагрузки
  - `tests/real_network.rs` — 11 тестов: RealTcpBlock loopback (accept/send/receive, двунаправленный, мультиклиент, close+reopen), RealUdpBlock loopback (send/receive, несколько датаграмм, broadcast, метрики)
  - `tests/real_wasm.rs` — 9 тестов: end-to-end WASM compile→instantiate→call, изоляция мультиблочной загрузки, загрузка+выполнение с диска, пакетное выполнение, невалидный бинарник, метаданные
  - `tests/real_threads.rs` — 10 тестов: реальное выполнение ОС-потоков, сигнал завершения, приостановка/возобновление, параллельность (8 потоков), обнаружение завершения, контроль RAM, приоритетное планирование, race-free атомарный счётчик, смешанные real+logical
  - `tests/real_hot_swap.rs` — 8 тестов: WASM deploy+call, горячая замена версии (v1→v2 с другой логикой), откат, health check pass/fail, история замен, независимая замена мультиблоков
  - `tests/full_lifecycle.rs` — 7 тестов: полная система (HAL+IPC+scheduler+telemetry+ACL), WASM жизненный цикл (deploy→swap→rollback), watchdog+scheduler+IPC, crypto+bus, планировщик+real threads, disk→WASM fibonacci, stability+ACL
- **RealUdpBlock**: добавлен метод `port()` для доступа к фактическому привязанному порту в интеграционных тестах
- **WatchdogRunner**: исправлена нестабильность теста `test_runner_pop_actions` (тайминг)
- 821 тестов всего, ноль clippy-предупреждений

### Привязка к CPU в планировщике (85% → 90%)
- **`aios-process-mgr/src/cpu_affinity.rs`**: платформенная привязка к CPU через сырой FFI
  - Windows: `SetThreadAffinityMask` / `GetCurrentThread`
  - Linux: `sched_setaffinity` с `cpu_set_t`
  - Fallback: no-op на неподдерживаемых платформах
- **`Scheduler::set_cpu_affinity(pid, cores)`**: привязывает реальный ОС-поток к указанным ядрам CPU
- **`Scheduler::get_cpu_affinity(pid)`**: запрос текущей привязки к CPU для потока
- **`Scheduler::available_cpu_cores()`**: возвращает количество доступных ядер CPU
- 4 юнит-теста в модуле `cpu_affinity`, 3 теста на уровне планировщика
- 828 тестов всего, ноль clippy-предупреждений

### WASM-движок live-обновлений — реальная замена модулей и миграция состояния
- `WasmLiveUpdateEngine` в `aios-live-update/src/wasm_engine.rs` — связывает `LiveUpdateEngine` с `WasmSandbox` для реальной замены WASM-модулей при горячей замене
- `deploy_block()` — компилирует, инстанцирует и автоматически вызывает функции `init`/`start` на WASM-блоках
- `swap_block()` — выполняет атомарную замену через `LiveUpdateEngine.perform_swap()`, затем компилирует и инстанцирует новый WASM-модуль, мигрирует состояние linear memory из старого экземпляра, готов к использованию
- `rollback_block()` — удаляет активный WASM-экземпляр и восстанавливает предыдущую версию через `LiveUpdateEngine.rollback()`
- `call_block_func()` — вызывает экспортированные WASM-функции на активных (развёрнутых/заменённых) блоках
- **Миграция linear memory**: `extract_linear_memory()` считывает WASM linear memory перед заменой, `restore_linear_memory()` записывает его в новый экземпляр после замены — состояние сохраняется при горячей замене
- Структура `SwapParams` — инкапсулирует конфигурацию замены (new_binary, new_version, health_check, isolation)
- `SwapResult` включает `memory_migrated: bool` для указания, была ли перенесена linear memory
- 7 тестов WASM-памяти (4 на уровне sandbox, 2 live-update, 1 интеграционный), 834 теста всего

### Атомарное перенаправление IPC-каналов (90% → 100%)
- **`IpcBus::reroute(old_target, new_target)`** — атомарно перезаписывает `target_block` во всех ожидающих пакетах, совпадающих с `old_target`, на `new_target`
- **`StateTransferManager::reroute_snapshot()`** — перенаправляет пакеты внутри замороженного снапшота перед восстановлением шины
- **`WasmLiveUpdateEngine::reroute_pending()`** — freeze→reroute→unfreeze в одной атомарной операции
- 4 новых теста (2 bus, 1 state_transfer, 1 wasm_engine), **838 тестов всего**, ноль clippy-предупреждений

### TCP-опции сокетов — реальная конфигурация сокетов на уровне ОС (100%)
- **`set_keepalive()`** — платформенный сырой FFI для `SO_KEEPALIVE` на TCP-сокетах
  - Windows: Winsock `setsockopt` с константами `SOL_SOCKET`/`SO_KEEPALIVE`
  - Unix: `libc::setsockopt` с `libc::SO_KEEPALIVE`
- **`SO_REUSEADDR`** — сырой FFI в `RealTcpBlock::start_listening()` позволяет быстрое повторное использование порта после остановки
  - `TcpConfig.reuse_addr: bool` (по умолчанию: `true`)
- **`SO_KEEPALIVE`** — применяется на принимаемых и подключаемых TCP-сокетах
  - `TcpConfig.keepalive: bool` (по умолчанию: `true`)
- **`TCP_NODELAY`** — устанавливается через `stream.set_nodelay()` на всех новых соединениях
  - `TcpConfig.nodelay: bool` (по умолчанию: `true`)
- **`get_keepalive()`** — тестовая вспомогательная функция через сырой `getsockopt` FFI для проверки состояния keepalive
- 4 новых теста: быстрое переназначение через `SO_REUSEADDR`, проверка keepalive, проверка nodelay, отключённое reuse_addr
- **842 тестов всего**, ноль clippy-предупреждений

### Ступенчатая эскалация Watchdog (100%)
- **`WatchdogState::Warned`** — новое промежуточное состояние между Monitoring и Suspended для ступенчатого реагирования
- **`WatchdogConfig.warn_threshold: u32`** — настраиваемый порог для состояния предупреждения (по умолчанию: 2)
- **Ступенчатый поток `check_timeout()`**: Monitoring → Warned → Suspended → Recovering → SafeMode
  - Пропуск `warn_threshold` heartbeat'ов → действие `WarnOrchestrator`, состояние → `Warned`
  - Пропуск `max_missed_heartbeats` → действие `SuspendOrchestrator`, состояние → `Suspended`
  - Следующая проверка после Suspended → действие `KillProcess(0)`, состояние → `Recovering`
  - Тайм-аут восстановления → действие `SafeModeShell`, состояние → `SafeMode`
- **`WatchdogAction::WarnOrchestrator`** — новое действие с серьёзностью 1 (предупреждение перед приостановкой)
- **`escalate_actions()`** — теперь включает состояние `Warned` с действием `DumpState`
- **`receive_heartbeat()`** — восстанавливает из состояния `Warned` обратно в `Monitoring` (сбрасывает missed_count)
- **Пересортировка серьёзности**: None(0) < WarnOrchestrator(1) < WaitForRecovery(2) < SuspendOrchestrator(3) < AttemptRecovery(4) < KillProcess(5) < DumpState(6) < EnterSafeMode(7) < SafeModeShell(8) < InSafeMode(9)
- **Интеграция с TUI** — `WatchdogState::Warned` отображается как "WARNING" жёлтым цветом
- 5 новых тестов (ступенчатая эскалация, восстановление из warned, escalate в warned, полный warn→safe, серьёзность), **845 тестов всего**

### Постоянный реестр ProcessId → JoinHandle
- **`RealThreadState`** — структура запроса состояния реальных ОС-потоков (pid, finished, suspended, terminated)
- **`Scheduler::get_real_thread_state(pid)`** — запрашивает текущее состояние реального потока по ProcessId
- **`Scheduler::list_real_threads()`** — возвращает все ProcessId с реальными потоками
- HashMap `real_threads" служит постоянным реестром ProcessId → JoinHandle с публичными аксессорами
- 2 новых теста: `list_real_threads`, `get_real_thread_state`

### Потоко-локальное хранилище метрик per-process
- **Модуль `process_metrics`** — атомарные метрики per-process с потоко-локальной привязкой для записи без contention
- **`ProcessMetricsInner`** — атомарные счётчики: `messages_sent`, `messages_received`, `bytes_sent`, `bytes_received`, `errors`, `syscall_count`, `wakeups`
- **`ProcessMetricsStore`** — глобальный `HashMap<ProcessId, Arc<ProcessMetricsInner>>` с `OnceLock`-синглтоном
- **Потоко-локальная привязка**: `bind_current_thread(pid)` / `current_pid()` — ассоциирует текущий поток с PID
- **Функции便利**: `record_sent(bytes)`, `record_received(bytes)`, `record_error()`, `record_syscall()`, `record_wakeup()` — авто-определение PID через потоко-локальное хранилище
- **`snapshot(pid)`** / **`snapshot_all()`** — атомарное чтение всех счётчиков без блокировок
- `register(pid)`, `unregister(pid)`, `clear()`, `count()` — управление жизненным циклом
- 7 юнит-тестов: register/snapshot, unregister, snapshot_all, привязка+запись через поток, clear, независимость атомарных операций, noop без привязки
- **854 тестов всего**, ноль clippy-предупреждений
- `DeployResult`, `SwapResult`, `RollbackResult` — типизированные структуры возврата
- 6 модульных тестов: развёртывание, вызов функции, реальная WASM-замена (add→multiply), откат, падение проверки здоровья, история
- **Примечание**: закрывает разрыв, обозначенный в TODO — Live Update теперь **РЕАЛЬНЫЙ** (не mock)

### Планировщик — реальное управление ОС-потоками
- `TerminateFlag(Arc<AtomicBool>)` и `SuspendFlag(Arc<AtomicBool>)` для кооперативного управления потоками
- Структура `RealThread` — оборачивает ОС-поток с `Thread` + `JoinHandle` + флагами завершения/приостановки
- `spawn_real_process<F>()` — порождает реальные ОС-потоки с поддержкой кооперативного завершения
- `kill_process()` — устанавливает флаг завершения, разблокирует поток, присоединяет handle
- `suspend_process()` / `resume_process()` — приостанавливает/возобновляет реальные потоки через `AtomicBool` + `thread::park()`
- `check_real_threads()` — обнаруживает завершённые потоки через `is_finished()`, присоединяет их, обновляет состояние
- `is_real_process()`, `real_thread_count()` — вспомогательные функции запросов
- 6 новых модульных тестов жизненного цикла реальных потоков

### BlockExecutor — мост выполнения WASM-блоков
- `BlockExecutor` в `aios-wasm/src/executor.rs` — связывает `BlockRegistry` с `WasmSandbox`
- `execute_block()` — компилирует бинарник из реестра как WASM, инстанцирует, автоматически вызывает `init`/`start`
- `call_block_func()` — вызывает экспортированные функции на уже выполненных блоках
- `execute_all()` — пакетное выполнение всех блоков из реестра
- 6 модульных тестов: init+start, вызовы функций, выполнение всех, несуществующие блоки

### WatchdogRunner — реальный фоновый поток
- `WatchdogRunner` в `aios-watchdog/src/runner.rs` — реальный фоновый `std::thread::spawn` с `AtomicBool` флагом остановки
- Автоматическая проверка тайм-аутов через настраиваемые интервалы, сбор действий через `Arc<Mutex<Vec<WatchdogAction>>>`
- `start()`, `stop()`, `receive_heartbeat()`, `pop_actions()`, `force_safe_mode()`, `reset()`
- Реализация `Drop` обеспечивает очистку потока на всех путях кода
- 8 модульных тестов: старт/стоп, heartbeat, пропуск обнаружения, аварийный режим, сброс, извлечение действий, drop, восстановление

### RealTcpBlock — реальные OS-сокеты
- `RealTcpBlock` в `aios-net/src/real_tcp.rs` — реальные `std::net::TcpListener`/`TcpStream` с неблокирующим accept
- `start_listening()`, `accept_pending()`, `connect()`, `send()`, `receive()`, `close_connection()`, `stop()`
- `max_connections` проверяется в `accept_pending()`
- 6 модульных тестов: listen/stop, connect+send, двусторонний, close, макс. соединения, нет ожидающих данных

### Новый крейт: `aios-optim` — движок оптимизации во время выполнения
- **12-й крейт рабочего пространства** — профилирование производительности, обнаружение горячих путей, оптимизация раскладки памяти и авторегулировка
- **Профилировщик** (`profiler.rs`): замер wall-clock со скользящими средними, гистограммы, перцентили (p50/p95/p99), отслеживание пропускной способности
- **Детектор горячих путей** (`hotpath.rs`): отслеживание мест вызова с подсчётом попаданий, накопление длительности, вывод flamegraph, динамические пороги
- **Оптимизатор раскладки памяти** (`layout.rs`): перестановка полей структур для выравнивания по кеш-линии, анализ размера, рекомендации по выравниванию
- **Авторегулировщик** (`tuning.rs`): поиск параметров со стратегиями сетки/случайного/бинарного поиска, отслеживание лучших результатов, сбор метрик, детекция сходимости
- 29 модульных тестов для всех модулей оптегизации

### Интеграция кольцевого буфера и IPC-шины
- `RingTransport` в `aios-ipc/src/ring_transport.rs` связывает кольцевые буферы с IPC-шиной
- Автоматическая маршрутизация тяжёлых полезных нагрузок (>4KB) через разделяемую память для zero-copy производительности
- Fallback на стандартную VecDeque-шину для малых сообщений
- `RingMetrics`: отслеживание отправок/получений через кольцо, байтов отправлено/получено
- 10 модульных тестов

### Интеграция сжатия и контекстного хранилища
- `CompressedTelemetryStore` в `aios-context/src/compressed_telemetry.rs`
- Автоматическое сжатие холодных записей телеметрии (>1 часа) через ZSTD из `aios-compress`
- Прозрачная распаковка при чтении — вызывающие коды видят обычные `TelemetryEntry`
- Настраиваемые пороги сжатия и возраст холодных записей
- 6 модульных тестов

### Интеграция CoW-персистентности и live-update
- `CowLiveUpdateEngine` в `aios-live-update/src/cow_live_update.rs`
- Сохранение записей отката (бинарник, состояние, версия) в CoW-хранилище для восстановления после сбоев
- При запуске восстанавливает ожидающие откаты с диска
- 4 модульных теста

### Мост безопасности оборудования
- `HardwareSecurityBridge` в `aios-security/src/hardware_bridge.rs`
- Единый интерфейс для слоёв защиты MPK, TEE и IOMMU
- `protect_block()`, `unprotect_block()`, `check_access()` — единый API для всего аппаратного обеспечения безопасности
- `ProtectionReport` со статусом по каждому слою
- Graceful fallback при недоступности аппаратных слоёв
- 10 модульных тестов

### Исправление ошибок
- **BUG-012**: Исправлена функция `get_pending_entries()` в журнале восстановления — завершённые записи не фильтровались, так как функция только пропускала строки-маркеры `COMPLETED:` без использования их для исключения соответствующих ID записей
- Исправлена ошибка Windows в `atomic_write` — `sync_all()` завершается с ошибкой "Access Denied" при использовании `File::open()`; заменено на `OpenOptions::new().write(true)`

### Бенчмарк-сюита
- **5 файлов бенчмарков** на `criterion` 0.5: IPC, кольцевой буфер, шина, сжатие, персистентность
- `aios-core/benches/ipc_bench.rs`: сериализация/десериализация IPC при 1КБ/64КБ
- `aios-ringbuf/benches/ring_bench.rs`: запись/чтение/zero-copy кольцевого буфера
- `aios-ipc/benches/bus_bench.rs`: отправка/получение/приоритет шины
- `aios-compress/benches/compress_bench.rs`: сжатие/распаковка/коэффициент
- `aios-persistence/benches/persist_bench.rs`: атомарная запись/чтение/полный цикл

### Тестирование на свойства (proptest)
- `aios-core/tests/proptest_ipc.rs`: 8 тестов — roundtrip сериализации, валидность контрольной суммы, уникальные ID, roundtrip ответа, сохранение payloads, непустая сериализация
- `aios-ringbuf/tests/proptest_ring.rs`: 5 тестов — roundtrip записи/чтения, границы ёмкости, available_read, последовательные записи, zero-copy

### Хаос-тестирование
- `tests/chaos_test.rs`: 13 тестов — повреждение IPC, переполнение шины, исчерпание памяти планировщика, resilience при циклических сбоях, тайм-аут watchdog→safe mode, быстрые команды safe mode, отсутствие токена/неправильная capability, подделка HMAC heartbeat, исчерпание context store→compact, дубликаты загрузчика блоков, конкурентный drain шины, консистентность kill-after-schedule

### Авто-компакт Context Store
- `EmbeddedContextStore::with_compact_threshold(max_entries, threshold_ratio)` — настраиваемый авто-компакт
- `should_compact()` — проверка превышения порога
- `compact()` — очистка старых телеметрий, дедупликация workflow, возвращает `CompactReport`
- 4 модульных теста для проверки порога, очистки и сохранения минимальных данных

### Интеграция безопасности в BlockManager
- `BlockEntry` теперь хранит опциональный `CapabilityToken` для каждого блока
- `assign_capabilities(id, token)` — привязка токена к загруженному блоку
- `check_capability(id, cap)` — проверка наличия требуемой capability у блока (с проверкой срока действия)
- Блоки без назначенных токенов отклоняются при всех проверках capabilities
- 2 новых теста: назначение/проверка, none по умолчанию

### Дополнительные бенчмарки
- Бенчмарки коэффициента сжатия по паттернам данных (повторяющиеся, случайные, телеметрия) в `aios-compress`
- Бенчмарки времени создания снимка CoW, задержки отката и накладных расходов диска в `aios-persistence`

### VM-развёртывание
- **Dockerfile обновлён** — все 20 крейтов workspace, `debian:bookworm-slim` runtime
- **main.rs полный рефакторинг** — авто-компакт, загрузка блоков с диска, восстановление из persistence, корректное завершение
- **`BlockLoader::load_from_directory(dir)`** — загрузка `.bin` блоков из `AIOS_BLOCKS_DIR` при старте (формат name_version.bin)
- **Интеграция `PersistentStore`** — сохранение телеметрии при завершении, восстановление при запуске из `AIOS_DATA_DIR`
- **Переменные окружения**: `AIOS_DATA_DIR`, `AIOS_BLOCKS_DIR`, `AIOS_MOCK_PROFILE` (modern/legacy/none)
- **Linux HAL** — чтение `/proc/cpuinfo` и `/proc/meminfo` для определения оборудования

### Интерактивное управление блоками в TUI
- **Горячие клавиши на вкладке блоков**: `U` = выгрузить блок, `L` = загрузить с диска (диалог ввода имени+версии), `H` = горячая замена бинарника (выгрузка + перезагрузка)
- **Двухшаговый диалог загрузки блока**: ввод имени → ввод версии → подтверждение
- **Панель деталей блока** — отображает информацию о выбранном блоке, результаты операций и доступные действия
- **Подвал** обновлён с горячими клавишами U/L/H
- `DashboardState` расширен: `BlockInputMode`, `block_input_buffer`, `block_operation_result`
- `selected_block_name_version()` — возвращает имя+версию выбранного блока

### Визуализация графа зависимостей блоков в TUI
- **5-я вкладка: Deps** — интерактивная таблица графа зависимостей блоков
- Отображает зависимости и зависимых для каждого блока
- **Панель порядка загрузки** — топологическая сортировка последовательности загрузки блоков
- `BlockRegistry::dependency_graph()` — построение `DependencyGraph` из зарегистрированных блоков
- `BlockRegistry::set_block_dependencies(name, deps)` — объявление межблочных зависимостей
- Структура `DependencySnapshot` для рендеринга TUI: blocks, load_order, edges
- 1 новый тест для метода `dependency_graph()`

### CI/CD
- **GitHub Actions CI** (`.github/workflows/ci.yml`): check, fmt, clippy, test, coverage
- **Отчёт покрытия** через `cargo-tarpaulin` (LLVM engine, XML output)
- **Интеграция Codecov** для загрузки покрытия
- **Автоматизация релизов** (`.github/workflows/release.yml`): мультиплатформенные сборки по тегу
- Цели: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`
- Автогенерация GitHub-релизов с changelog и бинарными архивами

### Инфраструктура измерения задержки
- `LatencyTracker` в `aios-optim/src/latency.rs` — отслеживание задержки по операциям с алертами по порогам
- `LatencyGuard` — RAII-гард для автоматического тайминга (`tracker.guard("op")` → `guard.stop()`)
- `LatencyStats` — агрегированная статистика: min, max, avg, p50, p95, p99, количество нарушений
- `LatencyThreshold` — настраиваемые пороги warn/critical для каждой операции
- `LatencyLevel` — классификация Normal/Warning/Critical
- FIFO-вытеснение по операционным бакетам
- 11 модульных тестов

### Протокол наследования приоритетов
- `PriorityInheritance` в `aios-process-mgr/src/priority_inheritance.rs`
- `acquire_lock()` — захват блокировки с наследованием приоритета для высокоприоритетных ожидающих
- `release_lock()` — освобождение с восстановлением приоритета и цепочкой пробуждения
- `request_resource()` — запрос ресурса с наследованием приоритета
- `apply_pending_boosts()` — извлечение накопленных рекомендаций по повышению приоритета
- `release_all()` — освобождение всех блокировок процесса (для очистки при сбоях)
- 12 модульных тестов

### Расширение обнаружения аппаратуры
- **Обнаружение NPU Intel Meteor Lake** через PCI vendor/device ID (8086:7D0B) на Linux и Windows
- **Обнаружение NPU Qualcomm X Elite** через PCI vendor/device ID (17CB:1100) и имя процессора
- Профили `mock_intel_meteor_lake()` и `mock_qualcomm_x_elite()` с NPU, GPU и интерфейсами
- **Перечисление USB-устройств** через `lsusb` (Linux) и WMI (Windows) с классификацией скорости
- **Перечисление Thunderbolt-устройств** через sysfs (Linux) и WMI (Windows) с Tb1–Tb5
- 11 новых тестов для NPU-профилей, типов USB/TB и сериализации

### Интеграция WebAssembly рантайма (aios-wasm)
- Новый крейт `aios-wasm` v1.0.0 — встраивание Wasmtime v47 для песочницы блоков
- `WasmSandbox` — создание движка с потреблением топлива и эпохальным прерыванием
- `WasmBlock` — жизненный цикл WASM-блока: компиляция, инстанцирование, вызов функций
- `SandboxConfig` — лимиты страниц памяти, топлива, максимальные экземпляры, тайм-аут
- `WasiFilter` — фильтрация WASI-системных вызовов с политиками Allow/Deny/Log
- `IsolationConfig` — изоляция «без общего контента»: уровни None/Process/Memory/Network/Full
- `ResourceLimits` — лимиты памяти, CPU-времени, хранилища, сети и файлов на блок
- `IsolationBoundary` — реестр изоляции по блокам с управлением межблочной коммуникацией
- 39 тестов: жизненный цикл песочницы, компиляция WASM, вызовы функций, фильтрация WASI, границы изоляции

### Маркетплейс блоков
- `BlockMarketplace` в `aios-block-mgr/src/marketplace.rs` — реестр блоков с управлением репозиториями
- `BlockMetadata` — имя, версия, описание, автор, sha256, теги, количество загрузок
- `RepositoryEntry` — метаданные, статус, локальный путь
- `BlockStatus`: Available, Installed, UpdateAvailable, Deprecated
- Публикация, поиск, установка, удаление, проверка обновлений
- Поддержка нескольких репозиториев с кросс-поиском
- 18 модульных тестов для жизненного цикла маркетплейса

### Сетевой стек (aios-net)
- Новый крейт `aios-net` v1.0.0 — TCP/UDP блоки для сетевого взаимодействия
- `TcpBlock` — TCP клиент/сервер с управлением соединениями
- `UdpBlock` — UDP сокет с привязкой, отправкой, широковещанием
- 27 тестов для TCP и UDP

### Абстракция файловой системы
- `FileSystem` в `aios-core/src/filesystem.rs` — единый слой доступа к файлам
- Виртуальная, локальная, наложенная (overlay) файловая система
- `FilePermissions` — чтение/запись/выполнение
- 20 модульных тестов

### Графический дашборд (aios-gui)
- Новый крейт `aios-gui` v1.0.0 — нативный графический дашборд на egui/eframe
- **6 вкладок**: Обзор, Процессы, Блоки, Маркетплейс, Метрики, Зависимости
- **Обзор**: карточки статистики, системная информация, графики RAM, журнал активности
- **Процессы**: таблица с PID, именем, приоритетом, состоянием; Kill/Suspend/Resume
- **Блоки**: таблица блоков, диалог загрузки, выгрузка, горячая замена
- **Маркетплейс**: поиск, установка/обновление/удаление блоков
- **Метрики**: прогресс-бар RAM, распределение приоритетов, статистика блоков
- **Зависимости**: граф зависимостей, порядок загрузки
- `AiosTheme` — тёмная тема с настраиваемыми цветами
- 7 тестов для приложения

### Качество кода
- Исправлены все предупреждения clippy (ноль предупреждений)
- Удалены `#[cfg(unix)]` из тестов persistence — все тесты работают на Windows
- Все 708 тестов проходят в 20 крейтах рабочего пространства

### Повышение версии
- Все крейты повышены до 1.0.0

## v0.6.0 (Планирование) — Продвинутая оптимизация и надёжность оборудования (2026-07-27)

### Фаза 15: Zero-Copy IPC Ring Buffers ✅ РЕАЛИЗОВАНО
- ✅ Lock-free кольцевые буферы для single-producer/single-consumer (`aios-ringbuf` крейт)
- ✅ O(1) эффективность передачи данных (без копий в ядре)
- ✅ Zero-copy указатели чтения/записи для прямого доступа к памяти
- ✅ Обработка переноса с гарантиями атомарности
- ✅ 8 unit-тестов, охватывающих все операции
- Интеграция с существующим `IpcBus` транспортом (запланирована)

### Фаза 17: AI KV-Cache & State Compression ✅ РЕАЛИЗОВАНО
- ✅ FP8 квантизация (32 бита → 8 бит) для AI буферов
- ✅ INT4 квантизация (32 бита → 4 бита) для памятеёмких состояний
- ✅ ZSTD сжатие для таблиц состояния системы
- ✅ LRU кэш распаковки с настраиваемым размером
- ✅ 16 unit-тестов для квантизации, сжатия и кэширования
- Автоматические пороги сжатия на основе нехватки памяти (запланировано)

### Фаза 18: Atomic Copy-on-Write Persistence ✅ РЕАЛИЗОВАНО
- ✅ CoW движок хранилища с атомарным протоколом записи
- ✅ Снимки состояния с проверкой целостности SHA-256
- ✅ Журнал восстановления для защиты от сбоев при передаче состояния
- ✅ Атомарное переименование для безопасности при потере питания (write → fsync → rename)
- ✅ 6 unit-тестов (Unix) для операций хранилища
- Поддержка отката при неудачных live-updates (готово)

### Фаза 16: Hardware-Enforced Memory Protection ✅ РЕАЛИЗОВАНО
- ✅ Поддержка Intel MPK (Memory Protection Keys) с детектированием через CPUID из крейта `x86`
- ✅ ARM Memory Domains (fallback) с управлением DACR регистром
- ✅ Распределение PKEY per-block (макс. 16 ключей на Intel, 4 домена на ARM)
- ✅ MpkSecurityBridge для интеграции с системой capability `aios-security`
- ✅ Детектирование оборудования через `HwMemoryProtection::detect()`
- ✅ 27 comprehensive unit-тестов, охватывающих все операции
- Поддержка cross-architecture с graceful деградацией на неподдерживаемом оборудовании
- Интеграция с политиками блоков и control доступа готова для Фазы 2

## v0.5.0 — RT-планировщик, стресс-тесты и расширение оборудования (2026-07-26)

### Улучшение TUI дашборда — полноценный интерактивный 4-вкладочный дашборд
- Полная перезапись `dashboard.rs` со статичной 3-зонной компоновки на 4-вкладочный интерактивный дашборд
- **Вкладка 1 (Обзор)**: панель информации об оборудовании (CPU, GPU, хранилище, система) + журнал активности
- **Вкладка 2 (Процессы)**: полная таблица процессов (PID, имя, приоритет, состояние, RAM, CPU, сбои) с выбором строки и панелью деталей
- **Вкладка 3 (Блоки)**: таблица реестра блоков (ID, имя, версия, состояние, размер) со статистикой
- **Вкладка 4 (Метрики)**: индикатор использования RAM, гистограмма распределения приоритетов, временной ряд RAM
- `DashboardState` расширен: `processes: Vec<ProcessSnapshot>`, `blocks: Vec<BlockSnapshot>`, `selected_row`, `ram_history`, `process_kill_result`
- Снимки процессов/блоков берутся каждый кадр для согласованного рендеринга
- Структуры `ProcessSnapshot`/`BlockSnapshot` для отвязки рендеринга от блокировок scheduler/registry
- Цветовые стили приоритетов (Critical=Red, High=Yellow, Normal=Green, Low=Blue, Bg=DarkGray)
- Цветовые стили состояний (Running=Green, Crashed=Red, Terminated=DarkGray)
- Индикатор RAM с пороговой окраской (>85% Red, >60% Yellow, иначе Green)
- Отображение `BlockState` со стилями Active=Green, Error=Red
- 6 новых unit-тестов

### Интерактивные горячие клавиши TUI
- `j`/`Down` — перемещение выбора вниз в таблицах процессов/блоков
- `k`/`Up` — перемещение выбора вверх
- `K` — убийство выбранного процесса (с отображением подтверждения)
- `1`/`2`/`3`/`4` — переключение вкладок (Обзор/Процессы/Блоки/Метрики)
- Выбор сбрасывается при переключении вкладок
- Панель деталей процесса показывает информацию или результат убийства
- `r` — обновление, `s` — запись телеметрии, `x` — статус системы

### Архитектурные изменения TUI
- `update_from_scheduler()` теперь принимает `&Scheduler` и `&BlockRegistry` (был только `&Scheduler`)
- Выбор вкладки: `selected_tab` (0-3), выбор строки: `selected_row`
- Кольцевой буфер истории RAM (60 записей, по одной на кадр)
- Отображение результата убийства процесса (`Option<String>`)

### Менеджер процессов: планировщик реального времени
- Перечисление `SchedulingMode`: `Normal` (взвешенный round-robin) и `RealTime` (на основе дедлайнов)
- Структура `JitterEntry`: `pid`, `expected_ms`, `actual_ms`, `timestamp` — отслеживание джиттера планирования
- `set_scheduling_mode()`, `scheduling_mode()` — переключение между Normal и RT
- `set_rt_deadline(pid, deadline_ms)` — назначение абсолютного дедлайна процессу
- `clear_rt_deadline(pid)` — удаление дедлайна у процесса
- RT-планирование: выбор процесса с ранним дедлайном (наименьшее оставшееся время)
- Отслеживание джиттера: запись при превышении ожидаемого time slice или пропуске дедлайна
- `jitter_log()` и `clear_jitter_log()` для аудита джиттера
- 9 новых unit-тестов: режим по умолчанию, смена режима, управление дедлайнами, ранний дедлайн, пропуск не-gotовых, запись джиттера, очистка джиттера, нет кандидатов, смена режима

### Стресс-тесты и бенчмарки
- 11 стресс-тестов в `tests/stress_test.rs`:
  - `test_stress_mass_spawn_1000` — создание 1000 процессов + цикл планирования
  - `test_stress_ipc_bus_throughput` — отправка/получение 10k IPC-пакетов
  - `test_stress_rt_scheduler_500` — планирование 500 RT-задач с дедлайнами
  - `test_stress_block_registry_500` — регистрация/запрос 500 блоков
  - `test_stress_context_store_1000` — 1000 записей телеметрии
  - `test_stress_hardware_mock_serialize` — 10k сериализаций HW-профиля
  - `test_stress_heartbeat_1000` — 1000 HMAC heartbeat циклов
  - `test_stress_storage_profiles` — проверка мок-профилей NVMe/SATA
  - `test_stress_message_router_500` — 500 диспатчей маршрутизатора
  - `test_stress_live_update_20` — 20 параллельных горячих замен
  - `test_stress_persistent_store_batch` — 500 записей телеметрии в redb

### HAL: Определение устройств хранения
- Структура `StorageDevice`: `name`, `interface`, `capacity_gb`, `model`
- Перечисление `StorageInterface`: `NVMe`, `SATA`, `USB`, `Unknown`
- `detect_storage()` — Windows: `wmic diskdrive` / Linux: `/sys/block`
- `HardwareProfile::storage_devices: Vec<StorageDevice>` — во всех 4 мок-профилях

### HAL: Определение AMD GPU
- `detect_gpu_amd()` — Linux: парсинг вывода `rocm-smi --showproductname --showmeminfo vram`

### HAL: Unit-тесты для хранения
- 7 новых тестов: проверка NVMe/SATA профилей, сериализация StorageDevice, сериализация полного профиля

### Новый крейт: `aios-exec-compat` — мультибинарная совместимость
- **11-й крейт рабочего пространства** — интерцептор выполнения и транслятор syscall дляforeign-бинарников
- Неинвазивная архитектура: подключается к IPC Message Bus как модуль системы
- Zero-trust sandboxing: foreign-исполняемые файлы работают с ограниченными `CapabilityTokens`

#### Парсер заголовков (`format.rs`)
- `ExecutableType`: `AiosNative`, `LinuxElf`, `WindowsPe`, `Unknown`
- `HeaderFormat::from_bytes(data: &[u8])` — magic bytes: `MZ` для PE, `\x7fELF` для Linux, `AIOS` для нативных
- `ExecutableType::from_extension(path)` — определение типа по имени файла (.exe/.dll → PE, .so/.elf → ELF, .aib → AIOS)
- `BinaryHeader::parse(data)` — полный парсинг заголовка: entry_point_offset, is_64bit, machine_arch, subsystem
- ELF64/ELF32: e_entry со смещения 24, определение класса по байту
- PE32/PE32+: MZ→PE смещение, machine arch (0x8664/0x014C), magic optional header
- `CompatCapability` (9 вариантов): FilesystemRead/Write, ProcessCreate, NetworkAccess, RegistryAccess, WinApiCompat, PosixCompat, MemoryMap, ThreadCreate
- Скоростной тест: <5us на определение заголовка

#### POSIX подсистема (`posix.rs`)
- `PosixSyscall` (18 вариантов): SysOpen, SysRead, SysWrite, SysClose, SysLseek, SysFork, SysExec, SysExit, SysMmap, SysMunmap, SysSocket, SysConnect, SysSend, SysRecv, SysGetpid, SysGetuid, SysStat, SysFstat
- `PosixTranslator`: трансляция syscall Linux → IPC-пакеты AIOS
- `DefaultPosixTranslator` — реальная реализация трансляции
- Скоростной тест: <5us на трансляцию

#### Win32 подсистема (`win32.rs`)
- `Win32Api` (16 вариантов): CreateFileW, ReadFile, WriteFile, CloseHandle, GetProcAddress, LoadLibraryW, VirtualAlloc, VirtualFree, CreateThread, ExitProcess, GetLastError и др.
- Диспатч по Win32 ordinal (стандартные SSN Windows)
- `Win32Translator` с поддержкой регистрации DLL

#### Исцелитель зависимостей (`dependency_healer.rs`)
- Автоматическое обнаружение недостающих .dll/.so библиотек
- Пайплайн: `scan_dependencies()` → `resolve_missing()` → `heal_dependencies()`
- Настраиваемые пути поиска, кэш резолюции, автозагрузка в sandbox

#### Совместимость песочницы (`sandbox_compat.rs`)
- `CompatSandboxConfig` — лимиты по типу: память, файлы, потоки, capabilities
- `CompatProcess` — проверка capabilities, ограничения ресурсов, блокировка syscall
- `CompatSandboxManager` — управление жизненным циклом с лимитом процессов

#### Интеграционные тесты
- Парсинг заголовков (ELF/PE/AIOS), POSIX трансляция, Win32 трансляция, исцеление зависимостей, изоляция песочницы, кросс-подсистемный жизненный цикл

### Статистика
- **11 крейтов** рабочего пространства + crate интеграционных тестов + стресс-тесты
- **~9,500 строк** Rust (без тестов)
- **344 теста** (286 unit + 28 интеграционных + 11 стресс + 19 exec-compat)
- **0 предупреждений** clippy
- **90+ публичных типов**, **320+ публичных методов**

### Документация
- Двуязычная документация для всех новых функций (EN + RU)

### HAL: Определение NVIDIA GPU через nvidia-smi
- `GpuInfo` расширен полями `driver_version: String`, `cuda_cores: u32`, `compute_capability: String`
- `detect_gpu_nvidia()` — Windows: запускает `nvidia-smi --query-gpu=name,memory.total,driver_version,compute_cap --format=csv,noheader,nounits`
- `estimate_cuda_cores(gpu_name)` — отображение имён GPU на количество CUDA-ядер (RTX 4090→16384, A100→6912, H100→16896)
- `detect_gpu_wmic()` — наследуемый fallback через Windows WMI
- 8 новых модульных тестов: поля GpuInfo, мок-профили, оценка CUDA-ядер, цикл сериализации

### Менеджер блоков: горячая перезагрузка из файловой системы
- Структура `HotReloader` в `hot_reload.rs` — мониторинг директории на наличие файлов `.bin`/`.aib`
- `scan_and_reload(registry)` — обнаружение новых, обновлённых и удалённых файлов блоков
- `HotReloadConfig`: `watch_dir`, `poll_interval_ms`, `auto_activate`
- `TrackedFile` — отслеживание `path`, `modified`, `sha256`, `loaded_id` для каждого файла
- `ReloadEvent` enum: `NewBlock`, `UpdatedBlock`, `RemovedBlock`, `Error`, `NoChange`
- Обнаружение изменений по SHA-256 — перезагрузка только при реальном изменении содержимого
- Автосоздание директории при отсутствии; журнал событий для аудита
- 9 модульных тестов

### Менеджер процессов: группы и сессии
- Структура `ProcessGroup`: `id`, `name`, `priority`, `member_pids`, `created_at_ms`, `session_id`
- Структура `Process` расширена полем `group_id: Option<u64>` и билдером `with_group()`
- Управление группами планировщика: `create_group()`, `create_session()`, `add_to_group()`, `remove_from_group()`
- Операции с группами: `kill_group()`, `suspend_group()`, `resume_group()`, `set_group_priority()`
- `group_members()`, `all_groups()`, `group_count()`, `get_group()`
- 10 новых модульных тестов: создание групп, сессий, добавление/удаление участников, kill/suspend/resume, смена приоритета, ошибки

### Документация
- Двуязычная документация: все 4 файла (ARCHITECTURE, CHANGELOG, BUGS, TODO) ведутся на английском и русском
- AGENTS.md обновлён правилом двуязычной документации и структурой документации

## v0.4.0 — Укрепление системы и приоритет 2 (2026-07-26)

### Улучшения IPC Bus
- **Политики обратного давления**: `BackpressurePolicy::Reject` (по умолчанию) и `BackpressurePolicy::DropOldest`
- Метод-билдер `IpcBus::with_backpressure()` для `IpcBus` и `SharedIpcBus`
- Drop-oldest извлекает из начала очереди и удаляет из набора дедупликации
- **Дедупликация сообщений**: `IpcBus::with_dedup()` включает дедупликацию по `packet_id` через `HashSet<u64>`
- Дублирующие отправки молча отбрасываются, счётчик хранится в `metrics.total_deduplicated`
- **Метрики шины**: структура `BusMetrics` с отслеживанием `total_sent`, `total_received`, `total_dropped`, `total_deduplicated`, `peak_queue_depth`, `avg_send_latency_us`
- Методы `metrics()` и `reset_metrics()` у `IpcBus`
- 7 модульных тестов: обратное давление (reject + drop-oldest), дедупликация, метрики, сброс, приоритет с drop-oldest

### Улучшения планировщика
- **Взвешенный round-robin**: `priority_weight()` отображает Background=1, Low=2, Normal=3, High=4, Critical=5
- Квант времени = `default_time_slice_ms * priority_weight` (пропорционально приоритету)
- `round_robin_positions: HashMap<Priority, usize>` отслеживает позицию внутри каждой очереди приоритетов
- Исправлен баг с ранним `break` при предотвращении голода: внутренний цикл теперь оценивает все процессы в очереди (старение может поднять позже поступившие процессы выше ранее поступивших)
- **Обнаружение нехватки памяти**: `memory_pressure_threshold` (по умолчанию: 0.8)
- Перечисление `MemoryPressure`: `Normal(usage)`, `Warning(usage)`, `Critical(usage)`
- Структура `MemoryPressureEvent` с уровнем, использованием, занято/всего МБ, именами колбэков
- Методы `register_memory_pressure_callback()` и `check_memory_pressure()`
- 5 новых модульных тестов: вес приоритета, взвешенный квант времени (внутри и кросс-приоритет), нехватка памяти (normal, warning, critical)

### Улучшения менеджера блоков
- **Граф зависимостей** (`dependency.rs`): `DependencyGraph` с рёбрами `HashMap<String, Vec<String>>`
- `add_block()`, `add_dependency()` с обнаружением циклов через DFS
- `load_order()` — топологическая сортировка (алгоритм Кана) для корректного порядка инициализации
- `unload_order()` — обратная топологическая сортировка для безопасного завершения
- `dependencies_of()`, `dependents_of()`, `remove_block()`, `blocks()`, `has_block()`
- **Семантическое версионирование** (`version.rs`): структура `SemanticVersion` (major, minor, patch)
- `parse()` с необязательным префиксом `v`, форматирование `Display`
- Реализация `Ord` для сравнения версий
- `is_compatible_with()` (одинаковый major, >= minor), `is_newer_than()`
- `bump_major/minor/patch()` для увеличения версий
- 9 тестов зависимостей + 7 тестов версий

### Исправление ошибок
- **BUG-010**: Исправлен ранний `break` в `schedule_next()` во внутреннем цикле очереди приоритетов — старение могло поднять позже поступивший процесс выше ранее поступившего в рамках одной очереди, но `break` не давал оценить все процессы. Удалён `break`, теперь все процессы в состоянии Ready в очереди оцениваются.
- **BUG-011**: Исправлен нестабильный `test_unload_order_reversed` — порядок топологической сортировки недетерминирован для независимых узлов (итерация HashMap). Тесты теперь проверяют ограничения зависимостей, а не абсолютные позиции.

### Дополнительные интеграционные тесты (21–28)
- `test_ipc_bus_backpressure_dedup_metrics` — политика DropOldest + дедупликация + сброс метрик
- `test_scheduler_weighted_round_robin` — round-robin внутри одного уровня приоритета
- `test_scheduler_memory_pressure_detection` — уровни предупреждения и критической нехватки памяти с колбэками
- `test_block_dependency_graph_ordering` — граф зависимостей из 6 блоков, проверка порядка загрузки/выгрузки
- `test_semantic_version_with_block_registry` — сравнение версий + интеграция с реестром
- `test_ipc_bus_priority_cross_crates` — упорядочивание очереди приоритетов через send_priority
- `test_dependency_graph_complex_cycle` — обнаружение циклов + удаление узлов + повторная проверка
- `test_cross_subsystem_scheduler_security_ipc` — кросс-крейт тест планировщика + безопасности + IPC шины

## v0.3.0 — Интеграция системы и улучшения планирования (2026-07-26)

### Старение процессов в планировщике (предотвращение голода)
- Добавлены `aging_threshold_ms` и `last_scheduled_ms` в `Scheduler` для отслеживания времени ожидания
- `schedule_next()` теперь вычисляет эффективный приоритет = базовый приоритет + время ожидания / порог (ограничено повышением до +4)
- Все процессы оцениваются глобально (без раннего выхода по уровню очереди) для корректного поведения старения
- `ProcessTimer::force_expire()` для детерминированного тестирования
- Публичное API: `force_preempt()`, `set_last_scheduled()`, `is_scheduled()`, `with_aging_threshold()`
- Модульный тест: `test_aging_boosts_low_priority`

### Интеграция Watchdog и TUI
- Добавлены зависимости `aios-watchdog` и `aios-context` в `aios-tui`
- Поток heartbeat watchdog работает в фоне во время сессии TUI
- Заголовок панели отображает текущее состояние watchdog: OK (зелёный), SUSPENDED (красный), RECOVERING (жёлтый), SAFE MODE (пурпурный)
- `DashboardState::update_watchdog()` для синхронизации состояния
- Новые привязки клавиш: `s` — запись телеметрии, `x` — статус системы
- `SafeModeShell` интегрирован в основной цикл для выполнения команд в безопасном режиме

### Связка Context Store и планировщика
- `EmbeddedContextStore` и `TelemetryStore` инициализируются в основном цикле
- Запись телеметрии по клавише `s`: записывает количество процессов и метрики ОЗУ
- API `TelemetryEntry`: паттерн билдера `with_block()`, `with_process()`
- Интеграционный тест: `test_context_store_wired_to_scheduler` проверяет регулировку приоритета на основе телеметрии

### Очередь приоритетов IPC шины
- Метод `IpcBus::send_priority()` для упорядочивания извлечения по приоритету
- Пакеты с более высоким приоритетом извлекаются первыми, FIFO в пределах одного уровня приоритета
- 2 модульных теста: `test_priority_queue_ordering`, `test_priority_fifo_within_same_level`

### Дополнительные интеграционные тесты (11–20)
- `test_watchdog_heartbeat_lifecycle` — координация watchdog и IPC шины
- `test_safe_mode_shell_lifecycle` — разбор и выполнение команд безопасного режима
- `test_security_sandbox_enforcement` — кросс-модульное принуждение capability и песочницы
- `test_context_store_cross_collection` — API запросов телеметрии, рабочих процессов и стабильности
- `test_watchdog_scheduler_crash_coordination` — watchdog запускает обработку аварийного завершения планировщика
- `test_security_ipc_packet_capability_check` — проверка capability для IPC пакетов
- `test_live_update_with_security_revocation` — координация горячей замены и отзыва токенов
- `test_telemetry_driven_priority_adjustment` — запросы телеметрии управляют изменением приоритета планировщика
- `test_scheduler_aging_starvation_prevention` — старение повышает планирование низкоприоритетных процессов
- `test_context_store_wired_to_scheduler` — телеметрия контекстного хранилища питает решения планировщика

### Документация
- Созданы: AGENTS.md, README.md, docs/ARCHITECTURE.md, docs/CHANGELOG.md, docs/BUGS.md, docs/TODO.md

## v0.2.0 — Системы безопасности, защиты и контекста (2026-07-25)

### Фаза 8: AI Watchdog и движок аварийного восстановления (`aios-watchdog`)
- Структура `Heartbeat` с аутентификацией SHA-256 HMAC
- `Watchdog` с 4-состоянием конечным автоматом: Monitoring → Suspended → Recovering → SafeMode
- Настраиваемые интервал heartbeat, порог пропусков и тайм-аут восстановления
- `WatchdogConfig` с разумными значениями по умолчанию (интервал 1с, 3 пропуска, восстановление 10с)
- Перечисление `WatchdogAction` для решений ядра
- Журнал аудита `WatchdogEvent` для всех переходов состояний
- `SafeModeShell` с детерминированными CLI-командами (ps, blocks, kill, unload, status, logs, restart)
- Ограничение перезапусков для предотвращения бесконечных циклов
- 19 модульных тестов, покрывающих аутентификацию heartbeat, конечный автомат watchdog, циклы восстановления, безопасный режим

### Фаза 9: Capability-based безопасность и песочница (`aios-security`)
- Перечисление `Capability` с 15 конкретными разрешениями (сеть, файловая система, оборудование, память, система)
- `CapabilityToken` с HMAC-подписями и ограниченным по времени сроком действия
- `AccessControlLayer` для выдачи, проверки, отзыва токенов и отслеживания нарушений
- `Sandbox` — изоляция для каждого блока с проверкой системных вызовов, ограничениями памяти и лимитами количества системных вызовов
- Журнал аудита `Violation` для попыток несанкционированного доступа
- 20 модульных тестов, покрывающих жизненный цикл токенов, контроль доступа, принуждение песочницы

### Фаза 10: Постоянное контекстное хранилище системы (`aios-context`)
- `EmbeddedContextStore`, объединяющий телеметрию, рабочие процессы и коллекции стабильности
- `TelemetryStore` с переполнением FIFO (10k записей), запросами метрик, запросами по временному диапазону, запросами для каждого блока
- `WorkflowStore` для изученных профилей приоритетов с отслеживанием использования
- `StabilityStore` для оценки надёжности блоков с отслеживанием аварий/аптайма
- 18 модульных тестов, покрывающих все операции хранилищ

## v0.1.0 — Начальная система (2026-07-25)

### Фаза 1: Рабочее пространство + IPC протокол
- Создано плоское рабочее пространство с 7 крейтами (aios-core, aios-ipc, aios-hal, aios-block-mgr, aios-live-update, aios-process-mgr, aios-tui)
- Реализован бинарный IPC протокол с сериализацией bincode
- Структура `Header` с packet_id, source/target блоками, command_id, приоритетом, payload_len, SHA-256 контрольной суммой
- Перечисление `Payload` с 15 вариантами, покрывающими все операции ОС
- Перечисление `Response` (Success/Failure/Timeout)
- `IpcPacket` с автоматически генерируемым packet_id (AtomicU64) и проверкой целостности SHA-256
- Перечисление `CommandId` (u16) с 13 типами команд, организованными по доменам (block=0x0001–0x0003, process=0x0010–0x0012, system=0x0020–0x0050)
- Скоростные тесты с двойными порогами (debug: 50мкс, release: 1мкс)

### Фаза 2: Уровень абстракции оборудования (HAL)
- `HardwareProfile` с обнаружением CPU, GPU, NPU, Memory, PCI
- Реальное обнаружение через `wmic` (Windows) и `/proc/cpuinfo` + `/proc/meminfo` (Linux)
- `CpuInfo` с флагами AVX-512, AVX2, SSE4.2, NEON
- Структуры `GpuInfo`, `NpuInfo`, `PciDevice`, `MemoryInfo`
- Классификация `AiTier`: Tier1 (локальный LLM), Tier2 (edge-инференс), Tier3 (только лёгкие задачи)
- 4 мок-профиля: legacy, modern, legacy_2012, custom
- `HalBlock`, реализующий трейт `StatefulBlock` (извлечение/восстановление состояния профиля)
- 8 модульных тестов для логики классификации уровней

### Фаза 3: Менеджер блоков
- `BlockRegistry` с регистрацией/выгрузкой/активацией, проверкой подписи SHA-256
- `BlockEntry` хранит манифест + состояние + бинарный файл
- `BlockLoader` для валидации бинарного файла и одноразовой загрузки + активации
- `MessageRouter` с диспетчеризацией обработчиков и перенаправлением маршрутов
- `BlockHandler` = `Box<dyn FnMut(&IpcPacket) -> Result<Option<IpcPacket>>>`
- 15 модульных тестов по реестру, загрузчику и маршрутизатору

### Фаза 4: Менеджер процессов
- `ProcessId`, `Priority` (5 уровней: Background/Critical), `ProcessState` (5 состояний)
- Структура `Process` с crash_count, max_restarts, parent_pid
- `ProcessTimer` для временных квантов с отслеживанием квоты
- `Scheduler` с очередями приоритетов на основе BTreeMap, контроль квоты ОЗУ
- Планирование по приоритету с round-robin внутри одного приоритета
- Устойчивость к авариям: автоматический перезапуск до max_restarts, логирование CrashEvent
- `handle_process_command()` для управления spawn/kill/adjust_priority через IPC
- 10 модульных тестов, включая тесты аварий и дочерних процессов

### Фаза 5: Движок live-update
- `StateTransferManager` для заморозки/извлечения/восстановления состояния IPC шины
- Структура `Snapshot` с фиксацией ожидающих пакетов + байтов состояния
- `LiveUpdateEngine` с атомарной горячей заменой (5-шаговый процесс):
  1. Заморозка IPC шины + извлечение состояния
  2. Валидация SHA-256 нового бинарного файла
  3. Проверка работоспособности (опциональное замыкание)
  4. Сохранение записи для отката
  5. Восстановление IPC шины
- `HotSwapEntry` для данных отката (старый бинарный файл, состояние, версия)
- Журнал аудита `SwapRecord` для всех операций замены
- Откат с настраиваемым предупреждением о тайм-ауте
- 9 модульных тестов, покрывающих сценарии успеха, сбоя и отката

### Фаза 6: AI-оркестратор + TUI
- `IntentEngine`, преобразующий естественный язык в `IpcPacket`
- 8 категорий намерений: memory, video, block_update, kill, spawn, priority, health_check, topology
- `IntentContext`, предоставляющий состояние системы для преобразования намерений
- `TranslatedCommand` с пояснением, описанием намерения и IPC-пакетом
- Панель Ratatui с 3-зонной компоновкой: заголовок (уровень + метрики), основная часть (системная информация + лог), нижний колонтитул (привязки клавиш)
- `DashboardState` с буфером лога на 100 записей и синхронизацией с планировщиком
- Цветовая кодировка: цвета уровней (зелёный/жёлтый/красный), цвета серьёзности логов
- Точка входа main.rs: обнаружение оборудования → классификация уровня → загрузка блоков → запуск процессов → цикл обработки событий терминала
- 10 модульных тестов для преобразования намерений и состояния панели

### Фаза 7: Интеграционные тесты
- 10 интеграционных тестов, покрывающих:
  1. Полный жизненный цикл системы (HAL → уровень → реестр → планировщик → топология)
  2. Скорость сериализации IPC (50k раундов)
  3. 100 параллельных запусков процессов с отслеживанием ОЗУ
  4. Live-update с 50 сообщениями IPC в полёте
  5. Устойчивость к авариям (3 аварии, принуждение политики перезапуска)
  6. Интеграция маршрутизатора сообщений (прямая + перенаправленная диспетчеризация)
  7. Жизненный цикл управления процессами через IPC (spawn → PID → настройка → kill)
  8. Классификация AI-уровней для всех профилей
  9. Раунд-трип для stateful блока (извлечение/восстановление)
  10. Полный жизненный цикл горячей замены с сохранением шины и откатом

## v1.0.0 — Интеграция TEE (Trusted Execution Environment) (2026-07-28)

### Phase 20: TEE (Trusted Execution Environment) Integration ✅ РЕАЛИЗОВАНО
- ✅ Обнаружение платформы TEE (Intel SGX, ARM TrustZone, AMD SEV) с корректным fallback
- ✅ Запечатывание и распечатывание данных с привязкой к платформе (`SealingKey`, `SealedData`)
- ✅ Фреймворк удалённой аттестации с поддержкой PCR (Platform Configuration Register)
- ✅ Управление жизненным циклом анклавов (Created → Initialized → Running → Suspended → Exited/Failed)
- ✅ Конфигурация анклавов с управлением памятью и потоками
- ✅ Сохранение и сериализация состояния для всех типов TEE
- ✅ Кросс-платформенная поддержка: Windows (x86-64), Linux, ARM64
- ✅ 28 модульных тестов, покрывающих все операции TEE
- ✅ Интеграция с системой capabilities `aios-security` для контроля доступа
- ✅ Полная поддержка сериализации через `bincode` для IPC-транспорта

### Docker: Multi-stage production сборка для VM
- Полностью переработан `Dockerfile`: multi-stage сборка (builder → runtime)
- **builder** (rust:1.97-bookworm): компиляция `--release` + прогон `--lib` тестов
- **runtime** (debian:bookworm-slim): минимальный образ ~80MB с бинарником `aios-tui`
- `docker-compose.yml` обновлён: использует `target: runtime`, добавлены `stdin_open`/`tty` для TUI
- Сигнал завершения: `SIGINT` для корректного shutdown через crossterm
- Переменные окружения: `AIOS_DATA_DIR`, `AIOS_BLOCKS_DIR`, `AIOS_MOCK_PROFILE`, `RUST_LOG`

## v1.3.0 — Headless Daemon и исправление Docker (2026-07-29)

### aios-daemon — Новый крейт: фоновый сервер
- Новый крейт `aios-daemon` — headless AIOS сервер для Docker/фонового запуска
- Минимальные зависимости: без ratatui, crossterm, egui, wasmtime
- Бинарник `aiosd` — выполняет ту же инициализацию, что и `aios-tui` (блоки, планировщик, watchdog, БД) без доступа к терминалу
- Фоновый heartbeat: логи процессов, RAM, статус watchdog каждые 10 секунд
- Сохранение телеметрии в redb каждые 60 секунд
- Конфигурация через переменные окружения: `AIOS_DATA_DIR`, `AIOS_BLOCKS_DIR`, `AIOS_MOCK_PROFILE`, `RUST_LOG`

### aios-tui: Headless-режим
- Добавлен флаг `--headless` и переменная `AIOS_HEADLESS=1`
- В headless-режиме пропускает инициализацию TUI (ratatui/crossterm) и работает в фоне

### Docker-инфраструктура
- Dockerfile упрощён: собирает только `aios-daemon` (быстрая сборка ~2мин)
- Использует `aiosd` как CMD по умолчанию — TTY не требуется
- docker-compose.yml: daemon по умолчанию, профиль `interactive` для TUI
- Убран `stdin_open`/`tty` из сервиса по умолчанию
- Размер образа: ~120MB (против ~800MB при сборке всего workspace)

## Заметки об отладке
- Пороги скоростных тестов требуют двойных значений для debug/release из-за замедления в 10–20 раз в неоптимизированных сборках
- PATH в Windows должен собираться вручную из переменных среды Machine + User перед командами cargo
- Линт `skip_while().next()` заменён паттерном `position().nth()` для более чистой логики итераторов

## Общая статистика (v0.5.0)
- **11 крейтов** рабочего пространства + crate интеграционных тестов + стресс-тесты
- **~10,200 строк** Rust (без тестов)
- **350 тестов** (292 unit + 28 интеграционных + 11 стресс + 19 exec-compat)
- **0 предупреждений** clippy
- **90+ публичных типов**, **320+ публичных методов**
