# Contributing to AIOS

We welcome contributions from the community! Whether you're fixing a bug, adding a feature, or submitting a block to the Store, please follow these guidelines.

---

## 📋 Table of Contents

- [Code of Conduct](#code-of-conduct)
- [Getting Started](#getting-started)
- [Reporting Bugs](#reporting-bugs)
- [Feature Requests](#feature-requests)
- [Development Workflow](#development-workflow)
- [Code Style](#code-style)
- [Testing](#testing)
- [Pull Request Process](#pull-request-process)
- [Adding a Block to the Store](#adding-a-block-to-the-store)
- [License](#license)

---

## Code of Conduct

Be respectful, inclusive, and constructive. We enforce a **zero-tolerance policy** for harassment, discrimination, or aggressive behaviour. Report incidents to `conduct@aios.dev`.

---

## Getting Started

1. Fork the repository
2. Clone your fork: `git clone https://github.com/YOUR_USER/aios-core.git`
3. Set up Rust: `rustup default stable`
4. Build: `cargo build --workspace`
5. Run tests: `cargo test --workspace`

---

## Reporting Bugs

Open a [Bug Report](https://github.com/aios-dev/aios-core/issues/new?template=bug_report.md) using the template.

**Required:**
- Clear steps to reproduce
- Risk level (`CRITICAL` / `HIGH` / `MODERATE` / `LOW`)
- Environment details (OS, Rust version, AIOS version, hardware)

**Good bug reports** include logs, backtraces, and a minimal reproduction case.

---

## Feature Requests

Start a [Discussion](https://github.com/aios-dev/aios-core/discussions/categories/ideas-feature-requests) in the **Ideas & Feature Requests** category.

- Describe the problem you're solving, not just the solution
- Mention if you're willing to implement it yourself
- For large features, wait for community feedback before opening a PR

---

## Development Workflow

```
1. Pick an issue → assign yourself or comment
2. Create a branch:   git checkout -b feat/my-feature
3. Make changes
4. Write/update tests
5. Run clippy:        cargo clippy --workspace   # must be 0 warnings
6. Run tests:         cargo test --workspace     # must pass
7. Format:            cargo fmt --all
8. Commit
9. Push & open PR
```

---

## Code Style

| Rule | Standard |
|------|----------|
| Language | Rust Edition 2021 |
| Formatter | `cargo fmt` (defaults) |
| Linter | `cargo clippy` — **zero warnings required** |
| Naming | `snake_case` functions/vars, `CamelCase` types, `SCREAMING_SNAKE` constants |
| Comments | Only when logic is non‑obvious. Use `///` for all public items |
| Error handling | All fallible functions return `aios_core::error::Result<T>` |
| Serialization | `bincode` for IPC, `serde_json` for human‑readable contexts |
| Imports | Group: `std` → external crates → workspace crates → `crate::` |

### File Structure

```
src/
├── lib.rs          # pub mod declarations only
├── main.rs         # binary entry point (if applicable)
├── *.rs            # modules
└── tests.rs        # #[cfg(test)] mod tests { ... }
```

---

## Testing

- **Unit tests** live in `src/*.rs` under `#[cfg(test)] mod tests { ... }`
- **Integration tests** live in `tests/integration_test.rs`
- All tests must pass in both **debug** and **release** modes
- Speed tests use dual thresholds:
  - Debug: `50 µs`
  - Release: `1–2 µs`
- Tests must not depend on specific hardware — use mock profiles

```bash
# Run all tests
cargo test --workspace

# Run tests for a specific crate
cargo test -p aios-core

# Run with release optimisation
cargo test --release --workspace
```

---

## Pull Request Process

### Title Convention

```
[component] Brief description of the change
```

Examples:
- `[aios-llm] Add Qwen2.5 GGUF inference support`
- `[aios-bridge] Fix capability check race condition`
- `[docs] Update architecture diagram for Phase 23`

### Checklist

Before submitting:

- [ ] Code compiles with `cargo build --workspace`
- [ ] `cargo clippy --workspace` — **zero warnings**
- [ ] `cargo fmt --all` — formatting is clean
- [ ] `cargo test --workspace` — **all tests pass**
- [ ] Documentation updated (if applicable):
  - `docs/ARCHITECTURE.md` — architecture changes
  - `docs/CHANGELOG.md` — new entry describing the change
  - `docs/BUGS.md` — bug fix or new known issue
  - `docs/TODO.md` — new feature or removed planned work
  - `docs/INTERFACE.md` — user‑facing changes
- [ ] Bilingual docs: if you changed an English doc, update the Russian version too

### Review Process

1. At least **one maintainer** must approve
2. All CI checks must pass
3. Squash merge preferred

---

## Adding a Block to the Store

1. Fork [`aios-official-store`](https://github.com/aios-dev/aios-official-store)
2. Create a directory: `blocks/your-block-name/`
3. Add:
   - `block.wasm` — compiled WASM binary
   - `manifest.json` — block metadata (name, version, capabilities, author)
   - `README.md` — description in English and Russian
4. Open a PR

### Manifest Template

```json
{
  "name": "my-block",
  "version": "1.0.0",
  "description": {
    "en": "Does something useful",
    "ru": "Делает что-то полезное"
  },
  "author": "your-gh-handle",
  "capabilities": ["filesystem:read", "network:connect"],
  "wasm_hash": "sha256:..."
}
```

---

## License

By contributing, you agree that your contributions will be licensed under the project's **dual license** (AGPLv3 for personal use, Commercial for enterprise). See [`LICENSE.md`](LICENSE.md) for details.

---

<p align="center">
  <sub>Made with ❤️ by the AIOS Team · © 2026 AIOS Project</sub>
</p>
