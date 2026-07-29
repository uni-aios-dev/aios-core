<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/uni-aios-dev/.github/main/assets/aios-banner-dark.svg">
    <img alt="AIOS — AI-Native Operating System" src="https://raw.githubusercontent.com/uni-aios-dev/.github/main/assets/aios-banner-light.svg" width="80%">
  </picture>
</p>

<h1 align="center">AIOS — The Zero-Trust, WASM-First AI Operating System</h1>

<p align="center">
  <em>Rust‑core · WASM sandbox · AI‑orchestrated · crash‑safe · live‑update</em>
</p>

<p align="center">
  <a href="https://github.com/uni-aios-dev/aios-core/actions"><img src="https://img.shields.io/github/actions/workflow/status/uni-aios-dev/aios-core/ci.yml?branch=main&label=build&logo=github" alt="Build Status"></a>
  <a href="https://github.com/uni-aios-dev/aios-core/blob/main/LICENSE.md"><img src="https://img.shields.io/badge/license-AGPLv3%20%2F%20Commercial-blue.svg" alt="License"></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/rust-1.75%2B-orange?logo=rust" alt="Rust Version"></a>
  <a href="https://github.com/uni-aios-dev/aios-core"><img src="https://img.shields.io/github/stars/uni-aios-dev/aios-core?style=flat&logo=github" alt="Stars"></a>
  <a href="https://github.com/uni-aios-dev/aios-core/discussions"><img src="https://img.shields.io/badge/discussions-online-brightgreen" alt="Discussions"></a>
  <br>
  <img src="https://img.shields.io/badge/tests-708/708-passing-brightgreen" alt="Tests 708/708">
  <img src="https://img.shields.io/badge/clippy-0%20warnings-brightgreen" alt="Clippy 0 warnings">
  <img src="https://img.shields.io/badge/WASI-preview2-9cf" alt="WASI preview2">
</p>

---

## 🇬🇧 Overview

**AIOS** is a microkernel‑style operating system built from scratch in **Rust**, designed for AI‑native workloads.
It combines a **WASM‑based sandbox** for third‑party blocks, a **zero‑trust security model** with capability tokens,
an **AI‑driven intent engine** that understands natural language commands (RU/EN), and a **crash‑safe persistent store**
backed by `redb`. Applications are written in **EasyLang** — a declarative DSL that compiles to WASM in milliseconds.

```
┌──────────────────────────────────────────────────────────┐
│                    User Interface (TUI/GUI)               │
│  ┌─────────────────────┐  ┌────────────────────────────┐ │
│  │  Intent Engine       │  │  Dashboard (ratatui/egui)  │ │
│  │  (NLP + LLM fallback)│  │  + Alt+Space Command Bar  │ │
│  └─────────┬───────────┘  └────────────────────────────┘ │
├────────────┼─────────────────────────────────────────────┤
│            ▼                                             │
│  ┌──────────────────────────────────────────────────┐    │
│  │           AI Orchestrator & Scheduler             │    │
│  │  Process Mgr · Block Mgr · Live Update · Watchdog │    │
│  └──────────┬───────────┬───────────┬────────────────┘    │
│             ▼           ▼           ▼                     │
│  ┌──────────────┐ ┌──────────┐ ┌────────────────┐        │
│  │  WASM Runtime │ │ Security │ │  IPC / Ringbuf  │        │
│  │ (Wasmtime)    │ │Capability│ │  (lock‑free)    │        │
│  └──────────────┘ └──────────┘ └────────────────┘        │
├─────────────────────────────────────────────────────────┤
│  ┌──────────────────────────────────────────────────┐    │
│  │  Hardware Abstraction Layer (HAL)                 │    │
│  │  Tier detection · CPU affinity · IOMMU · MPK · TEE │    │
│  └──────────────────────────────────────────────────┘    │
└──────────────────────────────────────────────────────────┘
```

### Key Architecture Points

| Layer | Technology | Purpose |
|-------|-----------|---------|
| **Core** | Rust 2021 edition, no_std‑compatible primitives | Foundation types, crypto, error handling |
| **IPC** | Lock‑free ring buffers, typed channels | Zero‑copy inter‑block communication |
| **Sandbox** | Wasmtime, WASI preview2, capability‑gated | Run untrusted code with OS‑level isolation |
| **Scheduler** | Priority‑based, CPU affinity, real‑OS threads | Cooperative + preemptive multitasking |
| **Security** | Ed25519 capability tokens, MPK, SEV/SGX | Zero‑trust from boot to shutdown |
| **Storage** | Copy‑on‑Write, redb, ZSTD compression | Crash‑safe persistence with atomic commits |
| **AI Engine** | Rule parser + LLM (cloud / local Qwen GGUF) | Natural language → system commands |
| **Live Update** | Atom‑slot A/B, state transfer, CoW | Zero‑downtime hot‑swap with rollback |
| **EasyLang** | Declarative DSL → WAT → WASM | Write blocks in 10 keywords (RU/EN) |

### Out‑of‑the‑Box Features

