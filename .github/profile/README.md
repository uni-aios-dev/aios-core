# AIOS — AI-Native Operating System

<p align="center">
  <img src="https://raw.githubusercontent.com/uni-aios-dev/.github/main/assets/aios-logo.svg" width="200" alt="AIOS Logo">
</p>

<p align="center">
  <em>The Zero-Trust, WASM-First AI Operating System · Rust · WASM · EasyLang</em>
</p>

<p align="center">
  <a href="https://github.com/uni-aios-dev/aios-core"><img src="https://img.shields.io/badge/aios--core-AGPLv3%20%2F%20Commercial-blue" alt="aios-core"></a>
  <a href="https://github.com/uni-aios-dev/easylang"><img src="https://img.shields.io/badge/easylang-spec%20%2B%20compiler-blueviolet" alt="easylang"></a>
  <a href="https://github.com/uni-aios-dev/aios-official-store"><img src="https://img.shields.io/badge/store-community%20blocks-orange" alt="store"></a>
  <a href="https://github.com/uni-aios-dev/aios-studio"><img src="https://img.shields.io/badge/studio-TUI%20%2F%20GUI-brightgreen" alt="studio"></a>
</p>

---

## Repositories

| Repository | Visibility | Description |
|------------|-----------|-------------|
| [`aios-core`](https://github.com/uni-aios-dev/aios-core) | Public 🟢 | OS kernel — HAL, Scheduler, IPC, Watchdog, Security, persistent DB |
| [`easylang`](https://github.com/uni-aios-dev/easylang) | Public 🟢 | EasyLang language spec, in-memory compiler, examples |
| [`aios-official-store`](https://github.com/uni-aios-dev/aios-official-store) | Public 🟢 | Decentralized app index — `index.json`, manifest schemas, PR templates |
| [`aios-studio`](https://github.com/uni-aios-dev/aios-studio) | Public 🟢 | User interface — TUI (ratatui), GUI (egui), CLI, Alt+Space command bar |
| [`.github`](https://github.com/uni-aios-dev/.github) | Public 🟢 | Organization profile, community health files, issue/PR templates |
| `aios-security-keys` | Private 🔴 | Ed25519 signing keys for releases and off-server |
| `aios-enterprise-fleet` | Private 🔴 | Fleet management console for 100+ corporate nodes |
| `aios-cloud-sync` | Private 🔴 | Encrypted context sync service and cloud vault |

## Getting Started

```bash
git clone https://github.com/uni-aios-dev/aios-core.git
cd aios-core
cargo build --release --workspace
cargo test --workspace
cargo run --release -p aios-tui
```

## License Policy

**Free for personal/educational use (AGPLv3).** Commercial use requires a paid license.

See [`aios-core/LICENSE.md`](https://github.com/uni-aios-dev/aios-core/blob/main/LICENSE.md) for details.

## Community

- [GitHub Discussions](https://github.com/uni-aios-dev/aios-core/discussions)
- [GitHub Issues](https://github.com/uni-aios-dev/aios-core/issues)

## Support

- **USDT (ERC-20):** `0x31f106eef39b1582d9851c984de0cbc60a3deda4`
