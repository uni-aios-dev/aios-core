# AIOS Interface — Usage Guide

## Live USB (bootable stick)

The bootable AIOS USB stick (`AIOS-LIVE`, hybrid BIOS+UEFI ISO) boots straight into the AIOS kernel TUI on `tty1`. There is no login prompt.

- Boot the machine from the USB stick (BIOS/UEFI boot menu → USB). GRUB shows a menu (10 s default):
  1. **AIOS Live** — quiet boot, auto-launch the AIOS TUI
  2. **AIOS Live (verbose console)** — same, with kernel messages visible
  3. **AIOS Installer (install to disk)** — same quiet boot; run `aios-install` afterwards
- Inside the TUI press `Esc`/`q` to exit to the shell prompt `#`. The system is read-only (squashfs); only `/tmp`, `/run`, `/var/log` are writable (tmpfs). Changes do not persist across reboots.
- Network: DHCP is attempted on all ethernet/wifi interfaces at boot (see the TUI Network tab for the assigned address).
- To install AIOS to the machine's disk, type `aios-install`. It will list the disks, ask for the target (e.g. `sda`), require confirmation `YES`, partition the disk (GPT: 512 MB EFI + ext4 root), copy the system, and install GRUB. Reboot after the message.

---

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
│ q=Quit Alt+1-8=Tab j/k=Nav K=Kill U=Unload L=Load H=HS F1=Help :=Cmd │  ← Footer
└──────────────────────────────────────────────────────┘
```

### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `1`-`8` | Switch tab (Overview / Processes / Blocks / Metrics / Deps / Web / Shell / Files) |
| `Alt`+`1`-`8` | Switch tab even while typing in the Shell command line or the Web URL bar |
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
- **Files**: Two-panel file manager (Volkov/Far style) over the `aios-vfs` virtual file system — navigate `AIOS://` and `HOST://`, copy/move/delete/mkdir/rename with background progress, AI file preview, capability-gated host access

### Files Tab Keys (when Files tab is active)

The Files tab shows two panels (left/right). The active panel is highlighted in the header. Files start in the `AIOS://` sandbox; press `g`/`w` to grant the `HOST://` read/write capabilities, then navigate to `HOST://` for real host paths.

| Key | Action |
|-----|--------|
| `Tab` | Switch the active panel |
| `↑` / `k` / `↓` / `j` | Move selection up / down |
| `Enter` | Open the selected directory, or AI-preview the selected file |
| `Backspace` | Go to the parent directory |
| `F3` / `o` | AI-preview the selected file |
| `F5` | Copy the selected item to the other panel |
| `F6` | Move the selected item to the other panel |
| `F7` | Create a directory (type the name, `Enter` confirms, `Esc` cancels) |
| `F8` | Delete the selected item |
| `F2` | Rename the selected item (type the new name, `Enter` confirms) |
| `F9` / `s` | Cycle the sort rule (Name / Size / Date / Type) |
| `g` | Grant the `vfs:host:read` capability |
| `w` | Grant the `vfs:host:write` capability |
| `r` | Refresh the panels |
| `Esc` | Close the AI preview / cancel the input dialog |

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
| `store` | `<sub>` | Block store management (see below) |
| `net` | `<sub>` | Network settings (see below) |
| `help` / `?` | — | Show all available commands |
| `exit` | — | Exit the safe-mode shell |

### Store Shell Commands

The block store fetches blocks from three kinds of sources: GitHub (`github:owner/repo`), a local directory (`local:path`) and an HTTP update service (`http://host:port`). Installations land in `AIOS_BLOCKS_DIR` as `{name}_{version}.wasm` + sidecar JSON; updates are backed up (`.bak`) and auto-rollback on failure.

| Command | Arguments | Description |
|---------|-----------|-------------|
| `store list` | — | List installed blocks (newest version per name) |
| `store sources` | — | List configured sources |
| `store add-source` | `<github:owner/repo\|local:path\|http://url>` | Register a new source |
| `store search` | `<query> [--source NAME]` | Search the source catalog |
| `store install` | `<name> [--source NAME]` | Install a block (newest version) |
| `store update` | `[name] [--source NAME]` | Check for and apply updates (auto-rollback on failure) |
| `store uninstall` | `<name>` | Remove every installed version of a block |
| `store rollback` | `<name>` | Restore the previous version from backup |
| `store publish` | `<file.wasm> [name] [version]` | Publish a local wasm file to the running update service via `POST /api/v1/store/publish` (bridge port from `AIOS_BRIDGE_PORT`, default `8080`); the file's SHA-256 is verified server-side |
| `store sign` | `<file.wasm> [name] [version] [--key <secret_hex>]` | Sign a local wasm file with Ed25519: computes SHA-256, builds the manifest and writes the signed sidecar JSON (`{name}_{version}.json`) next to the file; the key comes from `--key` or `AIOS_STORE_SIGNING_KEY` (32 bytes, 64 hex chars); prints the public key |
| `store verify` | `<name>` | Verify an installed block: recompute the SHA-256 of the installed binary and check the Ed25519 signature of its sidecar manifest; reports `SHA-256: OK/MISMATCH` and `Signature: OK/INVALID/none` |

