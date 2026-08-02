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
│ Overview │ Processes │ Blocks │ Metrics │ Deps │ Web │ Shell │  ← Tabs
├──────────────────────────────────────────────────────┤
│                                                      │
│              Main content area                       │
│                                                      │
├──────────────────────────────────────────────────────┤
│ q=Quit Alt+1-7=Tab j/k=Nav K=Kill U=Unload L=Load H=HS F1=Help :=Cmd │  ← Footer
└──────────────────────────────────────────────────────┘
```

### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `1`-`7` | Switch tab (Overview / Processes / Blocks / Metrics / Deps / Web / Shell) |
| `Alt`+`1`-`7` | Switch tab even while typing in the Shell command line or the Web URL bar |
| `j` / `k` | Navigate down / up in current list |
| `K` | Kill selected process |
| `U` | Unload selected block |
| `L` | Load block from disk (prompts for name + version) |
| `H` | Hot-swap block binary |
| `r` | Refresh data |
| `s` | Show telemetry |
| `x` | Show status |
| `W` | Launch the AIOS GUI dashboard (`aios-gui`) |
| `q` | Quit |
| `F1` or `?` | Toggle help overlay |
| `:` | Enter command mode in Shell tab |
| `Enter` | Execute command in Shell tab |
| `↑` / `↓` | Navigate command history in Shell tab |

### Tabs

- **Overview**: System info (CPU, GPU, storage) + activity log
- **Processes**: Table with PID, Name, Priority, State, RAM, CPU, Crashes + detail panel
- **Blocks**: Table with ID, Name, Version, State, Size + load/unload/hot-swap operations
- **Metrics**: RAM gauge, priority distribution, RAM history chart
- **Deps**: Dependency graph table + load order chain
- **Web**: Omnibox (search query or URL), page text content (headings highlighted), scrollable links list — pages load in the background and up to 20 recent pages are cached, so going back with `b` is instant
- **Shell**: Interactive command line with command history, fetch, search, open, clear commands

### Web Tab Keys (when Web tab is active)

The omnibox accepts either a full URL (`example.com`, `https://...`) or a plain search query (`how does AIOS work`) — queries are searched via DuckDuckGo automatically. After you press `Enter` the omnibox loses focus, so you can immediately navigate the results with `j`/`k` and open a link. Fetches run in the background (the TUI stays responsive) and recent pages are cached, so `b` back-navigation is instant. The links window scrolls with your selection and shows the visible range in its title. A **navigation sidebar** on the left shows the current page (marked `▸`) and the visit history; focus it with `\` and jump to any past page.

| Key | Action |
|-----|--------|
| `g` | Focus omnibox |
| `Enter` | Search / navigate (when omnibox focused); the omnibox auto-unfocuses after `Enter` |
| `o` / `Enter` | Open selected link (when omnibox not focused) |
| `j` / `k` | Move link selection down / up |
| `b` | Go back to the previously visited page |
| `B` | Open the current page in the **full native browser** window (WebView2 — JS/CSS/images); the window is reused across presses |
| `n` | Open the selected link in the native browser window |
| `u` / `d` | Scroll page text up / down by 1 line |
| `PageUp` / `PageDown` | Scroll page text up / down by 20 lines |
| `\` | Toggle navigation sidebar focus (history list) |
| `j` / `k` | Move sidebar selection down / up (when sidebar focused) |
| `Enter` / `o` | Open the selected sidebar entry / reload the current page (when sidebar focused) |
| `Esc` | Unfocus omnibox or sidebar |

### Shell Tab Keys (when Shell tab is active)

| Key | Action |
|-----|--------|
| `:` | Enter command mode |
| `Enter` | Execute command |
| `↑` / `↓` | Navigate command history |
| `Esc` | Exit command mode |

### Shell Commands

| Command | Arguments | Description |
|---------|-----------|-------------|
| `fetch` | `<url>` | Download and load a block from a URL |
| `search` | `<query>` | Web search via DuckDuckGo |
| `open` | `<query or url>` | Open/search on the Web tab |
| `clear` | — | Clear shell output |
| `ps` / `list` | — | List running processes |
| `blocks` / `ls` | — | List loaded blocks |
| `kill` | `<pid>` | Kill a process |
| `spawn` | `<name> [prio] [ram_mb]` | Spawn a process (prio: 0-4) |
| `load` | `<name> [version]` | Load a block |
| `unload` | `<id>` | Unload a block |
| `status` / `info` | — | System status |
| `logs` | — | View safe-mode event log |
| `restart` | — | Restart the orchestrator |
| `help` / `?` | — | Show all available commands |
| `exit` | — | Exit the safe-mode shell |

### F1 Help Overlay

The F1 help overlay shows all keyboard shortcuts and shell commands as a full-screen opaque panel (it replaces the dashboard background, so nothing blends into the help text).

| Key | Action |
|-----|--------|
| `F1` or `?` | Toggle help overlay |
| `F1`, `Esc`, or `?` | Dismiss help overlay |

---

## Kernel TUI (`aios`)

### Launch

```bash
cargo run --bin aios
# or from the compiled binary:
./target/release/aios.exe
```

### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `Tab` / `F1` | Next tab |
| `1`-`4` | Direct tab select |
| `Alt`+`1`-`4` | Direct tab select even while the browser URL prompt or the AI query line is active |
| `g` | Open bridge dashboard URL (`http://localhost:8080`) in the browser |
| `b` | Open a URL or search query in the native browser (input mode) |
| `W` | Launch the AIOS GUI dashboard (`aios-gui`) |
| `r` | Reprobe hardware |
| `Space` | Pause/resume event log |
| `q` | Quit |

### Browser Hotkey (`b`)

The browser block is registered at boot — no configuration, installed browser, or network required. Press `b`, type a full URL (e.g. `https://example.com`), a bare host (`example.com`), or a plain search query (`rust scheduler`), press `Enter`: the input is dispatched to the browser block over IPC (`open_native` command via `MessageRouter`). Queries and bare hosts are converted to a DuckDuckGo search / `https://` URL and opened in your OS default browser. The result is shown in the Events pane.

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
│ Browser  │                                           │
│ ──────── │───────────────────────────────────────────│
│ Quick    │  F1-F7 tabs | AIOS Dashboard              │  ← Bottom bar
│ Actions  │
└──────────┴───────────────────────────────────────────┘
  Sidebar          Main area
```

### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `F1`-`F7` | Switch tab |
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

#### Browser (F7)
- **Omnibox**: type a full URL (`https://...`), a bare host (`example.com`), or a plain search query (`rust scheduler`) — Enter resolves and loads it
- **Navigation buttons**: Back, Forward; **Open Browser** / **Close** toggle
- **Native engine**: the browser window is a real WebView (WebView2 / WebKitGTK / WKWebView) with full cookies, JavaScript and history support; the first navigation opens the window automatically
- **Status line**: shows the resolved target or the last action/error

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