| Feature | Description |
|---------|-------------|
| **Safe Coexistence** | Installs alongside Windows without repartitioning — runs from a folder |
| **Media Importer** | Detects USB/DVD media, imports photos/video with AI‑assisted tagging |
| **Private Search** | Anonymous web search via DuckDuckGo / SearXNG with local AI TL;DR |
| **App Store** | Community‑driven WASM block registry on GitHub, one‑click install |
| **Smart Hot‑Keys** | `Alt+Space` global command bar, natural language (RU/EN), fuzzy completion |
| **Crash‑Safe DB** | Every write is atomic — power loss never corrupts state |

---

## 🚀 Getting Started

### Prerequisites

- Rust toolchain **1.75+** (`rustup default stable`)
- Windows 10+ / Linux (kernel 5.10+) / macOS 12+

### Build & Run

```bash
# Clone
git clone https://github.com/uni-aios-dev/aios-core.git
cd aios-core

# Build everything
cargo build --release --workspace

# Run tests (708+)
cargo test --workspace

# Launch TUI dashboard
cargo run --release -p aios-tui

# Launch Native GUI
cargo run --release -p aios-gui
```

### First Steps

```bash
# Check system status
aiosctl status

# List running processes
aiosctl ps

# Install a block from the store
aiosctl install block-name

# Run EasyLang workflow
aiosctl run my-workflow.ez
```

> **Windows users:** Add `C:\aios\bin` to `PATH` after installation. The TUI and GUI dashboards work without admin rights.

---

## 📦 App Store & Compatibility

### How It Works

```
User ──► aiosctl search "browser" ──► GitHub API ──► uni-aios-dev/aios-official-store
                                              │
                                        index.json ──► block manifest
                                              │
                              ┌───────────────┴───────────────┐
                              ▼                               ▼
                        Pull `block.wasm`              Verify Ed25519 signature
                              │                               │
                              └───────────────┬───────────────┘
                                              ▼
                                        Wasmtime sandbox
                                              │
                                        Capability check
```