### Network Shell Commands

Reads/writes the network configuration through the `NetSettingsBlock`, persisted as JSON under `AIOS_DATA_DIR`/`network.json`.

| Command | Arguments | Description |
|---------|-----------|-------------|
| `net get` | — | Show the current network configuration (JSON) |
| `net set` | `key=value [key2=value2 ...]` | Apply a partial update (e.g. `hostname=myhost`, `listen_port=9090`, `allow_private_access=true`, `dns={"primary":"1.1.1.1"}`); valid keys: `hostname`, `listen_port`, `connect_timeout_ms`, `max_connections`, `user_agent`, `allow_private_access`, `proxy` (JSON object or `null`), `dns` (JSON object with `primary`/`secondary`/`search_domains`) |
| `net reset` | — | Restore factory defaults and persist |

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

# Safe mode (skip third-party disk blocks, disable the bridge)
cargo run --bin aios -- --safe-mode
```

At boot the header and the System tab show the detected **AI Tier** (e.g. `Tier1`/`Tier2`) plus CPU/RAM; safe mode is shown as `SAFE MODE`.

### Tabs

| # | Tab | Content |
|---|-----|---------|
| 1 | System & HW | CPU, RAM, OS/kernel, GPU, AI Tier, RAM gauge |
| 2 | Blocks & Svc | Block list (select/restart/unload/load) + process list |
| 3 | AI Console | LLM chat with slash commands |
| 4 | Studio Bridge | Bridge server status, URL, REST/WebSocket endpoints |
| 5 | Network & Store | Network settings (IPC) + installed block store |
| 6 | Web | Text-mode browser (omnibox, links, history, native viewer) |
| 7 | Shell | Command line (`ps`, `blocks`, `store`, `net`, ...) |

### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `Tab` / `F1` / `?` | Next tab / help overlay |
| `1`-`7` | Direct tab select |
| `Alt`+`1`-`7` | Direct tab select even while typing in the Shell / Web URL / AI input / net line |
| `W` | Launch the AIOS GUI dashboard (`aios-gui`) |
| `Space` | Pause/resume event log |
| `q` / `Ctrl+C` | Quit |

### Blocks Tab (2)

| Key | Action |
|-----|--------|
| `j` / `k` | Move block selection down / up |
| `r` | Restart the selected block |
| `k` | Unload the selected block |
| `l` | Load a block from disk (prompts for a path) |

### AI Console (Tab 3)

The AI Console is an interactive chat with the LLM. Press `i` to enter query mode, type a message (or a `/`-command), press `Enter` to send. The footer shows the live backend/model/temperature/tokens/state. Responses **stream in live**: the growing answer is rendered in yellow while the request is in flight and the final text is appended to the transcript when done. Long responses are word-wrapped to the pane width; user prompts are shown in cyan, errors in red.

| Key | Action |
|-----|--------|
| `i` | Enter query mode |
| `Enter` | Send the query or run a slash command |
| `Esc` | Exit query mode / close help |
| `Up` / `Down` | Navigate the last 50 prompts |
| `h` | Toggle the built-in help panel |
| `q` | Quit (when not typing) |

Slash commands are typed at the input line (e.g. `/status`) and executed with `Enter`:

| Command | Arguments | Description |
|---------|-----------|-------------|
| `/help` | — | Open the help panel + command summary |
| `/status` | — | Show backend, model, API key state, parameters, detected local GGUF models |
| `/clear` | — | Clear the chat output |
| `/history` | — | List the recent prompts |
| `/system` | `<prompt>` | Set the system prompt (no argument prints the current one) |
| `/model` | `<name>` | Set the model (no argument prints the current one) |
| `/backend` | `<groq\|openrouter\|google\|micro\|full>` | Switch backend/provider; local backends reset the API key |
| `/key` | `<api-key>` | Set the API key (no argument clears it) |
| `/temp` | `<0.0-2.0>` | Set sampling temperature |
| `/tokens` | `<1-8192>` | Set max output tokens |
| `/preset` | `<name>` | Apply a prompt template as the system prompt |
| `/preset` | `<name> <text>` | Define/override a prompt template |
| `/preset` | `list` / `del <name>` | List templates / delete one |
| `/save` | — | Persist the chat to disk (`AIOS_DATA_DIR/chat.jsonl`) |
| `/load` | — | Restore the chat from disk, replacing the current transcript |

Backend/model/key changes rebuild the shared `LlmEngine` asynchronously, so the HTTP `POST /api/v1/llm/query` endpoint uses the same configuration as the console. Cloud backends need an API key (`AIOS_LLM_API_KEY` or `/key`); local backends need a GGUF model (`AIOS_MODEL_PATH`).

The chat is **auto-persisted**: after every completed reply and on quit it is saved as JSON Lines to `AIOS_DATA_DIR/chat.jsonl` (default `aios_data/chat.jsonl`), and restored into the transcript on the next boot. Built-in prompt templates: `assistant`, `code`, `translator`, `explainer` (customize or add your own with `/preset <name> <text>`).

### Network & Store (Tab 5)

| Key | Action |
|-----|--------|
| `n` | Edit network settings (input line, applied over IPC as `key=value` pairs) |
| `g` | Show the current network configuration (JSON) |
| `s` | Refresh the installed block store list |

The same network commands are available from the Shell as `net get` / `net set`. Store operations (`store list`, `store search`, `store install`) are also available from the Shell.

### Web Tab (6)

The built-in text-mode browser loads pages in the background (the TUI stays responsive). The omnibox accepts a full URL, a bare host (`example.com`), or a plain search query (searched via DuckDuckGo). A sidebar lists the links of the current page; the page text wraps to the pane width and can be scrolled. The current page can be saved as a **bookmark** (`a`) and managed in the **bookmarks panel** (`m`); bookmarks persist in `AIOS_DATA_DIR/web_bookmarks.json`.

| Key | Action |
|-----|--------|
| `g` | Focus the omnibox |
| `Enter` | Search / navigate (when the omnibox is focused) |
| `j` / `k` | Move link selection down / up |
| `o` / `Enter` | Open the selected link |
| `u` / `d` | Scroll page text up / down by 1 line |
| `PageUp` / `PageDown` | Scroll page text up / down by 20 lines |
| `b` | Go back to the previously visited page |
| `B` | Open the current page in the full native browser window (WebView2) |
| `n` | Open the selected link in the native browser window |
| `a` | Save the current page as a bookmark (name prefilled with the page title) |
| `m` | Toggle the bookmarks panel (replaces the links list while open) |
| — | Inside the bookmarks panel: `j`/`k` move, `o`/`Enter` open, `d` delete, `Esc` close |
| `Esc` | Unfocus the omnibox or close the bookmarks panel |

### Shell Tab (7)

| Command | Arguments | Description |
|---------|-----------|-------------|
| `ps` | — | List running processes |
| `blocks` | — | List loaded blocks |
| `kill` | `<pid>` | Kill a process |
| `spawn` | `<wasm-path-or-file>` | Load a block from disk |
| `store list` | — | List installed blocks |
| `store search` | `<query>` | Search the block store catalog |
| `store install` | `<name>` | Install a block from the store |
| `net get` | — | Show the current network configuration (JSON) |
| `net set` | `key=value [key2=value2 ...]` | Apply a partial network update |
| `status` | — | Uptime, bridge state, AI tier, RAM, block count |
| `logs` | — | Show the last 20 event log entries |
| `restart` | — | Re-probe hardware and re-initialize subsystems |
| `help` / `?` | — | Show all available commands |
| `clear` | — | Clear the shell output |

`Esc` clears the current input line. Every keystroke on the Shell tab is captured by the input line, so `q` quits only from other tabs.

### Safe Mode

With `--safe-mode` AIOS boots with a minimal shell only: third-party disk blocks are not discovered, the bridge HTTP/WebSocket server is disabled, and the header shows `SAFE MODE`. Core blocks, the scheduler, the watchdog, the LLM engine and the TUI/Shell remain available.

---

## GUI (`aios-gui`)

### Launch

```bash
cargo run --bin aios-gui
```

### Layout

```
┌──────────┬───────────────────────────────────────────┐
│          │  AIOS v2.9.1 | HW Tier | IPC: 0 pkts      │  ← Top bar
│ System   │───────────────────────────────────────────│
│ WASM     │                                           │
│ AI Studio│          Central panel                    │
│ App Store│     (changes per selected tab)            │
│ Network  │                                           │
│ Deps     │                                           │
│ Browser  │                                           │
│ ──────── │───────────────────────────────────────────│
│ Quick    │  F1-F8 tabs | AIOS Dashboard | Status...  │  ← Bottom bar
│ Actions  │
└──────────┴───────────────────────────────────────────┘
  Sidebar          Main area
