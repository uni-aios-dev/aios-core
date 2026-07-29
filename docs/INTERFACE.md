# AIOS Interface — Usage Guide

## Overview

AIOS provides three interfaces for system management:

| Interface | Type | Binary / Path | Use Case |
|-----------|------|---------------|----------|
| **Web** | Browser SPA (HTML/CSS/JS) | `aios-studio/` served at `http://<host>:<port>/` | Remote management, visual dashboard, any device |
| **TUI** | Terminal (ratatui) | `aios-tui` | SSH, interactive, keyboard-driven |
| **GUI** | Native window (egui/eframe) | `aios-gui` | Desktop, visual dashboard, mouse + keyboard |
| **Daemon** | Headless server | `aiosd` | Docker, background, CI/CD, no terminal required |

Both interfaces display the same data and expose the same operations. The GUI is a graphical equivalent of the TUI.

---

## TUI (`aios-tui`)

### Launch

```bash
cargo run --bin aios-tui
```

### Layout

```
┌──────────────────────────────────────────────────────┐
│ AIOS v1.0.0 | Tier1 | WD: OK | CPU: 16 | RAM: ...  │  ← Header
├──────────────────────────────────────────────────────┤
│ Overview │ Processes │ Blocks │ Metrics │ Deps       │  ← Tabs
├──────────────────────────────────────────────────────┤
│                                                      │
│              Main content area                       │
│                                                      │
├──────────────────────────────────────────────────────┤
│ q=Quit 1-4=Tab j/k=Nav K=Kill U=Unload L=Load H=HS │  ← Footer
└──────────────────────────────────────────────────────┘
```

### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `1`-`5` | Switch tab (Overview / Processes / Blocks / Metrics / Deps) |
| `j` / `k` | Navigate down / up in current list |
| `K` | Kill selected process |
| `U` | Unload selected block |
| `L` | Load block from disk (prompts for name + version) |
| `H` | Hot-swap block binary |
| `r` | Refresh data |
| `s` | Show telemetry |
| `x` | Show status |
| `q` | Quit |

### Tabs

- **Overview**: System info (CPU, GPU, storage) + activity log
- **Processes**: Table with PID, Name, Priority, State, RAM, CPU, Crashes + detail panel
- **Blocks**: Table with ID, Name, Version, State, Size + load/unload/hot-swap operations
- **Metrics**: RAM gauge, priority distribution, RAM history chart
- **Deps**: Dependency graph table + load order chain

---

## GUI (`aios-gui`)

### Launch

```bash
cargo run --bin aios-gui
```

### Layout

```
┌──────────┬───────────────────────────────────────────┐
│          │  AIOS v1.0.0 | Tier1 | WD: OK | RAM: ... │  ← Top bar
│ Overview │───────────────────────────────────────────│
│ Processes│                                           │
│ Blocks   │          Central panel                    │
│ Marketpl.│     (changes per selected tab)            │
│ Metrics  │                                           │
│ Deps     │                                           │
│          │───────────────────────────────────────────│
│ ──────── │  F1-F6 tabs | AIOS Dashboard              │  ← Bottom bar
│ Quick    │
│ Actions  │
└──────────┴───────────────────────────────────────────┘
  Sidebar          Main area
```

### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `F1`-`F6` | Switch tab |
| `j` | Move selection down |
| `k` | Move selection up |

### Mouse

- Click tab names in sidebar to switch
- Click rows in tables to select
- Click buttons for actions (Kill, Suspend, Load, etc.)
- Type in search boxes (Marketplace)

### Tabs

#### Overview (F1)
- **Stat cards**: RAM (used/total), Blocks count, Processes count, Watchdog status
- **System panel**: CPU model, cores, threads, AVX2/AVX-512/SSE4.2, GPU, storage
- **RAM Usage**: progress bar + sparkline chart (last 60 samples)
- **Activity Log**: scrollable log with color-coded messages

#### Processes (F2)
- **Table**: PID, Name, Priority (color-coded), State (badge), RAM, CPU, Crashes
- **Actions**: Kill Selected, Suspend, Resume
- **Detail bar**: shows selected process info

#### Blocks (F3)
- **Table**: ID, Name, Version, State (badge), Size, Dependencies
- **Actions**: Refresh, Load Block (2-step dialog), Unload, Hot-Swap
- **Load Dialog**: Step 1 — enter block name, Step 2 — enter version, Enter/Cancel

#### Marketplace (F4)
- **Search box**: filter by name, description, or tags
- **Table**: Name, Version, Author, Status (badge), Downloads
- **Actions**: Install, Update, Uninstall
- **Status bar**: shows operation result

#### Metrics (F5)
- **RAM**: progress bar + sparkline
- **Priority Distribution**: bar chart (Background / Low / Normal / High / Critical)
- **Block Statistics**: count per state with progress bars
- **System Info**: CPU, GPU, storage summary

