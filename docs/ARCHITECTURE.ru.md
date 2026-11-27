# Архитектура AIOS

## Обзор системы

AIOS (AI-Native Operating System) — модульная ОС в стиле микроядра, разработанная для AI-ориентированных нагрузок. Система состоит из 33 Rust-крейтов, образующих слоистую архитектуру: базовые типы внизу, абстракция оборудования и управление процессами в середине, системы безопасности/контекста и пользовательские интерфейсы (нативный GUI ядра
на фреймбуфере, хостовый TUI, единый бинарник) наверху. С v2.81.0 рабочий стол
`aios-kernel` — единственный пользовательский GUI; хостовый egui/eframe-код
остаётся устаревшим справочным.

Вся коммуникация между крейтами осуществляется через бинарный IPC-протокол. Блоки (модули ядра) поддерживают горячую замену с автоматическим откатом. AI-оркестратор преобразует намерения на естественном языке в системные операции. Крейт `aios` предоставляет единый системный бинарник с обоими режимами — интерактивным TUI и headless-демоном, заменяя отдельные точки входа `aios-tui` и `aios-daemon`.

> **См. также:** `docs/SCHEME.ru.md` — схема программы уровня вызовов с картой функций по крейтам; `docs/AUDIT.ru.md` — отчёт о полном аудите проекта.

```
┌──────────────────────────────────────────────────────┐
│              Interface Layer (User-Facing)            │
│  Kernel GUI (fb) │  Host TUI   │  Единый `aios` bin  │
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

## Единая текстовая консоль (`aios-console`)

Три существующих командных фронтенда (отдельный `aios-tui`, TUI оркестратора
`aios` и шелл ядра `aios-kernel` в ring 0) использовали пять независимых
парсеров с конфликтующими именами и семантикой. `aios-console` — единое ядро
команд, совместимое с `no_std`, которое заменяет эти парсеры:

```
   текстовая строка ──> parser ──> registry.resolve ──> проверка области/прав
                                                      │
                                        CommandBackend::execute
                                                      │
                                          CommandOutput (текстовые строки)
```

- `parser.rs` разбирает кавычки, экранирование и комментарии `#`. Имена команд и
  подкоманд приводятся к нижнему регистру, аргументы сохраняют регистр.
- `spec.rs` / `registry.rs` содержат каноническую таблицу `CommandSpec`, таблицу
  алиасов и генерируемый `help`. Неоднозначные старые имена отклоняются со
  списком канонических альтернатив; отложенные имена возвращают `unavailable`.
  Однозначные написания (`processes`, `ver`, `cls`) отображаются напрямую, а
  алиасы без аргументов (`log`, `wifi`) выбирают подкоманду по умолчанию, не
  перехватывая `log clear` и `wifi scan`.
- `capability.rs` объявляет `Scope` (`[H]`, `[K]`, `[B]`) и `CapabilitySet`;
  `backend.rs` проверяет каждый вызов до передачи бэкенду.
- `output.rs` задаёт `CommandOutput`, коды возврата (`0` OK, `1` usage,
  `2` denied, `3` unavailable, `4` backend error) и `OutputControl`
  (`Clear`, `Exit`), чтобы баг ядерной команды `clear` не повторился в новом ядре.
- `deferred.rs` хранит закоментированные варианты GUI/WebView/LLM/кластера/конвейеров.

`aios-console/src/host.rs` реализует `HostBackend` за функцией `std`. Он
делегирует тем же библиотечным крейтам, что и текущий хостовый TUI: `Scheduler`
для `ps`/`process`, `BlockRegistry`/`BlockLoader` для `block`, `NetSettingsBlock`
для `net`, `StoreManager` для `store` и `AiosVfs` для `vfs`. `source <file>`
выполняет сценарий, передавая каждую строку в тот же `dispatch()`, с ограничением
вложенности в 8 уровней. `metrics` и `health` используют общий `MetricsSnapshot`,
собранный из планировщика (состояния процессов, лог крашей, RAM), реестра блоков
(состояния блоков) и состояния сессии (рестарты, лог); `metrics` выводит его как
текст, JSON или Prometheus exposition, а `health` формирует статус и причины по
каждой проблеме. `telemetry` владеет `TelemetryStore` из `aios-context`, поэтому
записанные через консоль отсчёты используют то же представление кольца, что и
остальная система. `ipc` работает с `Arc<Mutex<IpcBus>>`, общим с `BridgeContext`;
`freeze` удерживает извлечённые пакеты в бэкенде до `unfreeze`. `sys` и `hw` читают
снимки `HardwareProfile` из `aios-hal`: первая команда определяет оборудование
лениво, `hw rescan` вызывает повторное определение, а `hw profile` выбирает
mock-профиль; `hw fingerprints` запускает `aios_autohal::extract_fingerprints`
для этого профиля, а `set_hardware_profile` подставляет снимок для тестов.
Состояние сессии (история, лог, счётчик рестартов, глубина `source`) хранится
в бэкенде.
`HostBackend::from_handles` принимает общие дескрипторы
`Arc<Mutex<Scheduler>>` / `Arc<Mutex<BlockRegistry>>`, чтобы консоль и
оркестратор видели одно и то же состояние; `with_config` создаёт приватные
дескрипторы для тестов. Подкоманды без реальной реализации возвращают
`unavailable`, а не ложный успех. `TuiApp` владеет одним экземпляром
`HostBackend`, созданным из дескрипторов оркестратора; вкладка шелла направляет
строки `console <...>` в него. Голые `clear`/`cls` тоже идут через тот же путь
`console_execute`: оба не выводят строк и возвращают только `OutputControl::Clear`,
как и прежняя очистка буфера; при этом они добавляются в историю `HostBackend`.
Первая партия адаптеров для команд без префикса охватывает `help`/`?`, `ps`,
`blocks`, корректный `kill`, `logs`, `status`, `restart`, `store list`,
фиксированные usage-строки для `kill`/`spawn`/`store`/`net` (включая отсутствующие
аргументы `store search`/`store install`) и откат на неизвестную команду.
`execute_aios_adapter` возвращает флаг, а `shell_execute` прибегает к
`shell_execute_legacy` для всего остального. Чистые форматтеры находятся в
`aios-console/src/legacy.rs` (`AIOS_HELP`, `aios_unknown_text`,
`aios_usage_text`, `aios_kill_success`, `aios_kill_failed`, `aios_log_text`,
`aios_status_text`, `aios_restart_text`, `aios_restart_log`), а адаптеры данных —
в `HostBackend` (`aios_process_list_text`, `aios_block_list_text`, `aios_kill`,
`aios_store_list_text`). Адаптер `kill` сохраняет особую формулировку `aios`
(`  killed '<name>' (<pid>)` / `  kill failed: <error>`) вместо текста
`LegacyMutation` безопасного режима и не записывает событие, поскольку старый
диспетчер `aios` не журналирует kill. Команда без префикса `restart` оставляет
повторное зондирование оборудования во фронтенде, а фиксированные строки вывода и
журнала — в `aios-console`; это другая операция, нежели каноническое `wd reset` и
`recover` безопасного режима. Команды без префикса `cluster` используют
хостовый адаптер `aios-console/src/legacy_host.rs::aios_cluster_text`, работающий
с тем же handle `DistributedScheduler`; форматтер и все изменяющие подкоманды
находятся в `aios-console`, а сборка `aios-console` для ядра остаётся `no_std`,
поскольку новый модуль включён за признаком `std`. Команда `spawn <path>` без
префикса вызывает `HostBackend::aios_spawn_path`, работающий с общим реестром
блоков и возвращающий точные legacy-строки журнала; адаптер записывает их в
журнал TUI и не выводит строк в шелл, как и старый диспетчер. Корректные
`store search`/`store install` используют `aios_store_search_start` и
`aios_store_install_start` из `legacy.rs` над тем же асинхронным `store_spawn`, а
`net get`/`net set` — `AiosNetResult` с форматтерами `aios_net_get_text` и
`aios_net_set_text`; роутер и tokio-дескриптор остаются во фронтенде, но обычный
путь шелла больше не обрабатывает команды через `shell_execute_legacy`. Старый
диспетчер остаётся только эталоном побайтового паритета в тестах.
`aios-shell` также владеет одним `HostBackend`, созданным в `main`, и
распознаёт префикс `console <...>` без учёта регистра до раскрытия алиасов, поэтому
реестр никогда не видит сам префикс. Один `console` выводит генерируемую справку;
`OutputControl::Clear` очищает терминал, а `Exit(code)` завершает процесс.
Команды без префикса (`help`/`?`, `clear`/`cls`, `pwd`, `echo`, `history`,
`alias`, `unalias`, `cd`, `env`/`export`/`unset`, `source`/`.`,
`jobs`/`fg`/`bg`) обрабатывает `execute_shell_adapter`, возвращающий
`ShellOutcome` (`Lines`, `Stderr` или `Clear`) и использующий точные форматтеры
из `aios-console/src/legacy.rs` (`SHELL_HELP`, `shell_echo_text`,
`shell_pwd_text`, `shell_alias_text`, `shell_alias_set_text`,
`shell_history_text`, `shell_cd_error_text`, `shell_env_text`,
`shell_export_set_text`, `shell_env_value_text`, `shell_source_recursion_text`,
`shell_source_missing_text`, `shell_jobs_text`, `shell_fg_text`,
`shell_bg_text`, `shell_job_error_text`). Фронтенд по-прежнему выполняет реальные
побочные эффекты: файлы истории и алиасов, раскрытие `$VAR`, изменение окружения,
`OLDPWD` и текущей директории, выполнение строк из исходимого файла, очистку
мёртвых PID, вывод заданий, отправку `SIGCONT` и вывод на экран. Тесты паритета
сравнивают текст побайтово, включая исходный ANSI-баннер шириной 60 столбцов, а
тесты состояния проверяют побочные эффекты. Защита от рекурсии в `source`
освобождает заём `RefCell` до выполнения строк, поэтому рекурсивный `source`
сообщает об ошибке, а не вызывает панику, как старый диспетчер. Канонические
`console jobs`/`fg`/`bg` используют ту же `JobTable`: `main` внедряет её в
`HostBackend` как адаптер `JobControl`, поэтому консоль видит живые фоновые задания
шелла, а `fg <id>`/`bg <id>` отправляют настоящий `SIGCONT` через фронтенд-владельца.
Обычный `HostBackend` без этого адаптера по-прежнему сообщает `unavailable`. В
`run_command_legacy` остаются только по своей природе фронтендные строки: внешние
программы, запуск GUI/TUI, `logs`/`hw`/`services`, `type`/`exec`/`which`,
`aios-shell-reload` и `exit`.
Отдельный бинарик `aios-tui` также владеет одним `HostBackend`, созданным в
`main`. Его вкладка Shell распознаёт тот же префикс `console <...>` без учёта
регистра до TUI-дополнений и отката на `SafeModeShell`. `execute_shell_cmd`
возвращает флаг, чтобы `console exit` вышел из event loop через обычный путь
завершения, а `console clear` очищает буфер шелла. Его `Scheduler` и
`BlockRegistry` теперь имеют тип `Arc<Mutex<...>>` и разделяются с
`HostBackend::from_handles`, поэтому `console ps` и `console block list` видят
те же процессы и блоки, что и команды безопасного режима и дашборд. Команды без
префикса `ps`, `processes`, `list`, `blocks`, `ls`, `status`, `info`, `exit`,
`quit`, `q`, `help`, `?`, `logs`, `log`, `kill`, `spawn`, `load`, `unload`,
`restart` и `recover` теперь идут через хостовые адаптеры `HostBackend`, которые
отображают данные рантайма и реальные мутации обратно в точный старый текст,
поэтому их вывод не изменился.
Адаптеры только для чтения: `process_list_legacy_text`,
`block_list_legacy_text`, `status_legacy_text`, `exit_legacy_text`,
`legacy_help_text` и `legacy_log_text`. Пять адаптеров мутаций (`legacy_kill`,
`legacy_spawn`, `legacy_load`, `legacy_unload`, `legacy_restart`) разбирают
команду целиком так же, как `SafeModeShell`, выполняют реальную операцию
планировщика или реестра либо используют ограниченный счётчик перезапусков
через общий бэкенд и записывают legacy-событие при успехе. Чистое форматирование
вывода и события находится в `aios-console/src/legacy.rs::LegacyMutation`.
Адаптер журнала читает дескриптор, внедрённый через `HostBackend::set_event_log`;
`SafeModeShell::with_event_log` добавляет события безопасного режима в тот же
дескриптор, не создавая зависимости console→watchdog. Строки ошибок сохраняют
точную старую формулировку `Failed: <error>`, которую TUI печатает как
`Error: Failed: <error>`; единственное исключение — ошибка лимита перезапусков
со своей старой формулировкой `Max restarts (<max>) reached. Manual intervention
required.` (без префикса `Failed:`). `legacy_restart` намеренно использует
`HostBackend::restarts` / `config.max_restarts` вместо `WatchdogControl::reset`,
поэтому команды без префикса `restart` и `recover` сохраняют старую семантику
счётчика и события; только каноническое `wd reset` вызывает внедрённый seam
watchdog. Команды без префикса `exit`/`quit`/`q` печатают старую
строку, но сохраняют старый флаг завершения: только `console exit` разрывает
event loop. Адаптер справки сохраняет отдельный блок TUI-дополнений. Откат на
неизвестную команду также использует адаптер консоли
(`legacy::unknown_text`), поэтому он больше не выполняет `SafeModeShell`.
Каноническое `console info` остаётся неоднозначным: адаптер использует только
legacy-алиас безопасного режима. Остальные строки сохраняют прежнее поведение TUI.
Все четыре фронтенда используют `aios_console::frontend::console_prefix` —
вспомогательную функцию ядра без `std`: она распознаёт префикс без учёта регистра,
сохраняет регистр аргументов, а голый или состоящий только из пробелов `console`
трактует как запрос справки.
`BridgeContext.registry` теперь имеет тип `Arc<Mutex<BlockRegistry>>`,
поэтому bridge, TUI и консоль видят одни и те же блоки; `BridgeContext.ipc_bus`
также стал `Arc<Mutex<IpcBus>>` и разделяется с консолью. Старые диспетчеры
остаются до полной проверки паритета. Паритет проверяется файлом
`aios-console/tests/legacy_parity.rs` для всех пяти командных поверхностей,
а также юнит-тестами чистых точек входа `SafeModeShell::parse_command`,
`IntentEngine::translate` и расширителя алиасов `aios-shell`. Обработчики,
зависящие от оборудования, в эти тесты не входят: таблица проверяет только
результаты разрешения команд. Поэтому `legacy_parity.rs` подтверждает разрешение,
но не равенство вывода: `aios/src/tui/mod.rs` фиксирует точные старые строки для
`help`/`?`, неизвестной команды, usage-сообщений `kill`/`store`/`net` и пустого
`ps`, а шестнадцать бинарных тестов паритета сравнивают путь адаптера с
`shell_execute_legacy` побайтово (команды только для чтения используют один
экземпляр `TuiApp`, чтобы порядок итерации `HashMap` в
`Scheduler::all_processes` совпадал для обоих путей), включая `cluster` с
отключённой кластеризацией и с живым планировщиком; ещё один тест проверяет, что
`restart` без префикса добавляет legacy-строку в журнал;
`aios-console/tests/host_backend.rs`
фиксирует канонический вывод хоста для `help`, `clear` и usage-ошибок.
`aios-watchdog/src/safe_mode.rs` теперь также
фиксирует точные старые `ShellResponse.success` и `output` для каждой ветки:
пустые и заполненные списки, успех и ошибки жизненного цикла процессов и блоков,
`status`, пустые и нумерованные `logs`, успешный перезапуск и достижение лимита,
`help`, `exit` и смешанный регистр неизвестной команды. Это целевой вывод,
которому должна соответствовать будущая миграция `SafeModeShell`; таблиц
разрешения команд недостаточно.

`HostBackendConfig.context_db_path` задаёт файл redb для `context persist`.
`HostBackend::execute_line()` — точка входа с учётом алиасов и трассировки:
алиасы сессии из `alias`/`unalias` раскрываются до диспетчеризации, а каждый
вызов записывается как span в `aios-telemetry::TraceContext` (`trace list`).
Вторая партия адаптеров, построенных на реальных вызовах библиотек, покрывает
`sleep`, `alias|unalias`, `fault resolve`, `sched mode|quantum|aging|
rt-deadline|rt-clear`, `net validate|config|status|apply|lease`,
`store trust|verify`, `vfs readat`, `caps list|issue|revoke|check`,
`sandbox start|stop|log|mem`, `hw-protect status|mpk|tee|iommu|clear`,
`wasm compile|call|mem|grow`, `wasi policy`, `exec detect|caps|<program>`,
`deps scan|heal`, `compat list|kill`, `context stats|export|compact|persist`,
`workflow list|priority`, `stability list|best|worst`, `trace list`,
`crash report`, `driver list|failures|remove|rollback|install|provision`,
`hotplug start|events|drain`, `wifi status|scan|connect|disconnect|sim|real`
и `hub status|scan|wifi|layout`. Они используют необязательные std-зависимости
`aios-debug`, `aios-exec-compat`, `aios-iommu`, `aios-mpk`, `aios-security`,
`aios-sys-control`, `aios-tee`, `aios-telemetry` и `aios-wasm`; сборка ядра
`no_std` не затронута. `aios-live-update` теперь необязательная std-зависимость:
`block swap|rollback` и `update history|pending|recover` используют реальный
журнал `PersistedLiveUpdateEngine`, записи отката и восстановление после краша.
`block swap` сообщает, что swap записан и сохранена запись отката; он не утверждает,
что код работающего блока был заменён, поскольку реестр не раскрывает живые
экземпляры блоков.

Циклические зависимости для команд watchdog/bridge/auth/file-manager устранены
через точки внедрения в `aios-console/src/control.rs` (`WatchdogControl`,
`BridgeControl`, `AuthProvider`, `FileManagerControl`). Бинарник `aios` реализует
первые три в `aios/src/console_control.rs` на своих дескрипторах:
`WatchdogRunner::from_watchdog` подключает поток мониторинга к тому же `Watchdog`,
который читает endpoint статуса bridge; `start_server_with_shutdown` позволяет
`bridge stop` корректно закрыть слушатель, а `bridge start` — запустить его
заново; `auth` вызывает реальный `AuthStore`. `WatchdogControl::reset` вызывает
`Watchdog::reset` на общем экземпляре (подключённый раннер остаётся подключённым),
а `wd reset` требует этот адаптер до увеличения ограниченного счётчика
перезапусков. Адаптер `FileManagerControl` реализован в
`aios-fm/src/console_control.rs` поверх живого `FileManager`: `jobs()` отображает
снимок заданий движка в текст консоли, а `cancel()` вызывает реальный токен
`Progress::cancel()`. `aios-tui` внедряет этот адаптер после запуска своего
`FileManager`, поэтому `console fm jobs` и `console fm cancel <id>` наблюдают и
отменяют реальные фоновые передачи. Адаптер `JobControl` реализован таблицей
заданий `aios-shell`: она общая с командами без префикса, `jobs()` возвращает живой
снимок, а `fg`/`bg` находят нужный id и отправляют `SIGCONT`, обновляя таблицу или
оставляя её без изменений. `aios-shell` внедряет этот адаптер в `HostBackend` в
`main`, поэтому канонические `console jobs`/`fg <id>`/`bg <id>` реальны. Без
внедрённого адаптера эти команды по-прежнему возвращают `unavailable`.

`aios-console/src/unavailable.rs` хранит точную причину для зарегистрированных
команд, у которых всё ещё нет реального хостового API, поэтому `ERR unavailable`
называет недостающую возможность, а не только сообщает "нет хостового бэкенда":
`jobs|fg|bg` требуют таблицу заданий, принадлежащую фронтенду, с OS-группами
процессов, сигналами и foreground TTY, которых нет в `aios-process-mgr` (адаптер
`JobControl` предоставляет её в `aios-shell`). `acl grant|revoke` и `ping` теперь
реальны: `aios-vfs` предоставляет path-scoped `PathAcl`, который `AiosVfs`
проверяет после каноникализации, а `aios-net::icmp::ping` отправляет настоящие
ICMPv4 echo-запросы (`IcmpSendEcho` в Windows, ICMP-датграмный/raw-сокет в Unix)
и сопоставляет ответы по identifier, sequence и контрольной сумме. `HostBackend`
делит один `PathAcl` с `open_vfs`, поэтому гранты консоли влияют на ту же политику,
которую проверяет `vfs`, а для ping используется `IcmpPingControl` по умолчанию —
тесты подменяют его адаптером-заглушкой. Хостовые `net recheck`, `net apply` и
`net adapters` теперь реальны:
`NetManager` запрашивает ОС (`netsh` в Windows, `ip addr` в Linux), обновляет
кэшированное состояние link/lease и выводит реальные строки адаптеров, а
`net apply` сохраняет конфиг перед этой проверкой. Хостовые команды без
внедрённого адаптера остаются `unavailable`, пока не появятся указанные реальные
API. `fm jobs|cancel` теперь реальны там, где внедрён живой
`FileManagerControl` (`aios-tui`), а `jobs|fg|bg` — там, где внедрён живой
`JobControl` (`aios-shell`); обычный `HostBackend` без этих адаптеров по-прежнему
сообщает `unavailable`. `state extract|restore` теперь реальны:
`BlockExecutor::extract_state` и `restore_state` снимают и восстанавливают снимок
живой линейной памяти исполняемого WASM-блока (тот же механизм миграции, что
использует `WasmLiveUpdateEngine`), а `HostBackend` хранит снимок в сессии, чтобы
`state restore <block>` записал его обратно в живой экземпляр. `KernelBackend`
использует те же имена команд `net`, но читает и меняет собственные настройки
ядра через `aios-console::net_text`; его `net recheck` вызывает настоящий
`crate::net::recheck()`, а не сообщает кэшированный вердикт.

`aios-kernel/src/console_backend.rs` реализует `KernelBackend` с зависимостью
`aios-console` без `std` (`default-features = false`). Он адаптирует существующие
точки входа ядра: генерируемый `help`, `echo`, `version`, `kernel info|ver`,
`faults stats|history|pending`, `fault resolve`,
`wifi status|sim|real|scan|connect|disconnect`,
`net show|config|status|lease|set|apply|validate|reset|adapters|recheck`,
`udp`, `http` и `socktest`.
Всё остальное возвращает `unavailable`. Баг ядерного `clear` исправлен: бэкенд
очищает `SHELL.history` и выдаёт явное `OutputControl::Clear`. Ядерный шелл
предоставляет бэкенд как `console <...>` через общий префикс без учёта регистра,
поэтому `CONSOLE VER` и `CoNsOlE echo MiXeD Case` попадают в бэкенд; старый
диспетчер обрабатывает все остальные команды и сохраняет регистрочувствительность
голых написаний. Это отдельная реализация, поскольку `aios-kernel` собирается
под `x86_64-unknown-none` и не может линковать `std`, `ratatui`, `tokio` или
`reqwest`.

Команды без префикса в TUI ядра тоже переведены на адаптеры.
`aios-kernel/src/tui.rs::execute_kernel_adapter` вызывает работающие без `std`
форматтеры из `aios-console/src/legacy.rs` для `help`, `ver`, `info`, `faults`,
`wifi` (`sim`/`real`), `echo`, `clear` и отката на неизвестную команду.
`KERNEL_HELP`, `KERNEL_UNKNOWN`, `KernelFaultLine` и `KernelFaultStats`
закрепляют точный старый текст; `KernelFaultStats` объединяет счётчики ошибок,
чтобы форматтер не превышал лимит аргументов clippy без подавления предупреждения.
`run_command_legacy` остаётся только эталоном паритета, а тесты сравнивают оба
пути побайтово. Команда `clear` без префикса теперь очищает историю шелла так же,
как `console clear`. `tabs`, `gui`, `tui`, `udp`, `http` и `socktest` остаются на
старом пути до появления реальных адаптеров; каноническое `console info` по-прежнему
неоднозначно.

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
    pub audio_devices: Vec<AudioDevice>,
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

#### Гибридная модель блоков (обзор)

AIOS использует гибридную модель: небольшой **вкомпилированный базовый набор** нативных блоков + **динамические дисковые блоки**.

- **Базовые блоки (вкомпилированные)**: ключевые блоки HAL/browser/Net-settings компилируются в бинарь `aios` и регистрируются при старте. Набор выбирается флагами Cargo: `base-hal`, `base-browser`, `base-net-settings`, `base-all` (по умолчанию `["webview","base-all"]`). Фабрики базовых блоков обёрнуты в `cfg(feature = "...")`, что позволяет сборкам урезать набор (`--no-default-features --features ...`); скрипты сборки live/initramfs при `--no-default-features` добавляют `--features base-all`, сохраняя штатный базовый набор. Поверх выбора на этапе сборки **среда выполнения урезает набор под обнаруженный профиль** (`base_factories_for(total_ram_mb)`): вкомпилированный набор — верхняя граница, и блок с жёстким требованием к оборудованию не загружается, если машина ему не соответствует (v2.62.0 — ниже `BROWSER_MIN_RAM_MB` = 4096 Миб блок `browser` пропускается и пишется в лог).
- **Дисковые блоки (динамические)**: обнаруживаются в `AIOS_BLOCKS_DIR` в виде бандлов `.wasm` или `.bin` с сопутствующими метаданными (`block.toml` / `manifest.json`). Блоки `.wasm` исполняются через `aios-wasm` (Wasmtime), подключаются к `MessageRouter` и могут реализовывать IPC (ABI `handle_message`). Пэйлоады `.bin`/не-WASM **регистрируются только** (не исполняются). `DiskBlockReport` содержит `registered_only_count()` для их учёта.

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

#### Фабрика (`factory.rs`)

`BlockFactory` — создаёт вкомпилированный (нативный) блок, «базовую» половину гибридного загрузчика:
- `name()` / `version()` — ключ в реестре и версия манифеста
- `create(id)` → `Box<dyn StatefulBlock>` — создаёт блок, привязанный к его `BlockId`

`BlockFactoryRegistry` — упорядоченный каталог фабрик:
- `register(factory)` — отвергает дубликаты имён с `BlockAlreadyRegistered`
- `get(name)` / `contains(name)` / `names()` / `len()` / `is_empty()`
- `instantiate(name, id)` → `Box<dyn StatefulBlock>` — `BlockNotFound`, если фабрики нет

Конкретные фабрики живут у потребителя (`aios/src/blocks.rs`), поэтому этот крейт не тянет зависимости конкретных блоков.

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
- `aging_threshold_ms` (по умолчанию: 500мс) — время ожидания перед повышением приоритета; `with_aging_threshold` ограничивает ноль значением 1 мс, а `schedule_next()` защищает делитель
- `schedule_next()` вычисляет эффективный приоритет = базовый + (ожидание / порог), с ограничением +4 уровня
- Процессы с низким приоритетом, ожидающие 4x порога, повышаются до уровня Critical
- Все процессы оцениваются глобально (ранний выход по уровню очереди отсутствует)
- `force_preempt()` — принудительное истечение текущего временного слайса (для тестирования и ручного перепланирования)

