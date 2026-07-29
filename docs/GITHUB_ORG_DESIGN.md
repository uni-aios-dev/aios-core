# AIOS GitHub Organization Design

## Overview

This document defines the GitHub organization structure for the **AIOS** project (`github.com/uni-aios-dev`).
The organization follows an **Open-Core** model with **Dual Licensing** — public repositories for the core OS,
EasyLang, and community store; private repositories for enterprise and security-sensitive components.

---

## 1. Organization Profile

- **Name:** AIOS
- **URL:** `https://github.com/uni-aios-dev`
- **Description:** The Zero-Trust, WASM-First AI Operating System — Rust · WASM · EasyLang
- **Avatar:** Hexagonal gear + brain icon on dark background
- **Website:** `https://aios.dev`
- **Support Email:** `support@aios.dev`
- **License Email:** `license@aios.dev`

---

## 2. Repository Structure

### 2.1 Public Repositories

| Repository | Description | Topics | Visibility |
|-----------|-------------|--------|------------|
| `aios-core` | OS kernel — HAL, Scheduler, IPC, Watchdog, Security, redb, AI Engine | `rust`, `os`, `wasm`, `ai`, `kernel` | Public |
| `easylang` | EasyLang language specification, in-memory compiler, examples | `easylang`, `dsl`, `compiler`, `wasm` | Public |
| `aios-official-store` | Decentralized app index — index.json, manifest schemas, PR templates | `wasm`, `registry`, `app-store` | Public |
| `aios-studio` | User interface — TUI (ratatui), GUI (egui), CLI, command bar | `tui`, `gui`, `dashboard`, `cli` | Public |
| `.github` | Organization profile README, community health files, templates | `community`, `health-files` | Public |

### 2.2 Private Repositories

| Repository | Description | Access |
|-----------|-------------|--------|
| `aios-security-keys` | Ed25519 signing keys for releases and off-server | Core team only |
| `aios-enterprise-fleet` | Fleet management console for 100+ corporate nodes | Enterprise team |
| `aios-cloud-sync` | Encrypted context sync service and cloud vault | Enterprise team |

---

## 3. Branch Protection Rules

### For `aios-core` (main branch)

```yaml
- Require pull request before merging
  - Require approvals: 1
  - Dismiss stale reviews: true
  - Require review from Code Owners: true
- Require status checks
  - cargo-build
  - cargo-test
  - cargo-clippy
  - cargo-fmt
- Require branches to be up-to-date: true
- Restrict push access: Core team only
```

### For `easylang`, `aios-official-store` (main branch)

```yaml
- Require pull request before merging
  - Require approvals: 1
- Require status checks
  - CI / build
  - CI / test
- Allow force pushes: false
```

### For `.github`

```yaml
- Require pull request before merging
  - Require approvals: 2
- Restrict push access: Admin team only
```

---

## 4. Team Structure

| Team | Members | Repositories | Purpose |
|------|---------|-------------|---------|
| **Core** | 3–5 | `aios-core`, `.github` | Kernel development, architecture decisions |
| **Tools** | 2–3 | `easylang`, `aios-studio` | Compiler, TUI/GUI, CLI |
| **Community** | 1–2 | `aios-official-store` | Store moderation, PR review for blocks |
| **Enterprise** | 2–3 | Private repos | Fleet management, cloud sync, security keys |
| **Admin** | 1–2 | All | GitHub settings, secrets, CI/CD, releases |

---

## 5. GitHub Discussions Categories

Configured in repository **Settings → Discussions** for `aios-core`:

| Category | Description | Format |
|----------|-------------|--------|
| `📢 Announcements` | News, releases, and important updates | Announcement |
| `💡 Ideas & Feature Requests` | Propose new features and improvements | Q&A / Discussion |
| `💬 General Q&A` | Installation, configuration, usage questions | Q&A |
| `🛠 EasyLang Showcase` | Share WASM blocks and workflows you've built | Show & Tell |
| `🏢 Enterprise & Commercial` | Commercial licensing, fleet deployment questions | Q&A |

---

## 6. Issue Labels

### Severity

- `severity:critical` — System crash, data loss, security vulnerability
- `severity:high` — Major feature broken, no workaround
- `severity:moderate` — Non-critical bug, workaround exists
- `severity:low` — Cosmetic, minor enhancement

### Component