```

### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `F1`-`F8` | Switch tab |
| `j` | Move selection down |
| `k` | Move selection up |

### Mouse

- Click tab names in sidebar to switch
- Click rows in tables to select
- Click buttons for actions (Kill, Suspend, Load, etc.)
- Type in search boxes (Marketplace)

### Tabs

#### System Dashboard (F1)
- **Stat cards**: RAM (used/total), Blocks count, Processes count, Watchdog status
- **System panel**: CPU model, cores, threads, AVX2/AVX-512/SSE4.2, GPU, storage, detected HW Tier
- **RAM Usage**: progress bar + sparkline chart (last 60 samples)
- **Priority Distribution**: bar chart (Background / Low / Normal / High / Critical)
- **Processes**: table (PID, Name, Priority, State, RAM, CPU ms, Crashes) + Refresh, Kill, Suspend, Resume
- **Activity Log**: scrollable log with color-coded messages

#### WASM Blocks (F2)
- **Table**: ID, Name, Version, State (badge), Size, Dependencies
- **Actions**: Refresh, Load Block (2-step dialog), Unload, Hot-Swap
- **Load Dialog**: Step 1 — enter block name, Step 2 — enter version, Enter/Cancel

#### AI Studio (F3)
- **Chat panel**: message list, **streaming replies** (live yellow partial line while in flight), error highlights
- **Input**: type a message or a slash command, `Enter` (or the Send button) submits; focus stays in the input
- **Commands**: `/help /status /clear /history /system /model /backend /key /temp /tokens /preset /save /load` (same grammar as the kernel TUI AI Console)
- **Status line**: backend, model, temperature, token budget, busy/error state
- **Async model**: responses stream over a background tokio task; the UI stays responsive, partial text appears live
- **Persistence**: the chat auto-saves to `AIOS_DATA_DIR/chat.jsonl` after every completed reply and on window close (restored at boot); `/preset` templates persist to `AIOS_DATA_DIR/presets.json`

#### App Store (F4)
- **Search box**: filter by name, description, or tags
- **Table**: Name, Version, Author, Status (badge), Downloads
- **Actions**: Install, Update, Uninstall
- **Status bar**: shows operation result

#### Network Settings (F5)
- **Form**: hostname, listen port, connect timeouts, private access toggle, DNS, user agent
- **Actions**: Save (applies a partial JSON update over IPC to `net_settings`), Reset (restores defaults)
- **Preview**: live JSON of the config being edited

#### Dependencies (F6)
- **Summary**: block count + edge count
- **Load Order**: visual chain (block A → block B → block C)
- **Table**: Block name, Depends On, Depended By

#### Native Browser (F7)
- **Omnibox**: type a full URL (`https://...`), a bare host (`example.com`), or a plain search query (`rust scheduler`) — Enter resolves and loads it
- **Navigation buttons**: Back, Forward; **Open Browser** / **Close** toggle
- **Native engine**: the browser window is a real WebView (WebView2 / WebKitGTK / WKWebView) with full cookies, JavaScript and history support; the first navigation opens the window automatically
- **Non-blocking open**: the WebView is spawned on a background thread, so the dashboard stays responsive while it starts; repeated open attempts during startup are ignored and the status line reports `Opening browser: ...` / failure
- **Status line**: shows the resolved target or the last action/error