**Взвешенный round-robin** (пропорциональные временные слайсы):
- `priority_weight()` отображает: Background=1, Low=2, Normal=3, High=4, Critical=5
- Временной слайс = `default_time_slice_ms.saturating_mul(priority_weight)` (Critical получает слайс в 5 раз больше Background; нулевой `with_time_slice` ограничивается 1 мс)
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
- `RecoveryDecision { pid, action }` (v2.45.0) — применение решения crash-диалога
  (`Rollback`/`Terminate`/`Resume`) через `Scheduler::resolve_fault`.

#### Отказоустойчивость и восстановление (v2.45.0)

Вместо глобальной остановки фолтнувшая задача замораживается, снимается снапшот
(только метаданные) и пользователю предъявляется решение
`Rollback / Terminate / Resume` — один и тот же контракт в host-рантайме и в
bare-metal ядре.

- `aios-core::fault` — `FaultKind { Panic, Hang, TaskFault }` (с `name()`),
  `RecoveryAction { Rollback, Terminate, Resume }` (с `name()`) и
  `FaultInfo { pid, name, kind, message, timestamp_ms }`. Переносят это две
  IPC-полезные нагрузки: `Payload::FaultReport(FaultInfo)` и
  `Payload::RecoveryDecision { pid, action }`
  (`CommandId::FaultReport = 0x0070`, `CommandId::RecoveryDecision = 0x0071`).
- `aios-process-mgr::fault` — `run_isolated` выполняет задачу реального потока
  под `catch_unwind` с тихим thread-local panic-хуком, поэтому паника
  превращается в `FaultInfo` (`panic_message`, `fault_for`), а не разворачивает
  планировщик. `TaskSnapshot` снимает метаданные задачи (pid, имя, приоритет,
  квота RAM, block id, crash count), а `restore_to` её переармирует.
- `Scheduler` — реальные потоки записывают сообщение паники
  (`Arc<Mutex<Option<String>>>`), `poll_faults()` сливает упавшие потоки в
  `fault_queue`, а `resolve_fault(pid, action)` применяет решение: `Rollback`
  восстанавливает снапшот и возвращает `Ready`, `Terminate` удаляет задачу,
  `Resume` возвращает в очередь. `check_real_threads` больше не сбрасывает
  задачу из `Crashed` обратно в `Ready`, а `kill_process` чистит снапшот и
  записи сбоев.
- `aios-gui::crash_dialog` — `CrashDialogState` — чистая машина состояний
  (`present`/`choose`/`take_outcome`/`show`), отделённая от отрисовки; тик
  потока `GuiRuntime` опрашивает `poll_faults()`, а `App` показывает модальный
  диалог, резолвя его через `resolve_fault` в следующем `update()`.

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

**Где используется (v2.61.0):** AI Console ядерного TUI переводит и исполняет интенты через мост. В standalone TUI (`aios-tui`) шины сообщений нет, поэтому команда оболочки `intent <запрос>` использует тот же движок **только для перевода**: она печатает распознанный интент, план, IPC-команду и целевой блок, а исполнение оставляет ядру.

### Фоновые задачи оболочки (v2.61.0)

Сетевые команды не должны блокировать TUI, поэтому оболочка передаёт их фоновому рабочему потоку, а результат печатает позже:

- **Standalone TUI** (`aios-tui`): в `DashboardState` два outbox — `TaskOutbox` (строки вывода задачи) и `FetchOutbox` (байты загруженного блока). Счётчик поколений (`task_gen`) позволяет главному циклу отбросить результат superseded-задачи; одновременно выполняется только одна задача.
- **Ядерный TUI** (`aios`): в `TuiApp` хранится `StoreOut`; `store_spawn()` запускает задачу на рантайме tokio, `store_poll()` забирает результат на следующем тике UI.

Оба TUI выполняют запрос каталога хранилища, загрузку блока и веб-поиск в рабочем потоке, поэтому цикл событий продолжает отрисовывать картинку, пока запрос в полёте.

### Панель управления (`aios/src/tui`)

TUI ядра на базе Ratatui с компоновкой из 7 вкладок по спецификации (бинарник `aios`), переоформленный в классическую двухпанельную форму Far Manager / Midnight Commander (v2.31.0):

**Зона полосы статуса** (сверху, 1 строка, заменяет старые заголовок и панель вкладок):
- Версия, имя активной вкладки, статус системы (`OK`/`SAFE`)
- Uptime, AI tier, RAM, статус моста (`ON`/`OFF`)
- Статус ссылки `sys:` (Wi-Fi подключён/серый, из снимка `aios_sys_control`)

**Основная двухпанельная зона** (55/45): активная вкладка рендерится слева, **журнал событий** — справа (цветовая кодировка: Красный=ошибка, Жёлтый=предупреждение, Циан=Bridge, серый=информация).

**Вкладка 1 — System & HW**: модель CPU, ядра/потоки, флаги AVX, имя GPU/VRAM, хранилище, AI Tier; шкала RAM; таблица устройств Hardware Inspector и лента hot-plug тостов.

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

**Зона приглашения**: `AIOS>_` для shell, `net>_` при сетевом вводе `key=value`.

**Зона клавиш F** (внизу, 1 строка): `1Help 2Tabs 3View 4Edit 5Copy 6Move 7Mkdir 8Del 9Sort 10Rescan`; `F10` делает ручной re-probe железа (`refresh_hw`), выход — `q`/`Ctrl+C`.

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
5. Создание `BlockRegistry` — подключение вкомпилированного **базового набора** через фабрику блоков (`hal`, `browser`, `net_settings`) в `MessageRouter`, затем обнаружение + исполнение блоков на диске из `AIOS_BLOCKS_DIR` **кроме safe mode** (`blocks::register_base_blocks` / `blocks::load_disk_blocks`)
6. Создание `Scheduler` — запуск 3 процессов (ai_orchestrator, io_handler, health_monitor)
7. Создание `Watchdog` — запуск потока heartbeat в фоне
8. Создание `EmbeddedContextStore` + `TelemetryStore` — для системной телеметрии
9. Создание `SafeModeShell` — для команд восстановления в безопасном режиме
10. Запуск моста HTTP/WS на заданном порту **кроме safe mode**
11. Вход в raw-режим crossterm + альтернативный экран
12. Цикл событий: опрос клавиатуры, перерисовка панели, синхронизация состояния watchdog
13. Восстановление терминала при выходе

Горячие клавиши: `q`/`Ctrl+C`=Выход, `1-7`/`Alt+1-7`=Вкладка (Alt работает при вводе в Shell/URL/AI/сеть), `Tab`/`F1`/`?`=следующая вкладка/справка, `W`=Запуск рабочего стола GUI, `Space`=пауза журнала, Blocks: `r`=перезапуск `k`=выгрузка `l`=загрузка, Web: `g`/`Enter`/`j`/`k`/`o`/`u`/`d`/`PageUp`/`PageDown`/`b`/`B`/`n`/`Esc`, Network: `n`=редактор `g`=показать JSON `s`=обновить хранилище, ↑/↓=История Shell

### Гибридный загрузчик блоков (`aios/src/blocks.rs`)

Ядро блочное целиком: всё выше планировщика — это *блок*, и отличается только источник кода.

- **Базовый набор (вкомпилирован):** `base_factories()` строит `BlockFactoryRegistry` (`aios-block-mgr::factory`) с одной `BlockFactory` на каждый вкомпилированный блок (`hal`, `browser`, `net_settings`). `base_factories_for(total_ram_mb)` — вариант с учётом профиля (`aios/src/blocks.rs`): он исключает блоки, для которых `base_block_skip_reason(name, total_ram_mb)` вернул `Some` (сейчас — только `browser` при RAM ниже 4096 Миб), и пишет в лог `AIOS: base block '<name>' skipped for this profile: <reason>`; `register_base_blocks()` (`None` = полный набор) создаёт каждый блок через фабрику, регистрирует его в `BlockRegistry` (payload `native:<name>:<version>`) и подключает его `handle_message` в `MessageRouter`. Базовые блоки — настоящие `StatefulBlock`, поэтому диспетчеризуются по IPC точно так же, как дисковый блок: `hal` теперь живой `HalBlock`, а не заглушка.
- **Дисковый набор (выбирает установщик):** `load_disk_blocks()` вызывает `BlockRegistry::boot_discover` по `AIOS_BLOCKS_DIR`, затем исполняет каждый обнаруженный payload, начинающийся с сигнатуры WebAssembly (`\0asm`), через `BlockExecutor` (bring-up `init`/`start`), блок за блоком. Блок, экспортирующий `handle_message`, дополнительно подключается в `MessageRouter`, поэтому диспетчеризуется по IPC точно так же, как базовый блок (об этом сообщает `DiskBlockInfo.routed`). Payload `.bin`/плейсхолдеры только регистрируются и попадают в отчёт как `registered_only`. При загрузке присутствуют только блоки, скопированные установщиком в `/blocks`, поэтому стоимость bring-up пропорциональна установленному набору.
- `BlockExecutor` удерживается живым в `OrchestratorState` как `Arc<Mutex<BlockExecutor>>` (обработчики роутера обязаны быть `Send`), чтобы исполненные дисковые блоки оставались резидентными и обслуживали свой IPC-трафик; базовые блоки живут внутри своих обработчиков роутера.
- `ipc_bus` и `scheduler` остаются плейсхолдерами только ради топологии: они есть в графе зависимостей, но не обслуживаются по IPC.

### Safe Mode

`--safe-mode` загружает минимальное восстанавливаемое ядро: дисковые блоки не обнаруживаются и не исполняются (только вкомпилированный базовый набор), мост не запускается; в шапке показывается `SAFE MODE`. Ядро, планировщик, watchdog, LLM-движок, TUI и Shell остаются доступны.

### Нативный браузер WebView (`aios-webview`)

TUI не может отображать настоящие веб-страницы (нет движка CSS/JS), поэтому полноценный браузер — это **нативное окно** на `wry` (WebView2 на Windows, WebKitGTK на Linux, WKWebView на macOS) поверх событийного цикла `winit`:

- `WebBrowser::open(target)` запускает **единственный фоновый поток на весь процесс** при первом обращении и переиспользует его дальше (GTK может инициализироваться ровно в одном потоке): первый вызов блокируется только до появления окна, последующие показывают и навигируют существующее окно; дескриптор никогда не блокирует вызывающего
- Команды (`navigate`, `back`, `forward`, `close`) отправляются в событийный цикл браузера через `winit::EventLoopProxy` и применяются асинхронно; `close` скрывает окно, а поток движка продолжает работать (следующий `open` снова покажет окно)
- **Интеграция циклом на Linux (v2.43.0):** поток движка вызывает `gtk::init()` до построения event loop, включает `EventLoopBuilderExtX11::with_any_thread(true)`, подсевает записываемые `HOME`/`XDG_*`/`DISPLAY=:0` для headless live-бута и опустошает `gtk::events_pending()` из `about_to_wait` с опросом раз в 16 мс (`WaitUntil`) — X11-цикл winit не видит источники glib у GTK/WebKit (их собственное X-соединение, IPC с web-процессом), без прокачки окно замёрзло бы
- Куки и хранилище сохраняются между перезапусками через `WebContext` с профильным каталогом (`AIOS_DATA_DIR`/`aios/webview` или системный каталог данных)
- `resolve_target()` реализует правило омнибокса, общее с TUI: полный `http(s)`-URL → как есть, голый хост → `https://`, всё остальное → запрос DuckDuckGo (HTML-версия)
- Модуль `launcher` находит бинарник `aios-gui` (рядом с текущим исполняемым файлом, затем PATH) и запускает рабочий стол GUI

**Гейтинг фич (v2.32.0):** движок wry/winit живёт в опциональной фиче `webview` (включена по умолчанию; на Linux она тянет крейт `gtk`). `launcher` и `resolve_target` — на чистом `std` и компилируются всегда, поэтому клавиша `W` в TUI работает даже при выключенном движке; под фичей остаются только `B`/`n` (ядерный `WebBrowser`) и окно Native Browser на рабочем столе. Упразднённый Linux Live-образ собирал `aios`/`aios-gui` с `--no-default-features`, но унификация фич через `aios-tui → aios-webview` снова включала webview: удалённый сейчас `live/build.sh` проверял FATAL-ом, что оба основных бинарника слинкованы с `libwebkit2gtk-4.1`, потому что браузер с флешки был обязательной поставкой того пути. В нативном UEFI-образе такой проверки нет.

### Рабочий стол GUI (`aios-gui`)

Нативный **рабочий стол** на egui/eframe вместо дашборда с вкладками: тёмный фон, нижняя панель задач с `☰ Пуск` слева и системным траем справа. Встроенные функции открываются в отдельных перемещаемых окнах egui, внешние программы запускаются из общего каталога в `src/voice.rs`. Горячая клавиша `W` в обоих TUI запускает рабочий стол GUI через `aios_webview::launcher::launch_gui()`. При сборке `--no-default-features` пункт Native Browser и клавиша `F7` отсутствуют.

- **Меню «Пуск»** (`src/desktop.rs::start_entries()`): список с поиском, отсортированный по заголовку без учёта регистра; объединяет встроенные функции, внешние программы из `voice::Program` и системные действия `Обновить всё` / `Приостановить всё`.
- **Окна и панель задач**: `DesktopState`/`DesktopWindow` хранят открытые окна, фокус и свёрнутость; кнопка на панели задач переводит окно в фокус или сворачивает его, повторное открытие функции переводит существующее окно в фокус. `F1`-`F9` открывают соответствующее окно, `J`/`K` перемещают выделение в активном окне Blocks/Store, `Escape` закрывает «Пуск».
- **Настоящие системные действия**: `Обновить всё` заново опрашивает систему, процессы, блоки, железо и сетевые настройки; `Приостановить всё` приостанавливает все отслеживаемые планировщиком процессы. Прежние заглушки Quick Actions, только писавшие в журнал, убраны.
- **System Dashboard (F1)**: карточки статистики (RAM, блоки, процессы, watchdog), панель системы (CPU/GPU/хранилище/HW Tier), спарклайн RAM, распределение приоритетов, таблица процессов (PID, Имя, Приоритет, Состояние, RAM, CPU ms, Сбои) с Обновить/Убить/Приостановить/Возобновить, журнал активности
- **WASM Blocks (F2)**: таблица блоков + Обновить / Загрузить (2-шаговый диалог) / Выгрузить / Горячая замена; под панелью инструментов выводится строка сводки загрузчика (`base [...] | disk [...] | failed N | live M`), совпадающая со сводкой TUI
- **AI Studio (F3)**: асинхронный чат с LLM — список сообщений, потоковые ответы (живая жёлтая частичная строка), отправка по Enter (фокус сохраняется), слэш-команды `/help /status /clear /history /system /model /backend /key /temp /tokens /preset /save /load`, строка статуса (бэкенд/модель/температура/токены/busy); запросы стримятся через фоновую tokio-задачу, интерфейс остаётся отзывчивым. Чат автосохраняется в общий `AIOS_DATA_DIR/chat.jsonl`, а шаблоны `/preset` — в `AIOS_DATA_DIR/presets.json` (те же файлы, что и у AI Console TUI)
- **App Store (F4)**: каталог с поиском и действиями Установить/Обновить/Удалить
- **Network Settings (F5)**: форма hostname/port/таймауты/private-access/DNS/user-agent с Save (частичное JSON-обновление по IPC в `net_settings`) и Reset, плюс живой JSON-предпросмотр
- **Deps (F6)**: сводка графа зависимостей, цепочка загрузки, таблица зависит/зависят от
- **Native Browser (F7)**: омнибокс, Back/Forward, переключатель Open/Close, управляющие нативным окном `aios-webview`; первая навигация автоматически открывает браузер
- **Files (F8)**: двухпанельный файловый менеджер (`aios-fm`) на `aios-vfs` — панель инструментов (Refresh/Switch/Sort/Up/Mkdir/Rename/View/Copy/Move/Delete, HOST r/w), панели с выбором кликом/двойным кликом, модальный диалог mkdir/rename, сворачиваемое AI-превью, живой прогресс задач и показ capability ACL
- **Трай панели задач**: AI Tier, `RAM использовано/всего`, живой счётчик IPC-пакетов, состояние Wi-Fi/SSID, активная пара раскладок, батарея/температура CPU и бэкенд LLM (`local`/`cloud`) с обновлением раз в 2 с, плюс кнопка голосового ввода и поле ручной команды

#### Живое ядро для GUI (`aios-gui/src/runtime.rs`)

Начиная с v2.31.2 каждое окно рабочего стола питается **реальным ядром** (`GuiRuntime`) вместо заглушек:

- Настоящий `Scheduler` (round-robin, aging 5000 мс, слайс 100 мс) — таблица процессов, RAM и IPC-управление (`kill/suspend/resume`).
- Настоящий `BlockRegistry` с нативными модулями (`hal`, `ipc_bus`, `scheduler`, `browser`) и пополняемый `boot_discover` из `AIOS_BLOCKS_DIR` (по умолчанию `./blocks`). Окно WASM Blocks грузит/выгружает/заменяет записи реестра; Hot-Swap выполняет `LiveUpdateEngine::perform_swap` (SHA-256, запись отката) по установленной копии в store, затем `registry.swap_binary`, с панелью истории свопов.
- **Отчёт загрузчика (v2.59.0)**: `GuiRuntime::new` снимает `GuiBlockLoadReport` (засеянный базовый набор, обнаруженные дисковые блоки, неудачные обнаружения); `GuiBlockLoadReport::summary_line` форматирует `base [...] | disk [...] | failed N | live M`, а `AiosApp::block_load_summary` выводит её в окне Blocks. В отличие от TUI, рантайм GUI держит блоки резидентными, не исполняя WebAssembly, поэтому `live` — это число текущих резидентных блоков реестра (`disk` = обнаруженные/зарегистрированные), а не счётчик исполненных блоков.
- Настоящий `Watchdog` получает heartbeat раз в секунду (секрет `aios_gui_secret`); индикатор `WD:` отражает `WatchdogState` (`Monitoring → 0`, `Suspended → 1`, `Recovering → 2`, иначе safe-mode).
- Общая шина `IpcBus` (4096 слотов), которую дренирует счётчик IPC на панели задач. `GuiRuntime::start()` больше не создаёт демо-процессы `ai_orchestrator`, `io_handler`, `health_monitor` и `telemetry_agg`, а вспомогательная функция `ipc_generator` удалена: пакеты приходят только из явных/ручных действий, а таблица процессов стартует пустой. Сама шина остаётся доступной для реального IPC.
- `HotReloader` пересканирует каталог блоков при каждом опросе UI и горячо перезагружает изменённые бинарники.
- Локальный `BlockMarketplace` (репозиторий `official`, 4 внутренних предложения) + `BlockInstaller` пишут настоящие `<name>_<version>.wasm`/sidecar-манифесты; установка/обновление/удаление App Store пишут и удаляют реальные файлы store и записи реестра. Тестовые сборки перенаправляют каталог блоков в уникальную временную директорию, чтобы юнит-тесты не трогали репозиторий.
- Все фоновые **системные** потоки стартуют только в `start_runtime()` (вызывается из `aios-gui/src/main.rs`), поэтому `AiosApp::new` остаётся без побочных эффектов и потоков для тестов. `GuiRuntime::start()` идемпотентен и запускает только тик планировщика и heartbeat watchdog; таблица процессов остаётся пустой, пока процесс не запущен явно. В `aios-gui` 33 проходящих теста: отсортированность/уникальность/полнота пунктов «Пуск», фокус и сворачивание окон, отсутствие демо-процессов, завершение явного процесса, ручные IPC-пакеты и навигация по окнам рабочего стола.

#### Аутентификация WebSocket-телеметрии и политика входа моста (`aios-bridge`)

Авторизация по умолчанию запрещена для всех маршрутов `/api/` и `/ws/`. `/ws/telemetry` принимает токен в `?token=<session>` или заголовке `Authorization: Bearer` и проверяет его через `AuthStore` при каждом повышении соединения — обхода «нет зарегистрированных пользователей» больше нет. Публичная регистрация выключена, если не задано `AIOS_BRIDGE_ALLOW_SIGNUP=true`; CORS отключён, если `AIOS_CORS_ORIGIN` не задан как точный корректный origin. Отравленные общие мьютексы восстанавливаются с журналированием вместо паники. Запросы intent/workflow/LLM проверяют лимиты промптов, пакетов и `max_tokens`; бинарники блоков читаются только из `AIOS_BLOCKS_DIR` (проверка канонического пути). `HotSwap` моста выполняет live-update swap (`LiveUpdateEngine::perform_swap`), затем `registry.swap_binary`; `WorkflowExecution` последовательно запускает вложенные интенты через тот же путь `execute_intent`.

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

**Команды:** `ps`/`processes`/`list`, `blocks`/`ls`, `spawn`, `kill <pid>`,
`load`, `unload <id>`, `status`/`info`, `logs`/`log`, `restart`/`recover`,
`help`/`?`, `exit`/`quit`/`q`, а также обработка неизвестной команды с сохранением регистра.

Точный вывод каждой ветки закреплён в тестах `safe_mode.rs`. Отдельный TUI теперь
направляет команды без префикса `ps`/`processes`/`list`, `blocks`/`ls`,
`status`/`info`, `exit`/`quit`/`q`, `help`/`?`, `logs`/`log`, `kill`, `spawn`,
`load`, `unload`, `restart` и `recover` через адаптеры `aios-console` и проверяет
побайтовое совпадение с `SafeModeShell`, включая побочные эффекты и общие записи
журнала событий; эти двадцать написаний — мигрированные команды безопасного
режима.
Адаптер статуса получает число крашей планировщика из общего `HostBackend`,
поскольку канонический статус его не содержит. Адаптер выхода преобразует
`OutputControl::Exit` в старую строку, не изменяя флаг завершения команд без
префикса, поэтому только `console exit` разрывает event loop. Адаптер справки
отображает генерируемую общую справку в старой формулировке и оставляет
TUI-дополнения отдельным блоком фронтенда. Журнальный seam разделяет
`Arc<Mutex<Vec<String>>>` между `SafeModeShell` и `HostBackend`; консоль читает
его только для `logs`, а адаптеры мутаций добавляют успешные legacy-события,
поэтому канонические записи консоли никогда не смешиваются с событиями
безопасного режима. Адаптер перезапуска использует общий счётчик с каноническим
`wd reset` через `HostBackend`, поэтому статус и диагностика видят одинаковый
ограниченный счётчик; при успехе он записывает legacy-событие и сохраняет точную
ошибку лимита без префикса `Failed:`. Последний откат для неизвестной команды
теперь использует `legacy::unknown_text`, поэтому TUI печатает точную старую
строку `Error: Unknown command: '<input>'. Type 'help' for available commands.`
без выполнения `SafeModeShell`; ввод сохраняется дословно, включая регистр и
пробелы. `SafeModeShell` по-прежнему принадлежит TUI для действий вне шелла
(например, клавиша `x` для статуса) и для прямых тестов паритета со старым
диспетчером, но ни одна команда без префикса больше не выполняется через него.
Каноническое `console info` остаётся неоднозначным. В этом же
TUI `Scheduler` и `BlockRegistry` разделяются с `HostBackend`, поэтому
`console ps` и `console block list` видят состояние безопасного режима без
изменения вывода старых команд.

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
- Доступ к линейной памяти для IPC: `has_func`, `memory_size`, `grow_memory`, `write_linear_memory`, `read_linear_memory`

### Диспетчеризация IPC (`executor.rs`)

`BlockExecutor` связывает дисковые WebAssembly-блоки с шиной IPC ядра:

- **ABI:** блок, желающий принимать пакеты, экспортирует
  `handle_message(ptr: i32, len: i32) -> i64`. Хост размещает bincode-сериализованный
  `IpcPacket` в линейной памяти блока по адресу `ptr`, вызывает экспорт и трактует
  упакованный результат `i64` как `(resp_ptr << 32) | resp_len`; `resp_len == 0`
  означает «нет ответа», иначе хост читает `resp_len` байт по `resp_ptr` и
  bincode-десериализует ответ.
- `dispatch_wasm_packet(id, &IpcPacket)` / `dispatch_wasm(id, &[u8])` выполняют
  round trip; `has_export` / `routes_ipc` проверяют наличие точки входа в модуле.
- Буфер запроса выделяется через необязательный экспорт гостя `aios_alloc`, если он
  есть, иначе — в выращенной хостом памяти (`StoreState::ipc_scratch`), которая
  никогда не пересекается с собственными данными блока; `MAX_IPC_REQUEST` /
  `MAX_IPC_RESPONSE` (4 МиБ) ограничивают оба направления.
- Обработчик, регистрируемый в `aios/src/blocks.rs`, захватывает общий исполнитель
  под мьютекс и вызывает `dispatch_wasm_packet`, поэтому трафик в стиле
  `call_block_func` доходит до дисковых блоков через тот же `MessageRouter`, что и
  базовый набор.

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

## Автоматическое обеспечение оборудованием и хранилище драйверов (`aios-autohal`)

`aios-autohal` реализует Master Brief «Hardware Auto-Provisioning & Driver Store»: определяет устройства по fingerprint, скачивает/адаптирует открытые драйверы в изолированные `.wasm`-модули, выдаёт Capability-токены и инстанцирует их в песочнице Wasmtime, кэширует локально и отображает всё со 100% паритетом TUI/GUI.

### Fingerprint и манифест (`fingerprint.rs`, `manifest.rs`)

- `HardwareFingerprint { bus, vendor_id, device_id, class_code, serial_or_acpi }` с `BusType` (USB/PCI/Bluetooth/ACPI/NVMe); `extract_fingerprints(&HardwareProfile)` превращает слепки `aios-hal` (USB VID/PID, PCI class/subclass в `class_code`, класс NVMe-масс-хранилища) в ключи поиска (`usb.046d.0825`) и id драйверов (`driver.usb.046d.0825`).
- `DriverManifest` — JSON-схема (id, name, version, `supported_hardware`, `required_capabilities`, `hash_sha256`, `entry_point`) с сопоставлением `can_serve(fp)` (точное или wildcard-совпадение шины) и строгой валидацией (неизвестные права отклоняются, плохие хэши отклоняются). `DriverSource`: Redox Tree / Linux Core / Custom Store / Builtin / Generic.

### Каталог и загрузчик (`catalog.rs`, `fetcher.rs`)