- `comp:core` — aios-core (HAL, IPC, crypto)
- `comp:scheduler` — Process manager, priority
- `comp:security` — Capabilities, sandbox, MPK
- `comp:wasm` — Wasmtime integration, WASI
- `comp:ai` — LLM engine, intent parsing
- `comp:tui` — Ratatui dashboard
- `comp:gui` — egui/eframe dashboard
- `comp:easylang` — Language spec, compiler
- `comp:store` — App store, `index.json`
- `comp:docs` — Documentation, README

### Status

- `status:needs-triage` — Awaiting review
- `status:confirmed` — Reproduced and accepted
- `status:in-progress` — Being worked on
- `status:blocked` — Waiting on dependency
- `status:duplicate` — Already reported
- `status:wontfix` — Won't address

---

## 7. CI/CD Pipeline

### File: `.github/workflows/ci.yml` (in each public repo)

```yaml
name: CI

on:
  push:
    branches: [main, dev]
  pull_request:
    branches: [main]

env:
  CARGO_TERM_COLOR: always

jobs:
  build:
    strategy:
      matrix:
        os: [ubuntu-24.04, windows-2025, macos-14]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: actions-rust-lang/setup-rust-toolchain@v1
        with:
          toolchain: stable
      - run: cargo build --workspace
      - run: cargo clippy --workspace -- -D warnings
      - run: cargo test --workspace
      - run: cargo fmt --all --check
```

### File: `.github/workflows/release.yml` (aios-core only)

```yaml
name: Release

on:
  push:
    tags: ['v*']

jobs:
  release:
    strategy:
      matrix:
        os: [ubuntu-24.04, windows-2025, macos-14]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: actions-rust-lang/setup-rust-toolchain@v1
      - run: cargo build --release --workspace
      - run: cargo test --release --workspace
      - run: |
          # Sign binaries with Ed25519 key from secret
          # Upload to GitHub Release
      - uses: softprops/action-gh-release@v2
        with:
          files: target/release/*
          generate_release_notes: true
```

---

## 8. Community Health Files

Located in the `.github` repository (applied organization-wide):

| File | Description |
|------|-------------|
| `profile/README.md` | Organization profile (shown on `github.com/uni-aios-dev`) |
| `ISSUE_TEMPLATE/bug_report.md` | Bug report form with risk assessment |
| `CONTRIBUTING.md` | Contribution guidelines (core + store) |
| `SUPPORT.md` | Where to get help (Discord, Discussions, email) |
| `CODE_OF_CONDUCT.md` | Contributor Covenant v2.1 |
| `FUNDING.yml` | GitHub Sponsors / OpenCollective links |

---

## 9. Repository Initialisation Checklist

### For each new public repo:

- [ ] Create repo with README, LICENSE (AGPLv3), .gitignore (Rust)
- [ ] Enable Discussions
- [ ] Set up branch protection (main branch)
- [ ] Add GitHub Actions secrets (if needed)
- [ ] Configure repository topics
- [ ] Add to `uni-aios-dev` GitHub organisation
- [ ] Set default branch: `main`
- [ ] Enable "Allow squash merging" only
- [ ] Add Code Owners file (`.github/CODEOWNERS`)

---

## 10. Branding Assets

Location: `github.com/uni-aios-dev/.github/assets/`

| Asset | Format | Usage |
|-------|--------|-------|
| Logo | SVG | README banners, website |
| Logo (dark) | SVG | Dark mode display |
| Icon | PNG (512×512) | GitHub avatar, social |
| Banner | PNG (1280×640) | Social preview, website |
| Wordmark | SVG | Header, footer |

---

## 11. Communication Channels

| Channel | URL | Purpose |
|---------|-----|---------|
| **Discord** | `https://discord.gg/aios` | Real-time chat, support, community |
| **GitHub Discussions** | `https://github.com/uni-aios-dev/aios-core/discussions` | Long-form discussions, RFCs, Show & Tell |
| **Twitter / X** | `https://x.com/aios_os` | Announcements, project updates |
| **Email (Support)** | `support@aios.dev` | Private support requests |
| **Email (License)** | `license@aios.dev` | Commercial licensing inquiries |
| **Email (Security)** | `security@aios.dev` | Responsible disclosure |
| **Email (Conduct)** | `conduct@aios.dev` | Code of Conduct reports |
