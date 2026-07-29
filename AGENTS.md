# AIOS — Agent Development Rules

## Documentation Policy (MANDATORY)

**Every change to the codebase MUST include documentation updates.**

When making ANY modification:
1. Update `docs/ARCHITECTURE.md` if the change affects system architecture or module interfaces.
2. Update `docs/CHANGELOG.md` with a new entry describing the change.
3. Update `docs/BUGS.md` if the change fixes a bug or introduces a known issue.
4. Update `docs/TODO.md` if the change adds new features or removes planned work.
5. Update `docs/INTERFACE.md` if the change affects user-facing interfaces (TUI/GUI).
6. Add inline `///` doc comments to all new public structs, enums, traits, and functions.
7. Ensure all existing doc comments remain accurate after the change.

## Bilingual Documentation (MANDATORY)

**ALL documentation files MUST exist in both English and Russian.**

When making ANY documentation change:
1. Keep `docs/ARCHITECTURE.md` (English) and `docs/ARCHITECTURE.ru.md` (Russian) in sync.
2. Keep `docs/CHANGELOG.md` (English) and `docs/CHANGELOG.ru.md` (Russian) in sync.
3. Keep `docs/BUGS.md` (English) and `docs/BUGS.ru.md` (Russian) in sync.
4. Keep `docs/TODO.md` (English) and `docs/TODO.ru.md` (Russian) in sync.
5. Keep `docs/INTERFACE.md` (English) and `docs/INTERFACE.ru.md` (Russian) in sync.
6. Every new section, entry, or fix in English docs MUST have a corresponding Russian translation.
7. Code blocks, file paths, type names, and command examples remain in English in Russian docs.
8. Table headers in Russian docs are translated; table content (type names, variants) stays in English.

## Immediate Git Push Rule (MANDATORY)

**Every code change, doc update, or any file modification MUST be committed and pushed to GitHub immediately after completion and verification.**

1. After any change: `git add -A && git commit -m "<description>" && git push`
2. Commit message must be descriptive (in English), include what changed and why
3. Pushed to `origin main` on `uni-aios-dev/aios-core`
4. This rule applies even for single-file changes, typo fixes, and docs-only updates

## Session Reports (MANDATORY)

**After completing any multi-step task or significant change, write a session report in Russian.**

Report format:
1. **Цель** — что делали и зачем
2. **Что сделано** — конкретные изменения по файлам
3. **Баги** — найденные и исправленные проблемы
4. **Тесты** — сколько добавлено, что покрывают
5. **Верификация** — результаты build/test/clippy/fmt
6. **Статус** — что осталось делать

Report is written directly in the chat after task completion, not in a file.

## Build & Test Commands

```powershell
# PATH setup (run before any cargo command on this Windows machine)
$env:Path = [System.Environment]::GetEnvironmentVariable("Path","Machine") + ";" + [System.Environment]::GetEnvironmentVariable("Path","User")

# Build
cargo build --workspace

# Test
cargo test --workspace

# Clippy (must be zero warnings)
cargo clippy --workspace

# Format
cargo fmt --all
```

## Code Style

- **Language:** Rust 2021 edition
- **Formatter:** `cargo fmt` defaults
- **Linter:** `cargo clippy` — zero warnings required before any commit
- **Naming:** snake_case functions/variables, CamelCase types, SCREAMING_SNAKE constants
- **Error handling:** All fallible operations return `aios_core::error::Result<T>`
- **Serialization:** `bincode` for IPC, `serde_json` only for human-readable contexts
- **No comments unless explicitly requested** — code should be self-documenting via naming

## Architecture Rules

- All crate roots must be `pub mod` declarations only in `lib.rs`
- All inter-crate communication goes through `aios_core::ipc_protocol` types
- No direct memory pointers between blocks — data exchange only via `IpcPacket`
- Every `StatefulBlock` implementation must provide `extract_state()` / `restore_state()`
- All speed tests must have dual thresholds: debug (`cfg!(debug_assertions)`) and release

## Testing Rules

- Unit tests go in `src/*.rs` under `#[cfg(test)] mod tests { ... }`
- Integration tests go in `tests/integration_test.rs`
- All tests must pass in both debug and release modes
- Speed test thresholds: debug = 50us, release = 1-2us (adjust based on measurement)
- Tests must not depend on specific hardware — use mock profiles

## File Structure

```
aios-core/          — Foundation: types, IPC protocol, crypto, error handling
aios-ipc/           — IPC transport: bus (VecDeque), channel (mpsc)
aios-hal/           — Hardware abstraction: detect, classify, mock profiles
aios-block-mgr/     — Block management: registry, loader, message router
aios-process-mgr/   — Process management: scheduler, crash resilience, IPC control
aios-live-update/   — Live update engine: hot-swap, rollback, state transfer
aios-watchdog/      — Watchdog: heartbeat monitoring, safe mode shell
aios-security/      — Security: capability tokens, access control, sandboxing
aios-context/       — Context store: telemetry, workflows, stability scores
aios-exec-compat/    — Multi-binary compatibility: POSIX/Win32 translation, dependency healing
aios-wasm/           — WebAssembly runtime: Wasmtime embedding, WASI filtering, sandbox isolation
aios-tui/           — User interface: intent engine, ratatui dashboard
aios-daemon/        — Headless server: aiosd binary for Docker/background
aios-gui/           — Native GUI dashboard: egui/eframe, 6 tabs, dark theme
tests/              — Integration tests (28 tests covering full lifecycle)
docs/               — All documentation
```

## Documentation Structure

```
AGENTS.md              — This file: agent development rules
README.md              — Project overview and quick start
docs/ARCHITECTURE.md   — Full OS architecture: all layers, types, data flows
docs/ARCHITECTURE.ru.md — Полная архитектура ОС (русский)
docs/CHANGELOG.md      — Development history, phase-by-phase changes
docs/CHANGELOG.ru.md   — История разработки (русский)
docs/BUGS.md           — Known bugs, workarounds, risk analysis
docs/BUGS.ru.md        — Известные баги и риски (русский)
docs/TODO.md           — Roadmap, backlog, supplementary specifications
docs/TODO.ru.md        — Дорожная карта (русский)
docs/INTERFACE.md      — GUI/TUI usage guide: keyboard, layout, theming
docs/INTERFACE.ru.md   — Руководство по интерфейсу (русский)
```