#### Dependencies (F6)
- **Summary**: block count + edge count
- **Load Order**: visual chain (block A → block B → block C)
- **Table**: Block name, Depends On, Depended By

### Theming

Dark theme with customizable colors in `aios-gui/src/theme.rs`:

| Color | Usage |
|-------|-------|
| `accent` (#00C8DC) | Headers, active tabs, highlights |
| `success` (#32C850) | Running blocks, OK status, positive values |
| `warning` (#F0B41E) | Suspended, High priority, warnings |
| `danger` (#E63C3C) | Crashed, Critical, errors |
| `info` (#64A0FF) | Low priority, informational |
| `muted` (#78788C) | Background, dimmed text, Terminated |
| `surface` (#181820) | Main background |
| `surface_alt` (#22222E) | Card backgrounds, alternating rows |

---

## Web (`aios-studio`)

### Launch

The web interface is served automatically by `aios-bridge` as static files. Run:

```bash
cargo run --bin aios-tui   # or any binary that starts aios-bridge
# Open http://localhost:9876 in your browser
```

The SPA is available at the root URL. The bridge port is configurable via the `addr` parameter in `start_server()`.

### Dashboard (default tab)

- **Health cards**: System status, Watchdog state, Process count, Memory usage
- **RAM chart**: Real-time Canvas line chart with last 120 data points (12 seconds at 100ms intervals)
- **Process table**: PID, Name, Memory, CPU time, Status with color-coded state badges

### Smart Command Palette

- **Open**: `Ctrl+K` or `Cmd+K` (macOS)
- **Close**: `Escape` or click outside the palette
- **Usage**: Type any natural-language command and press Enter
- **Supported**: process actions (list, kill, spawn), block actions (load, unload), system queries (status, memory, CPU), memory compaction
- **Languages**: English + Russian (bilingual intent parser)
- **Result display**: Shows description, JSON result, and list of used capability tokens

### Security Center

- **Blocks table**: ID, Name, Version, State, Stop button per block
- **Capability tokens grid**: All six tokens (CAP_PROCESS_KILL, CAP_PROCESS_SPAWN, CAP_BLOCK_LOAD, CAP_BLOCK_UNLOAD, CAP_SCHED_MODIFY, CAP_MEM_ALLOC) with descriptions
- **Quick actions**: Compact Memory, List Blocks, List Processes

### Connection Status

- **Left sidebar footer**: Color-coded status indicator
  - Green = Connected
  - Yellow = Reconnecting (exponential backoff)
  - Red = Disconnected / Error
- WebSocket auto-reconnect with 1s → 15s exponential backoff

### Requirements

- Any modern browser (Chrome, Firefox, Safari, Edge)
- No JavaScript framework required — 0 npm dependencies
- WebSocket support (all modern browsers)

---

## Architecture Notes

- **TUI** uses `ratatui` + `crossterm` — runs in any terminal, works over SSH
- **GUI** uses `egui` + `eframe` — native window, GPU-accelerated rendering
- Both read from the same `DashboardState` / `AiosApp` model
- Neither interface performs hardware operations directly — they request operations via the scheduler/block manager
- All data is snapshot-based: `DashboardState::update_from_scheduler()` (TUI) or `AiosApp::new()` (GUI, initial only)

---

## Daemon (`aiosd`)

### Launch

```bash
# Default (headless, for Docker)
aiosd

# With custom directories
AIOS_DATA_DIR=/mnt/data AIOS_BLOCKS_DIR=/mnt/blocks aiosd

# Using mock profile
AIOS_MOCK_PROFILE=legacy aiosd
```

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `AIOS_DATA_DIR` | `/app/data` | Persistent database directory |
| `AIOS_BLOCKS_DIR` | `/app/blocks` | Disk block binaries directory |
| `AIOS_MOCK_PROFILE` | `modern` | Hardware profile: `modern`, `legacy`, `none` |
| `RUST_LOG` | `info` | Log level: `error`, `warn`, `info`, `debug`, `trace` |

The daemon performs the same initialisation as `aios-tui` but runs without a terminal interface:
1. Loads built-in blocks (hal, ipc_bus, scheduler)
2. Loads disk blocks from `AIOS_BLOCKS_DIR`
3. Opens the persistent database at `AIOS_DATA_DIR/context.redb`
4. Spawns system processes (ai_orchestrator, io_handler, health_monitor)
5. Starts watchdog heartbeat thread
6. Logs heartbeat with process count, RAM usage, watchdog state every 10 seconds
7. Persists telemetry to database every 60 seconds

## Running All Interfaces

All four interfaces (Web, TUI, GUI, Daemon) can run simultaneously. The Web SPA connects to `aios-bridge` via HTTP/WebSocket and is purely a remote client. TUI and GUI are in-process interfaces that create their own `Scheduler` and `BlockRegistry` instances locally.