#### Files (F8)
- **Toolbar**: Refresh, Switch panel, Sort, Up, Mkdir, Rename, View, Copy, Move, Delete, HOST r / HOST w (grant `vfs:host:read` / `vfs:host:write` capabilities)
- **Two panels**: navigate `AIOS://` (sandbox) and `HOST://` (real paths); single click selects and activates the panel, double-click opens a directory or AI-previews a file
- **Mkdir / Rename dialog**: text field + OK / Cancel (Enter / Esc)
- **AI Preview**: collapsible panel with the smart preview of the selected file
- **Jobs**: background copy/move/delete operations show live progress bars and completion status
- **ACL line**: shows which HOST capabilities are currently granted

### Status Bar

The bottom bar shows `HW Tier | IPC: N pkts | F6=Deps F7=Browser F8=Files`, where N is the live IPC packet counter, plus the last operation result.

### Theming

Dark theme with customizable colors in `aios-gui/src/theme.rs`:

| Color | Usage |
|-------|-------|
| `accent` (#00C8DC) | Headers, active tabs, highlights |
| `success` (#32C850) | Running blocks, OK status, positive values |
| `warning` (#F0B41E) | Suspended, High priority, warnings |
| `danger` (#E63C3C) | Crashed, Critical, errors |
| `info` (#64A0FF) | Low priority, informational |
| `muted` (#A5A5B9) | Dimmed text, labels, Terminated |
| `surface` (#181820) | Main background |
| `surface_alt` (#22222E) | Card backgrounds, alternating rows |
| `button_bg` (#2E2E3E) | Button backgrounds, distinct from cards |
| `extreme_bg_color` (#1E1E2A) | TextEdit input field backgrounds |

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