- `catalog.rs` — офлайн-каталог встроенных драйверов (`BuiltinDriver` с исходниками WAT) плюс Generic Fallback Driver (`GENERIC_FALLBACK_ID`, без прав).
- Конвейер `DriverFetcher`: builtin-каталог → реестр custom store (`{root}/drivers/{id}/driver.{json,wasm}`) → Redox Tree → зеркало Linux Core (`index.json`). Возвращает `FetchedDriver::Wasm` или `FetchedDriver::Source` (C/Rust); SHA-256 проверяется при каждой загрузке.

### Адаптер (`adapter.rs`)

`SourceAdapter` переписывает вызовы портов/MMIO (`inb/outb/inw/outw/readl/writel/ioread*`) на host-импорты `hal_*`, объявленные в WASI-преамбуле, и компилирует C/Rust в `wasm32-wasi`.

### Движок и self-healing (`engine.rs`)

`AutohalEngine` — асинхронный конвейер из 5 шагов: (1) детекция через HAL event loop → (2) локальный поиск в `DriverStore` → (3) сетевой поиск/адаптация/компиляция → (4) проверка SHA-256 + выдача `CapabilityToken` + инстанцирование в Wasmtime → (5) кэширование и регистрация. `provision_blocking`/`provision_dedicated` обслуживают пути IPC/UI. **Self-healing:** после 3 сбоев подряд устройство автоматически переходит на Generic Fallback Driver с предупреждающим тостом (`record_failure` → `rollback_to_generic`); поддержаны `uninstall_driver` (generic защищён) и override прав на устройство (`set_cap_override`).

### Хранилище и реестр (`registry.rs`)

`DriverStore`/`DriverIndex` персистентно хранятся в `AIOS_DATA_DIR/drivers` (bincode/serde): маппинг fingerprint→driver, счётчики сбоев и override прав.

### Паритет UI (`ui_tui.rs`, `ui_gui.rs`)