- **Community Store:** Browse at [`github.com/uni-aios-dev/aios-official-store`](https://github.com/uni-aios-dev/aios-official-store)
- **EasyLang:** 10 keywords — `spawn`, `load`, `unload`, `kill`, `timer`, `query`, `compact`, `status`, `pipe`, `wait`
- **Linux OCI Compatibility:** WASM blocks can be wrapped in OCI containers via `wasmtime serve` for Chrome OS integration
- **Binary Compatibility:** POSIX / Win32 translation layer (`aios-exec-compat`) heals missing `.dll` / `.so` at runtime

---

## 📚 Documentation

| Document | Description |
|----------|-------------|
| [Architecture](docs/ARCHITECTURE.md) | Full system architecture, all layers, types, data flows |
| [Interface Guide](docs/INTERFACE.md) | GUI/TUI keyboard shortcuts, layout, theme customization |
| [Changelog](docs/CHANGELOG.md) | Development history, phase‑by‑phase changes |
| [TODO / Roadmap](docs/TODO.md) | Upcoming phases and feature backlog |
| [Known Bugs](docs/BUGS.md) | Bug tracker with workarounds and risk analysis |
| [Development Rules](AGENTS.md) | Coding conventions, test requirements, session reports |

---

## 🧪 Project Stats

```
┌──────────────────────────────────────────────┐
│  Crates      40+                              │
│  Tests       708+ (all passing)               │
│  Clippy      0 warnings                       │
│  Rust        Edition 2021                     │
│  WASM        Wasmtime 47, WASI preview2       │
│  License     AGPLv3 / Commercial              │
│  Status      Active development               │
└──────────────────────────────────────────────┘
```

---

## 🛣 Roadmap (Next Phases)

| Phase | Feature | Status |
|-------|---------|--------|
| 23 | Multi‑Mode AI Engine + Local GGUF Inference | ✅ Done |
| 24 | EasyLang Engine & No‑Code App Builder | ✅ Done |
| 25 | Secure Web Surfing & Search (`aios-browser`) | ✅ Done |
| 26 | Atomic Updates & Decentralized App Store | 📋 Planned |
| 27 | Debug System & Black Box Telemetry | 📋 Planned |

---

## 🤝 Contributing

We welcome contributions! See [`CONTRIBUTING.md`](CONTRIBUTING.md) for guidelines.

- **Report bugs** — open a [Bug Report](https://github.com/uni-aios-dev/aios-core/issues/new?template=bug_report.md)
- **Suggest features** — start a [Discussion](https://github.com/uni-aios-dev/aios-core/discussions)
- **Submit blocks** — open a PR in [`aios-official-store`](https://github.com/uni-aios-dev/aios-official-store)

---

## 🛡 License

**Dual‑License: Free for Individuals · Paid for Commercial Enterprise**

| Use Case | License | Cost |
|----------|---------|------|
| Personal / hobby / education | AGPLv3 | Free |
| Open‑source project | AGPLv3 | Free |
| Commercial (≤5 seats) | Commercial EULA | $49/seat/year |
| Commercial (6–50 seats) | Commercial EULA | $29/seat/year |
| Commercial (50+ seats) | Enterprise Agreement | Contact us |

See [`LICENSE.md`](LICENSE.md) for full terms.

---

## 💬 Community

- **GitHub Discussions:** [github.com/uni-aios-dev/aios-core/discussions](https://github.com/uni-aios-dev/aios-core/discussions)
- **GitHub Issues:** [github.com/uni-aios-dev/aios-core/issues](https://github.com/uni-aios-dev/aios-core/issues)

## ❤️ Support

- **USDT (ERC-20):** `0x31f106eef39b1582d9851c984de0cbc60a3deda4`

---

<br>

<h2 align="center">🇷🇺 Русская версия</h2>

<p align="center">
  <em>Операционная система с нулевым доверием, WASM‑песочницей и AI‑управлением</em>
</p>

## Обзор

**AIOS** — это микроядерная операционная система, написанная целиком на **Rust**, спроектированная для AI‑нативных нагрузок. Она объединяет **WASM‑песочницу** для сторонних блоков, **модель безопасности с нулевым доверием** на основе capability‑токенов, **AI‑движок**, понимающий естественный язык (RU/EN), и **отказоустойчивое постоянное хранилище** на `redb`. Приложения пишутся на **EasyLang** — декларативном DSL, который компилируется в WASM за миллисекунды.

### Ключевые точки архитектуры

| Слой | Технология | Назначение |
|------|-----------|-----------|
| **Ядро** | Rust 2021, no_std‑совместимые примитивы | Фундаментальные типы, криптография, обработка ошибок |
| **IPC** | Lock‑free кольцевые буферы, типизированные каналы | Обмен данными между блоками с нулевым копированием |
| **Песочница** | Wasmtime, WASI preview2, контроль capability | Запуск непроверенного кода с изоляцией на уровне ОС |
| **Планировщик** | Приоритетный, привязка к CPU, реальные потоки ОС | Кооперативная + вытесняющая многозадачность |
| **Безопасность** | Ed25519 токены, MPK, SEV/SGX | Нулевое доверие от загрузки до завершения |
| **Хранилище** | Copy‑on‑Write, redb, сжатие ZSTD | Отказоустойчивость с атомарными транзакциями |
| **AI‑движок** | Правила + LLM (облачный / локальный Qwen GGUF) | Естественный язык → системные команды |
| **Live Update** | Два слота A/B, передача состояния, CoW | Горячая замена без остановки с откатом |
| **EasyLang** | Декларативный DSL → WAT → WASM | Создание блоков на 10 ключевых фразах (RU/EN) |

### Возможности из коробки

| Возможность | Описание |
|-------------|---------|
| **Безопасное соседство** | Устанавливается рядом с Windows без переразметки диска — работает из папки |
| **Импорт медиа** | Определяет USB/DVD, импортирует фото/видео с AI‑тегированием |
| **Приватный поиск** | Анонимный веб‑поиск через DuckDuckGo / SearXNG с локальным AI‑дайджестом |
| **Магазин приложений** | Реестр WASM‑блоков на GitHub, установка в один клик |
| **Умные хоткеи** | `Alt+Space` глобальная командная строка, естественный язык (RU/EN), fuzzy‑дополнение |
| **Отказоустойчивая БД** | Каждая запись атомарна — потеря питания никогда не повреждает состояние |

### Быстрый старт

```bash
git clone https://github.com/uni-aios-dev/aios-core.git
cd aios-core
cargo build --release --workspace
cargo test --workspace
cargo run --release -p aios-tui
```

### Магазин приложений

Поиск и установка блоков из сообщества:

```bash
aiosctl search browser
aiosctl install my-block
```

### Статистика проекта

```
Крейтов:      40+
Тестов:       708+ (все проходят)
Clippy:       0 предупреждений
Rust:         Edition 2021
WASM:         Wasmtime 47, WASI preview2
Лицензия:     AGPLv3 / Коммерческая
Статус:       Активная разработка
```

### Лицензия

**Двойная лицензия: бесплатно для физических лиц · платно для бизнеса**

| Сценарий | Лицензия | Стоимость |
|----------|---------|-----------|
| Личное использование / хобби / образование | AGPLv3 | Бесплатно |
| Open‑source проект | AGPLv3 | Бесплатно |
| Коммерческое (≤5 мест) | Коммерческая EULA | $49/место/год |
| Коммерческое (6–50 мест) | Коммерческая EULA | $29/место/год |
| Коммерческое (50+ мест) | Enterprise Agreement | Связаться с нами |

### Сообщество

- **GitHub Discussions:** [github.com/uni-aios-dev/aios-core/discussions](https://github.com/uni-aios-dev/aios-core/discussions)
- **GitHub Issues:** [github.com/uni-aios-dev/aios-core/issues](https://github.com/uni-aios-dev/aios-core/issues)

### Поддержать проект

- **USDT (ERC-20):** `0x31f106eef39b1582d9851c984de0cbc60a3deda4`

---

<p align="center">
  <sub>Made with ❤️ by the AIOS Team · © 2026 AIOS Project</sub>
</p>