- `HardwareInspector` (ratatui) — таблица устройств по шинам с бейджами статуса ([Active]/[Downloading...]/[Compiling]/[Generic]/[Failed]/[Rolled Back]), сводкой прав и лентой hot-plug тостов.
- `HardwarePanel` (egui) — таблица устройств (VID/PID, источник драйвера, цветные статусы), прогресс-бары скачивания/компиляции, интерактивная матрица прав безопасности (checkbox'ы) и кнопки [Update Driver]/[Rollback to Generic]/[Uninstall]/[Rescan].
- Оба рендерят одни и те же данные `DeviceView`/`Toast`, выдаваемые движком.

### Живая интеграция (kernel TUI + GUI)

- `aios-gui` (`AiosApp`): движок создаётся при старте (`hw_init`) с первичным проходом обеспечения; вкладка **Hardware & Drivers (F9)** (`tabs/hardware.rs`) рендерит общую панель `HardwarePanel` по обновляемым снимкам `hw_views`/`hw_toasts` и возвращает `GuiAction` (`Rescan`/`Update`/`Rollback`/`Uninstall`/`SetCapabilities`) движку через `apply_hw_actions`.
- Kernel `aios` (`TuiApp`): `hw_engine`/`hw_views`/`hw_toasts` инициализируются в `new()` через `init_hw_engine` (инертен в safe mode), обновляются каждый тик (`hw_refresh`) и пере-сканируются при аппаратном re-probe по `F10` (`refresh_hw`); `draw_system_tab` рендерит `HardwareInspector` третьим блоком вкладки System & HW.
- Таким образом, TUI и GUI показывают идентичное содержимое `DeviceView`/тостов для одной машины.

**Устойчивость TUI к сбоям (v2.31.1):** все `Mutex`-защёлки на пути отрисовки и в хендлерах (`app.state`, `bridge.registry`, `bridge.scheduler`, `ai_*`) проходят через отказоустойчивый к poisoning хелпер `locked()` (`app_state.rs`), поэтому паника фонового воркера бриджа, удерживавшего эти мьютексы, больше не может уронить рендерер. Дополнительно `run_tui` оборачивает цикл `run()` в `std::panic::catch_unwind` и безусловно восстанавливает терминал (выключение raw-режима, выход из alternate screen, возврат курсора), возвращая `Err("AIOS TUI crashed: …")` при пойманной панике вместо того, чтобы оставить консоль в raw-режиме.

### Цикл hot-plug событий (`hotplug.rs`)

`HotplugMonitor` — фоновый поток (по одному на UI-поверхность), который пере-определяет подключённый `HardwareProfile` и сравнивает набор отпечатков с предыдущим опросом, выдавая `HotplugEvent::Added/Removed` через канал `mpsc`. Первый скан лишь фиксирует базовую линию, поэтому старт тихий. Определение адаптивное: на Linux дешёвый сигнал (`cheap_signal`/`dir_signal_hash`) хэширует mtime каталогов `/sys/bus/{usb,pci,nvme}/devices` — они меняются только при подключении/отключении устройства — поэтому полный `detect()` запускается сразу при движении дерева устройств (частота `signal_poll_ms`, по умолчанию 250 мс), а `poll_ms` (по умолчанию 1000 мс) — страховочный интервал полного сканирования; платформы без такого сигнала используют фиксированную частоту `poll_ms`. Нативные push-события (Windows `WM_DEVICECHANGE`) коалесцируются не более чем в один полный скан за `poll_ms`, так что «флапающее» устройство не спамит пересканированием; `HardwareProfile::detect_usb` также пропускает PnP-артефакты `0000:0000`, и фантомные записи не доходят до дифа отпечатков. Монитор никогда не передаётся внутрь движка; каждый тик UI разгребает канал и применяет события в потоке UI: `Added` → `provision_blocking`, `Removed` → `AutohalEngine::remove_device` (выгружает WASM-инстанс и запись устройства, но сохраняет закэшированный драйвер, так что повторное подключение обеспечивается мгновенно). Монитор корректно останавливается в `Drop`.

- Kernel TUI: `TuiApp::hw_hotplug` запускается вместе с движком (инертен в safe mode); `hw_poll_hotplug` разгребает его каждый тик перед `hw_refresh`.
- GUI: `AiosApp::hw_hotplug` запускается вместе с движком; `hw_poll_hotplug` разгребает его в каждом кадре `update`.
- Периодический re-probe по `F10` остаётся ручным полным rescan.

---

## Сетевой стек (`aios-net`)

- **Крейт**: `aios-net` v1.0.0 — TCP/UDP блоки и ICMP-проверка доступности для сетевого взаимодействия
- **TCP** (`tcp.rs`): `TcpBlock`, `TcpConfig`, `TcpConnection`, `TcpMessage`, `TcpState` — mock state machine
- **Real TCP** (`real_tcp.rs`): `RealTcpBlock` — реальные `std::net::TcpListener`/`TcpStream` с неблокирующим accept, управлением соединениями, отправкой/получением, опциональным enforcement `CapabilityToken` (`CAP_NET_BIND` для `start_listening()`, `CAP_NET_CONNECT` для `connect()`)
- **UDP** (`udp.rs`): `UdpBlock`, `UdpConfig`, `UdpPacket`, `UdpState` — mock state machine
- **Real UDP** (`real_udp.rs`): `RealUdpBlock` — реальный `std::net::UdpSocket` с `bind()`, неблокирующим `send_to()`/`receive_from()`, broadcast через `SO_BROADCAST`
- **ICMP** (`icmp.rs`): `ping(host, count, timeout)` отправляет настоящие ICMPv4 echo-запросы (`IcmpSendEcho` в Windows, ICMP-датграмный/raw-сокет в Unix), проверяет контрольную сумму и identifier/sequence ответа и возвращает `PingReport` с RTT, TTL, размером и процентом потерь для каждого ответа; IPv6-адреса явно отклоняются
- Отслеживание соединений, отправка/получение через каналы, широковещание, статистика
- 57 тестов для mock + реального жизненного цикла TCP/UDP и ICMP echo/разбора/резолва

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
- **Реализации**: `AiosVfs` (песочница в локальной папке с проверкой `canonicalize_inside` и общим ACL путей через `with_path_acl()`) и `HostVfs` (реальные пути хоста, чтение/запись ограничены ACL-токенами `vfs:host:read` / `vfs:host:write`).
- **Операции** (`operations.rs`): `Progress` (атомарные счётчики байтов/файлов, `fraction()`, `pressure_fraction()`), `CancellationToken`, `total_bytes`, `copy_recursive`, `move_item`, `delete_item`, `read_head`, `read_at`.
- **Безопасность** (`security.rs`): `AclContext` — потокобезопасный набор capability-токенов (`Mutex<HashSet>`); `PathScope` (схема + нормализованный путь), `PathAclRule` и `PathAcl` с `grant`/`revoke`/`check`/`require`/`rules` для прав `vfs:read` и `vfs:write`; побеждает самая специфичная область, а запрет на потомке перекрывает разрешение на родителе. `AiosVfs` проверяет общий ACL после `canonicalize_inside(root, path)` при каждой операции чтения/записи.
- **AI-превью** (`ai_preview.rs`): `analyze_file(name, head)` → `AiPreview { title, headline, lines: Vec<(AiLineKind, String)> }`; разбирает WASM name-section, находит паники в логах, даёт подсказки по исходникам.
- 35 модульных тестов (отмена, байты WASM name-section, проверка путей, старшинство и применение ACL путей, copy/move/delete, превью).

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
- **Исполнители** (`executor.rs`): трейт `ProcessExecutor` (spawn/kill/set_priority/status/metrics/**extract_state**/**restore_state**). `MockProcessExecutor` — детерминированный, моделирует 16 GiB RAM для осмысленных долей нагрузки, засевает снимок состояния каждого процесса из `spec.payload`. `SchedulerProcessExecutor` — адаптер поверх реального `aios-process-mgr::scheduler::Scheduler`. Все общие блокировки планировщика/состояний восстанавливаются после отравления вместо паники. Снимки состояния — непрозрачные байты; исполнители хранят их по процессам, heartbeat-поток реплицирует их как контрольные точки, и `migrate` переносит их между узлами.
- **Конфигурация** (`config.rs`): `ClusterConfig` из переменных окружения `AIOS_CLUSTER_*` или JSON (`node_id`, `node_name`, `addr`, `tier`, `peers`, `heartbeat_ms`, `failover_threshold_ms`, `failover_respawn`, `strategy`, `checkpoint_ttl_ms`). Возвращает `None`, если кластеризация не запрошена.
- **Тесты**: 23 unit (протокол 6, транспорт 2, исполнитель 4, планировщик 9, конфиг 2) + 10 интеграционных (`tests/scheduling.rs`: двухузловой spawn/kill, round-robin, least-loaded, TCP loopback, failover-перезапуск, управление приоритетом, миграция процессов, миграция с переносом снимка состояния, пути ошибок миграции, ошибки неизвестного узла/без пиров) + 1 doc-тест.

---

## Маркетплейс (`aios-block-mgr`)

- `BlockMarketplace` — реестр блоков с управлением репозиториями
- `BlockMetadata` — имя, версия, описание, автор, sha256, теги
- `RepositoryEntry` — метаданные + статус (Available/Installed/UpdateAvailable/Deprecated)
- Публикация, поиск, установка, удаление, проверка обновлений
- 18 модульных тестов

---

## Архитектура тестирования

- **Модульные тесты**: 1354 теста в исходных файлах под `#[cfg(test)] mod tests` (базовый срез релиза v2.79.0)
- **Интеграционные тесты**: 217 тестов в целях `tests/` workspace (интеграционные, стресс, chaos, proptest) (базовый срез релиза v2.79.0)
- **Doc-тесты**: 1 тест в документации крейта
- **Базовый итог**: 1572 теста на момент релиза v2.79.0, все проходили, ноль предупреждений clippy
- **Актуальное состояние (повторная проверка v2.84.0)**: `cargo test --workspace` — **1824 пройдено / 0 упало в 100 наборах**; `cargo fmt --all -- --check` и `cargo clippy --workspace --all-targets -- -D warnings` проходят без замечаний (см. `docs/AUDIT.md` §2)

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
- **Веб-аутентификация** (v2.30.0, усилено после аудита): локальные аккаунты в `<AIOS_DATA_DIR>/users.json`, PBKDF2-HMAC-SHA256 (100 000 итераций) с солью на пользователя, constant-time проверки хэша и подписи, HMAC-SHA256 сессионные токены (TTL 12 ч), подписанные секретом из `aios_core::secrets` (`AIOS_AUTH_SECRET` → `<data_dir>/secrets/auth.key` → CSPRNG); пять неудачных входов блокируют аккаунт на 5 минут. Регистрация и логин используют общий `validate_credentials()` (имена 3–64 байта, пароли 12–1024 байта проверяются до хеширования), а учёт неудачных входов очищается и ограничен 10 000 записей. `require_auth` по умолчанию закрывает `/api/*` и `/ws/*`; CORS отключён или зафиксирован на `AIOS_CORS_ORIGIN`. Все встроенные в код секреты issuer/heartbeat заменены этим же resolver-ом.

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
  - `NetworkClient`: настраиваемые user-agent, таймаут, лимит редиректов; SSRF-политика — только HTTP(S), каждый hop резолвится через DNS и отклоняется для loopback/private/link-local/non-global адресов, явная цепочка редиректов, потоковое чтение тела с лимитом `max_response_bytes` (по умолчанию 8 МиБ). DNS-имена привязываются к проверенным адресам на время запроса через `reqwest::ClientBuilder::resolve_to_addrs`, чтобы повторное разрешение не могло выбрать приватный адрес. `BrowserConfig::allow_private_targets` (или `AIOS_BROWSER_ALLOW_PRIVATE`) включает доступ к локальным тестовым серверам. `BrowserConfig::for_untrusted()` отключает нативный headless-fallback и встроенные скрипты для URL от вызывающего, оставляя проверенный клиент единственным путём загрузки.
  - `Renderer`: DOM → markdown-подобный текст (заголовки `#`, ссылки `[text](url)`, списки `•`)
  - `Page`: `url`, `title`, `text_content`, `html`, `links: Vec<Link>`
  - `BrowserConfig`: `user_agent`, `timeout_secs`, `max_redirects`, `sandbox_enabled`, `headless_fallback` (включён по умолчанию), `allow_private_targets` (выключен по умолчанию), `max_response_bytes` (по умолчанию 8 МиБ)
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

### Фаза 25b: Встроенный JS-движок и состоятельная браузерная сессия (`aios-browser`, v2.39.0) — *ЗАВЕРШЕНО*
- **Конвейер исполнения JS** (`src/script.rs` + `src/script_prelude.js`): каждая загруженная страница проходит через встроенный чистый-Rust движок (boa 0.22)
  - Ноль host-функций: Rust инжектит `__dom` (дерево `DomNode` как JSON), `__meta` (url/title/user-agent/слушатели/хранилища) и `__console`, вычисляет прелоад, запускает скрипты страницы + userscripts, затем читает обратно `JSON.stringify(__dom/__meta/__console)` — нет поверхности FFI на каждую функцию моста
  - Свежий `boa_engine::Context` на страницу/`evaluate`: движок stateless между IPC-вызовами, детерминирован и защищён от утечек между страницами; `eval_js` воспроизводит сохранённый post-script HTML без повторного запуска скриптов
  - Преобразование JS → Rust считает массивом только конечный неотрицательный `length` и материализует не более 10 000 элементов (`MAX_SCRIPT_ARRAY_ITEMS`), поэтому подменённая длина не может зависнуть или исчерпать память задачи браузера
  - Прелоад даёт: гидрацию DOM (getElementById/querySelector(All), мутации textContent/innerHTML, createElement/appendChild/insertBefore/remove, рефлексивные атрибуты включая `src`/`href`, хранение addEventListener + реальный dispatchEvent, конструкторы `Event`/`CustomEvent`), консоль-шим с префиксами уровней и лимитом, `location` (сеттер href отражает навигацию в `__meta.nav`, выполняется движком — фаза 25d), `localStorage`/`sessionStorage` поверх `__meta`, шимы `alert`/`confirm`/`prompt`, шимы `URL`/`URLSearchParams`, host-backed `fetch`/`XMLHttpRequest`, который складывает запросы в `globalThis.__net` для обслуживания движком (фаза 25e), очередь таймеров `__aiosFlushTimers` с одним flush после всех скриптов (каждый callback ≤ 1× за flush)
  - Известные ограничения (задокументированы, заметка уходит в консоль): `document.write` — no-op, нет event loop для пользовательского ввода (клики/клавиши не рассылаются), события `error` хранятся, но не диспатчатся автоматически
- **Сериализатор DOM → HTML** (`src/serialize.rs`): round-trip с `HtmlParser::parse`, VOID/RAW_TEXT-наборы элементов, экранирование текста/атрибутов, санитизация имён атрибутов, созданных скриптами
- **`BrowserSession`** (`src/session.rs`): движок + стеки back/forward на вкладку + закладки + userscripts
  - `normalize_url` (добавляет `https://` без схемы; экспортируется для переиспользования в UI), `back`/`forward`/`reload`/`new_tab`/`close_tab`/`select_tab`, add/remove закладок с JSON-персистентностью, `eval_js`, `SessionSnapshot` для передачи состояния при live-update (страницы перезапрашиваются по требованию; старые блобы `(config, state)` тоже восстанавливаются)
- **IPC `BrowserBlock`** (`src/block.rs`): прежняя поверхность (`browse`, `open_native`, `browser_status`, `HealthCheck`) плюс `back`, `forward`, `reload`, `new_tab`, `close_tab`, `select_tab`, `session_status`, `add_bookmark`, `list_bookmarks`, `remove_bookmark`, `eval_js`, `add_user_script`; `browse` идёт через сессию, поэтому история накапливается; извлечение состояния = `(BrowserConfig, BlockState, SessionSnapshot)` с fallback-восстановлением старого 2-кортежа
- **Интеграция в UI**: вкладка Web в TUI `aios` — `J` открывает панель JS-консоли (число скриптов, ошибки красным, строки консоли), подсказка в футере, после загрузки лог `scripts=N errors=M console=K`, `web_navigate` использует `normalize_url`; `aios-tui::fetch_url` гонит полный `BrowserEngine` (fetch → парсинг → скрипты) на выделенном однониточном рантайме

### Фаза 25d: Итерация движка 2 — жизненный цикл, динамические скрипты, навигация (`aios-browser`, v2.40.0) — *ЗАВЕРШЕНО*
- **Синтетические события жизненного цикла** (`src/script_prelude.js`, `src/script.rs`)
  - Единое ядро диспетчеризации для узлов/документа/окна: не-перечисляемые `__ev`-мешки слушателей, `fireAt(obj, evt)` выполняет список `addEventListener`, затем свойство `on<type>`, исключения обработчиков ловятся в консоль как `[js] <type> handler error: message + stack`
  - Конструкторы `Event`/`CustomEvent` + работающий `dispatchEvent` на каждом узле, `document` и `window`; `window.addEventListener/removeEventListener`
  - `ScriptEngine::fire_dom_content_loaded` / `fire_load` (вызывает `run_page_scripts`): `readystatechange` → `DOMContentLoaded` на документе, затем после сброса таймеров `readystatechange` → `load` на окне → `load` на документе; `document.readyState` проходит `loading → interactive → complete` (`evaluate` помечает сохранённые страницы `"complete"` перед сниппетом)
  - Порядок фаз внутри `run_page_scripts`: скрипты страницы + userscripts → settle → `DOMContentLoaded` → settle → `__aiosFlushTimers` → settle → `load` → settle → чтение обратно (`settle` = раунды динамических скриптов, затем обслуживание `fetch`/XHR, повторяется пока ответы порождают новую работу — фаза 25e)
- **Исполнение динамических `<script>`** (`src/engine.rs`, `src/script.rs`)
  - `ScriptEngine::collect_dynamic_scripts` обходит живой `__dom` в поиске не-помеченных элементов script, созданных кодом страницы (`__aiosMarkScripts` предварительно помечает пакет из парсера в `ScriptEngine::new`); инлайн-код выполняется на месте, `src` резолвится относительно URL страницы и грузится через `NetworkClient`; каждый элемент помечается при сборе (возвращается один раз); не-JS типы пропускаются с заметкой в консоль, элементы `type="module"` собираются с флагом `module` и исполняются как модули
  - Лимиты, общие для всех фаз одной страницы: `MAX_DYNAMIC_ROUNDS = 8`, `MAX_DYNAMIC_SCRIPTS = 20`, заметки в консоли при достижении обоих
- **Цикл навигации** (`src/engine.rs`, `src/html_parser.rs`)
  - `HtmlParser::extract_meta_refresh` (`MetaRefresh { delay_secs, target }`) парсит `<meta http-equiv="refresh">`; навигирует только `delay_secs == 0`, отложенные refresh получают `[nav] meta refresh in Ns ignored`
  - `build_page` зациклен: прогнать конвейер → взять навигацию из скрипта (`__meta.nav`) или meta refresh → резолвить относительно текущего URL (`Url::join`) → защита same-URL петли / не-http схем / `MAX_NAV_HOPS = 5` → fetch → повтор; каждый шаг пишет `[nav]`-строки, ошибка загрузки сохраняет текущую страницу с ошибкой `[nav] failed to load`
  - Возвращаемый `Page::url` — финальный URL цепочки; headless-fallback и извлечение ссылок используют его
- **`unsafe impl Send for ScriptEngine`** (документировано на impl): контекст boa закономерно пересекает асинхронные fetch'и, а `BrowserBlock::block_on` требует `Send`-фьючерсы; движок остаётся привязан к одной логической задаче (опросы задач никогда не перекрываются)
- **Покрытие тестами v2.40.0**: +23 теста (lifecycle/dynamic/навигация в engine, включая локальный HTTP тест-сервер для end-to-end цепочек `location.href`/meta-refresh/hop-limit; unit-тесты fire/readyState/dispatch/collect в script)

### Фаза 25e: Итерация движка 3 — ES-модули и host-backed fetch/XMLHttpRequest (`aios-browser`, v2.44.0) — *ЗАВЕРШЕНО*
- **ES-модули** (`src/script.rs`): `ScriptEngine::run_module` разбирает исходник через `Module::parse(Source, None, context)` из boa и исполняет его через `load_link_evaluate`; синтаксис `export`/`import` принимается, top-level код модуля исполняется в контексте страницы, а отклонённое исполнение наблюдается через JS-пробы `.then(onFulfilled, onRejected)` (`PromiseState` в boa — crate-private) и репортится как ошибка скрипта. По этому пути идут и статические, и динамически собранные `type="module"`-скрипты.
- **Двухфазный сетевой мост** (`src/script_prelude.js`, `src/script.rs`, `src/network.rs`, `src/types.rs`): prelude складывает каждый `fetch`/`XMLHttpRequest` в `globalThis.__net` (id → `{resolve, reject, xhr}` плюс вычерпываемый список `requests`); движок вызывает `pending_requests()`, выполняет каждый запрос через `NetworkClient::request(method, url, body)` (GET/POST/PUT/DELETE/HEAD) и резолвит отложенный promise/колбэк через `resolve_request(id, FetchResponse)` — дизайн без host-функций сохранён. `FetchResponse { status, status_text, url, body, headers, error }` трактует не-2xx как данные; транспортный сбой выставляет `error` и отклоняет/шлёт `error`.
  - `fetch` резолвится в `Response` (`ok`/`status`/`statusText`/`headers`/`text()`/`json()`/`clone`); `XMLHttpRequest` поддерживает `open`/`setRequestHeader`/`send`/`addEventListener` и шлёт `readystatechange` → `load` → `loadend` (или `error`). Относительные URL запросов резолвятся относительно URL страницы через шim `URL`.
  - `settle` чередует `run_dynamic_rounds` и `resolve_fetches`, пока ответы порождают новую работу; лимиты `MAX_FETCH_ROUNDS = 8`, `MAX_FETCHES = 32` (запросы сверх лимита отклоняются, чтобы promise не завис).
- **Покрытие тестами v2.44.0**: +4 теста в крейте (исполнение модуля с `export`, цикл очередь+резолв для fetch, отклонение при транспортной ошибке, доставка `onload` у XHR) — всего 106 в `aios-browser`.

### Фаза 28: Headless Daemon (`aios-daemon`) — *ЗАВЕРШЕНО*
- **Крейт `aios-daemon`**:
  - Бинарник `aiosd`: headless-сервер с той же инициализацией, что и `aios-tui`, без терминала
  - Загружает встроенные блоки (hal, ipc_bus, scheduler) и дисковые блоки из `AIOS_BLOCKS_DIR`
    (пропускается, если задана `AIOS_SAFE_MODE`)
  - Открывает persistent store (`redb`) в `AIOS_DATA_DIR/context.redb`
  - Запускает системные процессы (ai_orchestrator, io_handler, health_monitor)
  - Запускает поток heartbeat watchdog
  - Фоновый цикл: heartbeat (процессы, RAM, watchdog) каждые 10с, сохранение телеметрии каждые 60с
  - Минимальные зависимости: без ratatui, crossterm, egui, wasmtime
  - Конфигурация через `AIOS_DATA_DIR`, `AIOS_BLOCKS_DIR`, `AIOS_MOCK_PROFILE`, `AIOS_SAFE_MODE`, `RUST_LOG`
- **Headless-режим `aios-tui`**:
  - Флаг `--headless` и `AIOS_HEADLESS=1`: пропускает TUI-инициализацию, работает в фоне
- **Docker**:
  - Dockerfile собирает `aios` (ядро, `--daemon` — headless-режим) и `aios-daemon` (`aiosd`), копирует SPA `aios-studio` в `/app/aios-studio` (её отдаёт мост через `ServeDir`), объявляет `EXPOSE 8080` и задаёт `AIOS_BRIDGE_PORT`
  - `docker-compose.yml`: сервис `aios` публикует `8080:8080`; профиль `safe-mode` стартует ядро с `--safe-mode` и `AIOS_SAFE_MODE=1`
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
- **Паритет единой консоли** — `HostBackend` реализует те же семантики `store sign` и `store publish`, что и TUI. `store sign` вычисляет SHA-256, пишет `{name}_{version}.json` рядом с входным файлом, используя только последний компонент пути имени и версии, и выводит публичный ключ Ed25519; `store publish` включает подпись при наличии `--key` или `AIOS_STORE_SIGNING_KEY`. Обе команды снова зарегистрированы как подкоманды и больше не сообщают об отложенном статусе.
- **`store trust <source> [--key <public_hex>] [--clear]`** — задаёт/очищает `StoreSource.trusted_public_keys` из шелла `aios-tui` (hex-ключ валидируется как настоящий публичный ключ Ed25519), сохраняется через `StoreManager::save_config` в конфиг источников; `store sources` показывает число доверенных ключей на источник

### Интеграция `net_settings` в ядро (Фаза 41)
- `net_settings` регистрируется в реестре блоков ядра при загрузке (`aios/src/orchestrator.rs`), обработчик подключается к `MessageRouter`; итоговый `BlockId` доступен как `OrchestratorState::net_block_id`
- Горячая клавиша `n` в TUI ядра (`aios`) открывает режим ввода пар `key=value`; по `Enter` токены преобразуются в частичное JSON-обновление и уходят как `net_set` через IPC, возвращённый JSON конфигурации выводится в панель событий
- Команда `store publish <file.wasm> [name] [version]` (доступна в `aios-tui` и через `console ...` в `aios-console`) вычисляет SHA-256 файла, кодирует wasm в base64 и отправляет `StorePublishRequest` в `POST /api/v1/store/publish` (порт моста из `AIOS_BRIDGE_PORT`, по умолчанию `8080`); имя по умолчанию — имя файла без расширения, версия — `1.0.0`. `HostBackend` собирает тот же JSON через `serde_json::json!` без зависимости от `aios-bridge`, а `HostBackend::set_publish_url` подменяет адрес в тестах
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
| W | Запуск рабочего стола AIOS GUI (`aios-gui`); ищется по `PATH` или рядом с исполняемым файлом `aios` (на live доступен как сессия `DISPLAY=:0` X/Xorg) |
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
4. Инициализация BlockRegistry — подключение вкомпилированного базового набора (`hal`, `browser`, `net_settings`) через фабрику блоков в `MessageRouter`, затем обнаружение + исполнение дисковых блоков из `AIOS_BLOCKS_DIR` (по умолчанию `./blocks`) кроме safe mode
5. Настройка AccessControl + Watchdog
6. Инициализация LLM Engine (облачный бэкенд по умолчанию)
7. Инициализация WASM Executor (BlockExecutor)
8. Создание BridgeContext со всеми подсистемами
9. Запуск Bridge HTTP-сервера (axum, порт из `--bridge-port`, по умолчанию 8080) — пропуск в safe mode
10. Запуск цикла событий TUI (или цикла демона)

Браузер работает «из коробки» на новом компьютере: для запуска не нужны ни конфиг-файлы, ни установленный браузер, ни сеть — блок активен в топологии, доступен по IPC, а вкладка Web (`B`/`n`/омнибокс) открывает любой URL в нативном WebView.

## Слой 7: Live USB развёртывание — упразднён (v2.77.0)

Linux Live USB-путь (каталог `live/` с `build.sh`, `init.rs`, `sfs-up.sh`,
`aios-install`, `grub.cfg` и др., а также скрипты `scripts/build-live-iso.*` и
`scripts/fix-wsl2.ps1`) удалён в v2.77.0. Загрузка на железо теперь использует
только нативный UEFI-only образ GPT/ESP, описанный в разделе Native UEFI Boot
Chain: `aios-image` собирает его, `aios-install` записывает на диск. Инструменты
initramfs `aios-init` из слоя 8 сохранены как отдельный компонент, но больше не
встроены в поставляемый Linux Live ISO. Прежние вехи с упоминанием `live/`
остались в журнале изменений как история.

## Слой 8: `aios-init` и автономный initramfs (`aios-init/`, `build_initramfs.sh`)

### Обзор
`aios-init` — это выделенный Rust `/init` для initramfs AIOS: статически скомпилированный (`x86_64-unknown-linux-musl`) супервизор PID 1, который монтирует базовые VFS, передаёт управление блоку AIOS и никогда не паникует — тем самым устраняя `Kernel panic: No working init found`.

### Обязанности (порядок загрузки)
1. Установка обработчиков `sigaction`: SIGTERM/SIGINT/SIGHUP выставляют флаг завершения; SIGCHLD (`SA_NOCLDSTOP`) будит цикл сборки зомби; SIGPIPE игнорируется.
2. Монтирование базовых VFS: `/proc` (proc), `/sys` (sysfs), `/dev` (devtmpfs; если недоступна — `mknod` для `/dev/console` 5:1, `/dev/null` 1:3, `/dev/tty` 5:0), `/tmp` (tmpfs), `/run` (tmpfs).
3. Открытие `/dev/console` и `dup2` в fd 0/1/2, чтобы все журналы загрузки шли на консоль.
4. `set_user_env()`: полный `PATH` BusyBox/Alpine, `TERM=linux`, `DISPLAY=:0`, `AIOS_DATA_DIR=/tmp/.aios`, `XDG_RUNTIME_DIR=/run/aios` — чтобы наблюдаемый дашборд и ядерные блоки получали готовое окружение юзерспейса.
5. `bring_userspace()` (вариант live): spawn + супервизия `/bin/sh /sfs-up.sh` — загрузка модулей storage/loop/squashfs/ext4 и GPU (DRM в порядке зависимостей, фолбэки fbdev/vesa), поиск и bind-mount юзерспейса Alpine из `aios.squashfs`, подъём сети (`rcS`) и eudev, затем запуск **Xorg на `:0`/VT7**. Если юзерспейс недоступен, система остаётся в TUI-only-режиме на консоли (прежнее поведение).
6. Запуск и супервизия `/system/aios-core` (запасной `/installer`), до 3 перезапусков (задержка 300 мс) при падении.
7. Сборка каждого потомка через `waitpid(-1, WNOHANG)`, чтобы осиротевшие «внуки» не становились вечными зомби.
8. По SIGTERM/SIGINT: передача сигнала блоку, ожидание до 5 с, затем SIGKILL.
9. Аварийный запасной вариант: если блок отсутствует или перезапуски исчерпаны — запуск спасательного шелла (`/bin/sh` → `/bin/busybox sh` → `/bin/ash`); если шелла нет — idle-цикл со сборкой зомби, без паники ядра.

### Сборка initramfs
```
rustup target add x86_64-unknown-linux-musl
./build_initramfs.sh                     # initramfs.cpio.gz (ядерный TUI + init)
./build_initramfs.sh --keep-rootfs       # оставить стейджинг-каталог rootfs/
./build_initramfs.sh --no-aios-core      # без ядерного бинарника aios (только спасательный шелл)
BUSYBOX_PATH=/usr/bin/busybox.static ./build_initramfs.sh   # + спасательный шелл
```
Скрипт выполняет `cargo build --release --target x86_64-unknown-linux-musl` для `aios-init` и (если не задан `--no-aios-core`/`SKIP_AIOS_CORE=1`) `cargo build -p aios --release --target x86_64-unknown-linux-musl --no-default-features` для реального ядерного TUI. Он формирует структуру в `rootfs/`, копирует `aios-init` в `/init` и `aios` в `/system/aios-core`, затем упаковывает `find . | cpio --null -ov --format=newc | gzip -9`. Защита очистки отказывается удалять путь за пределами каталога скрипта; `--keep-rootfs` сохраняет стейджинг-каталог. Когда присутствует `/system/aios-core`, `aios-init` сразу загружает полный ядерный TUI; спасательный шелл остаётся только запасным вариантом (v2.13.0).

### Вариант Live-образа (исторический, удалён в v2.77.0)
Удалённый `live/build.sh` по умолчанию упаковывал aios-init-initramfs: `aios-init` как `/init`, бинарник `aios` как `/system/aios-core`, busybox только как спасательный шелл + скрипт `sfs-up.sh`, копируемый в initramfs (подъём юзерспейса и X.org в `bring_userspace`) — ядро грузилось сразу в полный юзерспейс с ядерным TUI на консоли и GUI-дашбордом по `W`; отдельное GRUB-меню с записями `init=/init console=tty0` записывалось на шаге [5]. Прежний вариант busybox-initramfs (монтирование squashfs + `switch_root`, `init.rs`) хранился за флагом-отключением `USE_BUSYBOX_INIT=1` (v2.14.0; в v2.13.0 aios-init включался опционально через `USE_AIOS_INIT=1`). Этот путь больше не поставляется; см. пометку об упразднённом слое 7 выше.

### Параметры ядра Linux
- GRUB: `menuentry "AIOS" { linux /boot/vmlinuz init=/init console=tty0 quiet; initrd /boot/initramfs.cpio.gz; }`
- Syslinux: `LABEL aios\n KERNEL /boot/vmlinuz\n APPEND init=/init console=tty0 quiet\n INITRD /boot/initramfs.cpio.gz`
- `init=/init` указывает ядру запускать этот бинарник вместо `/sbin/init`; `console=tty0` направляет вывод ядра и init на основную консоль.



## Системная управляющая плоскость (`aios-sys-control`) - v2.29.0
Общая системная управляющая плоскость для TUI, GUI, bridge и интеграционных тестов:
- `net_manager` — управление Wi-Fi поверх двух бэкендов (`WifiBackend::Simulated` эфир с 3 сетями / `WifiBackend::Host` через `netsh wlan`), wire-кодек DHCP Discover/Request/Ack, сохраняемый lease, `SysControlHub`, агрегирующий сеть + раскладку + питание в сериализуемый `SysStatusSnapshot` с `status_line()`.
- `input_i18n` — менеджер раскладок EN/RU: хоткеи (`alt_shift` по умолчанию), переопределения по окнам, `status_segment()` с активной парой первой.
- `power_mgr` — сэмплинг батареи/температуры (`Mock` или host sysfs) + термал-гувернёр с гистерезисом 80/70 °C: при нагреве LLM уходит в облако (`Groq`), при остывании возвращается локально.
- `keyring` — `KeyringVault`: секреты AES-256-GCM в redb, canary-проверка мастер-пароля, PBKDF2-HMAC-SHA256 120k раундов, ключ шифрования привязан к TEE, ре-ключ мастера.

Ядро в v2.29.0 получает вехи 3 и 4 на bare-metal-ветке: `sched.rs` — round-robin-планировщик по тикам PIT (переключение каждые TIMER_HZ/4 тиков) с frame-copy переключением контекста внутри ISR таймера плюс ring-0 worker; `user.rs` — две ring-3 демо-программы (сырые машинные циклы над `int 0x80`) с user-мапингом на CODE_BASE 0x40000000 / STACK_TOP 0x7F000000; `ipc.rs` — почтовые ящики на pid (MAX_PID=4 × MAILBOX_DEPTH=16) за вентилем `int 0x80` c DPL-3 (флаги IDT 0xEE), заголовок пакета зеркалит `aios_core::ipc_protocol`; proof-строки `[stats] switches/sent/recv` раз в 5 с, проверяемые `scripts/qemu-smoke.ps1`. С v2.38.11 адресат `SYS_SEND` — это **слот планировщика** получателя (worker = 1, задачи = 2/3/4 в порядке спавна, плюс GUI-клиент = 5 с v2.38.25): программы шлют на константы слотов-собеседников `SLOT_A`/`SLOT_B`, а не на `PID_*`, а счётчики ящиков/`mailbox_len` питают экранный дашборд. Вентиль вырос до шести вызовов (`SYS_SEND`/`SYS_RECV`/`SYS_WRITE`/`SYS_GETPID`/`SYS_SLEEP`/`SYS_GUI`); в v2.38.25 `MAX_TASKS`/`MAX_PID` подняты с 5 до 6 под четвёртую ring-3 демо-задачу, а `SYS_GUI` (id 6) — точка входа в оконный сервер (CREATE/PRESENT/GET_EVENT, см. пункт `gui`). С v2.46.0 вентиль обслуживает также `SYS_PING` (id 7, кооперативный heartbeat сторожа зависаний) и `SYS_FAULT_POLL` (id 8, вычерпывает ленту событий фолтов ядра для ring-3-монитора — freestanding-двойник host-IPC `FaultReport`). С v2.49.0 он обслуживает семейство `SYS_SOCK_*` (id 9–18: `SYS_SOCKET`/`BIND`/`CONNECT`/`SEND`/`RECV`/`CLOSE`/`STATUS`/`LISTEN`/`ACCEPT` — последние два с v2.50.0, и `SYS_SOCK_ADDR`, v2.51.0, отдающий ring-3 собственный привязанный IPv4) — ring-3-лицо сокетного слоя ядра; каждый буфер-аргумент проверяется на всю длину через `memory::translate_user` (единственный гейт указателей пользователя для `int 0x80`), затем копируется побайтово `user_read`/`user_write`, а некорректные буферы возвращают `u64::MAX`; каждый вызов дополнительно проверяет владение сокетом (`sock::owner(fd) == pid`). С v2.51.0 пятая ring-3 демо-задача (программа E) упражняет всё семейство из userspace через обратную петлю — UDP-пинг-понг и, с v2.52.0, круг TCP `listen`/`connect`/`accept`/эхо за цикл (`sock-e ok` / `sock-t ok`; см. пункт `user`).
## Собственная цепочка загрузки UEFI (`aios-boot`, `aios-bootinfo`, `aios-config`, `aios-wasmhost`, `aios-easylang`, `aios-image`, `aios-bootstore`, `aios-storectl`, `aios-install`, `aios-sbctl`) — v2.76.0 (Secure Boot с v2.79.0)

AIOS грузится на голом железе без Linux, Windows и сторонних загрузчиков:
`aios-boot` — собственное UEFI-приложение, `aios-bootinfo` — контракт
загрузчика и ядра, `aios-image` — сборщик дискового образа на машине сборки,
а `aios-install` — копирование проверенного образа на диск. Только UEFI
(GPT + ESP, путь BIOS/El Torito в v1 не нужен).

- `aios-boot` (`boot/aios-boot`, таргет `x86_64-unknown-uefi`, `uefi` 0.41 с
  `alloc`/`global_allocator`/`panic_handler`) собирается в один PE32+-образ
  `\EFI\BOOT\BOOTX64.EFI` (~64 КиБ). Крейт разделён так, что большая часть
  тестируется на хосте:
  - `config` — разбирает `\aios\boot.conf` (ключи `kernel`, `config`,
    `block`) и находит drop-in-файлы в `\aios\config\*.conf` и
    `\aios\blocks\*.wasm` (`BootConfig::discover`): только простые имена
    файлов, фильтр по расширению, дедупликация с явными записями
    ASCII-сравнением без учёта регистра (ESP — это FAT32) и сортировка по
    имени, чтобы загрузка была детерминированной;
  - `elf` — проверяет и загружает статический `ET_EXEC`-образ ядра
    (сегменты `PT_LOAD`, `p_offset`/`p_vaddr`/`p_memsz`, страницы `.bss`
    заливаются нулями);
  - `paging` — собственные `FramePool` и 4-уровневые таблицы: образ ядра по
    `0xffff_ffff_8000_0000` в PML4[511], HHDM (`0xffff_8000_0000_0000`) в
    PML4[256], куча/MMIO ядра в PML4[510], загрузочный стек 64 КиБ
    (`0xffff_fe00_0000_0000`) в PML4[508]; маппинги по 2 МиБ и 4 КиБ с
    `PTE_NX`/`PTE_PCD`;
  - `BootError` — общий enum ошибок всех шагов загрузчика.
  Только для UEFI: `main` (GOP-фреймбуфер, `ExitBootServices`, заполнение
  `BootInfo`), `framebuffer`, `serial` (COM1) и `runtime` (libcall `wcslen`,
  который LLVM генерирует для `CStr16::from_ptr`).
- Передача управления: финальные `mov cr3` + `jmp` выполняются из
  identity-mapped stub-страницы 4 КиБ. Она загружает таблицы страниц ядра,
  ставит `rsp = BOOT_STACK_TOP - 8` (выравнивание входа SysV, которого ждёт
  ядро) и прыгает в `_start` с `rdi = HHDM_BASE + phys(BootInfo)`; `cr3`,
  `cli` и прыжок — последнее, что видит окружение прошивки.
- `aios-bootinfo` (`boot/aios-bootinfo`, `no_std`, используется обеими
  сторонами): `#[repr(C)] BootInfo { magic = AIOSBOOT, version = 1, size,
  hhdm_offset, kernel_phys/virt/size, rsdp, framebuffer,
  memory_region_count, module_count, memory_regions[128], modules[64] }`.
  Фиксированная раскладка, 8800 байт, без указателей; `memory_regions()` и
  `modules()` возвращают корректные префиксы, `is_valid()` заново проверяет
  магию, версию и размер.
- `aios-image` (`boot/aios-image`, чистый Rust, без `mkfs.vfat`/`diskpart`):
  собирает загрузчик и ядро, стейджит `boot/assets` в `boot/out/assets`,
  компилирует каждый `aios/easylang/*.easy` в `aios/blocks/<stem>.wasm` (M3.3)
  и убирает исходники с ESP, затем пишет GPT-диск с ESP на 320 МиБ
  (кластеры 4 КиБ, 2 FAT, 32 зарезервированных сектора, длинные имена VFAT
  плюс сгенерированные псевдонимы 8.3 вроде `EMPTY~1.WAS`) и заново
  разбирает MBR, заголовок GPT, boot-сектор FAT32 и обязательные пути
  (`/EFI/BOOT/BOOTX64.EFI`, `/aios/kernel.elf`, `/aios/config/system.conf`,
  `/aios/blocks/empty.wasm`, `/aios/blocks/hello.wasm`). `--headless` загружает образ в QEMU/OVMF,
  сохраняет COM1 в `boot/out/serial.log` и печатает его; `--no-run` только
  собирает и проверяет.
  Начиная с M4.2 начальное хранилище сначала материализуется реальными файлами
  в дереве стейджа через `aios-storectl::init`, и только потом `copy_tree`
  переносит их в FAT; `verify_assets` проверяет это дерево через
  `aios-storectl::verify`. Флаг `--reuse-assets` сохраняет уже отредактированное
  `boot/out/assets` вместо повторного стейджа — так сценарии обновления и
  отката в QEMU пересобирают образ, не теряя новое поколение. С M5.2 флаг
  `--sign-keys DIR` подписывает загрузчик через `aios-sbctl` до копирования на
  ESP (подписанная копия остаётся в `boot/out/BOOTX64.signed.efi` и
  проверяется), `--enroll-out DIR` создаёт payload'ы PK/KEK/db `.auth`
  (требует `--sign-keys`), `--secure-fw` выбирает код прошивки OVMF с Secure
  Boot, а `--ovmf-vars PATH` — шаблон NVRAM с внесёнными ключами.
- `aios-sbctl` (`boot/aios-sbctl`, `std`, отдельный крейт, исключён из
  workspace, v2.79.0, M5.2): хостовая утилита подписи с командами `keygen`,
  `sign`, `verify` и `enroll`. Для ключей RSA, сертификатов X.509, подписи
  PKCS#7 и проверки CMS она вызывает `openssl`; внешних Rust-зависимостей и
  `sbsign` нет. Подпись PE/COFF следует соглашению edk2/`sbsign`:
  `eContentType` = `SPC_INDIRECT_DATA_OBJID`, хеш покрывает тело
  `SpcIndirectDataContent` без внешнего SEQUENCE, `eContent` содержит всё
  DER-значение, а блоб ставится в таблицу сертификатов PE. Для внесения ключей
  создаются буферы `EFI_VARIABLE_AUTHENTICATION_2` с 16-байтовым `EFI_TIME`
  GMT, `WIN_CERTIFICATE` с `wCertificateType = WIN_CERT_TYPE_EFI_GUID (0x0ef1)`
  (значение `0x0002` из PE заставляет EDK2 отвергнуть PK с
  `EFI_SECURITY_VIOLATION`), сертификатом `EFI_CERT_TYPE_PKCS7_GUID` и
  отсоединённой подписью CMS по
  `(VariableName, VendorGuid, Attributes, TimeStamp, Payload)` в сериализации
  edk2; полезная нагрузка — `EFI_SIGNATURE_LIST` с `EFI_CERT_X509_GUID` и
  DER-сертификатом. 16 юнит-тестов покрывают разбор DER/PE, формат подписи и
  сериализацию переменных.
- Инвариант выравнивания стеков: вершина любого стека ядра должна быть
  выровнена на 16 байт. Статики `[u8; N]` имеют выравнивание 1, а
  `core::fmt::Arguments` помечает свою форму «статическая `&str`» битом
  `args & 1`; нечётный адрес массива аргументов декодируется как строка
  размером в эксабайты. `sched::stack_top()` округляет вершины стеков вниз до
  `STACK_ALIGN`, а `spawn_kernel`/`spawn_user` проверяют инвариант.
- `aios-config` (`boot/aios-config`, `no_std`, без аллокаций, v2.70.0): единый
  ридер `key = value` для обеих сторон — комментарии `#`, ключи после trim из
  ASCII-букв/цифр/`. _ -`, необязательные кавычки `"`/`'`, пустое значение
  допустимо, некорректная строка пропускается, повтор ключа — last-one-wins.
  `Config` только заимствует текст и разбирает его лениво
  (`entries`/`get`/`get_u32`/`get_bool`/`count`/`duplicates`), а
  `parse_u32`/`parse_bool` декодируют отдельное значение (десятичное или hex
  `0x`, `true/yes/on/1` и `false/no/off/0`) — ядро никогда не копирует и не
  аллоцирует файл настройки.
- Слои настройки (M2, v2.70.0): `boot.conf` только запускает загрузку;
  каждый остальной `\aios\config\*.conf` находится автоматически и передаётся
  как `MODULE_CONFIG`, а каждый `\aios\blocks\*.wasm` — как `MODULE_BLOCK`.
  Модуль `config` ядра (`aios-kernel/src/config.rs`) разбирает каждый слой на
  месте через HHDM, логирует `[cfg] layer '<name>' = N key(s)`, каждую пару
  `key = "value" (layer, effective|informational)` и дубли ключей, а поиск идёт
  от последнего слоя к первому — поэтому drop-in вроде `config/net.conf`
  переопределяет базовый `system.conf` без его правки. `KeyPolicy` и
  `policy(key)` задают политику: `Effective` только у `blocks.<name>.rights` и
  `blocks.<name>.fuel` (их потребляет хост WASM); `system.*`, `boot.*`, `net.*`
  и неизвестные ключи — `Informational`: они разбираются, проверяются и
  логируются с источником, но не являются настройками времени выполнения.
  `validate_informational()` проверяет `boot.debug`/`net.dhcp` как булевы и
  `net.mtu` как положительное целое, предупреждает о некорректных значениях и
  считает их в `Summary.invalid_values` вместе с числом слоёв и ключей.
  Устаревшие `system.version`, `scheduler.tick_hz` и `memory.heap_min_kib`
  удалены в v2.79.0. Таймер, куча и версия намеренно не имеют ключей:
  `interrupts::TIMER_HZ`, `heap::HEAP_SIZE` и `aios-kernel/src/version.rs`
  (`SEMVER` = `env!("CARGO_PKG_VERSION")`, `KERNEL` =
  `concat!("AIOS kernel v", env!("CARGO_PKG_VERSION"))`) — compile-time факты
  из `aios-kernel/Cargo.toml`, единственного источника версии; загрузочный лог
  печатает фактические значения таймера/кучи и строку конфига с этими
  константами. Штатные шаблоны
  `boot/assets/aios/config/{system,net,blocks}.conf` несут те же комментарии о
  политике, а их копии в `boot/out/assets/aios/config/` идентичны по SHA-256;
  `net.conf` и `blocks.conf` намеренно не перечислены в `boot.conf` — это
  доказательство работы drop-in.
- `aios-wasmhost` (`boot/aios-wasmhost`, `no_std` + `alloc`, v2.71.0, M3.1):
  хост WebAssembly в песочнице для `MODULE_BLOCK`. `module` разбирает и
  статически проверяет бинарник на месте (без копирования): сигнатуры только
  `i32`, до 8 параметров и 1 результата, одна линейная память до 16 страниц
  (1 MiB), не более 16 импортов, 64 функций, 16 экспортов и 16 сегментов
  данных; проверяются каждый опкод, глубина ветвлений, индексы локальных
  переменных и функций, баланс `if`/`else`, а секции таблиц, глобалов,
  элементов и data-count отвергаются, а не игнорируются. `cap` — вентиль
  capability: `Rights` (`log`, `exit`, `clock`, `yield`, с тем же написанием,
  что в `*.conf`) и `Token { block, rights, fuel }`. `interp` исполняет
  проверенное тело без рекурсии по стеку Rust: фиксированные кадры вызовов
  глубиной 16, стек значений на 96 слотов, 64 локальных переменных и 64 кадра
  управления; каждая инструкция тратит единицу топлива (`aios.log` —
  `1 + len/64`), а каждое чтение/запись памяти проверяется по объявленным
  страницам. Единственная поверхность хоста — `Env` (`log`, `now_ms`,
  `idle`) и импорты ABI `aios.log(ptr, len)`, `aios.exit(code)`,
  `aios.now_ms() -> i32`, `aios.yield()`; токен без нужного права превращает
  вызов в `Trap::Denied`. У крейта нет зависимости от железа: 42 юнит-теста и
  один doc-тест покрывают парсер, валидатор, права, арифметику, локальные
  переменные, циклы/ветвления, вызовы, память, топливо, start/exit и все
  ловушки, поэтому ядро может на него опираться ещё до подключения в M3.2.
- Запуск блоков в ядре (`aios-kernel/src/blocks.rs`, v2.72.0, M3.2): вызывается
  один раз из `_start` шагом 6, сразу после `heap online` и до GDT/IDT и
  планировщика, поэтому блок исполняется в одном потоке и его ничто не
  вытесняет. Для каждого `MODULE_BLOCK` он выводит имя из имени модуля
  (`blocks/hello.wasm` → `hello`), читает `blocks.<stem>.rights` (через запятую
  `log`/`exit`/`clock`/`yield`, отсутствие = нет доступа к хосту) и
  `blocks.<stem>.fuel` (отсутствие = `DEFAULT_FUEL`) через слой `config`,
  разбирает образ на месте через HHDM, отвергает блок, чьи
  `required_rights()` не покрыты токеном, выделяет в куче ядра один
  `Vec<u8>` размером `pages × 64 КиБ`, инстанцирует его (сегменты данных и
  start-функция) и вызывает экспорт `run` или `_start` без аргументов.
  `Env` реализует `SerialEnv`: `aios.log` превращается в
  `[serial] [block] <name>: <text>`, `aios.now_ms` читает `interrupts::TICKS`
  с частотой `TIMER_HZ`. На каждый блок пишется строка-доказательство
  (`ran: entry 'run' (N page(s))`, `used X of Y instruction(s)`),
  `Trap::Exited` считается нормальной остановкой, skip и fail считаются
  отдельно, и плохой блок никогда не останавливает ядро. Штатный образ
  доказывает всю цепочку блоком `blocks/hello.wasm` (найден автопоиском, не
  указан в `boot.conf`), который печатает `hello from aios-wasmhost`, а
  политику задаёт найденный `config/blocks.conf`.
- `aios-easylang` (`boot/aios-easylang`, библиотека `no_std` + `alloc` и
  host-CLI, v2.73.0, M3.3): компилятор EasyLang Core — исходного языка блоков.
  Это императивный DSL над `i32`: `let`, присваивание, `log "текст"`, `exit`,
  `yield`, `if`/`else`, `while`, `and`/`or`/`not`, сравнения и арифметика,
  `now_ms()`, `true`/`false`, комментарии `#` и разделители newline/`;`.
  `lexer` и `parser` строят AST с позициями; `codegen` делает два прохода
  (анализ разрешает имена, выбирает импорты и упаковывает строки; эмиссия
  пишет секции) и нацелен ровно на подмножество `aios-wasmhost`: один экспорт
  `run`, только реально использованные host-импорты (в порядке `log`, `exit`,
  `now_ms`, `yield`), не более 16 переменных на функцию, `while` через
  `block`/`loop`, `and`/`or` с нормализацией в 0/1 и все строковые литералы в
  одном data-сегменте на одной странице. Ошибка компиляции содержит
  `line:column`. Справочник — [`docs/EASYLANG.ru.md`](EASYLANG.ru.md). Бинарь
  `aios-easylang` — тот же компилятор для командной строки
  (`aios-easylang hello.easy [-o out.wasm] [--check]`). 21 юнит-тест
  компилирует, валидирует и исполняет программы через `aios-wasmhost`.
- `aios-bootstore` (`boot/aios-bootstore`, `no_std` без `alloc`, v2.75.0,
  M4.1/M4.2): формат постоянного хранилища и политика коммита, общие для
  загрузчика и сборщика образа. Внутри нет ни одного обращения к файловой
  системе, поэтому весь контракт тестируется на хосте (26 тестов). Хранилище
  лежит в `/aios/store`:

  ```text
  /aios/store/manifest.a    копия манифеста A
  /aios/store/manifest.b    копия манифеста B
  /aios/store/slot0/header  запись коммита слота 0
  /aios/store/slot0/...     payload-файлы слота 0
  /aios/store/slot1/header  запись коммита слота 1
  /aios/store/slot1/...     payload-файлы слота 1
  ```

  - `manifest` — фиксированная little-endian структура на 1000 байт: заголовок
    на 40 байт (магия `AIMS`, версия, размер заголовка, `sequence`,
    `generation`, `active_slot`, `previous_slot`, число слотов,
    `entry_count`) и 16 записей по 60 байт (имя модуля до 48 байт с NUL-
    дополнением вроде `blocks/hello.wasm`, 32-битный размер, CRC-32, kind,
    slot, flags). CRC32 покрывает весь буфер, поле контрольной суммы при этом
    считается нулевым; имена проверяются (относительные, без `..`/`\`/NUL) и
    обязаны быть уникальными без учёта регистра.
  - `slot` — заголовок на 32 байта на слот: магия `AISL`, версия, размер,
    `SlotState` (`Empty`/`Staging`/`Active`/`Retired`/`Bad`), `sequence`,
    `manifest_crc` и `entry_count`, плюс собственный CRC32. Заголовок
    «подходит», только если он bootable, несёт sequence и CRC выбранного
    манифеста и считает ровно те записи манифеста, что лежат в его слоте.
    Привязка по CRC не даёт заголовку поручиться за другое поколение
    манифеста.
  - `Store::load(copy_a, copy_b)` выбирает валидную копию с наибольшим
    `sequence`; единственная валидная копия побеждает независимо от sequence,
    а при равенстве предпочтение отдаётся копии A. `stale_copy()` всегда
    возвращает копию, которую обязан заменить писатель (меньший sequence или
    невалидную), поэтому прерванная запись не может уничтожить новейший
    манифест.
  - `UpdatePlan` (из `plan_update`/`plan_rollback`) несёт байты нового
    манифеста, целевой слот и копию для перезаписи. `staging_header`,
    `active_header` и `retired_header` выводят точные заголовки слотов, а
    `plan_update` сохраняет записи ещё активного слота в манифесте, чтобы
    последующий откат мог указать на них без перечитывания payload-файлов.
  - Порядок коммита (задокументирован в корне крейта): payload-файлы →
    заголовок `Staging` → заголовок `Active` → устаревшая копия манифеста →
    заголовок `Retired` для старого слота. Падение до записи манифеста
    оставляет валидным старое поколение; падение после — новое.
- Интеграция хранилища (M4.1):
  - `aios-image` пишет начальное поколение при сборке образа: слот 0 получает
    все стейдженные `*.wasm`-блоки, слот 1 остаётся пустым под первое живое
    обновление, обе копии манифеста идентичны с sequence 1, у слота 0
    заголовок `Active`, у слота 1 — `Empty`. Затем образ читает хранилище
    обратно (манифест, заголовок слота 0, CRC и число записей) до того, как
    образ будет принят.
  - `aios-boot` читает `manifest.a`/`manifest.b`, выбирает новейшую копию,
    пробует сначала активный слот, затем `previous_slot` (путь отката), и
    проверяет заголовок слота плюс размер и CRC32 каждого payload-файла.
    Поколение принимается или отвергается целиком, поэтому частично
    скопированный слот никогда не запускается. При валидном хранилище оно
    заменяет автопоиск в `/aios/blocks` (хранилище — авторитетный полный
    набор, поэтому смешать версии невозможно) и логирует `+N from store`;
    записи `block = ...` из `boot.conf` сохраняются. Отсутствующее или
    повреждённое хранилище — не ошибка загрузки: загрузчик логирует причину и
    откатывается на drop-in-каталог.
  - Штатный образ QEMU/OVMF доказывает цепочку: `aios-image: verified store
    sequence 1, slot0 header 2 entries`, `aios-boot: store manifest sequence 1
    (copy 0), active slot 0, previous slot 1`, `store slot 0 verified at
    sequence 1 (2 block(s))`, `modules = 5 (3 conf, 2 wasm; ... +1 from store)`,
    а блок по-прежнему печатает `hello from aios-wasmhost`.
- `aios-storectl` (`boot/aios-storectl`, `std`, отдельный крейт, v2.75.0, M4.2):
  offline-писатель, который превращает `UpdatePlan` в реальные файлы ESP.
  `StoreIo` — маленький trait над путями относительно корня ESP
  (`aios/store/...`); `DirStore` реализует его для каталога на хосте, поэтому
  инструмент работает с примонтированным ESP, с `boot/out/assets` или с
  `MemoryStore` в тестах. Публичные операции: `init` (новое поколение в слоте 0),
  `commit` (новое поколение в неактивный слот), `rollback` (переключить манифест
  назад без копирования payload) и `verify` (повторить проверки загрузчика).
  `commit` в точности следует порядку `UpdatePlan`: payload → заголовок
  `Staging` → заголовок `Active` → устаревшая копия манифеста → заголовок
  `Retired` старого слота, а `--fail-after
  payloads|staging|active|manifest|retired` останавливает запись после любой
  контрольной точки, имитируя обесточивание. `verify` сообщает sequence, оба
  индекса слотов и каждую причину отказа, а CLI возвращает ненулевой статус,
  если загружаемого слота нет. 11 модульных тестов покрывают все пять точек
  прерывания, повреждённый payload с откатом на `previous_slot`, откат,
  дубликаты/невалидные имена и чередование копий манифеста.
- `aios-install` (`boot/aios-install`, `std`, отдельный крейт, v2.76.0, M5.1):
  установщик на хосте, который заменяет PowerShell-флешер для нативного образа
  UEFI. `inspect` проверяет защитный MBR, CRC обоих заголовков GPT, геометрию
  (`first_usable = 34`, `last_usable + 32 + 1 = backup_lba`), запись `AIOS ESP`
  и её FAT32-загрузочный сектор; `plan` отвергает цель меньше образа и сам
  исходный файл; `execute` пишет чанки по 1 МиБ через разные дескрипторы чтения
  и записи, сравнивает каждый чанк, а затем повторно проверяет и хеширует
  префикс цели. CLI по умолчанию печатает план и требует `--write --yes` для
  записи; `--list` только читает `\\.\PhysicalDrive0..15` в Windows. Физический
  диск нужно предварительно отмонтировать: в `std` нет API блокировки тома.
  Восстановленная цель загружается в QEMU/OVMF байт-в-байт как образ. В Windows
  размер raw-диска берётся из `IOCTL_DISK_GET_LENGTH_INFO` (`SeekFrom::End`
  неприменим к блочным устройствам), а открытие идёт через `CreateFileW` с
  `FILE_SHARE_READ|FILE_SHARE_WRITE`; запись использует выровненный по сектору
  буфер и не вызывает `flush`. В v2.76.1 проверена настоящая флешка Kingston на
  31 ГБ: записано и перечитано 336 609 792 байт с CRC32 `466BA1BB`, один
  ESP-раздел 320 МиБ со смещением 1 МиБ.
- Инвариант, исправленный в M4.2: `UpdatePlan::retired_header()` несёт **новый**
  sequence и новый CRC манифеста (не `sequence - 1`), потому что загрузчик
  проверяет слот по тому манифесту, который только что выбрал. Без этого
  активный слот загружался бы, но автоматический откат на `previous_slot`
  проваливал проверку заголовка: обновление со сломанным новым payload
  отключало бы блоки вместо отката.
- Доказательство пути записи в QEMU/OVMF (каждый сценарий пересобирает образ из
  дерева стейджа с `--reuse-assets`):
  - успешный `update`: `store manifest sequence 2 (copy 0), active slot 1,
    previous slot 0`, `store slot 1 verified at sequence 2 (1 block(s))`, новый
    `blocks/updated.wasm` печатает `hello from aios-wasmhost`;
  - повреждённый payload в слоте 1: `store slot 1 unusable: payload checksum
    mismatch`, `store slot 0 verified at sequence 2 (2 block(s))`, вместо него
    запускается старый `blocks/hello.wasm`;
  - `update --fail-after staging`: загрузчик по-прежнему видит sequence 1,
    активный слот 0 и исходный блок. Отдельный `rollback` через CLI возвращает
    управление слоту 0 на sequence 3 без копирования payload.
- Доказательство Secure Boot в QEMU/OVMF (M5.2): с `--secure-fw` и шаблоном
  NVRAM `--ovmf-vars` с внесёнными ключами образ, собранный с `--sign-keys`,
  логирует `aios-boot: entering kernel`, `[cfg] system = AIOS 2.79.0 x86_64
  (3 layer(s), 10 key(s), 0 invalid)` и `periodic 1000 Hz tick armed`. В том же
  окружении неподписанный или изменённый загрузчик никогда не входит в ядро
  (`kernel_entries=0`, `aios_boot_lines=0`), а OVMF сообщает `Access Denied`
  для `Boot0001`. Файлы внесения ключей проходят отсоединённую проверку CMS
  через OpenSSL и принимаются реальным `SetVariable` OVMF (`PK/KEK/db: OK`,
  attr NVRAM `0x27`); с этим NVRAM подписанный образ доходит до ядра. Физическая
  флешка не перезаписывалась.

## Голое ядро (`aios-kernel`, `aios-kernel-run`) — Limine + GOP с v2.34.0

Микроядро `x86_64-unknown-none`. С **v2.34.0** оно грузится как ELF по протоколу **Limine** с гибридного BIOS+UEFI ISO (тот же Limine, что и у Live-образа) и выводит текст через нативную консоль на **framebuffer GOP/VBE**; прежний путь `bootloader::BiosBoot` + текстовый VGA удалён. Это основа самодостаточного ядра; сейчас предоставляет консольный ввод-вывод, прерывания (GDT/TSS, IDT, ремап PIC, таймер PIT, клавиатура PS/2), paging (собственный обход таблиц страниц + аллокатор кадров + куча ядра), вытеснение и IPC ядра.

С **v2.78.0** в крейте **два пути загрузки, выбираемых на этапе сборки** (второй описан в разделе Native UEFI Boot Chain выше) — их выбирает опциональная Cargo-feature `limine` (`default = []`):
- `--features limine` (используется `aios-kernel-run` для гибридного BIOS+UEFI ISO/USB-образа): Limine разрешает запросы, а `_start` без аргументов преобразует ответы в общий `aios_bootinfo::BootInfo` перед вызовом `kmain`. `linker.ld` сохраняет секции `.requests*` через `KEEP`, чтобы запросы не исчезли при линковке.
- без features (используется `aios-image` для нативного UEFI-образа GPT/ESP): `aios-boot` передаёт `BootInfo` в `rdi` точке входа `_start(bootinfo: *const BootInfo)`, которая также вызывает `kmain`.

`kmain`, `boot::init()` и все последующие подсистемы остаются независимыми от загрузчика. На пути Limine `modules` и `kernel_phys/virt/size` равны нулю (модули не раскладываются); `config::init()` допускает ноль модулей. Адрес framebuffer из виртуального HHDM-адреса Limine переводится обратно в физический до того, как `Framebuffer::new` один раз добавит offset.

- `aios-kernel`: крейт `no_std`/`no_main`. Контракт загрузки:
  - `.cargo/config.toml` линкует с `-Tlinker.ld`; `linker.ld` — скрипт Limine для higher-half (база `0xffffffff80000000`, `ENTRY(_start)`, `KEEP` секций `.requests_start`/`.requests`/`.requests_end`).
  - `main.rs`: при `--features limine` объявляет статики запросов Limine (`BaseRevision`, `EntryPoint(_start)`, `StackSize(64 КиБ)`, `Hhdm`, `Memmap`, `Rsdp`, `Framebuffer`) и `#[no_mangle] pub unsafe extern "C" fn _start() -> !` без аргументов; `limine_bootinfo()` преобразует ответы в `BootInfo`. Без feature `_start(bootinfo: *const BootInfo)` остаётся прежней точкой входа `aios-boot`. С v2.78.0 оба пути ведут в общий `kmain` и логируют активный загрузчик через `LOADER_NAME`.
  - `serial` — опросный UART-драйвер COM1 (`outb`/`inb`), `kprintln!`.
  - `framebuffer` — `Framebuffer` поверх буфера GOP/VBE Limine: `put_pixel`/`fill_rect`/`clear`/`scroll_up`/`read_pixel`, 2/3/4 байта на пиксель по маскам каналов Limine; палитра `colors`. Базовый указатель — это `address` Limine как есть: по протоколу он уже содержит off-сет HHDM (`phys + hhdm`), поэтому никакая дополнительная трансляция не применяется (v2.38.13 задокументировано на `base_addr()`). С v2.38.13 `direct_test(color)` заливает всю панель строго через `core::ptr::write_volatile`, адресуя пиксели по физическому stride (`pitch / bytes_per_pixel`), так что компилятор не удаляет записи, а проход по строкам совпадает с железом даже при `pitch != width * bytes_per_pixel`. С v2.38.39 `fill_rect` заливает построчно через приватный `write_row` (один `pack` на вызов, ветка 4/3/2 bpp выбирается один раз) вместо наивного per-pixel цикла, а модуль пополнился `fill_rect_vgrad` (двухстоповая вертикальная заливка, один lerp на строку) и `get_color` (чтение пикселя для блендера векторного слоя).
  - `console` — текстовая консоль под spin-lock на framebuffer (глифы рисуются шрифтом ниже, перенос по ширине экрана, скролл); `vprintln!`. С v2.38.10 резервирует нижнюю строку `GLYPH_H` как фиксированную полосу overlay; `scroll_up(pixels, fill, region_height)` ограничен областью консоли, чтобы пиксели overlay (квадрат heartbeat, рисуется `idle_loop`) не скроллились/не дублировались. С v2.38.21 консоль владеет только компактной верхней полосой (`CONSOLE_TOP_ROWS` = 6 строк); каждая строка ниже принадлежит интерактивной панели TUI, поэтому журнал загрузки скроллится в своём маленьком окне, а панель становится экраном только после выхода из рабочего стола.
  - `font8x8` — вендоренный public-domain bitmap-шрифт 8x8, `BASIC: [[u8; 8]; 128]` (Basic Latin). Крейт `font8x8` 0.3.1 — `std`-only и здесь неприменим.
  - `psf` — (v2.38.13) `no_std`-парсер форматов шрифтов консоли Linux: PSF1 (`36 04` magic, флаг режима 256/512 глифов, ширина фиксирована 8 бит, высота из заголовка) и PSF2 (`72 B5 4A 86` magic; размер заголовка, число глифов, байт/глиф, ширина и высота — little-endian `u32`), плюс проверяемый по границам доступ `glyph(index)` по bitmap-области. `synth_psf2_basic()` собирает валидный PSF2-поток из встроенных глифов `font8x8::BASIC`, чтобы весь путь parse/render проверялся при загрузке без поставки отдельного бинарного шрифта.
  - `port` — хелперы `outb`/`inb`/`outl`/`inl`/`io_wait` для портового I/O x86.
  - `acpi` — (v2.38.20) минимальный обход ACPI-таблиц: проверка сигнатуры/ревизии RSDP, корневая таблица (XSDT — записи 8 байт, или RSDT — 4 байта) и поиск FADT. `init()` принимает указатель RSDP от Limine в любом виде — уже со смещением HHDM или голый физический — через `virt_addr()` (смещение применяется ровно один раз); слоты записей таблиц и поля FADT всегда идут через `memory::physical_to_virtual`. Публикует `acpi_found()`, ревизию FADT, физический адрес DSDT; FADT — затвор живости для пробы EC.
  - `ec` — (v2.38.20) проба ACPI Embedded Controller по фиксированным портам 0x62/0x66 (в FADT EC не перечисляется): точечные чтения `read_ram(off)` с ограниченными ожиданиями IBF/OBF (до 1 000 000 итераций, чтобы платы без EC чисто деградировали), `dump_full()` читает все 256 байт RAM, `probe()` логирует полный дамп строками по 16 байт. `active()` = FADT найден и прочитано 16+ байт.
  - `lid` — (v2.38.20) детект крышки по RAM EC: окно скана 8 байт кольцуется по 256 байтам; первый битовый флип пинит кандидата оффсет+бит, последующие сканы того же бита подтверждают полярность. Живое значение `lid_open()`, `candidate_offset()`/`candidate_bit()` для калибровки на MSI, `lid_state()` (`--`/`?`/`0`/`1`) и `lid_detail()` (`1@0x..b`) питают TUI. Раскладка RAM и полярность открытия платформо-специфичны (см. BUGS).
  - `thermal` — (v2.38.20, доработка корректности v2.68.4) цифровой датчик температуры CPU: затвор по CPUID.06H:EAX[0] (фолбэк — leaf 1 EDX bit 22), TjMax из MSR 0x1A2 (биты 16:23, при недоступном или неправдоподобном значении — задокументированные 100 °C), живая температура из MSR 0x19C (цифровая дельта 7 бит, `temp = TjMax - delta`); бит невалидного чтения (31) и неправдоподобная дельта отвергаются, вместо поддельных 0 °C/TjMax. Каждый RDMSR выполняется внутри **#GP-безопасного asm-пробника** (`_aios_probe_rdmsr` / `_aios_probe_rdmsr_fault`): ветка вектора 13 диспетчера перенаправляет фолт на fault-метку, так что MSR, дающий #GP (QEMU рекламирует DTS, но фолтится на термальных MSR), деградирует к синтетической волне 45..60 °C вместо остановки. Синтетическое значение помечается `~` в статус-баре TUI (`sim=1` во вкладке System, `temp …c~ (simulated)` в окне System GUI) и никогда не включает затвор; `critical()` (≥ `THRESHOLD_C` = 90 °C) ставит ring-3 задачи на паузу только по реальным чтениям DTS.
  - `ps2` — (v2.38.20) IRQ-free поллинг-драйвер PS/2 поверх i8042 (статус 0x64 bit5 MOBF = aux / bit0 OBF = клавиатура): `drain()` забирает до 32 байт за вызов, `init()` поднимает AUX-порт (`CMD_ENABLE_AUX` 0xA8), переводит мышь в режим репортов (`SET_REPORTING` 0xF4) и читает ID по `GET_ID` 0xF2 (ACK сливается отдельно, так что в хранимом ID — реальный: 0x00 PS/2, 0x03 IntelliMouse, 0x04 five-button). 3-байтные пакеты декодируются с sign-extended `sign9` dx/dy (y инвертирован); клавиши переиспользуют `scancode_to_char`. С v2.38.23 `feed_key` держал одноключевой latch `EXT`; с v2.83.0 он гасит только префикс 0xE0/0xE1 и кладёт следующий make-код в очередь (байты Ctrl лишь обновляют флаг Ctrl), поэтому Win/стрелки/Fn попадают в `gui::handle_scancode()` через общий FIFO, а `idle_loop()` больше не держит второй latch расширенных кодов; с v2.38.36 `feed_key` пушит каждый выживший байт в общий кольцевой FIFO клавиатуры (`interrupts::key_push`) вместе с IRQ1-путём, так что поллинговые и IRQ-байты встают в очередь в порядке прихода. Атомарно публикует `KEY_SEQ`/`KEY_SCANCODE`, `MOUSE_SEQ`/`MOUSE_DX`/`MOUSE_DY`/`MOUSE_BUTTONS`, `MOUSE_ID`; с v2.38.38 `decode_packet` дополнительно защёлкивает press-edge кнопки (`prev & !cur & 0x07`) в `MOUSE_BTN_EDGE`, который снимается через `take_button_edges()` (swap в ноль), так что пара press+release, склеенная в один `drain()`, всё равно доносит press до главного цикла (BUGS v2.38.38). С v2.38.30 мышиные дельты *накапливаются* (`fetch_add` в `decode_packet`) и считываются читателем (`mouse_dx`/`mouse_dy` делают swap в ноль): контроллер PS/2 разбывает движения больше одного пакета, а старое store-на-пакет публиковало только новейший кусок, когда между двумя опросами ввода приходило несколько пакетов (см. BUGS v2.38.30). С v2.38.35 `decode_packet` также **отбрасывает целиком пакеты с установленными битами переполнения i8042** (`b0 & 0xC0 != 0` — контроллер уже отбросил это движение; декодирование перекрученных знаковых битов давало прыжки в сотни пикселей) без bump `MOUSE_SEQ` и без публикации nibble кнопки, а слив читателя идёт через `drain_axis`: одно применение клампится до `MOUSE_MAX_STEP = 256` px на ось, остаток возвращается в аккумулятор (`fetch_add` обратно, применяется на последующих итерациях — см. BUGS v2.38.35).
  - `crt` — **удалён в v2.36.0**: сильные определения `memcpy`/`memmove`/`memset`/`memcmp`/`bcmp` из этого модуля порождали циклический PLT для `memset` и вешали загрузку (см. BUGS); ядро теперь использует слабые `mem*` из `compiler_builtins`.
  - `pci` — доступ к конфигурационному пространству PCI через legacy-порты `0xCF8`/`0xCFC`; полное перечисление bus/device/function в фиксированный массив записей `PciDevice` (id, class/subclass/prog-if, BAR, IRQ) с декодированием `class_name()` — слой обнаружения для драйверов AHCI/NVMe и xHCI.
  - `ahci` — блочный драйвер AHCI (SATA) (v2.36.0): включает memory space + bus-mastering PCI, сбрасывает HBA (`GHC.HR`/`AE`), строит на порт кадры command-list / FIS-receive / command-table через `memory::alloc_frame`, запускает каждый реализованный порт, читает `PxSIG` после запуска (пропуская ATAPI/SEMB/PM), выполняет `IDENTIFY DEVICE` (модель с попарным свопом байт + объём 28/48 бит) и перемещает сектора командами `READ`/`WRITE DMA EXT` через элемент PRDT; `AhciDrive`/`read_sectors()`. BAR контроллера доступен через `memory::map_mmio`.
  - `nvme` — блочный драйвер NVMe (v2.37.0): включает memory space + bus-mastering PCI, мапит 64-битный BAR0 через `memory::map_mmio`, сбрасывает контроллер (`CC.EN=0`), программирует `AQA`/`ASQ`/`ACQ` и ждёт `CSTS.RDY`; выполняет `Identify Controller` (ASCII `MN`/`SN`), `Create I/O CQ`/`SQ`, `Identify Namespace` (NSZE + формат LBA) и команды `Read` с опросом (PRP1 в один DMA-кадр); `NvmeDrive`/`read_blocks()`.
  - `xhci` — драйвер USB 2/3 dual-mode хост-контроллера: включает PCI-функцию, находит capability/operational/runtime/doorbell-регистры через `CAPLENGTH`/`RTSOFF` (v2.38.12 читает их из `base+0x18` с масками, а не из HCIVERSION-выводного смещения), программирует DCBAA + scratchpad + кольца event/command/EP0/EP1 и поднимает первую boot-клавиатуру на root-портах (port reset → Enable Slot → Address Device → EP0 control-трансферы → boot protocol → Configure Endpoint для interrupt-IN канала HID). MSI пока нет: задача idle-цикла планировщика опрашивает event ring (`xhci::poll`), забирает 8-байтные HID boot-репорты и перевооружает endpoint; клавиша публикуется как `KEY_SCANCODE` (set-1 make-код PS/2) + инкремент `KEY_SEQ` для детекта нажатий, а `poll`/`idle_loop` декодируют её через `scancode_to_char` (`usb key '<c>' (0x..)`). С v2.38.14 по-poll/событийные диагностики скрыты за F8 `DEBUG_MODE` (`dbg_kprintln!`) — обычная загрузка тихая при сохранении одноразовых readback подъёма; путь USB → HID → scancode → символ проверен сквозно в QEMU через monitor-`sendkey` (`usb-kbd`); с v2.38.40 `usage_to_scancode` покрывает полный boot-набор (буквы/цифры/хвост пунктуации со слотами `0x2F`..`0x39`, F1–F12, Caps/Scroll/Num Lock, цифровые `/ * - +`/цифры/точка через `KP_SCANS`; с v2.83.0 HID-usage навигации `0x49..=0x52` отображаются в Set-1-коды Home/End/Page/стрелок, поэтому «Пуск» и списки программ ядра работают и на USB-клавиатуре), а неотображённые коды логируются как `[xhci] usb key usage 0xNN unmapped` под `DEBUG_MODE`. С v2.38.15 HID boot-mouse (протокол интерфейса 2, например QEMU `usb-mouse`) поднимается на втором слоте/root-порту (порт клавиатуры пропускается) с собственным interrupt-IN кольцом — `poll` забирает репорты обоих слотов из общего event ring и публикует знаковые `MOUSE_DX`/`MOUSE_DY` плюс `MOUSE_BUTTONS` с bump'ом `MOUSE_SEQ` на каждый репорт (с v2.38.38 `harvest_mouse` также защёлкивает press-edge `prev & !cur & 0x07` в `MOUSE_BTN_EDGE`, снимаемый через `take_button_edges()`;с v2.38.30 дополнительно `fetch_add` в аккумуляторы, которые сливает `xhci::take_mouse_delta` — репорты, пришедшие подряд между двумя опросами ввода, суммируются вместо перезаписи; с v2.38.35 этот слив клампит одно применение до `MOUSE_MAX_STEP = 256` px на ось и паркует остаток обратно в аккумулятор, зеркаля `ps2::drain_axis`; простые `MOUSE_DX`/`MOUSE_DY` остаются «последним репортом» для вкладки диагностики HID), что idle-цикл превращает в курсор-стрелку 8x8 с граничным ограничением на фреймбуфере (`mouse-seq` показывается в строке драйверов TUI).
  - `nic` — (v2.38.34, расширен в v2.48.0) драйверы проводного Ethernet, выбираемые по **точной таблице PCI vendor/device-id** (класс 0x02, не Wi-Fi), а не только по vendor: `driver_kind()` сопоставляет тип с шестивариантным `Kind`, а `Nic::init` поднимает первый **поддерживаемый** чип, логируя `nic <kind> <vid>:<did> mac=…` либо `nic unsupported <vid>:<did> (<reason>)` для остальных. Бэкенды: **e1000** и **e1000e**/`igb` (общее MMIO-ядро кольца дескрипторов, `STATUS.LU`), **rtl8139** (PIO линейное RX-кольцо, `BMSR`), **rtl8169**/8168/810x (MMIO 16-байтные дескрипторы, `OWN` в `opts1`, замок `Cfg9346`, запуск `TPPoll.NPQ`, link по `PHYstatus`), **virtio-net legacy** (`0x1AF4:0x1000/0x1001`, I/O BAR, split-virtqueue с used-кольцом на следующей границе 4 КиБ, `VIRTIO_NET_F_MAC`) и **AMD PCnet** (`0x1022:0x2000/0x2001`, 16-битный I/O, init-блок `SSIZE32` `BCR20=0x0102`, запуск `TDMD`). Все опрашиваются (без прерываний): каждый бэкенд предоставляет `link_up`/`poll_rx`/`send` и несёт `kind`, `qsize` и общие курсоры `rx_pos`/`tx_pos` (копирование RA-таблицы e1000 пишет MAC двумя little-endian `u16` — в v2.38.34 исправлен panic записи 4 байт в 2-байтовый срез на первом принятом кадре).
  - `net` — (v2.38.34, укреплён в v2.48.0) сетевой стек: выбор аплинка (`uplink_mac`/`uplink_send` предпочитают ассоциированную Wi-Fi-станцию, иначе проводной NIC, иначе `nic: none (wifi uplink only)`), DHCP-клиент, ARP-кэш, ICMP-echo, DNS-запрос и машина состояний `State` (`no-nic` → `checking` → `link-only`/`internet`), опрашиваемая периодически и по запросу (`net::recheck` — кнопка `Test` в GUI); `cmos_save`/`cmos_load` сохраняют настройки между загрузками, а `net::apply` (кнопка `Apply` в GUI) валидирует все пять dotted-quad значений перед коммитом. С v2.48.0 клиент устойчив: DHCP обрабатывает **NAK** (перезапуск discover либо отказ от аренды после NAK на rebind), **продлевает** на T1 (unicast REQUEST) и **делает rebind** на T2 = 7/8 времени аренды (broadcast REQUEST без server id), с **откатом на статический адрес**, когда DHCP исчерпал попытки; `net::poll` выполняет **восстановление при смене линка** (фронт down→up / up→down сбрасывает `NET_OK`, уводит в `Phase::Idle`, очищает ARP-кэш и заново запускает получение адреса); ARP-кэш **стареет** (`ARP_TTL` = 5 мин, вычищается `arp_expire`), а повторная проверка доступности откатывается экспоненциально (потолок 60 с) после подряд идущих неудач. Опрашивается каждый тик из idle-цикла `main` вместе с `wifi::poll`. С v2.49.0 он также предоставляет помощники маршрутизации для сокетного слоя (`now_ticks`, `bound_ip`, `next_hop`, `arp_resolve`, `emit_ipv4`, `l4_checksum`) и направляет входящие L4-полезные нагрузки в `sock` (`PROTO_TCP`/`PROTO_UDP` → `sock::deliver_tcp`/`deliver_udp`), при этом `net::poll` каждый вызов тикает `sock::tick`. С v2.50.0 `emit_ipv4` отправляет кадры, адресованные на привязанный IP / `127.0.0.1`, в **кольцо обратной петли** (`LOOPQ`, 16 × 1472 байт), вычерпываемое `drain_loopback` на следующем `poll` — ядерные серверы и клиенты могут общаться на одной машине без NIC, а доставка никогда не пере-входит в состояние сокета, пока его ещё держит отправитель. С v2.56.0 кольцо также хранит адрес назначения кадра (`LOOP_DST`) и заворачивает обратно `255.255.255.255`, направленные subnet-broadcast и class‑D multicast кадры — сокетный слой может раздать дейтаграмму каждому участнику на порту назначения.
  - `sock` — (v2.49.0, серверная сторона v2.50.0) сокетный слой IPv4 в ядре: фиксированная таблица из `MAX_SOCKS = 16` дескрипторов (поднята с 8 в v2.55.0) (`static mut SOCKS`), каждый — либо connectionless **UDP**-эндпоинт (кольцо из 3 дейтаграмм, MTU 1024), либо **TCP**-сокет на обе стороны: минимальный клиент (active open: `SynSent` → …) и минимальный сервер (`listen` → `Listen`, по SYN создаётся дочерний сокет в `SynRecv` с ответом SYN-ACK, финальный ACK клиента переводит его в `Established`, а `accept` выдаёт дочерний наружу; состояния `Closed`/`Listen`/`SynRecv`/`SynSent`/`Established`/`CloseWait`/`FinWait1`/`FinWait2`/`LastAck`/`TimeWait`). API: `open`/`bind`/`listen`/`accept`/`connect`/`send`/`recv`/`close`/`status`/`owner`, а также входящие хуки `deliver_udp`/`deliver_tcp` (зовутся из `net`) и `tick` (таймеры ретрансмита/`TIME_WAIT`). TCP намеренно прост — stop-and-wait над 8 КиБ очередью передачи (отправка больше `TCP_MSS` ставится в очередь целиком и сегментируется, один сегмент в полёте, следующий пинается каждым ACK, завершающим текущий), кольцо RX 8192 байт, 4 ретрансмита с шагом 50 тиков, фиксированное окно `TCP_WINDOW = 64240` (без window scaling; только учёт контроля перегрузки `cwnd`/`ssthresh` с v2.54.0); порты UDP матчатся по привязанному локальному порту и требуют ответа от соединённого пира, если вызывался `connect`. С v2.56.0 UDP-сторона также покрывает broadcast и multicast: подключение к `255.255.255.255` делает сокет участником широковещания, а подключение к class‑D адресу (224.0.0.0/4) — участником multicast-группы, и `deliver_udp` (теперь получающая адрес назначения кадра) раздаёт broadcast/групповые дейтаграммы **каждому** сокету-участнику на порту назначения, тогда как unicast-доставка по-прежнему матчит ровно одного пира. Свободные порты выделяются из `49152..60000`. Лицо для ring-3 — семейство syscall'ов `SYS_SOCK_*` (id 9–18, см. `syscalls`), а пути упражняются из shell TUI (`udp`/`http`/`socktest` — последний также гоняет 3000-байтную сегментированную передачу с проверкой сборки) и, с v2.51.0, напрямую ring-3 демо-задачей, гоняющей UDP-пинг-понг по обратной петле и (с v2.52.0) круг TCP `listen`/`connect`/`accept`/эхо через syscall'ы (`sock-e ok` / `sock-t ok` и строки `[sock]` в serial доказывают оба вертикальных среза целиком).
  - `user` — ring-3 демо-программы (сырой машинный код над вентилем `int 0x80`, каждая отображена по `CODE_BASE + pid × 2 МиБ` со стеком в 4 страницы): A/B гоняют пинг-понг по почтовым ящикам слотов (`SYS_SEND`/`SYS_RECV`, доказательные строки `u1`/`u2`), C — спящая (`SYS_SLEEP` круги), D — GUI-клиент (`SYS_GUI` заливка/клавиша/клик), E (v2.51.0) — сокетный клиент: с v2.52.0 **полный сокетный круг за цикл**: два UDP-сокета гоняют `ring3`/`ack!` по кольцу обратной петли, затем TCP-клиент `connect`ится к слушающему сокету, `accept` выдаёт выросшего ребёнка, пара эхит `tcp`/`ok`, а ребёнок забирается двойным `SYS_SOCK_CLOSE` (первый close лишь шлёт FIN и оставляет слот в `FinWait2`); вердикты `sock-e ok` / `sock-t ok` через `SYS_WRITE`. fd живут в r8d/r9d (UDP) и r12d/r13d/r14d (TCP), снимок IP — в ebx — все сохраняются вентилем ядра; каждый неудачный шаг ретраится после короткого сна. Пятая демо-задача — причина, по которой `MAX_TASKS`/`MAX_PID` = 7 (демо занимают слоты 2..6).
  - `wifi` — (v2.38.34, честный с v2.38.36) Wi-Fi-станция + эмулируемый AP на **тест-радио**, явно помеченном sim. С v2.38.36 `wifi::init(&pci_devices)` детектит настоящий контроллер через `pci::is_wifi` (сетевой класс `02`, подкласс `80`) и отчитывается `[wifi] card: <bdf>` / `card: none`; радио по умолчанию — **real** (`radio: real (test radio off, shell: wifi sim)`), реальный скан без карты отказывается (`[wifi] scan: no wifi card`), а пустой результат даёт подсказку `no networks found`; симулированное радио включается только командами шелла `wifi sim` / `wifi real` / `wifi` (`set_sim` переключает `SIM_ON`, логируется как `test radio enabled (sim, 3 APs)` / `test radio disabled`) — так его включают смоуки B/C, а F доказывает выключенность. Sim предлагает три встроенных AP (`AIOS-Test` открытый, `SecureNet` WPA2-PSK пароль `AIOS-Test`, `Neighbor` открытый), wildcard-сканирование (beacons/probe-ответы, до 8 результатов), открытая/WPA2-ассоциация с полным 4-way handshake — PBKDF2-SHA1 (4096) PMK, PRF-512 PTK, обёртка GTK по RFC 3394, EAPOL M1–M4 с HMAC-SHA1 MIC, шифрование данных AES-CCM (CCMP) — и AP-сторона LAN на `10.0.9.0/24`, отвечающая на DHCP/ARP/ICMP/DNS (`ap dhcp offer/ack -> 10.0.9.15`), по которой идут пробы `net` и `state internet`. Три lock-free очереди кадров (`TXQ` STA→AP, `RXQ` AP→STA, расшифрованный `ETHQ` в `net`) продавливаются по 8 кадров за тик в `wifi::poll`, он же таймаутит сканы (`SCAN_TICKS`) и попытки ассоциации (`CONNECT_TICKS`); MAC станции — `02:00:00:00:00:AA`.
  - `gdt` — GDT на 7 записей (null, код/данные ядра и пользователя, 64-битный TSS с отдельным IST-стеком для double fault); `Descriptor` объявлен как `#[repr(C, packed)]`, поэтому `lgdt` читает базу с корректного смещения. `reload_segments` (v2.38.12) перезагружает SS в `0x10`, делает far return в `CS=0x08` через эту GDT и обновляет DS/ES/FS/GS в `0x10` — Limine передаёт CPU с селекторами *своей* GDT (`cs=0x28/ss=0x30`), и пока CS/SS не перезагружались, первый `iretq` перевалидировал `cs=0x28` по GDT AIOS, где на слотах 5/6 лежит TSS-дескриптор → `#GP` (см. `BUGS.md`); `ltr` устанавливает TSS.
  - `idt` — IDT на 256 записей, строится из ассемблерной `aios_handler_table` (стабы векторов в `.text`, таблица с релокациями при загрузке); для гейта 8 используется double-fault IST.
- `interrupts` — C-обвязка `aios_interrupt_common` (`aios_handle_interrupt`) диспетчеризует по вектору: фатальные фолты (0/6/8/13/14) печатаются и ядро останавливается (с диагностического обновления v2.84 ветка `#PF` из ring 0 использует `fatal_fmt` и рисует `PAGE FAULT <class> addr/ip/err/rsp` на framebuffer, а `pf_kind` разбирает биты кода ошибки, поэтому краш диагностируется без COM1; `#PF` из ring 3 уходит в `fault::intercept`), диапазон IRQ `0x20..=0x2F` получает PIC-EOI (IRQ0/PIT увеличивает `TICKS` с частотой `TIMER_HZ` (1000 Гц с v2.78.2), IRQ1/клавиатура с v2.38.36 пушит сырой байт в `key_push` — кольцевой буфер `KEY_FIFO` на 256 слотов с атомарными HEAD/TAIL вместо прежнего одиночного слота `LAST_SCANCODE`, поэтому 1–2-секундный простой idle-цикла больше не затирает непрочитанные make-/break-коды; `ps2::feed_key` кормит тот же FIFO), а выделенный LAPIC-вектор `0x90` (v2.38.12) увеличивает `TICKS`, рисует прогресс-бар, делает LAPIC-EOI и кормит `sched::tick`, как PIT-путь (первое срабатывание один раз логирует сохранённый фрейм `cs/ss/rip`). С v2.38.20 ветка вектора 13 сначала проверяет `thermal::probe_active()` и отдаёт фолт пробирующего RDMSR в `thermal::on_probe_gp` (переписывает RIP фрейма на `_aios_probe_rdmsr_fault`) вместо остановки. `init_pic` ремапит 8259 PIC на `0x20`/`0x28` и маскирует всё, кроме IRQ0/IRQ1; `init_pit` (режим 2, rate generator, с v2.68.3) программирует канал 0 на `TIMER_HZ`. `set_pit_masked(true)` маскирует PIT IRQ0 после подъёма LAPIC-таймера, оставляя один аппаратный источник тиков. `pit_count`/`delay_ms`/`wait_for_irq32` управляют тем же отсчётом напрямую (без IRQ) для software-tick фолбэка.
- `lapic` — (v2.38.12) драйвер таймера Local APIC, основной аппаратный источник тиков на любой плате: предпочитает x2APIC (IA32_APIC_BASE + расширенный MSR-диапазон), иначе fallback на xAPIC MMIO через `memory::map_mmio`, калибрует `bus_hz` по отсчёту PIT (`bus_hz = lapic_delta * 1_193_182 / pit_delta`, reload-aware via `pit_elapsed` since v2.68.3) и запускает периодический тик `TIMER_HZ` на векторе `0x90` (1000 Гц с v2.78.2). `active()` кормит режим дашборда (`HW-LAPIC`). Работает вообще без 8259 PIC.
- `sched` — round-robin по тикам таймера над `MAX_TASKS = 7` кадрами (слот 0 = idle, worker = 1, пять ring-3 демо-задач в слотах 2..6). Ring-3 задачи вытесняемы (переключаются по любому аппаратному тику — PIT или LAPIC); ring-0 задачи переключаются только кооперативно через `yield_kernel` (`int 0xfa`), что охраняет предикат `hw_tick`, покрывающий и вектор `0x90`. До v2.38.12 на платах, где legacy PIC не доставлял IRQ0 (маршрутизация через IO-APIC), **прерываний для вытеснения не было** — ring-3 задача без syscall не отдавала CPU никому, останавливая экономику тиков; LAPIC-таймер вернул вытеснение на таких платах, но демо-программы по-прежнему обязаны спать (`SYS_SLEEP`). `idle_loop` по-прежнему обнаруживает отсутствие аппаратных тиков (`IRQ32_SEEN == false`) и синтезирует тактовость `TIMER_HZ` опросом счётчика PIT. С v2.38.20 `schedule()` также применяет термальный затвор: пока стоит `thermal::critical()` (только реальные чтения DTS с v2.68.4), ring-3 задачи (`frame.cs == USER_CS`) пропускаются — демо-задачи замирают на горячем кристалле, а задачи ядра и idle продолжают работать, TUI продолжает обновляться. С v2.45.3 обновление outgoing-контекста также ведёт **сторожевой таймер зависаний**, с v2.46.0 учитывающий прогресс: пер-слотовые `STALL_SINCE`/`STALL_RIP` отслеживают, когда сохранённый `rip` ring-3 задачи менялся в последний раз, а пер-слотовый `LAST_ACTIVITY` (обновляется на каждом `int 0x80` через `sched::note_activity`, он же heartbeat `SYS_PING`) — её последний syscall. Задача, у которой **одновременно** неизменён `rip` **и** не было syscall с момента якоря застоя в течение `HANG_WATCHDOG_TICKS` (10 с), передаётся в `fault::record_hang` и замораживается — настоящий бесконечный спиннер ловится без исключения CPU, а ограниченные циклы заливки буфера (двигающие `rip`) и задачи, активно делающие syscall, ложных срабатываний не дают. `task_snapshot(pid)` отдаёт сохранённый фрейм задачи (запечатанный `TaskSnapshot`) для шлюза целостности Resume.
  - `memory` — ядро paging поверх HHDM Limine (`physical_offset = hhdm.offset`). `translate` обходит активные таблицы страниц (PML4/PDPT/PD/PT, большие страницы), читая физическую память через HHDM; `map_page`/`unmap_page` редактируют таблицы и выделяют кадры для таблиц по требованию, сбрасывая TLB через `invlpg`; `init` строит bump-аллокатор кадров по регионам `MEMMAP_USABLE` Limine, переданным как `MemRegion { start, end }`; `selftest` мапит тестовую страницу, пишет/читает паттерн и проверяет `translate` до и после unmap. `verify_region(virt, bytes)` (v2.38.13) обходит живые таблицы и возвращает `RegionCheck { pages_total, pages_present, pages_writable }`, считая huge-листья по одной записи на 4 КиБ-субстраницу с флагами самого листа — загрузочный путь использует её, чтобы доказать, что весь диапазон VRAM GOP промаплен. `map_mmio(phys, size)` мапит страницы BAR устройств в выделенное окно `0xffffff00_2000_0000` (слот PML4 510) — HHDM покрывает только RAM; `physical_to_virtual(phys)` возвращает HHDM-алиас, используемый для DMA-буферов. Отдельный гейт для буферов ring-3: `USER_SPACE_LIMIT` и `user_range(virt, len)` отвергают нуль (для ненулевой длины), верхнюю половину/нарушение канонического адреса и переполнение `virt + len`, а `translate_user(virt, len, write)` обходит каждую затронутую страницу через `walk_user_leaf()` и требует `PRESENT`/`USER` на PML4/PDPT/PD/листе (и `WRITABLE` при записи, включая huge-страницы). `translate` остаётся только для адресов ядра/драйверов/DMA. `user_read`/`user_write` возвращают `Option` и не разыменовывают отклонённый диапазон; `write_user_string` проверяет каждый байт и безопасно останавливается на `SYS_STR_CAP` или переполнении `addr + 1`.
  - `heap` — куча ядра 4 МиБ (`HEAP_START = 0xffff_ff00_0000_0000`, `HEAP_SIZE = 4 MiB`), замаплена страница за страницей через `memory::map_page`; `#[global_allocator]` free-list аллокатор — first-fit по *адресно-отсортированному* списку свободных: `alloc` сплитит с остатком на исходном адресе (порядок выживает), `dealloc` вставляет по порядку и сливается с **обеими** соседними блоками при смежности (прежнее слияние только с головой списка оставляло соседние фрагменты и вело к OOM на долгих сессиях), под spin-lock; `init_heap` создаёт единый свободный блок, `test_heap` выделяет `Vec`/`String`/`Box` и стресс-цикл из 200 строк,   `fragmentation_selftest` (v2.38.33, запускается из `main` сразу за `test_heap`) вырезает 24 × 96 KiB блока, освобождает их в порядке аллокации и заново добывает 1.5 MiB целиком через `try_reserve_exact`, логируя `heap: fragmentation self-test ok` (грепается смоуком) или `FAILED`. GUI backbuffer стоит на `HEAP_START + 4 MiB` — впритык, но без пересечения страниц.
- `fault` — (v2.45.0, расширен v2.45.1/2/3/4/2.46.0) Перехватчик фолтов Ring 0,
  ядерный близнец host-функции `run_isolated`. `intercept(frame, kind)` вызывается
  из обработчиков исключений для **пользовательского** фрейма: снимает
  `TaskSnapshot` полного CPU-фрейма (`TaskSnapshot::capture` — pid,
  `rip`/`cs`/`rsp`/`rflags`/`ss` и все 16 регистров общего назначения,
  запечатанный FNV-1a-хэшем `hash_of()`; `verify()` перепроверяет запись), ставит
  в очередь `FaultRecord`
  (`FaultCode { DivideError, InvalidOpcode, GeneralProtection, PageFault, Hang }`,
  `count =` номер сбоя данного pid) в **системную очередь фолтов**
  (`FAULT_QUEUE_LEN = MAX_TASKS`, FIFO), замораживает задачу
  (`sched::freeze_task`) и вызывает `schedule(frame)` для ухода с неё.
  `publish` также добавляет `FaultEvent { pid, kind, count, rip }` в ограниченное
  **кольцо событий фолтов ядра** (`FAULT_EVENT_LEN = 16`, при переполнении
  отбрасывается самый старый), вычерпываемое `poll_event()` / syscall'ом
  `SYS_FAULT_POLL` — freestanding-двойник host-IPC `FaultReport`; `events_queued()`
  показывает накопление.
  `peek()`/`queued()` питают GUI-оверлей (он разрешает начало, от старой к
  новой), а `resolve(Recovery)` применяет
  `Resume` (шлюз целостности: живой фрейм замороженной задачи — `sched::task_snapshot`
  — должен совпасть с запечатанным хэшем записи из очереди, иначе задача
  принудительно Terminate; `state verified` / `resume state mismatch -> terminate`),
  `Terminate` (`remove_task`) или `Rollback`
  (`restore_task` пишет обратно в фрейм задачи **весь снятый набор регистров**,
  поэтому возобновление входит в точное CPU-состояние на момент сбоя, повторно
  проверяя `verify()`), затем выталкивает начало — следующая запись очереди
  становится текущей без закрытия диалога. Пер-задачный **бюджет сбоев**
  (`MAX_FAULTS_PER_TASK = 3`)
  запирает диалог на Terminate при его исчерпании; каждое решение уходит в
  ограниченное кольцо истории (`FAULT_HISTORY_LEN = 8`) с агрегатами
  `fault_total()` / `resolution_count()`, а `history_records()` (копия кольца от
  старой к новой) питает shell-команду `faults` (v2.45.2). С v2.45.3
  `record_hang(snapshot)` — точка входа **сторожевого таймера зависаний**:
  планировщик зовёт её для ring-3 задачи, которая ни меняла `rip`, ни делала
  syscall в течение `sched::HANG_WATCHDOG_TICKS` (10 с), списывая тот же бюджет и
  замораживая задачу через тот же диалог (`FaultCode::Hang`, заголовок диалога
  `Task Hung`); в отличие от `intercept` она работает внутри `schedule`, поэтому
  не перепланирует. Фреймы ядрового контекста по-прежнему идут в
  `interrupts::fatal_with`.
- `tui` — интерактивный экранный TUI (v2.38.11 — дашборд только для просмотра;
  **интерактивен с v2.38.19**): **резервный диагностический экран** с v2.81.0
  (основной — нативный рабочий стол на фреймбуфере) — компактная полоса журнала
  загрузки (`console::CONSOLE_TOP_ROWS` = 6 строк) остаётся вверху, а `render()`
  рисует панель на всю оставшуюся высоту (`panel_rows()` = всего глиф-строк −
  полоса журнала − статус-бар; строка вкладок сверху, контент ниже) с частотой
  ~20 Гц (каждые 5 тиков) из `idle_loop`, плюс
  двусторонний нижний статус-бар (слева: версия, активная `tab n/7`, счётчик
  тиков, режим тика — `HW-LAPIC`/`HW-IRQ`/`SOFT`, отсчёт PIT; справа: последний
  PS/2 scancode, USB-ключ и последовательность мыши). С v2.38.20 правая часть
  статус-бара — `k=0x.. uk=… pk=… m=… pm=… lid=… T=…c` (PS/2 scancode,
  USB-секвенции клавиш/мыши PS/2, состояние крышки, живая температура), а
  вкладка System дополнена строкой `lid=… temp=… tjmax=… sim=… ps2key=…
  ps2mouse=id0x..` через `lid_state()`/`lid_detail()`. Семь вкладок — System /
  Sched / USB / IPC / Storage / Shell / About — показывают живую телеметрию
  только из атомарок и целочисленных счётчиков, без блокировок: планировщик
  (`switch_count` + состояние задач), IPC (`syscalls::stats` + `mailbox_len`),
  xHCI-контроллер/клавиатура/мышь, статусы `G_AHCI`/`G_NVME` (`-1` нет, `0`
  сбой, `1` ок, `2` чтение проверено), память (`memory::frames_allocated`).
  Ввод: `1`-`7` переключают вкладки (вне вкладки Shell, где цифры вводятся),
  курсор-стрелка USB-мыши ограничен фреймбуфером и перерисовывается последним
  при каждой перерисовке — ни одно обновление его не стирает — а левый клик по
  строке вкладок переключает вкладку; во вкладке Shell работает встроенный
  командный шелл (`Enter`=0x1C выполняет, `Backspace`=0x0E правит, `Esc`=0x01
  очищает; каждая введённая команда с v2.38.36 эхоится в serial как
  `[serial] [shell] > <line>`, а `wifi` / `wifi sim` / `wifi real`
  переключают радио окна Network — по умолчанию **real**, sim включается
  явно). Текст панели рендерится через парсер `psf` (`ensure_font()`
  разбирает загрузочный синтезированный PSF2-поток в кэшированный шрифт) с
  таблицей `font8x8` как фолбэком.
- `gui` — оконный рабочий стол (v2.38.22; с v2.38.23 рендер по damage; с v2.38.24
  RAM-бэкбуфер + dirty-rect блit и каденция 60 FPS): единственный GUI,
  запускаемый из `idle_loop()` при загрузке с v2.81.0. Консоль/TUI —
  диагностический резерв: `tui` или `Esc` без фокусного окна покидают рабочий
  стол. Пока `gui::active()`, путь `vprintln!` консоли обходится и heartbeat
  приостанавливается. Владеет тёмным фоном, кнопкой `☰ Пуск` в левом нижнем
  углу и отсортированной панелью «Пуск» с поиском (`start_entries`/
  `start_click`, раскладка `START_*`), нижним таскбаром с кнопкой на каждое
  открытое окно (стабильный порядок — см. ниже) и списком окон в z-порядке
  (строка заголовка с кластером minimize/maximize/close, перемещаемое тело,
  ресайз-ручки по краям/углам) в статических слотах `MAX_WINS = 8`. Текст идёт
  через учитывающие символы хелперы TUI `draw_text`/`draw_char`/`text_width` и
  общий шаг `console::TEXT_ADVANCE = GLYPH_W + SCALE`: между глифами остаётся
  зазор 2 px, а русские заголовки «Пуск» и текстовые поля рисуются из
  `font8x8::CYRILLIC`. `draw_text` отсекается по прямоугольнику, который
  устанавливают `tui::set_text_clip` / `gui::set_surface_clip` (окно, панель
  «Пуск», таскбар, модальное окно и диалог сбоя), и по `max_px`; слишком длинные
  подписи таскбара/«Пуска» получают `...` через `fit_label` / `short_label`,
  а не выходят на соседнюю поверхность. `clear_text_clip()` возвращает полный
  фреймбуфер для резервного TUI. Ввод: `Tab` циклит фокус, `Esc` закрывает/
  выходит, печатные клавиши печатают в сфокусированное окно; клики мыши
  открывают/фокусируют/закрывают/перетаскивают (и, с v2.38.27, ресайзят —
  `hit_resize` отображает нажатие в 5 px (`RESIZE_BORDER`) от внешних краёв окна
  в маску `EDGE_L/R/T/B`, проверяемую после кнопки закрытия и до
  заголовка/тела; drag ключируется по `WinKind`, а не по индексу слота, потому
  что `focus_window` → `bring_to_front` может сдвинуть слоты в момент старта
  drag). Ввод читается с обоих
  источников (клавиатура/мышь) через lock-free атомики и никогда не блокирует
  idle-loop; GUI и рендер развязаны (события ввода лишь меняют состояние окна и
  грязный `Rect`, отрисовка происходит позже в `render()`). С v2.38.23 каждая
  мутация регистрирует грязный `Rect`
  (`dirty_all`/`dirty_rect`/`dirty_window`). С v2.38.24 `render()` рисует кадр
  целиком в RAM-бэкбуфер (`Framebuffer::from_ram`, `ensure_backbuffer` — мапится
  один раз из frame-аллокатора по `BACKBUF_BASE = 0xFFFF_FF00_0040_0000`,
  никогда не освобождается; 4 МиБ для панели 1280×800; при сбое деградация на
  прямой VRAM-рендер через `BACKBUF_FAIL`) и пушит в VRAM только прямоугольник
  повреждений через `Framebuffer::blit_region` (одна `copy_nonoverlapping` на
  строку) — панель видит только полные кадры, поэтому нет ни фликера, ни
  тиринга. VRAM пишется ровно один раз на грязную строку. С v2.38.39 отрисовка идёт через модуль `vector` — слой с антиалиасингом по покрытию (`fill_round` / `fill_round_grad` / `stroke_round` / `fill_circle` / `stroke_ellipse` / `line` / `shadow_round`, маски углов `TL/TR/BR/BL/ALL/TOP`, хелперы `mix`/`blend`), держащий стоимость краёв O(perimeter) за счёт построчных заливок в интерьерах: окна и плитки иконок получили скругление 7px, мягкую тень и обводку, рабочий стол и таскбар — двухстоповые заливки (`Framebuffer::fill_rect_vgrad`), глобус в трее стал настоящим кругом (кольцо/меридиан/экватор), а каждый `dirty_rect` расширяется на `SHADOW_PAD = 7`, чтобы хвосты теней перерисовывались вместе со своей фигурой. Непрерывные пути
  (курсор, перетаскивание, живые окна System/Uptime) отмечают только свои
  прямоугольники через
  `mark_live_dirty()`. `idle_loop` гоняет `gui::render()` на собственной
  каденции — 60 Гц (примерно каждый 17-й тик 1000-Гц таймера с v2.82.0, вместо
  250 Гц в v2.79.0 и 100 Гц в v2.78.2) — плюс немедленный repaint после каждого
  репорта мыши; TUI рендерится на 30 Гц (было 100 Гц), а датчики (крышка/термия)
  остаются на ~20-Гц гейте. Меньшая частота выше порога восприятия и не сжигает
  CPU на невидимые кадры.
  Runaway-гейт указателя измеряется прошедшими тиками (`TIMER_HZ`, одна секунда),
  а не числом пакетов, поэтому при 1000 Гц он не срабатывает в десять раз быстрее.
  Большинство Extended-сканкодов (0xE0/0xE1) гасятся до таблиц GUI/TUI; с
  v2.81.0 Win (`0x5B`), вверх/вниз (`0x48`/`0x50`) и Enter (`0x1C`)
  перенаправляются в `gui::handle_ext_scancode()` для навигации по «Пуску»
  (см. `ps2`/`main`/`xhci`). Окно System переиспользует
  `tui::tick_mode()`/`lid_state()` и `version::KERNEL`.
  С v2.82.0 пять ring-3 демо-задач **выключены по умолчанию** в сборке десктопа:
  `user::start_demos()` идемпотентен и создаёт их только при выборе пункта
  `Пуск → Ring-3 демо` либо при загрузке с Cargo-feature `ring3-demo` (headless-
  проверки через `AIOS_KERNEL_FEATURES=limine,ring3-demo`).
  `user::demos_started()` скрывает пункт «Пуск» после запуска задач. Демо A/B IPC
  отдыхают через `SYS_SLEEP` + `SYS_PING` вместо busy-spin, GUI-клиент презентует
  кадр каждые 250 мс, а `syscalls.rs` пишет только первый `SYS_SLEEP` каждой задачи
  и каждое 64-е IPC-значение — поэтому десктоп в простое доходит до `[sched] idle`.
  С v2.38.25 оконная система открыта для ring-3: `syscalls.rs` маршрутизирует
  `SYS_GUI` (id 6) в `gui::client_syscall`, который диспетчеризует по `rdi` —
  CREATE (`rdi = 0`, `rsi` → 16-байтовая `CreateReq { w, h, buf, title }` из
  user-VAs) проверяет запрос, весь буфер и каждый байт заголовка через
  `memory::translate_user` — единственный гейт указателей пользователя для
  `int 0x80`, отвергающий адреса ядра, переполнение и не-user-маппинги с
  `u64::MAX`, — ограничивает
  размеры (8..=512 px, ≤ 512×512×4 байт), копирует NUL-терминированный
  заголовок (≤ 24 байт) и регистрирует `ClientWin` в реестре `CLIENTS`
  (`MAX_CLIENTS = 4`); PRESENT (`rdi = 1`) заново помечает
  повреждённым окно клиента `WinKind::Client(id)` (пересоздавая его через
  `spawn_client`, если окно закрыли, так что `leave()` не убивает живого
  клиента); GET_EVENT (`rdi = 2`, с v2.38.26) забирает единственное
  ожидающее событие ввода клиента (`EV_NONE`/`EV_KEY | ascii<<8`/
  `EV_CLICK | x<<8 | y<<24`, keep-first, неизвестный pid → `u64::MAX`);
  любой другой `rdi` → `u64::MAX`. Инъекция ввода (v2.38.26): `handle_scancode`
  отдаёт печатные клавиши сфокусированному клиенту вместо поля набранного
  текста, `dispatch_click` публикует клики по телу ДО перестановки
  z-порядка (адресация по id клиента — сдвиг слотов в `bring_to_front` не
  «осиротит» событие), а `raise_clients()` фокусирует самое верхнее
   клиентское окно, чтобы ввод шёл сразу. Поскольку клиент может отмапить или
   переиспользовать буфер после CREATE, `draw_client()` заново проверяет всю
   поверхность через `translate_user` (read-only) при **каждом** composite и
   пропускает blit, если какая-либо страница не прошла проверку, вместо
   разыменования устаревшего указателя. Рендер композитит плотный
   buffer-клиента с учётом pitch через
   `Framebuffer::blit_at` под тайтл-баром или — с v2.38.27, как только окно
   отресайзили — через nearest-neighbour `Framebuffer::blit_scaled`, поэтому
   клиент сохраняет свой родной буфер 96×64, а окно масштабируется (клампы
   ресайза: `RESIZE_MIN_W = 96`, `RESIZE_MIN_H = TITLE_H + 64`, правый/нижний
   края ограничены фреймбуфером минус таскбар; отпускание drag логирует
   `[gui] resize … -> WxH`, а `draw_window` обрезает текст заголовка/тела по
   собственному правому краю окна); `win_live_title` показывает заголовок
  из реестра в тайтл-баре и на панели задач;   `enter()` поднимает
  зарегистрированные клиентские окна над встроенными через `raise_clients()`
  (они регистрируются, пока экран ещё у консоли, иначе Welcome/System/Clock
  окажутся сверху).
  С v2.38.28 окна несут флаги `minimized`/`maximized` и сохранённый прямоугольник
  `restore`: строка заголовка декодирует якоренный справа 48-пиксельный кластер
  `TitleBtn { Minimize, Maximize, Close }` (`title_btn()`, проверяется до
  зон перетаскивания/ресайза, поэтому `hit_resize` пропускает свёрнутые и
  максимизированные окна), `minimize_window` прячет окно (render его
  пропускает, hit-testing игнорирует, `cycle_focus` отфильтровывает) и
  передаёт фокус самому верхнему видимому окну, `toggle_maximize` разворачивает
  на рабочий стол минус таскбар и восстанавливает сохранённый прямоугольник
  вторым нажатием, а `focus_window` разминимизирует — через него проходят все
  клики кнопок таскбара/иконок, логируя `[gui] restore … -> WxH`
  (`[gui] minimize …`, `[gui] maximize|restore … -> WxH` для остальных
  переходов). Кнопки таскбара раскладываются по `taskbar_slots()`/`kind_rank`
  (Welcome < System < Clock < About < клиенты по id), а не по z-order, поэтому
  перефокусировка не может подвинуть кнопку под курсор; у свёрнутых окон
  приглушённая заливка `BAR_MIN`. Хоткеи используют обычные make-коды
  F-клавиш (`idle_loop` фильтрует break-коды гейтом `sc & 0x80` и префиксы
  E0/E1, поэтому отслеживание модификаторов не нужно). С v2.81.0 `F1`-`F9`
  открывают или фокусируют девять пунктов `APPS` через `open_app`, а управление
  окнами переехало на `F10` (`toggle_maximize`), `F11` (`close_window`) и
  `F12` (`minimize_window`); каждый возвращает `true`, так что ничего не утекает
  в поле набранного текста. Цели берутся из `focused_kind()`/`focused_idx()`, а
  переходы логируются строками `[gui] close …` / `[gui] focus …` (метка фокуса
  снимается до `focus_window`, потому что `bring_to_front` перекладывает окно в
  другой слот). «Пуск» переключается клавишей Win (`0x5B` на PS/2, HID `0xE3`
  на USB), навигируется ↑/↓ (`0x48`/`0x50`), подтверждается Enter (`0x1C`) и
  закрывается Esc; `handle_ext_scancode()` игнорирует эти коды, пока клавиатуру
  удерживает диалог краша или модалки. С v2.38.30 `close_window`
  дополнительно гейтится на незасохранённый текст: окно с
  `note_len > 0` (через Esc / `X` / `F11`) открывает модальный диалог
  подтверждения — `MODAL: Option<(WinKind, title)>` — вместо закрытия, а
  `close_window_raw` — принудительный разбор, который использует путь
  подтверждения. Диалог — фиксированный оверлей 464×140 поверх окон и
  таскбара в `render()` (перерисовывается при любом частичном повреждении,
  его пересекающем), с хит-пластинами, точно совпадающими с `draw_modal`.
  Пока `MODAL` установлен, весь WM закрыт: `handle_scancode` → `modal_key`
  (Enter/Esc разрешают, всё остальное логируется как
  `[gui] modal blocks scancode 0xNN`), нажатия мыши → `modal_click`
  (работают пластины, остальное логируется как
   `[gui] modal blocks click (x,y)`), а `focus_window`/`spawn`/`cycle_focus`
   отказывают наотрез — защита от фокус-стила живёт и во входных гейтах, и на
   этих узких местах. С v2.38.31 закрывающееся окно записывает свой
   логический z-ранг в `CLOSED_Z: [Option<(WinKind, u8)>; MAX_WINS]`
   (в `close_window_raw`, линия доказательства `[gui] close <label> (z <rank>)`,
   очищается в `leave()`), а оба пути открытия проходят через
   `place_window`: она вставляет окно на запомненный ранг (лог
   `[gui] reopen <label> at z <rank>`), уводит наверх при отсутствии памяти
   (первое открытие — поведение до v2.38.31) и уплотняет массив слотов,
   чтобы `WINS` оставался плотной последовательностью снизу вверх.
    `spawn_client` тоже получил модальный гейт — present клиента во время
    диалога больше не может создаться сфокусированным под ним.
    С v2.38.32 тела окон могут содержать виджеты: плитки `Widget::Button`
    объявлены в координатах относительно окна в таблицах по видам
    (`widgets_for`, `WELCOME_WIDGETS` на Welcome) с действием
    `WidgetAction::{Open(WinKind), CloseHost}`, рисуются `draw_widgets` после
    контента конкретного вида (плитка `BTN_BG`, центрированная подпись,
    `BAR_ON` при наведении) и проверяются `dispatch_click` раньше любых
    событий ring-3 клиента (линия доказательства
    `[gui] widget <host>/<label> click`, клиентское событие не отправляется,
    если плитку поглотила клик). Состояние наведения хранится в
    `WIDGET_HOVER: Option<(WinKind, usize)>`, пересчитывается на движения
    курсора через `update_widget_hover` по тому же правилу «сверху вниз»,
    что и путь клика, замораживается при установленном `MODAL` и очищается в
    `leave()`; смена наведения делает dirty только окна-хосты. Действие
    `Open` пропускает `focus_window` хоста — иначе хост поднялся бы над
    только что размещённым окном и зеркально исказил его восстановленный
    v2.38.31 z-ранг, — а `CloseHost` переиспользует `close_window`, сохраняя
    модальный гейт заметки.
    С v2.38.34 рабочий стол также владеет окном **Network**
    (`WinKind::Network`, метка `Net` в «Пуске», открывается и по
    глобусу-трией в правом конце таскбара): кнопки режимов `DHCP`/`Static`,
    пять полей адреса (`ip`/`mask`/`gw`/`dns1`/`dns2`) + `Apply`/`Test` и
    Wi-Fi-группа (`ssid`/`pass` + `Scan`/`Connect`/`Disc`). Поля —
    window-local таблички `Widget::Button` с `WidgetAction::NetField(0..6)`:
    клик фокусирует поле, клавиши печатают в `NET_DRAFT` через
    `net_type`/`net_append`/`net_backspace` (`Tab` гоняет `net_focus_next`),
    а открытие окна загружает живую конфигурацию через `net_draft_load`.
    Кнопки команд выполняют `net_button`: `Dhcp`/`Static` переключают режим
    черновика, `Apply` разбирает все пять dotted-quad значений и коммитит
    через `net::apply` (с сохранением в CMOS; статус `applied` либо
    `bad ip`/`bad mask`/`bad gw`/`bad dns1`/`bad dns2`/`need ip`), `Test`
    зовёт `net::recheck`, `Scan` — `wifi::start_scan` (честный в real-режиме с v2.38.36: без детектированной карты отказывает `[wifi] scan: no wifi card` и статусом окна `no card`; строка статуса также показывает `wifi card: <bdf>` / `card: none`, а пустой результат рисует подсказку `no networks found`),
    `Connect` — `wifi::connect` (`wait` при приёме, текст ошибки радио при отказе),
    `Disc` — `wifi::disconnect`. Доказательные строки: `[gui] tray globe
    click`, `[gui] widget Network/<label> click`, `[serial] [gui] net
    connect: no ssid` / `net connect err: … (ssid len N)`, плюс след
    посимвольного ввода `[serial] [gui] type '<c>' focus=N ssid_len=N
    pass_len=N` и `[serial] [gui] backspace focus=N len=N` (v2.38.36).
    С v2.38.35 есть также окно **Task Manager**
    (`WinKind::Tasks`, метка `Tasks` в «Пуске», 440×248 в
    (460,140)): полоса-шапка `Task`/`Status`, по одной строке 16 px на
    каждое открытое окно в порядке `taskbar_slots` (ранги) — выбор через
    `task_row_click` в `TASK_SEL: Option<WinKind>`, отрисовка плиткой
    `BAR_ON`, статус `Running`/`Background`/`Minimized` — и таблички
    `End Task`/`Switch To` (`TaskBtn`) над подвалом `N tasks` с
    `sel <label>`. `task_end` идёт через `close_window` (действует
    модальный гейт заметки v2.38.30), `task_switch` фокусирует и
    поднимает через `focus_window`, оба логируют `no selection` /
    `<label> gone`, когда `TASK_SEL` устарел; `task_key` маппит `Enter`
    на сфокусированном Tasks-окне в `task_switch`.
    `kind_rank`/`taskbar_slots` ставят `Tasks` на ранг 5 (клиенты 6+id),
    так что список зеркалит панель задач, а `spawn()` логирует
    `[gui] open <label>` для каждого окна. Доказательные строки:
    `[gui] open Task Manager`, `[gui] tasks select <label>`,
    `[gui] tasks end <label>`, `[gui] tasks switch <label>`,
    `[gui] widget Task Manager/End Task|Switch To click`.
    С v2.47.0 слой виджетов стал обобщённым и интерактивным: к `Widget::Button`
    добавились `TextField`, `List`, `Scrollbar` и `Menu`, причём каждый вариант
    отдаёт оконно-локальный бокс через `widget_rect`, поэтому `widget_index_at`
    (хит-тест), `hover_widget` и `draw_widgets` единообразно покрывают все типы.
    С v2.63.0 к ним добавляется шестой вариант `Widget::Checkbox` (бокс с
    подписью и `id` в форму хозяина): `draw_checkbox` рисует бокс (рамка
    `BAR_ON` при наведении) и галочку во включённом состоянии, `widget_click`
    переключает бит через `form_check_toggle` и пишет `[gui] widget
    <host>/check <label> = on|off`. С v2.64.0 добавляется седьмой вариант
    `Widget::Radio` (круглый бокс с подписью, `group` + `value`): `draw_radio`
    рисует контур круга и заполненный кружок у выбранного, `widget_click`
    выбирает значение через `form_radio_select` и пишет `[gui] widget
    <host>/radio <label> = <value>`. С v2.67.0 добавляется восьмой вариант
    `Widget::Slider` (трек с бегунком, `id` + `min`/`max`): `draw_slider` рисует
    подпись, утопленный трек с заливкой `BAR_ON`, бегунок `TEXT` и числовое
    значение; клик по треку отображает x в значение через `slider_value_at` и
    сохраняет его через `form_slider_set`, логируя `[gui] widget
    <host>/slider <label> = <value>`.
    С v2.68.0 текстовые поля хранят UTF-8-скаляры: `form_type_char` дописывает
    закодированный символ, если он целиком влезает в `LAB_FIELD_CAP`,
    `form_backspace` удаляет один целый скаляр, а отрисовщик поля идёт по
    символам, считая каретку в скалярах; с v2.81.0 каретка и `draw_field_text`
    используют общий путь глифов с кириллицей вместо `?` для не-ASCII.
    Внутренний буфер обмена ядра (`CLIP`,
    128 байт) обслуживает `Ctrl+C`/`Ctrl+X`/`Ctrl+V` через
    `form_clip_copy`/`form_clip_cut`/`form_clip_paste`, логируя
    `[gui] clipboard copy|cut|paste <n> bytes` и показывая `no field`/`full`/`empty` в
    строке статуса. `CTRL_DOWN` (interrupts) питается make/break байтами PS/2
    `0x1D`/`0x9D` (включая `0xE0`-префиксные формы) и байтом модификаторов HID
    из `xhci::harvest_report`, который заодно возвращает Shift на
    USB-клавиатурах.
    С v2.65.0 состояние виджетов принадлежит каждому хозяину: `WidgetForm`
    (поля, длины, фокус, меню, выбор, прокрутка, `msg`, `checks`, `radios`,
    `sliders`) живёт в глобале `FORMS: [WidgetForm; FORM_SLOTS]`, по слоту на
    встроенное окно (`form_slot`), поэтому чекбокс Network не пересекается с
    Widget Lab. `form_load` сбрасывает слот при открытии; `form`, `form_msg`,
    `form_check_toggle`, `form_checked`, `form_radio`, `form_radio_select`,
    `form_slider` и `form_slider_set` — общие для всех хостов.
    На десктопе появляется окно **Widget Lab** (`WinKind::Widgets`, метка
    `Widgets` в «Пуске», 460×332) с двумя текстовыми полями,
    прокручиваемым списком из десяти строк с привязанным скроллбаром,
    выпадающим меню `Edit`, кнопкой закрытия, двумя чекбоксами
    (`Auto-connect` / `Persist config`), двумя радиокнопками (`DHCP` /
    `Static`) и слайдером `Volume`. Ввод с клавиатуры маршрутизируется
    фокусному хозяину до глобальных горячих клавиш через `widget_key`
    (вызывается из `handle_scancode` после `task_key`/`net_key`): с v2.66.0
    маршрутизация общая — работает для любого окна с таблицей виджетов, кроме
    Network — и диспетчеризуется через `form_focus_cycle`/`form_backspace`/
    `form_type_char`/`form_enter`: `Tab` перебирает фокус виджетов, `Backspace`
    редактирует фокусное поле, `Enter` активирует (поля подтверждают, список
    берёт выбранную метку), печатные символы набираются в поле или двигают
    список `n`/`p`, а `Escape` закрывает открытое меню. Клики мыши по вариантам
    диспетчеризует `widget_click` (фокус поля, выбор строки списка, прыжок
    скроллбара, переключение меню, переключение чекбокса, выбор радио,
    установка слайдера), а открытое меню обслуживается первым через
    `lab_dropdown_click` — клики по его пунктам запускают `lab_menu_pick` до
    обычного хит-теста, клик по плитке переключает, клик в другом месте закрывает
    и проходит дальше. Место под вид `Widgets` (ранг 6, клиенты 7+id)
    зарезервировано в `kind_rank`/`taskbar_slots`. Линии доказательства:
    `[gui] widget Widget Lab/field <n> focus`, `[gui] widget Widget Lab/list row
    <n> click`, `[gui] widget Widget Lab/menu open|close`, `[gui] widget Widget
    Lab/menu item <n> click`, `[gui] widget Widget Lab/check <label> = on|off`,
    `[gui] widget Widget Lab/radio <label> = <value>`, `[gui] widget Widget
    Lab/slider <label> = <value>`.
    С v2.81.0 оболочка — стартовый экран: `idle_loop()` вызывает `gui::enter()`
    до опроса драйверов. `start_entries()` собирает отсортированный каталог из
    `START_BASE` (системные окна и действия), девяти `APPS`, живых ring-3-клиентов
    `SYS_GUI` и системных действий (`Обновить всё`, `Консоль ядра`);
    `start_matches()` складывает регистр и у латиницы, и у кириллицы. С v2.83.0
    `open_app()` направляет Dashboard в настоящее окно `WinKind::System`, сеть
    (включая программный псевдоним) — в `WinKind::Network`, Терминал — в консоль
    ядра, а App Store, Files, Калькулятор и Текстовый редактор — в настоящие
    окна ядра `WinKind::App(id)`; `app_load()` готовит их состояние, а
    `app_key()`/`app_click()` обрабатывают клавиши и клики по телу окна раньше
    общих виджетов. `STORE_IDS` — каталог App Store из шести программ,
    `FILE_ROWS` — живые псевдофайлы `system/...`, `CALC` — целочисленное состояние
    калькулятора. Proof-строки: `[gui] store select`, `[gui] store launch`,
    `[gui] app launch` и `[gui] calc result`. По-настоящему хостовые пункты
    по-прежнему открывают окна-уведомления: в голом ядре нет запуска процессов,
    WebView, аудио и LLM. Хостовый крейт `aios-gui` — устаревший справочный код,
    его никогда не запускают ни ядро, ни хостовые TUI.
- `main` — читает ответы framebuffer/rsdp/hhdm Limine, выполняет self-check чтения framebuffer, фильтрует карту памяти в `[MemRegion; 64]`, затем инициализирует память и кучу, GDT/TSS (с перезагрузкой CS/SS), IDT, PIC и PIT, поднимает LAPIC-таймер (`lapic::init()`) и маскирует PIT IRQ0, когда тот жив, `sti`, печатает на экран показания живости `[probe] irq32_seen/ticks delta`, перечисляет шину PCI и поднимает драйверы хранилища AHCI и NVMe (после `sti`, чтобы исключения обрабатывались), планирует kernel worker и, с v2.82.0, запускает пять ring-3 задач только при включённом feature `ring3-demo` (иначе они ждут выбора `Пуск → Ring-3 демо`); задачи — четыре IPC/GUI-демо плюс, с v2.51.0, клиент socket-пинг-понга. Далее объявляет атомарные статики статуса драйверов `G_AHCI`/`G_NVME`/`G_XHCI` (проставляются init-кодом драйверов) и входит в `idle_loop`, где работают квадрат heartbeat и `gui::render()` (или
  `tui::render()` после выхода из рабочего стола); с v2.81.0 `idle_loop()` вызывает
  `gui::enter()` при загрузке. Клавиши и отчёты мыши маршрутизируются владельцу экрана: `gui::handle_scancode`/`gui::on_mouse`, когда `gui::active()`, иначе `tui::*`. Байты клавиатуры приходят из 256-слотового FIFO, сливаемого через `while let Some(sc) = key_pop()` в начале каждой итерации (v2.38.36 — фильтрация extended/shift происходит уже по сливаемому потоку, поэтому простой цикла больше не теряет burst-ввод из-за прежнего перезаписа одиночным слотом), а на edge PS/2-кнопки бандл сначала сбрасывает весь накопленный `mouse_dx`/`mouse_dy` (до 64 шагов `drain_axis`) до вызова `on_mouse`, так что press диспатчится по устоявшейся позиции курсора, а не посреди бэклога (v2.38.36, фикс пропущенных кликов). Дельты мыши считываются **каждую итерацию idle-цикла** — обе полосы безусловно читают `ps2::mouse_dx`/`mouse_dy` и `xhci::take_mouse_delta`, так что carry-over-остаток клампа 256 px применяется даже без новых отчётов, а пакеты, пришедшие за время, пока цикл был занят, накапливаются, а не перезаписывают друг друга (v2.38.30) — а с v2.38.38 защёлкнутый press-edge (`take_button_edges()` в обеих полосах) или изменение состояния кнопки (`btn_changed = seq_changed && buttons != last`) доставляются через новый `deliver_pointer`, который воспроизводит до трёх состояний (залипший release → press с dx/dy → финальный release), так что пара press+release, склеенная между двумя итерациями цикла, всё равно доходит до `gui::on_mouse` (BUGS v2.38.38); сам кламп применения живёт в драйверах (`MOUSE_MAX_STEP`, v2.38.35). Прежде чем что-то логировать или рендерить, цикл спрашивает владельца экрана `gui::input_allowed`/`tui::input_allowed` (гейт runaway, v2.38.33): 60 пакетов подряд, толкающих уже прижатый к краю курсор дальше в тот же край, подавляют ввод указателя до первого пакета движения прочь от края, а само движение агрегируется максимум в одну serial-строку на бандл в секунду (`MoveLog`, статики `USB_MOVE`/`PS2_MOVE` — `[serial] usb mouse +N pkts dx=… dy=…`); смена кнопок сохраняет поотчётные строки. В serial логирует селекторы передачи от Limine (`boot selectors cs/ss/ds`) и выбранный источник тиков (`timer=LAPIC|PIT`). С v2.38.13 раздел framebuffer дополнительно логирует виртуальный адрес и `framebuffer pages: N present, M writable, K total` (`memory::verify_region`), выполняет `write_volatile`-тест `direct_test` со `sfence` и readback пикселя (0, 0) плюс OK-зелёную пробу, затем `psf_check()` синтезирует PSF2-поток из `font8x8::BASIC`, парсит его обратно, проверяет round-trip глифа 'A' и рисует его в центре нижней полосы экрана.
  - Panic-обработчик печатает версионированную строку `KERNEL PANIC [semver]: …`
    во framebuffer-консоль и serial, рисует отчёт на экране без аллокаций через
    `console::panic_report("KERNEL PANIC: ", …)` с `version::KERNEL`, затем уходит
    в `halt_loop`. Фатальные прерывания идут тем же путём с префиксом
    `FATAL [semver]`, поэтому по скриншоту видна точная сборка, в которой
    произошёл сбой.
- `aios-kernel-run`: собирает ядро с включённым путём Limine (`cargo build --target x86_64-unknown-none --release --features limine`, с v2.78.0), раскладывает дерево Limine ISO (`boot/aios-kernel`, `boot/limine.conf`, CD-стадии Limine BIOS/UEFI, `EFI/BOOT/BOOTX64.EFI`), создаёт гибридный ISO через `xorriso` + `limine bios-install` и байт-в-байт идентичный USB-образ `out\aios-kernel-usb.img` (isohybrid — загружается на legacy BIOS через MBR Limine и на UEFI через ESP). Грузит в QEMU — по умолчанию UEFI/OVMF (настоящий GOP), иначе legacy BIOS; `AIOS_QEMU_USB=1` подключает USB-образ как mass-storage вместо CD-ROM. Переопределения: `AIOS_LIMINE_DIR`, `AIOS_LIMINE_TOOL`, `AIOS_XORRISO`, `AIOS_QEMU`, `AIOS_QEMU_UEFI`, `AIOS_QEMU_USB`, `AIOS_SKIP_QEMU`. С v2.84.0 сборщик читает `[package] version` из `aios-kernel/Cargo.toml` и сводит его к релизной ветке через `release_train()` (`2.84.0` → `v2.84+`), записывая `limine.conf` с двумя записями: `/AIOS kernel v2.84+ (current, Limine + GOP)` и `/AIOS kernel v2.79 rollback (Limine + GOP)`. Текущий ELF раскладывается как `boot/aios-kernel`; резервный по умолчанию берётся из `out/aios-kernel-v279.elf` через `AIOS_FALLBACK_KERNEL` (сборщик требует наличия файла, поэтому чистая копия репозитория должна предоставить настоящий ELF v2.79), а его подпись задаётся через `AIOS_FALLBACK_VERSION` (по умолчанию `v2.79`). Устаревший `out\aios-kernel.iso` удаляется перед каждой сборкой, после чего `verify_hybrid_image()` проверяет сигнатуру MBR `55 AA` и дескриптор ISO9660 `CD001` по адресам `0x8001`/`0x8801`/`0x9001`, поэтому урезанный или негибридный образ не попадает ни в QEMU, ни в скрипт прошивки.
- Запись на реальную USB-флешку: `scripts\flash-usb.ps1` от имени Администратора (записывает образ на `\\.\PhysicalDriveN`, проверяет SHA-256 и сигнатуру MBR `55 AA`). Скрипт блокирует и отмонтирует тома цели (`FSCTL_LOCK_VOLUME`/`FSCTL_DISMOUNT_VOLUME`), открывает диск с `FileShare.ReadWrite`, дополняет каждую запись до границы сектора и вызывает `Flush($true)`, затем снимает блокировки и выполняет ограниченный по времени recovery `Update-Disk` + `pnputil /rescan-disks` (каждая операция — в `Start-Job` с таймаутом `AIOS_FLASH_RECOVERY_TIMEOUT`, по умолчанию 30 с) плюс восемь попыток ожидания готовности диска. Весь образ повторно хешируется с raw-диска и сравнивается с исходным SHA-256 перед проверкой MBR; при запуске без прав скрипт сам перезапускается с повышением, записывая весь вывод в один лог `%TEMP%`, который родительский процесс выводит один раз. Чистый гибридный образ AIOS bare-metal загружается и на legacy BIOS, и на UEFI ноутбуках. Для нативного образа UEFI-only (GPT) используйте вместо него `aios-install`: он проверяет структуру GPT/ESP до записи (см. раздел «Собственная цепочка загрузки UEFI» выше).
- Проверка: `cargo run --release` в `aios-kernel-run` — в serial выводятся `aios-kernel: Limine handoff`, смещение HHDM, RSDP, `framebuffer = WxH pitch=… bpp=… usable=true`, `framebuffer self-check OK (readback 0x…)`, `usable memory regions = N`, `paging selftest OK.`, `heap: …`, `interrupts online.`, `[sched] ready`, `five ring-3 tasks armed`. QEMU `screendump` подтверждает, что консоль действительно рисуется (гистограмма пикселей текста/фона).

### Двухядерный Limine-образ: историческая флешка и текущий результат сборщика (v2.84.0)
- **Историческое, устаревшее (v2.81.0 → патч v2.84.0):** перепрошитая флешка
  Kingston использовала байтовую правку проверенного гибридного ISO, а не полную
  пересборку через `xorriso` (хостовые бинарники были недоступны). В `/boot`
  лежали `AIOS_KER.;1` (Rock Ridge `aios-kernel`, экстент 32) для desktop ELF и
  `AIOS_KER2;1` (Rock Ridge `aios-ker2`, экстент 600) для старого ELF v2.79, а
  `limine.conf` размером 202 байта в ISO-секторе 1780 задавал `timeout: 3`.
  Обновление v2.84.0 заменило только ELF по умолчанию в ISO LBA 32: 1 054 032
  байта (SHA-256 `BA534ECF802153D64D70D04FD0BBE9A5A4CD10E2FE2D7FFA1D3D4C0E1CB563D9`),
  SHA-256 всего образа
  `C172387199711E544EF5CEE1F112170B49096C77C8284F07064EBD78B0D43929`; резервное
  ядро v2.79 и его хеш не менялись. Флешка прочитана целиком (SHA-256
  `8E0CE7F3F3CF5A0B66D6D9CCD2CFC196A5E74A6DE3540A4C55DEB98D0ED296A5`) с
  ELF-магией в обоих смещениях, а QEMU Legacy BIOS подтвердил новую строку лога
  NIC и полный цикл DHCP discover/request/bound до физической записи.
- **Почему это устарело:** байтовая правка перезаписала UEFI-стадию в LBA 32,
  поэтому физическая флешка по-прежнему работает только с Legacy BIOS, а её
  ручное меню (`/AIOS Desktop v2.84 (native kernel)` /
  `AIOS Legacy v2.79 (old GUI)`) больше не совпадает с тем, что генерирует
  сборщик. Новой пересборки здесь не выполнялось: на этом хосте отсутствуют
  `xorriso.exe` и бинарники Limine, поэтому раздел описывает существующий
  артефакт, а не заново проверенный образ.
- **Копия для проверки GUI (v2.84.0):** для исправления зазоров и отсечения текста
  свежее релизное ядро (`1 046 336` байт, SHA-256
  `FD69FF2DDE3969C74BC9CEDAFC3F4E2DB7C3A48A0CA8724987F6F7C7B713EE17`) записано
  в LBA 32 ISO копии образа выше, а запись каталога `AIOS_KER.;1` обновлена под
  новую длину. Копия
  (`out/aios-kernel-usb-limine-v284-gui.img`, `4 683 776` байт, SHA-256
  `3B9DA3E1D0E890DF8FCA5FD87FB2C15738E550E28FCEA0F6E993D4DE6050525F`)
  проверялась в Legacy BIOS QEMU (GOP `1280x800`, DHCP bound, окна открыты
  клавишами `F4`/`F5`/`F9`), а затем была записана на физическую флешку (см.
  ниже); она наследует описанное выше ограничение UEFI.
- **Физическая прошивка (исправление GUI v2.84.0):** образ выше записан на Disk 3
  после явного согласия. `scripts/flash-usb.ps1` сообщил об успешной записи, но
  при чтении назад вернул прежний образ: все три попытки блокировки/отмонтирования
  томов завершились неудачей. Вместо него применён проверенный путь
  `diskpart clean` + P/Invoke `WriteFile`/`FlushFileBuffers`
  (`boot/out/flash-gui-disk3.log`). Полный read-back всех 4 683 776 байт дал
  SHA-256 `3B9DA3E1D0E890DF8FCA5FD87FB2C15738E550E28FCEA0F6E993D4DE6050525F`,
  хеш ELF в LBA 32
  `FD69FF2DDE3969C74BC9CEDAFC3F4E2DB7C3A48A0CA8724987F6F7C7B713EE17`, размер
  записи каталога `1 046 336`, MBR `55 AA` и ручное меню v2.84. Для поддержки
  UEFI всё ещё нужен чистый гибридный образ.
- **Физическая прошивка (после v2.84.0, диагностика page fault):** после явного
  согласия диагностический образ
  `out/aios-kernel-usb-limine-v284-pfdiag.img` (`4 683 776` байт, SHA-256
  `3DB811A18CB93C69B17B3776F63D5CDC8177B7FD8E62F9F9F8044866A43692E9`; ELF ядра
  `1 046 488` байт, SHA-256
  `BD2D6AEEDCE91159877BFBADF0DFDD068D22EA86276F4EB0C91FD5CCE95CDB02`) заменил
  образ с GUI-исправлением на Disk 3 тем же проверенным путём `diskpart clean` +
  `WriteFile`/`FlushFileBuffers` (`boot/out/flash-pfdiag-disk3.log`). Полный
  read-back совпал по SHA-256; magic ELF в LBA 32 `7F 45 4C 46`, хеш полезной
  нагрузки `BD2D6AEE…C95CDB02`, размер записи каталога `1 046 488`, MBR `55 AA`
  и ручное меню подтверждены независимым верификатором
  (`boot/out/verify-pfdiag-disk3.log`). Дополнительный шаг `ReadFile(LBA32)` в
  самом скрипте прошивки упал на уже использованном handle после цикла подсчёта
  SHA; независимый верификатор всего диска прошёл, поэтому образ на флешке
  корректен. Для поддержки UEFI по-прежнему нужна чистая гибридная пересборка.
- **Актуальное состояние (результат сборщика, v2.84.0):** `aios-kernel-run`
  раскладывает два ELF в чистый гибридный ISO/IMG и пишет меню
  `/AIOS kernel v2.84+ (current, Limine + GOP)` (по умолчанию) и
  `/AIOS kernel v2.79 rollback (Limine + GOP)`, проверяя образ через
  `verify_hybrid_image()` (MBR `55 AA` и ISO9660 `CD001`). Чтобы вернуть
  поддержку UEFI + BIOS на физической флешке, требуется новая прошивка
  сгенерированным образом; до этого флешка остаётся описанным выше
  Legacy-BIOS-only артефактом.

## Загрузочный установщик AIOS (Live ISO → диск, v2.33.0)

Загрузочный ISO собирается из `iso/stage` (BIOS/UEFI CD-образы Limine, `limine.conf`,
`vmlinuz`, самопальный `initramfs.cpio.gz`) с помощью `xorriso` (El Torito BIOS + UEFI)
и `limine.exe bios-install` (isohybrid MBR). Меню Limine из двух пунктов:

- **AIOS Live (ядро TUI)** — `rdinit=/init`: грузит ровно тот initramfs, в котором
  проводятся live-тесты с флешки/ISO.
- **AIOS Installer** — `rdinit=/installer` с опциональными флагами автоматизации
  `aios.target=<dev>` / `aios.yes`.

В обеих записях стоит `console=ttyS0 console=tty0` (tty0 последней): `/dev/console`
резолвится в VGA-экран, поэтому TUI ядра, установщик и аварийный шелл видны на реальном
железе (одиночный `console=ttyS0` оставлял экран ноутбука чёрным — исправлено в v2.33.2).

### Пре-инит загрузчик GPU-модулей (v2.33.3)

Запись `/init` в initramfs — это не бинарник `aios-init`, а шелл-скрипт `aios-loader`,
который выполняется *до* юзерспейса и решает конфликт: fbcon нужен в фреймбуфере, а
GPU-модуль создаёт его только через DRM-драйвер. Загрузчик:

1. монтирует `proc`, `sys`, `dev` (busybox `mknod /dev/console c 5 1`), затем `mkdir -p /dev/shm`
2. делает `modprobe` всех пяти x86 GPU-DRM-драйверов (`radeon`, `nouveau`, `gma500_gfx`,
   `i915`, `amdgpu`); отсутствующие или упавшие молча пропускаются
3. извлекает `dd`-ом оригинальный ELF `aios-init` — **встроенный прямо в скрипт** после
   маркера `#BIN\n` — в `/tmp/aios-init`, используя запечённые фиксированной ширины
   (10 цифр) смещения: `BINOFF:=000000001234` (skip= байтовое смещение маркера в `/init`),
   `BINSZ:=0000000382240`
4. `chmod 755 /tmp/aios-init` и `exec /tmp/aios-init`

Оригинальный `aios-init` по-прежнему остаётся PID 1, остальная цепочка загрузки
(`aios-init` → `bring_userspace()` / sfs-up.sh → `/system/aios-core` → ядерный TUI)
не меняется; загрузчик прозрачен.

### Ограничения распаковщика initramfs ядра

Работает **только** in-place замена содержимого записи `/init`:
- Распаковщик ядра пишет записи до первого `TRAILER!!!`; записи, добавленные после,
  молча игнорируются (файл физически есть в архиве, но ядро не создаёт его на диске).
- Удаление или переименование записи `/init` (смена имени) заставляет ядро отвергнуть
  весь архив с `VFS: Cannot open root device`.
- Поэтому скрипт заменяет исходные байты payload записи `/init` на том же смещении в raw
  cpio; остальные 6657 записей архива остаются нетронутыми.
- `BINOFF`/`BINSZ` должны использовать **поля фиксированной ширины** (`0000000000`) — замена
  с переменной длиной сдвигает длину скрипта, меняет позицию маркера `#BIN` и приводит к
  извлечению неверных байтов; тогда `ash` видит ELF как скрипт и парсит его построчно.

### Runtime установщика

`/init` или `/installer` были PID 1 интрамфса упразднённого Linux Live-пути;
обёртка установщика (`/installer`) поднимала proc/sys, `tmpfs` `/dev` с busybox
`mdev -s`, затем `exec`-овала старый `aios-install`
(`aios-core/live/aios-install`, удалён в v2.77.0). Подъём устройств был без udev:

1. модули ядра storage/filesystem грузятся через busybox `modprobe` (в initramfs
   лежит дерево модулей ядра, включая `modules.dep`/`modules.symbols`),
2. `mdev -s` пересоздаёт `/dev`, а `ensure_nodes()` `mknod`-ит партиции напрямую из
   `/proc/partitions`, потому что devtmpfs гоняется с загрузкой модуля.

### Установка на целевой диск

`aios-install` валидирует устройство, затем:

1. `wipefs -a` + `sfdisk --wipe always` с GPT-разметкой из трёх разделов:
   **1 МиБ BIOS boot** (`type=21686148-6449-6E6F-744E-656564454649` — обязателен,
   чтобы GRUB смог встроить BIOS `core.img` на GPT), **512 МиБ EFI System** и
   остальное как **ext4 root**.
2. принудительное форматирование: `mkfs.fat -F 32 -I` (EFI) и `mkfs.ext4 -F` (root) —
   устаревшие сигнатуры ФС никогда не блокируют подтверждением.
3. `tar`-копия живого rootfs (initramfs, включая `boot/vmlinuz` и встроенный
   `boot/initramfs.cpio.gz`) на целевой диск.
4. выбор опциональных блоков (`install_blocks`): читает каталог
   `/usr/share/aios/blocks` и выбор по умолчанию `/etc/aios/blocks.conf`,
   спрашивает `all`/`none`/список имён (по умолчанию берётся из conf; `AUTO=1`
   использует значение по умолчанию без вопроса), копирует выбранные
   `{name}_{version}.wasm|.bin` (+ sidecar `.json`) в `/blocks` целевой системы
   (каталог, который ядро сканирует через `AIOS_BLOCKS_DIR`) и записывает
   итоговый выбор в `/etc/aios/blocks.conf` на цели.
5. GRUB ставится для **BIOS** (`--target=i386-pc`) на целевой диск и для **UEFI**
   (`--target=x86_64-efi --removable`) в `/boot/efi`; stderr уходит в `grub-err.txt`
   на целевом диске, `rc` печатается.
6. `grub.cfg` пишется heredoc **без кавычек**, чтобы `${DEV}` раскрылся шеллом в
   реальное устройство (`root=/dev/sda3 console=ttyS0`,
   `initrd /boot/initramfs.cpio.gz`).

Установленный диск дальше грузится SeaBIOS/GRUB → ядро → initramfs → `aios-init`
(PID 1) → `/system/aios-core` → AIOS TUI. Полный цикл проверен в QEMU, ISO записан
raw-записью на физическую USB-флешку.

Каждый шаг с меткой времени пишется в журнал установки `/aios-install.log` в корневом
разделе целевого диска (завершаясь маркером `AIOS INSTALL COMPLETE`), так что
упавшая физическая установка диагностируется повторной загрузкой целевого диска.
`aios.verbose` в командной строке (или `--verbose` при прямом запуске
`aios-install`) дополнительно выводит финальную GPT-таблицу через
`sfdisk --dump` перед копированием. Если корневой раздел уже несёт метку AIOS,
интерактивный запуск потребует `Type YES to FORCE reinstall` перед затиранием;
безлюдный путь `aios.yes` продолжает без лишнего запроса. Корневой раздел всегда
занимает остаток диска, а его фактический размер фиксируется в журнале.

## Блоки официального store и `block run`

Каталог store принимает две формы: исходный массив `Vec<ManifestInfo>` и объект,
который создаёт `aios-official-store/tools/indexgen` (`schema_version`, `name`,
`description`, `blocks`). `aios_store::catalog::parse_index` приводит обе формы к
`ManifestInfo`: берёт английский текст из локализованного объекта `description`,
срезает префикс `sha256:` у `wasm_hash` и преобразует псевдонимы прав вида
`filesystem:read` в `CAP_FS_READ`. `StoreSource::block_url` теперь указывает на
`blocks/<name>/block.wasm` и при HTTP 404 откатывается на плоский URL
`blocks/<name>.wasm`; локальные источники сначала читают вложенный `block.wasm`
и sidecar `manifest.json`. Если в манифесте нет `wasm_sha256` или
`wasm_size_bytes`, загрузчик каталога вычисляет их из скачанного бинарника, чтобы
`store install` всё равно мог проверить блок.

Команда `block run <name> <text...>` замыкает путь консоли от установки до
выполнения. `HostBackend` находит установленный бинарник через `StoreManager`,
один раз регистрирует его в общем `BlockRegistry` и хранит
`BTreeMap<String, BlockId>`, чтобы повторные вызовы использовали тот же
экземпляр. Остаток строки после имени блока упаковывается в
`IpcPacket::new(..., CommandId::Custom, Payload::Custom(name, bytes))` и
доставляется через `BlockExecutor::dispatch_wasm_packet`; ответы `Payload::Text`
печатаются по строкам. `HostBackend::set_executor` внедряет исполнитель
оркестратора, а `TuiApp::new` его передаёт, поэтому блоки, запущенные из
консоли, сохраняют состояние между командами и используют ту же живую WASM-среду,
что и блоки, загруженные при старте. Именно поэтому
`block run calculator ans+1` работает после `block run calculator 2+3*4`, в
отличие от `wasm call`, который передаёт пустой payload и создаёт новый
экземпляр при каждом вызове.

## Сетевые настройки, хостовые API link и обеспечение NIC (v2.84.0)

`aios-net-config::NetworkConfig::apply_updates` теперь принимает полную форму
`dns`: объект с `primary`, `secondary` и `search_domains`, строку-адрес, массив
поисковых доменов или `null`. Метод также проверяет и заменяет `interfaces`
массивом типизированных объектов `InterfaceConfig`, отклоняет некорректные поля,
сохраняет версию конфига и поднимает её минимум до `2`. Хостовый DHCP-клиент
держит один transaction id между DISCOVER и REQUEST, читает его из правильного
смещения заголовка DHCP (`p[4..8]`) и игнорирует ответы другой транзакции.

`aios-sys-control::host_net` разбирает вывод `netsh interface ipv4 show config`
в Windows и `ip -4 addr show` плюс sysfs в Linux в строки `AdapterState`;
парсеры чистые и не требуют железа. `NetManager` получил `recheck()`,
`adapters()` и `lease()`: recheck выполняет новый сетевой обмен и обновляет
кэш link/lease, adapters сообщает обнаруженные ОС-интерфейсы, а сохранение lease
больше не затирает соседние поля конфига. `HostBackend` представляет их как
`console net recheck|apply|adapters|lease`; `net apply` сначала сохраняет
конфиг, а затем делает recheck, вместо того чтобы выдавать кэш за свежий статус.

`aios-console::net_text` — общий модуль, совместимый с `no_std`, для строгого
разбора dotted-quad, обновлений `key=value`, валидации и стабильного вывода
статуса/lease. `KernelBackend` использует его для
`net show|config|status|lease|set|apply|validate|reset|adapters|recheck`, поэтому
ядерный текстовый путь покрыт тестами рабочего пространства; `net lease`
сообщает `no DHCP lease acquired in this session`, только когда lease ещё нет.

`aios-autohal::nic` классифицирует PCI-сетевые функции как
`NicClass::Ethernet` (класс `0x02`, подклассы `0x00`/`0x01`) или
`NicClass::Wifi` (`0x02/0x80`). `SupportedHardware::for_class` добавляет
сопоставление по маске класса, а точные записи vendor/device остаются доступны;
каталог содержит `NIC_WAT`, а также точные и классовые NIC-драйверы, а
`driver nics` показывает найденные карты, драйвер, состояние и статус токена.
`ProvisioningEngine` теперь сохраняет подписанный `CapabilityToken` для каждого
живого экземпляра, открывает его через `tokens()`/`token()` и удаляет при
откате, удалении или деинсталляции.

`aios-kernel::nic` предоставляет `Kind::name()`, `NicDeviceInfo`,
`SUPPORTED_DEVICES` и `describe()`, поэтому лог загрузки называет и точный тип
драйвера, и бэкенд. Все эти дополнения проверены статически; новый запуск NIC
на реальном железе не заявляется.

## Архитектура голоса (2026-11-27)

Хост добавляет `aios-voice` как входной слой перед существующим путём EasyLang/WASM. `MicrophoneSource` передаёт моно PCM 16 кГц в `CaptureController`, который реализует VAD и измерение уровня; `SttEngine` абстрагирует локальный Candle Whisper и HTTP-бэкенды, совместимые с Whisper, а `UnavailableSttEngine` является безопасным значением по умолчанию. `VoicePipeline` публикует фазы в broadcast-шину, поэтому REST, WebSocket и оба интерфейса видят одинаковое состояние.

`VoiceRuntime` моста потребляет события `Transcribed` в фоновом потоке. Генерация делегируется `ScriptGenerator`: он использует `ScriptLlm` (адаптер реальной LLM, мок или детерминированный офлайн-фолбэк), обеспечивает грамматику EasyLang «только код», повторяет при ошибке валидации и компилирует через `WorkflowCompiler`. Сгенерированный WASM не считается доверенным: `AutoManifestGenerator` извлекает возможности из защищённых импортов `aios`, `AccessControlLayer` проверяет их по токену голосовой сессии, а `WasmBlock::instantiate_with_policy` устанавливает такой же `HostPolicy`. Host-вызовы работают только в режиме аудита: они не запускают процессы и не открывают сетевые соединения, а URL не http/https для `net_check`/`browse` отклоняются.

Привилегированные скрипты переводятся в `AwaitingConfirmation` со списком разрешений, и только явный запрос с `confirm:true` выпускает токен с необходимыми возможностями. Перед подтверждением `aios_builder::diff::script_diff()` вычисляет ограниченный LCS-diff относительно последнего успешно выполненного скрипта; событие, REST-статус и Studio показывают этот diff, чтобы пользователь видел точные изменения. `SYSTEM_PROMPT` генератора содержит подсказки по ключевым словам RU/EN для `net_check`, `browse`, `console_cmd` и `notify`, но на выходе всё равно разрешены только команды EasyLang. Успешные рабочие процессы сохраняются вместе с исходной фразой для трассируемости.

Защищённые host-вызовы каждого выполнения берутся из `audit_log` объекта `WasmBlock` и дублируются в `FlightRecorder` как события `Syscall` и в `TelemetryStore` как метрика `voice.host_calls`. Они также дописываются ограниченными JSON-строками в `<AIOS_DATA_DIR>/voice_audit.jsonl` и доступны через аутентифицированный `GET /api/v1/voice/audit`, поэтому журнал переживает перезапуск процесса.

## Инвентарь аудиоустройств и драйверов (2026-11-27)

`aios-hal::audio` перечисляет активные входные/выходные устройства через `cpal` и объединяет их с данными драйверов ОС в `AudioDevice { name, direction, bus, driver, vendor_id, device_id, id_source, class_code, is_default, sample_rate, channels, sample_format }`. Linux читает из sysfs PCI-класс `0x04` и USB-аудиокласс `03`, Windows разбирает вывод `wmic sounddev ... /format:list`. Список хранится в `HardwareProfile::audio_devices`, сериализуется через IPC и выводится `aios-console` строкой `audio`.

Сопоставление идёт по оценке совпадений, а не по первому совпадению: алиасы `/proc/asound/cards` (идентификатор карты, драйвер ALSA и понятное имя) привязываются к записи PCI/USB, найденной через `/sys/class/sound/cardN/device`, а WMI-записи добавляют алиасы из имени и производителя. Когда настоящие ID шины получить нельзя, `stable_ids_for_name()` выводит детерминированные ID из нормализованного имени и шины; известные производители сохраняют реальный vendor id, а device id является стабильным хешем. Поле `id_source` различает `"reported"`, `"stablesynthetic"` и `"unknown"`, чтобы синтетические ID не принимались за данные перечисления.

Hotplug теперь отслеживает `/sys/class/sound` наряду с USB/PCI/NVMe, а `BusHint::Audio` оставляет sound-uevent релевантными. `aios-autohal` отображает устройства в `BusType::{USB, PCI, Bluetooth, Audio}` с PCI-классом мультимедиа `0x0004xxxx`, поставляет `AUDIO_WAT` и встроенный драйвер `driver.audio.generic` и подготавливает его обычным путём по отпечатку. Мост предоставляет аутентифицированные `GET /api/v1/audio/devices` и `POST /api/v1/audio/rescan`. Обнаружение работает по best-effort: на безголовом хосте возвращается пустой список, а не ошибка.
