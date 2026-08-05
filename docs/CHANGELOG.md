# AIOS Development Log

## v2.9.1 — GUI AI Studio: streaming, `/preset`, persistence parity (2026-08-05)

### `aios-gui`: AI Studio matches the TUI AI Console
- Chat replies now **stream**: deltas arrive over an unbounded channel from a background tokio task and are rendered live (yellow line) while the request is in flight. Requests are deduplicated into a single worker slot (`pending_ai`) so concurrent sends cannot corrupt the transcript.
- Chat log persists as JSON Lines to the shared `AIOS_DATA_DIR/chat.jsonl` (same schema as the TUI): auto-saved after every completed reply and on window close, restored at boot via `ai_load_persisted`; manual control via `/save` and `/load`.
- Prompt templates persist to `AIOS_DATA_DIR/presets.json` (pretty JSON, same format as the TUI): `/preset <name>` applies a template as the system prompt, `/preset <name> <text>` defines one, `/preset list` lists all, `/preset del <name>` removes one. Built-in seeds (`assistant`/`code`/`translator`/`explainer`) are overlaid by saved presets at boot.
- New slash commands: `/system <text>`, `/history`, `/preset`, `/save`, `/load` (added to the AI Studio help panel); UI hints updated.
- The kernel TUI header version is bumped to `AIOS v2.9.1`.
- Files: `aios-gui/src/app.rs`, `aios-gui/src/main.rs`, `aios-gui/src/tabs/ai_studio.rs`, `aios/src/tui/ui.rs`

## v2.9.0 — AI Console: chat persistence, `/preset` templates, streaming (2026-08-05)

### `aios-llm`: streaming queries
- New `LlmEngine::query_stream(&LlmRequest, LlmStreamSink)` pushes text deltas over a `tokio` unbounded channel instead of returning a full response.
- Cloud backend (`cloud.rs`) reads the HTTP body as a byte stream, splits SSE `data:` lines and extracts deltas from both the OpenAI (`choices[0].delta.content` / legacy `choices[0].text`) and Google AI Studio (`candidates[0].content.parts[0].text`) shapes via the new `extract_stream_delta` helper.
- Local backend (`local.rs`) refactored: `query` and `query_stream` share a `generate_tokens` loop that calls an `on_delta` callback per decoded token, so local models stream per-token as well.
- 4 new unit tests for `extract_stream_delta`.
- Files: `aios-llm/src/types.rs`, `aios-llm/src/cloud.rs`, `aios-llm/src/local.rs`, `aios-llm/src/factory.rs`, `aios-llm/Cargo.toml`

### `aios`: AI Console persistence and prompt presets
- Responses now **stream** into the AI Console: deltas accumulate in `ai_stream` and are rendered live in yellow while the request is in flight; the final text is appended to the transcript when done. `/help` documents the streaming behavior.
- Chat log persists as JSON Lines to `AIOS_DATA_DIR/chat.jsonl` (default `aios_data/chat.jsonl`): auto-saved after every completed reply and on quit, restored into the transcript at boot; manual control via `/save` and `/load`.
- New `/preset` command family with four built-in templates (`assistant`, `code`, `translator`, `explainer`): `/preset <name>` applies a template as the system prompt, `/preset <name> <text>` defines/overrides one, `/preset list` lists all, `/preset del <name>` removes one.
- Files: `aios/src/tui/mod.rs`, `aios/src/tui/app_state.rs`, `aios/src/tui/ui.rs`

## v2.8.0 — 7-tab kernel TUI, safe mode, GUI AI Studio & Network Settings (2026-08-05)

### `aios`: kernel TUI upgraded to the 7-tab spec
- The `aios` TUI (ratatui) now ships with 7 tabs: **System & HW**, **Blocks & Svc**, **AI Console**, **Studio Bridge**, **Network & Store**, **Web**, **Shell**. Direct selection via `1`-`7`, `Alt`+`1`-`7`, `Tab`/`F1`, help overlay via `?`. The header shows the detected **AI Tier** and the app version (`AIOS v2.8.0`).
- Blocks tab gained `r`/`k`/`l` (restart / unload / load from disk) in addition to selection. The Web tab implements the full spec keymap (`g` omnibox, `j/k` link selection, `o`/`Enter` open, `u/d` and `PageUp`/`PageDown` text scroll, `b` back, `B` native viewer, `n` open selected link natively). The Shell tab implements `ps`, `blocks`, `kill`, `spawn`, `store list/search/install`, `net get/set`, `status`, `logs`, `restart`, `help`, `clear` and is fully typed inline (every keystroke goes to the input line; `q` quits only from other tabs).
- The `n`/`g` network keys moved to the Network & Store tab; the old kernel-wide `b`/`r` hotkeys and the `dispatch_open_url` bridge hack were removed.
- Files: `aios/src/tui/mod.rs`, `aios/src/tui/ui.rs`, `aios/src/tui/app_state.rs`

### `aios`: `--safe-mode` boot flag
- New `--safe-mode` CLI flag: AIOS boots with a minimal shell only — third-party disk blocks are not discovered, the bridge HTTP/WebSocket server is disabled, the header shows `SAFE MODE`. Core blocks, scheduler, watchdog, LLM engine and TUI/Shell remain available.
- Files: `aios/src/main.rs`, `aios/src/orchestrator.rs`

### `aios-gui`: AI Studio and Network Settings tabs
- The GUI was restructured from the 6-tab spec into 7 tabs: **System Dashboard** (merged overview + metrics + processes), **WASM Blocks**, **AI Studio**, **App Store**, **Network Settings**, **Deps**, **Native Browser**. The `processes` and `metrics` tab modules were removed.
- AI Studio: async LLM chat with slash commands (`/help /backend /model /key /temp /tokens /clear /history`), status line, Enter-to-send with focus retention; requests run on a background tokio task so the UI stays responsive.
- Network Settings: hostname/port/timeouts/private-access/DNS/user-agent form with Save (partial JSON update over IPC to `net_settings`) and Reset, plus a live JSON preview.
- Status bar now shows `HW Tier | IPC: N pkts | F6=Deps F7=Browser` with a live IPC packet counter.
- Files: `aios-gui/src/app.rs`, `aios-gui/src/tabs/mod.rs`, `aios-gui/src/tabs/ai_studio.rs`, `aios-gui/src/tabs/network.rs`, `aios-gui/src/tabs/overview.rs`, `aios-gui/Cargo.toml`

## v2.7.0 — Bug-fix pass: correctness, robustness, UI fixes (2026-08-04)

### `aios-browser`: `extract_text` returns empty on pages with `<!DOCTYPE html>`
- **BUG-021 (HIGH)** — `HtmlParser::extract_text` iterated the body's own text, so a document root of `<!DOCTYPE html><html>...` yielded no text at all. The parser now walks the element children of the document root and returns the visible text regardless of doctype.
- New regression test `test_extract_text_with_doctype`.
- Files: `aios-browser/src/html_parser.rs`

### `aios-ipc`: `DropOldest` policy evicted the most critical packet
- **BUG-022 (MED)** — `IpcBus` `DropOldest` popped from the front, but the queue is ordered highest-priority-first, so overflow discarded the most important packet and kept the least important one. It now drops from the back (lowest priority) instead.
- New test `test_drop_oldest_keeps_highest_priority`.
- Files: `aios-ipc/src/bus.rs`

### `aios-bridge`: process listing missed the newest process; status metrics always reported 0
- **BUG-023 (MED)** — `status_handler` probed PIDs `0..process_count`, but process IDs start at 1, so the newest process was never listed. It now uses `scheduler.all_processes()`.
- **BUG-024 (MED)** — the `MetricType::All` branch captured `process_count` after the scheduler was dropped, hardcoding `0`. The count is now read before dropping.
- Files: `aios-bridge/src/server.rs`

### `aios-tui`: Web back-navigation ping-ponged forever
- **BUG-025 (MED)** — pressing `b` to go back pushed the current page back onto the history stack, so navigating back to A and re-pressing `b` returned to B (A↔B loop). `load_url` now takes `push_history: bool`; back navigation pops without re-pushing. Updated all call sites (navigation, link click, sidebar open).
- **BUG-026 (MED)** — rapid `B` presses could spawn a second native browser window. A `WEB_BROWSER_SPAWNING` atomic guard now allows only one spawn in flight.
- Files: `aios-tui/src/main.rs`

### `aios-gui`: browser open blocked the UI for up to 45 s and could double-spawn
- **BUG-027 (MED)** — opening the WebView happened synchronously on the egui thread. It now spawns on a background thread with `pending_browser`/`pending_browser_error` slots and a `browser_opening` guard; `poll_browser_open` picks up the result each frame.
- Files: `aios-gui/src/app.rs`

### `aios-search`: DuckDuckGo `uddg` redirect URL was not resolved
- **BUG-028 (MED)** — search result URLs pointing at `/l/?uddg=...` were returned as-is. `resolve_duckduckgo_url` now unwraps the `uddg` parameter (skipping non-http/s values).
- `aios-search` adds the `url` dependency; 4 new tests.
- Files: `aios-search/src/backends.rs`, `aios-search/Cargo.toml`

### `aios-context`: telemetry and compression batches clobbered earlier data
- **BUG-029 (MED)** — `save_telemetry` wrote every batch under the same key, so later batches overwrote earlier ones. Keys now come from a monotonic `TELEMETRY_NEXT_KEY` counter in `META_TABLE`.
- **BUG-030 (MED)** — compressed-telemetry chunk keys were derived from a metric key/timestamp that collided each round, so every compression overwrote the previous chunk. Chunks now use a monotonic `next_chunk_id`.
- New tests `test_save_telemetry_does_not_clobber_previous_batches` and `test_multiple_compression_rounds_do_not_collide`.
- Files: `aios-context/src/persistence.rs`, `aios-context/src/compressed_telemetry.rs`

### `aios-core`: `response_err` dropped the error message
- **BUG-031 (MED)** — `response_err` returned `Payload::Empty`, discarding the error text. The message is now carried as `Payload::Text(msg)`.
- New test `test_response_err_carries_message`.
- Files: `aios-core/src/ipc_protocol.rs`

### `aios-security`: capability `remaining_ms` was inverted
- **BUG-032 (LOW)** — `remaining_ms()` computed `now − expires`, returning near-zero for long-lived capabilities. Now `expires_at_ms.saturating_sub(now_ms())`.
- New test for a future expiry.
- Files: `aios-security/src/capability.rs`

### `aios-process-mgr`: inheritance counter never counted
- **BUG-033 (LOW)** — `total_inheritances` was declared but never incremented, so it always reported 0. It now increments in both the lock-acquire and resource-request boost paths and is surfaced via `state()`.
- New tests.
- Files: `aios-process-mgr/src/priority_inheritance.rs`

### `aios-wasm`: linear-memory restore silently truncated oversized data
- **BUG-034 (LOW)** — `restore_linear_memory` copied `min(data, memory)`, silently dropping bytes. It now fails loudly when data exceeds the linear memory; `aios-live-update` logs a warning when restore fails.
- New test `test_restore_linear_memory_rejects_oversized_data`.
- Files: `aios-wasm/src/sandbox.rs`, `aios-live-update/src/wasm_engine.rs`

### `aios-process-mgr`: CPU affinity was applied to the scheduler thread
- **BUG-035 (LOW)** — the OS affinity call targets the calling thread, so affinity was applied to the scheduler thread instead of the spawned process thread. The mask is now stored per-thread and applied by the spawned thread itself before running the payload; `validate_cores` pre-validates the mask.
- Files: `aios-process-mgr/src/cpu_affinity.rs`, `aios-process-mgr/src/scheduler.rs`

### `aios`: TUI/bridge lock-order inversion
- **BUG-036 (LOW)** — the TUI blocks tab locked `scheduler → registry` while the bridge used `registry → scheduler`, a classic deadlock ordering. Both now lock `scheduler → registry`; the process list iterates `all_processes()` instead of hardcoded PIDs 1..5.
- Files: `aios/src/tui/ui.rs`

### `aios`: WMIC `AdapterRAM` 32-bit overflow
- **BUG-037 (LOW)** — a 0xFFFFFFFF `AdapterRAM` value (VRAM > 4 GB) was reported as a bogus ~4 GB instead of unknown. Such values are now treated as unknown (0).
- Files: `aios/src/hw_probe.rs`

### `aios-wasm`: `timeout_ms` now enforced via an epoch ticker
- **BUG-038 (LOW)** — `timeout_ms` was never enforced as wall-clock time: no thread ever called `Engine::increment_epoch()`, so the epoch deadline was unreachable and only the fuel limit bounded runaway wasm. A per-engine background ticker (`EpochTicker`) now increments the epoch every `timeout_ms / 4`; stores are armed with `EPOCH_TICKS_PER_TIMEOUT = 4` ticks and `call_func`/`instantiate` (plus the executor's `init`/`start`) re-arm the deadline before every wasm call, so each call is bounded by `timeout_ms` while long-lived stores keep working.
- New tests `test_epoch_timeout_interrupts_runaway_wasm` and `test_epoch_deadline_rearmed_between_calls`; `aios-wasm` total now 56 unit tests.
- Files: `aios-wasm/src/sandbox.rs`, `aios-wasm/src/executor.rs`

### Tests & verification
- Workspace suite: 82 test targets, 0 failures in debug. `cargo clippy --workspace --all-targets -- -D warnings` — 0 warnings; `cargo fmt --all --check` clean.
- 17 new tests covering each fix (incl. the two epoch-timeout tests).

## v2.6.0 — AI Console: slash commands, help panel, runtime reconfiguration (2026-08-04)

### Kernel TUI (`aios`) — AI Console (tab 3) overhaul
- **Slash-command system** in the AI query line: `/help`, `/status`, `/clear`, `/history`, `/system <prompt>`, `/model <name>`, `/backend <groq|openrouter|google|micro|full>`, `/key <api-key>`, `/temp <0.0-2.0>`, `/tokens <1-8192>`
- **Runtime reconfiguration**: `/backend`, `/model` and `/key` rebuild the shared `LlmEngine` inside `BridgeContext` asynchronously, so the HTTP `POST /api/v1/llm/query` endpoint uses the same configuration as the console; every query also re-applies the current console config before running
- **Help panel (справка)**: built-in styled reference of keys + slash commands in the AI tab, toggled with `h` or `/help`, dismissed with `Esc`/`h`/`q`
- **Prompt history**: `Up`/`Down` navigate the last 50 prompts while typing; `/history` prints them; `/clear` resets the chat
- **Status footer**: live `backend | model | temp | tokens | state` line (`thinking...` / `done: Nms[, N tokens]` / error); `/status` prints a full report incl. detected local GGUF models
- **Render polish**: long responses word-wrapped to the pane width, user prompts (`>`) styled cyan, `[error]` lines red, larger output buffer (200 entries)
- `TuiApp` gains `ai_system_prompt`, `ai_config`, `ai_history`/`ai_history_index`, `ai_show_help`, `ai_status`; new helpers `submit_ai_query`, `handle_ai_command`, `apply_config_async`, `push_ai_line`

### `aios-llm`: config introspection
- `LlmEngine::config() -> LlmConfig` plus `config()` accessors on `CloudEngine`/`LocalEngine`; new `provider_name(&CloudProvider)` helper and `LlmEngine::backend_label()`
- 1 new unit test `test_engine_config_accessor` (aios-llm total: 9)

### Tests & verification
- Full workspace suite grows to 1149 tests, all passing in debug and release (changed crates)
- `cargo clippy --workspace` — 0 warnings, `cargo fmt --all` clean

## v2.5.0 — Ed25519-signed block manifests with trust enforcement (2026-08-04)

### `aios-store`: real Ed25519 signing & verification
- `manifest::canonical_bytes()` — deterministic canonical serialization `aios-manifest-v1\n` + name/version/description/author/sorted capabilities/size/`wasm_sha256`
- `manifest::sign_manifest(manifest, &SigningKey) -> SignatureInfo` — Ed25519 signature (`ed25519-dalek` v2, `rand_core` feature) over the canonical bytes; `verify_signature` now runs a real `verify_strict` check; `verify_signature_with_keys(manifest, &[String])` validates against a list of trusted public keys
- Workspace root `Cargo.toml` gains `ed25519-dalek = { version = "2", features = ["rand_core"] }`; `aios-store` adds `ed25519-dalek` dep and `rand_core` dev-dep (for `OsRng` in tests)
- 11 manifest tests: sign/verify roundtrip, tampered wasm/capabilities, wrong key, trusted-key accept/reject, missing signature error, bad algorithm

### `aios-store`: signature enforcement in `BlockInstaller`
- `BlockInstaller.trusted_keys: Vec<String>` — when non-empty, `install_from_bytes` rejects unsigned manifests and any manifest not signed by one of the trusted keys
- New constructors `with_trusted_keys(dir, keys)` and `from_env(dir)`; `Default` reads `AIOS_TRUSTED_PUBLIC_KEYS` (`,`/`;`-separated); with no trusted keys the signature is still verified against the embedded signing key
- Sidecar now persists the full `ManifestInfo` (including the signature) so signed installs remain verifiable via `store verify`
- 16 installer tests: reject unsigned/wrong key, accept correct signature, tampered manifest reject, env parsing, signature preserved in sidecar

### `aios-store`: per-source trust policy
- `StoreSource.trusted_public_keys: Vec<String>` (`#[serde(default)]`); `StoreManager::verify_source_manifest(source, manifest)` rejects a manifest not signed by one of the source's trusted keys; enforced in `install()` and `update()`
- `github_default` inherits the official key from `AIOS_OFFICIAL_PUBLIC_KEY` via `official_public_key()`; `StoreManager::new`/`with_sources` now use `BlockInstaller::from_env`
- 2 manager tests: reject untrusted / accept trusted signature from a source

### `aios-tui` shell: `store sign` / `store verify`
- `store sign <file.wasm> [name] [version] [--key <secret_hex>]` — computes SHA-256, builds the manifest, signs it with Ed25519 (key from `AIOS_STORE_SIGNING_KEY` if `--key` is omitted), writes the signed sidecar JSON next to the file and prints the public key
- `store verify <name>` — checks the installed block: SHA-256 of the binary + Ed25519 signature of the sidecar manifest
- `aios-tui` now depends on `ed25519-dalek`

### Tests & verification
- `aios-store` grows to 56 unit tests; total workspace suite 1148 tests, all passing
- Full workspace build, `cargo test --workspace`, `cargo clippy --workspace` (0 warnings), `cargo fmt --all` all pass

## v2.4.0 — Net settings block in kernel + store publish (2026-08-03)

### Kernel `aios` binary: net settings over IPC
- `net_settings` block registered at boot in the kernel registry and wired into the `MessageRouter` (`aios/src/orchestrator.rs`); its `BlockId` is exposed as `OrchestratorState::net_block_id`
- New `n` hotkey in the kernel TUI (`aios`): input mode for `key=value` partial network-config updates, dispatched to the block over IPC (`net_set` via `MessageRouter`); the returned config JSON is logged to the Events pane; `Esc` cancels
- TUI event plumbing: `TuiApp` gains `net_input` / `net_mode`; `ui.rs` renders a net prompt line and updated help; Alt+digit tab switch now also resets net mode

### `aios-tui` shell: `store publish`
- New `store publish <file.wasm> [name] [version]` command — reads the file, computes SHA-256, base64-encodes the wasm and posts a `StorePublishRequest` to `POST /api/v1/store/publish` on the local update service (bridge port from `AIOS_BRIDGE_PORT`, default `8080`); name defaults to the file stem, version to `1.0.0`
- `StorePublishRequest` / `StorePublishResponse` in `aios-bridge::dto` are now both `Serialize + Deserialize` so a client can round-trip them
- `aios-tui` now depends on `aios-bridge`, `sha2`, `hex`, `base64`

### Tests & verification
- New kernel tests in `aios/src/orchestrator.rs` (4): `net_settings` registered in the registry, `net_get` / `net_set` / `net_reset` routed over IPC via `MessageRouter`
- Full workspace build, `cargo test --workspace`, `cargo clippy --workspace` (0 warnings), `cargo fmt --all` all pass

## v2.3.0 — Block store: update service + network settings (2026-08-03)

### `aios-store`: sources, catalog, installer, manager
- New module `aios-store::source`: `StoreSource` / `SourceKind` — three block sources: `github:owner/repo`, `local:path`, `http://host:port` (update service)
- New module `aios-store::catalog`: `fetch_index` / `download_block` (async HTTP + local scan), `parse_name_version`
- New module `aios-store::installer`: `BlockInstaller` — installs `{name}_{version}.wasm` + sidecar JSON, verifies SHA-256, `list_installed` / `find_installed` / `uninstall`, `backup` / `rollback` (`.bak`), `check_updates`, semantic `cmp_version`
- New module `aios-store::manager`: `StoreManager` facade — `search`, `install`, `update` (auto rollback on failure), `check_updates`, `parse_source_spec`, `block_on` for sync contexts
- Fix: `rollback` now removes the current (broken/newer) version file before restoring the backup, so `find_installed` returns the reverted version

### New crate `aios-net-config`
- `NetworkConfig` / `ProxyConfig` / `DnsConfig` / `InterfaceConfig` / `ProxyProtocol` with JSON serialization and partial updates (`apply_updates` with validation)
- `NetworkConfigStore`: atomic JSON persistence (temp file + rename) under `AIOS_DATA_DIR`
- `NetSettingsBlock`: `StatefulBlock` over the IPC bus with custom commands `net_get`, `net_set`, `net_reset`, `net_persist`; state extract/restore via bincode

### `aios-bridge`: update-service endpoints
- `GET /index.json` and `GET /store/index.json` — raw on-disk block catalog
- `GET /blocks/{name}.wasm` and `GET /store/blocks/{name}.wasm` — block binary download
- `POST /api/v1/store/publish` — publish a user-created block (base64 wasm + SHA-256 + manifest); serves the local update-service role
- `BridgeContext` gains `blocks_dir` (from `AIOS_BLOCKS_DIR`, default `./blocks`)

### `aios-tui`: shell commands
- `store list | sources | add-source <spec> | search <q> [--source N] | install <name> [--source N] | update [name] [--source N] | uninstall <name> | rollback <name>`
- `net get | net set key=value ... | net reset` — view/change/persist network config through `NetSettingsBlock`

### Tests & verification
- `aios-net-config`: 32 unit tests (validation, JSON roundtrip, block IPC, state roundtrip)
- `aios-store`: 42 unit tests (source URLs, catalog scan, installer, rollback, manager)
- Integration: `test_block_store_update_flow` (search → install → tamper rejection → update → rollback) and `test_net_settings_block_roundtrip`; total integration suite now 30 tests
- Full workspace build, `cargo test --workspace`, `cargo clippy --workspace` (0 warnings), `cargo fmt --all` all pass

## v2.2.9 — Full native browser from the Web tab (2026-08-02)

### `aios-tui`: open any page in the real browser
- `B` on the Web tab opens the current page in the **full native browser** (`aios-webview`: WebView2 — JavaScript, CSS, images, real rendering). The window is reused across key presses and re-created automatically if it closed; opening happens on a background thread so the TUI never freezes
- `n` opens the currently selected link in the native browser window (complementing `o`/`Enter`, which open it in the text view)
- Browser handle lives in a module-level `OnceLock<Mutex<Option<WebBrowser>>>` — no kernel, block registry or scheduler changes
- Text fetches now send a desktop-browser User-Agent + `Accept: text/html` header and use a 15s timeout (`http_client()`), so more sites answer instead of bot-blocking, and a stuck host cannot hang the fetch

## v2.2.8 — Web tab navigation sidebar with history (2026-08-02)

### `aios-tui`: history sidebar in the Web tab
- New fixed-width navigation sidebar (`SIDEBAR_WIDTH = 26`) to the left of the page pane: current page first (marked `▸`), then the visited history newest-first, deduplicated
- Sidebar labels are compacted URLs (`https://www.example.com/deep/path` → `example.com/deep/path`), truncated with `…` to fit the pane
- Focus toggled with `\` (like `g` for the omnibox): `j`/`k`/`Up`/`Down` move the selection, `Enter`/`o` opens the highlighted entry (reloads the current page when it is selected), `Esc` returns to the links list; selection wraps around the list
- Page text width now accounts for the sidebar: `web_page_width()` computes the wrap width from the terminal width minus the sidebar, borders and line prefix; `wrap_width` is derived from it at startup and on every `Event::Resize` (completing the Phase 37 "proportional pane" follow-up)
- New helpers `web_nav_entries()`, `compact_url_label()`, `web_page_width()`; 8 new unit tests

## v2.2.7 — Word-wrapped page text in the Web tab (2026-08-01)

### `aios-tui`: scroll units now match visual lines
- New `wrap_text()` word-wrap helper (no new dependency): wraps every page line to the terminal width, hard-splitting over-long words and preserving blank lines and leading indentation (nested lists/tables keep their structure)
- `draw_web` renders the pre-wrapped lines instead of relying on ratatui wrapping, so a "page line" always equals one terminal row; the scroll hint and `u`/`d`/`PageUp`/`PageDown` scrolling now operate on **visual** lines — pressing `d` moves exactly one visible row and the bottom of a wrapped page is reachable
- `WebState.wrap_width` tracks the terminal width: initialised from `crossterm::terminal::size()` at startup and refreshed on every `Event::Resize`
- `web_scroll` clamps against the wrapped line count; 4 new unit tests for `wrap_text` (word-boundary split, hard split of long words, indent/blank preservation)

## v2.2.6 — Responsive Web tab: background fetch, page cache, link scrolling (2026-08-01)

### `aios-tui`: non-blocking web fetches
- `load_url` / `navigate_web` no longer block the TUI: page and search fetches run on background threads and the result is picked up by `check_page_cache()` each frame (the previously unused `page_cache` outbox is now wired up)
- A monotonic fetch generation counter (`WebState.web_fetch_gen`) drops stale results, so a slow older fetch can never overwrite a newer navigation
- The "Loading..." pane stays live while a page is being fetched

### `aios-tui`: bounded page cache
- `WebState.cache` stores up to `WEB_CACHE_CAP = 20` recently fetched pages keyed by URL (oldest evicted); revisiting or going back (`b`) through a cached URL renders instantly without a network round-trip
- 2 unit tests: cache insert/lookup/dedupe and cap eviction

### `aios-tui`: link list scrolling + heading colors
- The links window scrolls with the selection (`WebState.links_scroll`): with more than `LINKS_VIEW_ROWS = 6` links the window follows the selected row, and the title shows the visible range (`3–8 / 23`)
- Page text now color-codes structure: headings (`#`) render bold cyan, blank lines dark gray
- 3 new unit tests: links-scroll clamping, fetch result application, stale-generation drop

## v2.2.5 — WHATWG-compliant HTML rendering in the TUI Web tab (2026-08-01)

### `aios-browser`: HtmlParser rebuilt on `scraper`/html5ever
- The old regex-based HTML parser was replaced with a **WHATWG-compliant** `html5ever` pipeline (`scraper` 0.21 + `ego-tree` 0.9; most deps were already in the lockfile through `wry` → `dom_query`, so the footprint grew minimally)
- Text extraction is now **structured**: headings become `#`/`###`, lists `•`/`1.`, `pre`/`br` preserve formatting, table rows use `|`, `hr` renders as a rule, images render as `[alt]`; `<script>`, `<style>`, `<head>`, `<iframe>` and hidden elements are skipped
- Link extraction resolves every `href` against the page base URL (protocol-relative and relative links now work), deduplicates, and filters non-web schemes (`javascript:`, `mailto:`, `tel:`, `#anchor`); root URLs are canonicalized without the trailing slash
- **28 unit tests** (up from 21) covering text extraction, links, titles, script stripping and structured layout

### `aios-tui`: Web tab navigation & rendering
- `WebState` gains `history: Vec<String>` — the previously visited URL is remembered before every navigation
- New keys on the Web tab: `b` = back in history, `u`/`d` = scroll the page text ±1 line, `PageUp`/`PageDown` = ±20 lines
- The page text area renders through the visible window height with wrapping (`Wrap { trim: false }`) and a scroll indicator `X–Y` in the title; the links window title documents the full key set
- `draw_web` no longer overflows the page pane; F1 help lists the new keys

## v2.2.4 — Full-featured native browser (WebView) + GUI dashboard hotkey (2026-08-01)

### New crate: `aios-webview` — real browser engine
- Full-featured browser impossible in a terminal (no CSS/JS rendering) — now implemented as a **native WebView** window (WebView2 on Windows, WebKitGTK on Linux, WKWebView on macOS) with cookies, JavaScript and history out of the box
- `WebBrowser::open(target)` spawns the browser on a dedicated background thread (winit 0.30 event loop + wry 0.56 webview) so the caller never blocks; `navigate`/`back`/`forward`/`close` are non-blocking commands posted to the browser's event loop via `EventLoopProxy`
- Persistent profile: cookies/storage survive restarts via `WebContext` under `AIOS_DATA_DIR`/`aios/webview` (OS data dir when unset), honoring `AIOS_DATA_DIR`
- `resolve_target()` omnibox logic: full URL → as-is, bare host → `https://`, plain query → DuckDuckGo (HTML edition); **5 unit tests**
- `launcher` module: locates the `aios-gui` binary (sibling of current exe, then PATH) and spawns it; **2 unit tests**
- Added to workspace members; headless-safe (`cargo test` opens no windows)

### `aios-gui`: Browser tab (7th) with native webview
- New **Browser** tab (F7, `🌐 Browser` in sidebar): omnibox (URL or search query), Back/Forward buttons, Open/Close toggle, status line
- First navigation auto-opens the browser window; the tab drives the native window — cookies, JS and history live in the engine
- Bottom bar updated: `... F6=Deps F7=Browser`; `AiosApp` gains `browser`/`browser_addr`/`browser_status` fields and open/navigate/back/forward/close methods
- New unit test for closed-browser error paths

### `aios-tui` & kernel `aios`: GUI hotkey `W`
- Press **`W`** to launch the AIOS GUI dashboard from either TUI; failure (binary not found) is logged instead of crashing
- F1 help lists the new hotkey

## v2.2.3 — Web omnibox & opaque F1 help (2026-07-31)

### `aios-tui`: Web Tab Omnibox
- The URL bar is now an **omnibox**: type a full URL (`https://...`), a bare host (`example.com`, auto-prefixed with `https://`), or a plain search query (`how does AIOS work`, searched via DuckDuckGo and rendered as a page)
- After `Enter` the omnibox **auto-unfocuses**, so the input no longer "gets stuck active" — you can immediately navigate the results with `j`/`k` and open a link
- `Enter` now opens the selected link (like `o`) when the omnibox is not focused
- New `search_query` field in `WebState`; the bar shows `search: <query>` for search pages
- New unit tests for `is_url_input` URL-vs-query detection (4 tests)

### `aios-tui`: F1 Help Overlay
- Help is now a **full-screen opaque panel**: a `Clear` is rendered first and the content is padded to fill the screen, so the dashboard background no longer bleeds into the help text (previously stale cells below the help content remained visible, blending everything together)

### Kernel `aios`: Browser Hotkey (`b`)
- `dispatch_open_url` now normalizes the input: bare hosts become `https://` URLs and plain queries become DuckDuckGo search links, opened in the OS default browser; the input prompt reads `URL/query:`

## v2.2.2 — Safe-Mode Shell fixed in aios-tui (2026-07-31)

### `aios-tui` & `aios-watchdog`
- **BUG-020 fixed:** all SafeModeShell commands (`ps`, `blocks`, `kill`, `spawn`, `load`, `unload`, `status`, `logs`, `restart`, `help`, `exit`) previously returned `Error: Unknown command` on the Shell tab — `execute_shell_cmd` routed everything but `fetch`/`search`/`open`/`clear` into `ShellCommand::Unknown`, bypassing `SafeModeShell::parse_command`
- Commands now route through the single `SafeModeShell::parse_command` parser, restoring the full safe-mode command set in the TUI
- `help`/`?` now also list the TUI-specific commands (`fetch`, `search`, `open`, `clear`)
- `blocks` output cleaned: block state printed as `Active` instead of `Some(Active)` via `registry.topology_with_state()`

## v2.2.1 — Alt+digit tab switching (2026-07-31)

### `aios-tui` & kernel `aios`
- New `Alt+1`-`Alt+7` hotkeys switch tabs in `aios-tui` even while the Shell command line, the Web URL bar, the block-load prompts, or the F1 help overlay is active — previously digit keys were consumed by the active input field, making tab switching impossible from the Shell tab
- Kernel `aios` gains `Alt+1`-`Alt+4` tab switching, which also works while the browser URL prompt (`b`) or the AI query line is active; switching exits `browser_mode`/`ai_mode`
- Plain digit tab switching in kernel `aios` no longer steals digits typed into the AI query line
- Refactored the seven digit-key branches in `aios-tui` into a shared `switch_tab` helper

## v2.2.0 — Phase 33: Browser Block Out of the Box (2026-07-31)

### `aios-browser`: First-Class Kernel Block (`BrowserBlock`)
- New `BrowserBlock` implementing `StatefulBlock` (`aios-browser/src/block.rs`), exported as `aios_browser::BrowserBlock`
- IPC commands: `browse` (fetch + parse page, returns bincode-serialized `Page`), `open_native` (open URL in OS default browser via `open` crate), `browser_status` (config + state as JSON); `HealthCheck` supported
- No persistent runtime field — each navigation runs on a dedicated on-demand current-thread Tokio runtime, safe from both sync and async callers (fixes runtime-drop panic inside async contexts)
- State extract/restore via bincode (`BrowserConfig` + `BlockState`)
- **7 new unit tests** for `BrowserBlock`

### Kernel (`aios`) — Blocks Registered at Boot
- Fixed: kernel previously booted with an **empty block registry** (contradicting `docs/ARCHITECTURE.md`)
- Boot now registers 4 core blocks (hal, ipc_bus, scheduler, browser), boot-discovers disk blocks from `AIOS_BLOCKS_DIR` (default `./blocks`), and wires the browser block into the `MessageRouter` (`OrchestratorState` gains `router` + `browser_block_id`)
- Browser works out of the box on a fresh machine: no config file, no installed browser, no network needed to start

### Kernel TUI — Browser Hotkey
- New `b` hotkey: URL input mode, `Enter` dispatches `open_native` to the browser block via `MessageRouter`, result logged to Events
- Browser URL prompt line added above the help bar; help bar updated (`[b] browse`)

### `aios-tui` & `aiosd`
- Both binaries now register the browser block at boot alongside hal/ipc_bus/scheduler

### Pre-existing test failures fixed
- `tests/browser_search_tests.rs` `test_html_parser_extract_text` failed — `HtmlParser::extract_text` included `<head>`/`<title>` text; now strips `<head>...</head>` (page body text only), matching real browser rendering
- `tests/browser_search_tests.rs` `test_duckduckgo_parse_results` failed — `DuckDuckGoBackend::parse_html_response` used offset `+7` after `href="` (6 chars), dropping the leading `h` from every URL; corrected to `+6`
- Added unit test `test_extract_text_strips_head` (aios-browser total: 18)
- `tests/chaos_test.rs` `test_chaos_reporter_rapid_fire` failed — asserted plaintext `event #0` for a report that was deliberately zero-knowledge redacted (even indices); assertions now verify redaction (`event #0` absent, `event #1`/`event #99` present, `"redacted":true` present)
- `tests/e2e_pipeline_test.rs` `test_e2e_easylang_wasm_pipeline` failed — `WorkflowCompiler::generate_wat` emitted `init`/`start` with `(result i32)` while `BlockExecutor::execute_block` calls them with an empty results buffer, so the calls errored and `functions_called` never contained `init`/`start`; `init`/`start` now export without a result (matching the executor contract and its unit fixtures)
- `tests/e2e_pipeline_test.rs` `test_e2e_bridge_http_endpoints` failed — the bridge `MetricCollector` was never recorded anywhere, so `/api/v1/metrics` always returned an empty Prometheus text (no `# HELP` lines); added an axum request middleware that records `http_requests_total` counter, `http_last_latency_ms` gauge and `http_request_latency_ms` histogram for every request
- `tests/stress_fault_tolerance.rs` `test_fault_tolerance_scheduler_survives_crash` failed — asserted the high-priority replacement was scheduled immediately, but the scheduler continues the current process until its quantum expires (time-slicing, no mid-quantum preemption — same contract as `test_priority_scheduling`); test now schedules after spawning the replacement



### End-to-End Pipeline Integration Tests (`tests/e2e_pipeline_test.rs`)
- HW & Core Probe: mock_modern profile validation (CPU model, cores, RAM, AI tier, serialization)
- LLM & Intent Routing: IntentParser tests for show/kill/list/check commands (EN/RU)
- EasyLang & WASM Pipeline: EasyLangParser → WorkflowCompiler → wasm → BlockLoader → BlockExecutor chain
- IPC & Context Store: IpcBus send/receive, EmbeddedContextStore telemetry, RingBuffer zero-copy, Crypto hash, PersistentStore redb
- Bridge HTTP Gateway: axum server on ephemeral port, /api/v1/health, /status, /workflow, /metrics, /intent endpoints

### Stress & Fault Tolerance Tests (`tests/stress_fault_tolerance.rs`)
- 50 parallel WASM blocks: registration, execution, identity function verification, IPC data transfer
- IPC throughput: 500 packets across 50 blocks with timing thresholds (<2s send, <2s receive)
- Block panic isolation: crash reporter generates BlockCrash report, remaining 9 blocks stay operational
- Scheduler crash survival: 20 processes → kill victim → 19 survivors + replacement scheduling
- Back-to-back crashes: 10 processes, kill 5, scheduler continues scheduling

### Cross-Platform Install Scripts (`scripts/`)
- `scripts/install.sh` (Linux/macOS): dependency check (git, curl, cargo), rustup auto-install, `cargo build --release`, binary to /usr/local/bin or ~/.local/bin, `~/.aios/{models,blocks,logs}` directory setup, Qwen2.5-0.5B GGUF model download
- `scripts/install.ps1` (Windows): same logic for PowerShell, PATH user env var update, Windows-specific directory layout

### Maintenance
- Fixed pre-existing compilation errors in `chaos_test.rs` (moved-value bug) and `browser_search_tests.rs` (raw string delimiter, wrong import path for BrowserEngine)
- Added `aios-llm`, `aios-builder`, `tokio`, `serde_json`, `portpicker`, `reqwest` to integration test dev-dependencies

## v2.0.0 — Phase 31: Unified `aios` Binary (2026-07-30)

### New crate: `aios` — Unified system binary
- New `aios` crate merges all 17+ workspace crates into a single executable
- `aios` (interactive TUI) and `aios --daemon` (headless server) modes
- Workspace member in root Cargo.toml

### Hardware Detection (`hw_probe.rs`)
- Real CPU detection: brand name, physical/logical cores, x86_64/ARM64 arch, instruction flags (AVX2, AVX-512, SSE4.2, AES-NI, NEON)
- Real RAM detection: total/used/free in bytes and GB via sysinfo
- Real GPU detection: model + VRAM via nvidia-smi (Linux), wmic (Windows), system_profiler (macOS)
- AI tier classification: Tier1 (AVX-512/AVX2+16GB+GPU) / Tier2 (AVX2+4GB) / Tier3 (fallback)

### Async Orchestrator (`orchestrator.rs`)
- Async initialization of all subsystems: IPC bus, Scheduler, BlockRegistry, AccessControl, Watchdog, LLM Engine, WASM Executor, Telemetry (TraceContext/FlightRecorder/MetricCollector)
- Bridge HTTP server (axum) spawns on port 8080 with graceful shutdown support
- Log pipeline via Arc<Mutex<Vec<String>>> shared with TUI

### Interactive TUI Dashboard (`src/tui/`)
- Header: version, status, uptime, CPU, RAM
- 4 navigation tabs via Tab/F1/1-4
- Tab 1: System & HW — CPU, RAM gauge, GPU, OS, AI tier, subsystem status
- Tab 2: Blocks & Processes — BlockRegistry contents, Scheduler process list
- Tab 3: AI Console — interactive LLM query console with real output
- Tab 4: Studio GUI Bridge — bridge URL, API endpoints, status
- Footer: event log stream (3 visible lines) with color coding (ERROR=red, WARN=yellow, Bridge=cyan)
- Hotkeys: q=quit, g=open browser, r=reprobe HW, Space=pause logs, Tab/F1=next tab, 1-4=goto tab

### Dependencies
- Clap for CLI parsing
- ratatui + crossterm for TUI
- sysinfo for hardware detection
- open for browser launch

## v1.3.0 — Phase 30: Shell Tab & F1 Help System (2026-07-30)

### aios-tui: Shell Tab (Tab 7) & F1 Help Overlay
- New Shell tab (tab 7, key '7') with interactive command line
- Type commands at prompt, Enter to execute, ↑/↓ for command history
- F1 help overlay toggled with F1 or '?', dismissed with F1/Esc/'?'
- New shell commands: `fetch <url>` download & load block from URL, `search <query>` web search via DuckDuckGo, `open <url>` navigate web tab to URL, `clear` clear output
- ShellState: input_buffer, output (Vec<String>), command_history, history_pos
- New functions: draw_shell(), draw_help(), execute_shell_cmd()
- Footer updated: 1-7, F1=Help, :=Cmd (removed g/o since they are in help)

## v1.3.0 — Phase 29: Web Browser Tab in TUI (2026-07-30)

### aios-tui: Web Browser Tab (Tab 6)
- New Web tab (tab 6, key '6') in TUI dashboard for keyboard-driven web browsing
- `g` — Focus URL bar, type URL and press `Enter` to navigate
- `o` — Open selected link
- `j/k` — Move link selection up/down
- `Esc` — Unfocus URL bar
- Background fetching via reqwest blocking + HtmlParser from aios-browser
- WebState struct with url_input, current_url, page (PageContent), loading, error, input_focused, scroll
- PageContent struct with url, title, text, links Vec<(String,String)>
- Bilingual documentation updates (CHANGELOG, INTERFACE, ARCHITECTURE)

## v1.1.0 — Phase 25: Secure Web Surfing & Search (2026-07-29)

### aios-browser — New Crate: WASM-Based Web Browser
- New crate `aios-browser` — sandboxed web browser with HTML parser, text renderer, and capability-based network
- `BrowserEngine` — main struct with `navigate(url)` method for fetching and rendering web pages
- `HtmlParser` — extracts text content, links, title from HTML; strips scripts, styles, comments
- `NetworkClient` — HTTP client via `reqwest` with configurable timeout, user-agent, redirect limits
- `Renderer` — converts DOM to markdown-like text output with headings, links, lists
- `Page` type — `url`, `title`, `text_content`, `html`, `links` for structured page data
- `BrowserConfig` — `user_agent`, `timeout_secs`, `max_redirects`, `sandbox_enabled`
- 10 unit tests: text extraction, link parsing, title extraction, URL resolution, strip comments

### aios-search — New Crate: Anonymous Web Search
- New crate `aios-search` — multi-backend anonymous search engine with AI TL;DR summarization
- `SearchEngine` — dispatches queries to configurable backends: DuckDuckGo, SearXNG, Brave
- `DuckDuckGoBackend` — POST via `html.duckduckgo.com/html/`, parses HTML response
- `SearXngBackend` — GET with `format=json`, parses JSON response
- `BraveBackend` — GET via `api.search.brave.com`, requires API key in `X-Subscription-Token` header
- `SearchSummarizer` — integrates with `aios-llm` to generate AI TL;DR summaries of search results
- `SearchConfig` — `backend`, `api_key`, `api_url`, `max_results`, `enable_summary`
- 3 unit tests: config defaults, engine creation, backend URLs

### aios-bridge: Browser & Search REST Endpoints
- `POST /api/v1/browse` — accepts `{ "url": "..." }`, returns title, text content, links
- `POST /api/v1/search` — accepts `{ "query": "...", "backend": "...", "max_results": N, "enable_summary": bool }`, returns results with optional AI summary

## v1.2.0 — Phase 26+27: Atomic Updates, Store, Telemetry & Debug (2026-07-29)

### aios-updater — New Crate: Atomic Dual-Boot & Hot-Swap
- New crate `aios-updater` — atomic updates with dual-boot slots, hot-swap engine, and timed rollback
- `DualBootManager` — A/B slot management with `swap()`, `boot_success()`, `detect_active_slot()`, slot info
- `HotSwapEngine` — wraps aios-live-update's engine for ID-based block hot-swap tracking with counter
- `RollbackManager` — snapshot-based rollback with configurable timeout (default 1s auto-rollback), snapshot pruning
- 12 unit tests: slot creation, swap, boot success, hot-swap, rollback success/timeout/pruning

### aios-store — New Crate: Decentralized WASM Registry
- New crate `aios-store` — WASM block store with SHA-256 validation, Ed25519 signatures, and store registry
- `ManifestInfo` — name, version, description, author, capabilities, wasm_sha256, signature, store_url
- `ManifestValidator` — SHA-256 content validation, Ed25519 signature verification, capability whitelist
- `StoreRegistry` — name@version keyed map with `register()`, `get()`, `find_all()`, `list()`, `unregister()`
- `StoreClient` — HTTP client for fetching store index and downloading WASM blocks
- 9 unit tests: SHA-256 validation (pass/fail), capability validation (valid/invalid), registry CRUD

### aios-telemetry — New Crate: Structured Tracing & Metrics
- New crate `aios-telemetry` — end-to-end structured tracing, flight recorder, Prometheus-compatible metrics
- `TraceContext` — span tree with `begin_span()`, `end_span()`, `set_tag()`, `set_status()`, `to_json()` export
- `FlightRecorder` — ring buffer with kind-based filtering, configurable max events + retention, dump by kind
- `MetricCollector` — counters, gauges, histograms with `snapshot()`, `to_prometheus()` (Prometheus text format)
- 17 unit tests: span nesting, error status, JSON export, flight recorder record/dump/clear, all metric types

### aios-debug — New Crate: Crash Reporting & Panic Handler
- New crate `aios-debug` — zero-knowledge crash reports and custom panic handler
- `CrashReporter` — generates crash reports with optional zero-knowledge mode (hash redaction, no flight data)
- `CrashKind` — Panic, WatchdogTimeout, OOM, BlockCrash, Unknown
- `PanicHandler` — custom panic hook that routes panic info to CrashReporter
- 6 unit tests: report generation, zero-knowledge mode, JSON export, latest/bulk reports

### aios-bridge: Store, Metrics, Traces & Crash-REST Endpoints
- `GET /api/v1/store/index` — lists all registered manifests in the store
- `POST /api/v1/store/register` — registers a new manifest
- `GET /api/v1/metrics` — returns Prometheus-format metrics from MetricCollector
- `GET /api/v1/traces` — returns current TraceContext as JSON
- `POST /api/v1/crash-report` — triggers a crash report (for debugging), returns JSON report

### BridgeContext Enriched
- Added `StoreRegistry`, `MetricCollector`, `FlightRecorder`, `TraceContext`, `CrashReporter`, `PanicHandler` to BridgeContext
- All new instances initialized with sensible defaults in `BridgeContext::new()`

## v1.0.0 — Phase 23: Multi-Mode AI Engine + Hybrid Intent Router (2026-07-29)

### aios-llm: Real GGUF Inference (Micro-Local & Full-Local)
- `LocalEngine` rewritten: real GGUF inference via `candle-core` 0.11 + `candle-transformers` 0.11
- Qwen2.5 GGUF support: `quantized_qwen2::ModelWeights::from_gguf()`, token-by-token generation via `LogitsProcessor`
- Micro-Local: Qwen2.5-0.5B-Instruct-GGUF (~300 MB RAM, INT4)
- Full-Local: Qwen2.5-7B-Instruct-GGUF (~4-8 GB RAM, INT4)
- `hf-hub` 1.0 integration: `HFClientSync` (blocking) for automatic model download from Hugging Face Hub
- `LocalModelKind::Micro` / `LocalModelKind::Full` backend enum variants
- `detect_local_models()` scans `AIOS_MODELS_DIR` or `models/` for `.gguf` files
- `download_default_model()` downloads Qwen2.5 GGUF + tokenizer.json via HF Hub
- `LlmEngine::from_config()` now dispatches to `MicroLocal`/`FullLocal` engines
- `factory.rs` updated: `BackendKind::MicroLocal` and `BackendKind::FullLocal`

### aios-bridge: LLM Fallback in Intent Router
- `IntentParser::parse_with_llm_fallback()` — when rule-based parser returns `UserIntent::Unknown`, calls LLM for classification
- LLM receives structured system prompt with available intent types (ProcessControl, BlockManagement, SystemQuery, MemoryCompaction)
- Response is parsed from JSON back into `UserIntent` via `parse_llm_response()`
- `intent_handler` and `workflow_handler` updated to use LLM fallback
- 8 unit tests: default config, serde round-trip, provider defaults, engine dispatch, 3x from_config, detect_local_models

### aios-builder: New Crate — EasyLang Engine & Auto-Manifest Generator
- New crate `aios-builder` — EasyLang compiler, workflow engine, and auto-manifest generator
- `Workflow` type — JSON-serializable workflow with named steps, validation, and serde round-trip
- `AutoManifestGenerator` — WASM binary analysis via `wasmparser`: detects capability requirements from export/import names; keyword-based workflow intent analysis for capability inference; generates sidecar `BlockManifestJson` (`name`, `version`, `capabilities`)
- `WorkflowCompiler` — Workflow-to-WASM compilation pipeline: WAT text generation, WAT→WASM compilation via `wat` crate
- 8 unit tests: WASM export/import capability detection, JSON manifest generation, workflow intent analysis, compile/empty/with-steps WAT output

#### EasyLang Parser — Text DSL → Workflow
- `EasyLangParser` — line-oriented declarative DSL: `spawn "browser"`, `timer 5000`, `load "network"`, `query "memory"`, `compact`, `status`
- Automatic label generation from command text; optional `label:` prefix for custom names
- Comments: `//` and `#` lines; blank lines ignored
- 10 unit tests: empty/comment parsing, single/multi commands, custom labels, label-with-spaces error, unicode labels, JSON round-trip

### aios-llm: New Crate — Multi-Mode AI Engine
- New crate `aios-llm` — unified LLM interface with Cloud, Micro-Local, and Full-Local backends
- `LlmConfig` — serializable configuration: backend kind, model name, API key/URL, max tokens, temperature
- `CloudEngine` — HTTP/JSON backend for Groq, OpenRouter, Google AI Studio (OpenAI-compatible API)
- `LocalEngine` — stub for future GGUF/ONNX local inference (Micro-Local / Full-Local)
- `LlmEngine` enum with `from_config()` factory and `async query()` dispatch
- 7 unit tests: default config, serde round-trip, provider defaults, engine dispatch, local unavailable

### aios-bridge: Workflow Execution Endpoint
- `POST /api/v1/workflow` — new endpoint accepting `{prompts: [string, ...]}` for batch intent execution
- Parses and executes each prompt sequentially, returns per-step success/failure with results
- Capability checking for each step individually
- Builder `runWorkflow()` updated to use single batch request instead of N individual requests

### aios-studio: Easy Builder Tab
- New "Builder" tab in sidebar with visual workflow step editor
- Block palette (Triggers: Timer, Event; Actions: Spawn, Kill, Load, Unload, Compact, Query)
- Add/remove/reorder steps; per-step custom prompt editing with inline text input
- Named workflow save/load via localStorage with dropdown picker and delete
- "Run Workflow" button sends each step via `POST /api/v1/intent` and displays per-step results
- Toast notifications for save/load/delete operations

### aios-studio: SPA Web Dashboard
- New directory `aios-studio/` — self-contained HTML/CSS/JS single-page application
- Real-time telemetry dashboard with Canvas RAM chart, process table, health cards
- Smart Command Palette (Ctrl+K) — send natural-language intents via `POST /api/v1/intent`
- Security Center tab — block list, capability tokens inventory, quick action buttons
- WebSocket auto-reconnect with exponential backoff and visual connection indicator
- Dark theme, minimal dependencies (zero npm packages), works in any modern browser

### aios-bridge: Static File Serving
- `tower-http` upgraded with `fs` feature for `ServeDir`
- Router fallback to `aios-studio/` directory — serves SPA at `/`, CSS at `/style.css`, JS at `/app.js`
- API routes (`/api/v1/*`, `/ws/*`) take priority; all other routes fall through to static files

## v1.0.0 — Bridge & Intent Engine (2026-07-28)

### aios-bridge: HTTP/WebSocket API Gateway
- New crate `aios-bridge` — external API bridge for GUI/Web clients
- `GET /api/v1/health` — health check with version and uptime
- `GET /api/v1/system/status` — full system snapshot (processes, blocks, watchdog, RAM)
- `POST /api/v1/intent` — natural language intent processing with capability enforcement
- `GET /ws/telemetry` — WebSocket endpoint streaming real-time metrics at 100ms intervals
- CorsLayer permissive for cross-origin web clients
- Axum 0.7 async server with tokio runtime

### aios-bridge/intent_engine: Rule-Based Intent Parser
- `IntentParser` with bilingual (RU/EN) rule matching
- `UserIntent` enum: ProcessControl, BlockManagement, SystemQuery, MemoryCompaction, WorkflowExecution
- Process actions: List, Kill, Spawn, AdjustPriority
- Block actions: List, Load, Unload, HotSwap
- `ExecutionPlan` DAG with `CapabilityToken` requirement mapping
- 25 unit tests covering all intent types in both languages
- Graceful `Unknown` fallback with hints

### aios-bridge/security: Capability Enforcement
- Every intent execution checks `AccessControlLayer` before system calls
- Missing capability returns HTTP 403 Forbidden with description
- Bridge operates with its own `bridge_block_id` for ACL identity

## v1.0.0 — Boot Discovery, Manifest Parsing, Capability Enforcement (2026-07-28)

### Block Registry: Boot Discovery
- `BlockRegistry::boot_discover(root)` — recursive directory walk that discovers all `.wasm` and `.bin` files in nested subdirectories and registers them
- Creates the root blocks directory if it does not exist
- Fixed bug where `walk_recursive` created an internal registry instead of registering into `self`
- 3 new tests: directory creation, subdirectory walk, non-block file skipping

### Block Loader: Sidecar JSON Manifest Parsing
- `BlockLoader::load_from_directory()` now looks for sidecar `.json` files alongside `.wasm`/`.bin` files (e.g., `mynet_1.0.0.json` for `mynet_1.0.0.wasm`)
- `BlockManifestJson` struct: parses `name`, `version`, `capabilities`, `ttl_ms` from JSON
- Capabilities parsed from string names (`CAP_NET_BIND`, `CAP_NET_CONNECT`, etc.) into `CapabilityToken`
- When a manifest file exists, its values override filename-derived defaults and auto-assign a `CapabilityToken` to the block entry
- Falls back to filename-based parsing when no `.json` sidecar exists (backward compatible)
- `BlockLoader::load_from_binary_with_capabilities()` — new method for loading with optional capability assignment
- 5 new tests: manifest parse capabilities, empty caps, from_file, with sidecar, without sidecar fallback

### RealTcpBlock: Capability Token Enforcement
- `RealTcpBlock` now holds an optional `CapabilityToken` via `set_capability()`
- `start_listening()` checks `CAP_NET_BIND` before binding
- `connect()` checks `CAP_NET_CONNECT` before outbound connection
- No token = allow all (backward compatible with existing code)
- Expired tokens are rejected
- `Capability::All` grants every capability
- Added `aios-security` dependency to `aios-net` crate
- 7 new tests: no token allows all, grant/deny bind, grant/deny connect, expired denial, All grants everything
- 605 unit tests pass, zero clippy warnings, fmt clean

## v1.0.0 — Release: Full Integration & Production Quality (2026-07-27)

### Interface Documentation
- Added `docs/INTERFACE.md` + `docs/INTERFACE.ru.md` — comprehensive GUI/TUI usage guide
- Covers: layout diagrams, keyboard shortcuts, mouse actions, all 6 GUI tabs, theming
- TUI section: 5 tabs, 11 keyboard shortcuts, terminal compatibility notes
- GUI section: 6 tabs (Overview, Processes, Blocks, Marketplace, Metrics, Deps), F1-F6 navigation, mouse support, dark theme color reference
- Updated `AGENTS.md`: new rule #5 — INTERFACE.md must be updated on any user-facing change

### Runtime Transition: Mock → Real (65% → 75%)
- **BlockRegistry disk loading**: `load_from_path(dir)` scans directory for `.wasm` and `.bin` files, parses version from filename, registers + activates all discovered blocks
- **BlockExecutor load-and-execute**: `load_from_path_and_execute()` performs one-shot load + WASM compile + instantiate + `init`/`start` from a directory
- **BlockLoader .wasm support**: `load_from_directory()` now handles `.wasm` files alongside `.bin`
- **Watchdog active recovery**: graduated escalation — `KillProcess(pid)`, `DumpState(path)`, `SafeModeShell` actions with severity ordering
- **WatchdogRunner escalation**: `escalate()` triggers context-appropriate recovery actions based on current state
- **RealUdpBlock**: real `std::net::UdpSocket` with `bind()`, `send_to()`, non-blocking `receive_from()`, broadcast, and per-socket metrics
- **TODO Runtime Transition Checklist**: full 6-section checklist with per-milestone readiness targets documented
- 766 total tests, zero clippy warnings

### Integration Tests: Real I/O (75% → 85%)
- Added 6 new integration test files with **real** hardware I/O, replacing mock data:
  - `tests/real_file_io.rs` — 10 tests: SnapshotManager, CopyOnWriteStorage, RecoveryLog, BlockRegistry disk loading, large payloads
  - `tests/real_network.rs` — 11 tests: RealTcpBlock loopback (accept/send/receive, bidirectional, multi-client, close+reopen), RealUdpBlock loopback (send/receive, multiple datagrams, broadcast, metrics)
  - `tests/real_wasm.rs` — 9 tests: end-to-end WASM compile→instantiate→call, multi-block isolation, disk load+execute, batch execute, invalid binary, metadata
  - `tests/real_threads.rs` — 10 tests: real OS thread execution, terminate signal, suspend/resume, parallel (8 threads), finished detection, RAM enforcement, priority scheduling, race-free atomic counter, mixed real+logical
  - `tests/real_hot_swap.rs` — 8 tests: WASM deploy+call, hot-swap version change (v1→v2 with different logic), rollback, health check pass/fail, swap history, multi-block independent swap
  - `tests/full_lifecycle.rs` — 7 tests: full system (HAL+IPC+scheduler+telemetry+ACL), WASM lifecycle (deploy→swap→rollback), watchdog+scheduler+IPC combined, crypto+bus, scheduler aging+real threads, disk→WASM fibonacci, stability+ACL
- **RealUdpBlock**: added `port()` method to expose actual bound port for integration tests
- **WatchdogRunner**: fixed flaky `test_runner_pop_actions` timing issue
- 821 total tests, zero clippy warnings

### Scheduler CPU Affinity (85% → 90%)
- **`aios-process-mgr/src/cpu_affinity.rs`**: platform-specific CPU affinity via raw FFI
  - Windows: `SetThreadAffinityMask` / `GetCurrentThread`
  - Linux: `sched_setaffinity` with `cpu_set_t`
  - Fallback: no-op on unsupported platforms
- **`Scheduler::set_cpu_affinity(pid, cores)`**: pins a real OS thread to specified CPU cores
- **`Scheduler::get_cpu_affinity(pid)`**: queries current affinity pinning for a thread
- **`Scheduler::available_cpu_cores()`**: returns number of available CPU cores
- 4 unit tests in `cpu_affinity` module, 3 scheduler-level tests
- 828 total tests, zero clippy warnings

### WASM Live Update Engine — Real Module Replacement & State Migration
- `WasmLiveUpdateEngine` in `aios-live-update/src/wasm_engine.rs` — bridges `LiveUpdateEngine` with `WasmSandbox` for real WASM module replacement during hot-swap
- `deploy_block()` — compiles, instantiates, and auto-calls `init`/`start` functions on WASM blocks
- `swap_block()` — performs atomic swap via `LiveUpdateEngine.perform_swap()` then compiles + instantiates new WASM module, migrates linear memory state from old instance, immediately usable
- `rollback_block()` — removes active WASM instance and restores previous version via `LiveUpdateEngine.rollback()`
- `call_block_func()` — invokes exported WASM functions on active (deployed/swapped) blocks
- **Linear memory migration**: `extract_linear_memory()` reads WASM linear memory before swap, `restore_linear_memory()` writes it into new instance after swap — state survives hot-swap
- `SwapParams` struct — encapsulates swap configuration (new_binary, new_version, health_check, isolation)
- `SwapResult` includes `memory_migrated: bool` to indicate if linear memory was transferred
- 7 WASM memory tests (4 sandbox-level, 2 live-update, 1 integration), 834 total tests

### IPC Channel Atomic Reroute (90% → 100%)
- **`IpcBus::reroute(old_target, new_target)`** — atomically rewrites `target_block` in all pending packets matching `old_target` to `new_target`
- **`StateTransferManager::reroute_snapshot()`** — reroutes packets inside a frozen snapshot before bus restore
- **`WasmLiveUpdateEngine::reroute_pending()`** — freeze→reroute→unfreeze in one atomic operation
- 4 new tests (2 bus, 1 state_transfer, 1 wasm_engine), **838 total tests**, zero clippy warnings

### TCP Socket Options — Real OS-Level Socket Configuration (100%)
- **`set_keepalive()`** — platform-specific raw FFI for `SO_KEEPALIVE` on TCP sockets
  - Windows: Winsock `setsockopt` with `SOL_SOCKET`/`SO_KEEPALIVE` constants
  - Unix: `libc::setsockopt` with `libc::SO_KEEPALIVE`
- **`SO_REUSEADDR`** — raw FFI in `RealTcpBlock::start_listening()` allows quick port rebinding after stop
  - `TcpConfig.reuse_addr: bool` (default: `true`)
- **`SO_KEEPALIVE`** — applied on accepted and connected TCP sockets
  - `TcpConfig.keepalive: bool` (default: `true`)
- **`TCP_NODELAY`** — already set via `stream.set_nodelay()` on all new connections
  - `TcpConfig.nodelay: bool` (default: `true`)
- **`get_keepalive()`** — test-only helper via raw `getsockopt` FFI to verify keepalive state
- 4 new tests: `SO_REUSEADDR` quick rebind, keepalive verification, nodelay verification, disabled reuse_addr
- **842 total tests**, zero clippy warnings

### Watchdog Graduated Escalation (100%)
- **`WatchdogState::Warned`** — new intermediate state between Monitoring and Suspended for graduated response
- **`WatchdogConfig.warn_threshold: u32`** — configurable threshold for warning state (default: 2)
- **Graduated `check_timeout()` flow**: Monitoring → Warned → Suspended → Recovering → SafeMode
  - Miss `warn_threshold` heartbeats → `WarnOrchestrator` action, state → `Warned`
  - Miss `max_missed_heartbeats` → `SuspendOrchestrator` action, state → `Suspended`
  - Next check after Suspended → `KillProcess(0)` action, state → `Recovering`
  - Recovery timeout expires → `SafeModeShell` action, state → `SafeMode`
- **`WatchdogAction::WarnOrchestrator`** — new action with severity 1 (pre-suspend warning)
- **`escalate_actions()`** — now includes `Warned` state with `DumpState` action
- **`receive_heartbeat()`** — recovers from `Warned` state back to `Monitoring` (resets missed_count)
- **Severity reordering**: None(0) < WarnOrchestrator(1) < WaitForRecovery(2) < SuspendOrchestrator(3) < AttemptRecovery(4) < KillProcess(5) < DumpState(6) < EnterSafeMode(7) < SafeModeShell(8) < InSafeMode(9)
- **TUI integration** — `WatchdogState::Warned` displayed as "WARNING" in yellow
- 5 new tests (graduated escalation, recovery from warned, escalate in warned, full warn→safe, severity), **845 total tests**

### ProcessId → JoinHandle Persistent Registry
- **`RealThreadState`** — queryable state struct for real OS threads (pid, finished, suspended, terminated)
- **`Scheduler::get_real_thread_state(pid)`** — queries current state of a real thread by ProcessId
- **`Scheduler::list_real_threads()`** — returns all currently tracked ProcessIds with real threads
- `real_threads` HashMap serves as persistent ProcessId → JoinHandle registry with public accessors
- 2 new tests: `list_real_threads`, `get_real_thread_state`

### Thread-Local Per-Process Metrics Store
- **`process_metrics` module** — atomic per-process metrics with thread-local binding for zero-contention recording
- **`ProcessMetricsInner`** — atomic counters: `messages_sent`, `messages_received`, `bytes_sent`, `bytes_received`, `errors`, `syscall_count`, `wakeups`
- **`ProcessMetricsStore`** — global `HashMap<ProcessId, Arc<ProcessMetricsInner>>` with `OnceLock`-based singleton
- **Thread-local binding**: `bind_current_thread(pid)` / `current_pid()` — associates current thread with a PID
- **Convenience functions**: `record_sent(bytes)`, `record_received(bytes)`, `record_error()`, `record_syscall()`, `record_wakeup()` — auto-detect PID from thread-local
- **`snapshot(pid)`** / **`snapshot_all()`** — atomically read all counters without locking
- `register(pid)`, `unregister(pid)`, `clear()`, `count()` — lifecycle management
- 7 unit tests: register/snapshot, unregister, snapshot_all, thread-local bind+record, clear, atomic independence, no-binding noop
- **854 total tests**, zero clippy warnings
- `DeployResult`, `SwapResult`, `RollbackResult` — typed return structs for all operations
- 6 unit tests: deploy, call function, real WASM swap (add→multiply), rollback, failing health check, history tracking
- **Note**: This closes the gap identified in TODO readiness assessment — Live Update is now **REAL** (not mock)

### Scheduler — Real OS Thread Management
- `TerminateFlag(Arc<AtomicBool>)` and `SuspendFlag(Arc<AtomicBool>)` for cooperative thread control
- `RealThread` struct — wraps OS thread with `Thread` + `JoinHandle` + terminate/suspend flags
- `spawn_real_process<F>()` — spawns real OS threads with cooperative termination support
- `kill_process()` — sets terminate flag, unparks thread, joins handle
- `suspend_process()` / `resume_process()` — park/unpark real threads via `AtomicBool` + `thread::park()`
- `check_real_threads()` — detects finished threads via `is_finished()`, joins them, updates state
- `is_real_process()`, `real_thread_count()` — query helpers
- 6 new unit tests for real thread lifecycle

### BlockExecutor — WASM Block Execution Bridge
- `BlockExecutor` in `aios-wasm/src/executor.rs` — bridges `BlockRegistry` with `WasmSandbox`
- `execute_block()` — compiles binary from registry as WASM, instantiates, auto-calls `init`/`start`
- `call_block_func()` — calls exported functions on already-executed blocks
- `execute_all()` — batch-executes all blocks from registry
- 6 unit tests covering init+start, function calls, execute all, nonexistent blocks

### WatchdogRunner — Real Background Thread
- `WatchdogRunner` in `aios-watchdog/src/runner.rs` — real background `std::thread::spawn` with `AtomicBool` stop flag
- Automatic timeout checking at configurable intervals, action collection via `Arc<Mutex<Vec<WatchdogAction>>>`
- `start()`, `stop()`, `receive_heartbeat()`, `pop_actions()`, `force_safe_mode()`, `reset()`
- `Drop` impl ensures thread cleanup on all code paths
- 8 unit tests: start/stop, heartbeat, missed detection, force safe mode, reset, pop actions, drop, recovery

### RealTcpBlock — Real OS Sockets
- `RealTcpBlock` in `aios-net/src/real_tcp.rs` — real `std::net::TcpListener`/`TcpStream` with non-blocking accept
- `start_listening()`, `accept_pending()`, `connect()`, `send()`, `receive()`, `close_connection()`, `stop()`
- `max_connections` enforced in `accept_pending()`
- 6 unit tests: listen/stop, connect+send, bidirectional, close, max connections, no pending data

### New Crate: `aios-optim` — Runtime Optimization Engine
- **12th workspace crate** — performance profiling, hot-path detection, memory layout optimization, and auto-tuning
- **Profiler** (`profiler.rs`): wall-clock timing with rolling averages, histograms, percentiles (p50/p95/p99), throughput tracking
- **Hot-Path Detector** (`hotpath.rs`): call-site tracking with hit counts, duration accumulation, flamegraph output, dynamic thresholding
- **Memory Layout Optimizer** (`layout.rs`): struct field reordering for cache-line alignment, size analysis, alignment recommendations
- **Auto-Tuner** (`tuning.rs`): parameter search with grid/random/binary strategies, best-result tracking, metric collection, convergence detection
- 29 unit tests covering all optimization modules

### Ring Buffer ↔ IPC Bus Integration
- `RingTransport` in `aios-ipc/src/ring_transport.rs` bridges ring buffers with IPC bus
- Auto-routes heavy payloads (>4KB) through shared-memory ring buffers for zero-copy performance
- Falls back to standard VecDeque bus for small messages
- `RingMetrics`: tracks ring sends, receives, ring_bytes_sent, ring_bytes_received
- 10 unit tests covering creation, send/receive, fallback, metrics, multi-path routing

### Compression ↔ Context Store Integration
- `CompressedTelemetryStore` in `aios-context/src/compressed_telemetry.rs`
- Auto-compresses cold telemetry entries (>1 hour old) using ZSTD via `aios-compress`
- Transparent decompression on read — callers see normal `TelemetryEntry` data
- Configurable compression threshold and cold-entry age
- 6 unit tests covering write, compress, read, thresholds

### CoW Persistence ↔ Live-Update Integration
- `CowLiveUpdateEngine` in `aios-live-update/src/cow_live_update.rs`
- Persists rollback entries (binary, state, version) to CoW storage for crash recovery
- On startup, recovers any pending rollback from disk
- 4 unit tests covering persist, recover, crash recovery

### Hardware Security Bridge
- `HardwareSecurityBridge` in `aios-security/src/hardware_bridge.rs`
- Unified interface across MPK, TEE, and IOMMU protection layers
- `protect_block()`, `unprotect_block()`, `check_access()` — single API for all hardware security
- `ProtectionReport` with per-layer status (mpk_enabled, tee_status, iommu_status)
- Graceful fallback when hardware layers unavailable
- 10 unit tests covering block protection, access checking, reports, no-std panic safety

### Bug Fixes
- **BUG-012**: Fixed `get_pending_entries()` in recovery log — completed entries were not filtered because the function only skipped `COMPLETED:` marker lines without using them to exclude matching entry IDs
- Fixed `atomic_write` Windows error — `sync_all()` fails with "Access Denied" when using `File::open()`; changed to `OpenOptions::new().write(true)`

### Benchmark Suite
- **5 benchmark files** using `criterion` 0.5: IPC, ring buffer, bus, compression, persistence
- `aios-core/benches/ipc_bench.rs`: IPC serialize/deserialize/checksum at 1KB/64KB
- `aios-ringbuf/benches/ring_bench.rs`: ring write/read/zero-copy throughput
- `aios-ipc/benches/bus_bench.rs`: bus send/receive/priority/throughput
- `aios-compress/benches/compress_bench.rs`: compress/decompress/ratio
- `aios-persistence/benches/persist_bench.rs`: atomic write/read/roundtrip

### Property-Based Testing (proptest)
- `aios-core/tests/proptest_ipc.rs`: 8 tests — serialize roundtrip, checksum validity, unique IDs, response roundtrip, payload preservation, non-empty serialization
- `aios-ringbuf/tests/proptest_ring.rs`: 5 tests — write/read roundtrip, capacity bounds, available_read, sequential writes, zero-copy

### Chaos Testing
- `tests/chaos_test.rs`: 13 tests — IPC corruption, bus overflow, scheduler memory exhaustion, scheduler crash-loop resilience, watchdog timeout→safe mode, safe mode rapid commands, ACL missing token/wrong cap, heartbeat HMAC forgery, context store exhaustion→compact, block loader duplicates, concurrent bus drain+send, kill-after-schedule consistency

### Context Store Auto-Compact
- `EmbeddedContextStore::with_compact_threshold(max_entries, threshold_ratio)` — configurable auto-compact
- `should_compact()` — check if store exceeds threshold
- `compact()` — prune old telemetry, deduplicate workflows, returns `CompactReport`
- 4 unit tests for threshold check, pruning, and minimum data preservation

### BlockManager Security Integration
- `BlockEntry` now stores optional `CapabilityToken` per block
- `assign_capabilities(id, token)` — bind a token to a loaded block
- `check_capability(id, cap)` — verify block has required capability (checks expiry + capability set)
- Blocks without assigned tokens are denied all capability checks
- 2 new tests: assign/check, none-by-default

### Additional Benchmarks
- Compression ratio benchmarks by data pattern (repetitive, random, telemetry) in `aios-compress`
- CoW snapshot creation, rollback latency, and disk overhead benchmarks in `aios-persistence`

### VM Deployment
- **Dockerfile updated** — all 20 workspace crates, `debian:bookworm-slim` runtime
- **main.rs full rewrite** — auto-compact, block loading from disk, persistence recovery, graceful shutdown
- **`BlockLoader::load_from_directory(dir)`** — loads `.bin` blocks from `AIOS_BLOCKS_DIR` at startup (name_version.bin format)
- **`PersistentStore` integration** — saves telemetry on shutdown, recovers on startup from `AIOS_DATA_DIR`
- **Environment variables**: `AIOS_DATA_DIR`, `AIOS_BLOCKS_DIR`, `AIOS_MOCK_PROFILE` (modern/legacy/none)
- **Linux HAL** — reads `/proc/cpuinfo` and `/proc/meminfo` for hardware detection

### TUI Interactive Block Management
- **Block tab keybindings**: `U` = unload block, `L` = load from disk (interactive name+version input), `H` = hot-swap binary (unload + reload)
- **Two-step input dialog** for block loading: enter name → enter version → confirm
- **Block detail pane** shows selected block info, operation results, and available actions
- **Footer keybindings** updated with U/L/H shortcuts
- `DashboardState` extended with `BlockInputMode`, `block_input_buffer`, `block_operation_result`
- `selected_block_name_version()` — returns name+version of currently selected block

### TUI Dependency Graph Visualization
- **5th tab: Deps** — interactive block dependency graph table
- Shows each block's dependencies and dependents in tabular format
- **Load order panel** — displays topological sort of block load sequence
- `BlockRegistry::dependency_graph()` — builds `DependencyGraph` from registered blocks
- `BlockRegistry::set_block_dependencies(name, deps)` — declares inter-block dependencies
- `DependencySnapshot` struct for TUI rendering: blocks, load_order, edges
- 1 new test for `dependency_graph()` method

### CI/CD
- **GitHub Actions CI** (`.github/workflows/ci.yml`): check, fmt, clippy, test, coverage
- **Coverage reporting** with `cargo-tarpaulin` (LLVM engine, XML output)
- **Codecov integration** for coverage uploads
- **Release automation** (`.github/workflows/release.yml`): multi-platform builds on tag push
- Targets: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`
- Auto-generates GitHub releases with changelogs and binary archives

### Latency Measurement Infrastructure
- `LatencyTracker` in `aios-optim/src/latency.rs` — per-operation latency tracking with threshold alerting
- `LatencyGuard` — RAII guard for automatic timing (`tracker.guard("op")` → `guard.stop()`)
- `LatencyStats` — aggregated stats: min, max, avg, p50, p95, p99, violation count
- `LatencyThreshold` — configurable warn/critical thresholds per operation
- `LatencyLevel` — Normal/Warning/Critical classification
- FIFO eviction per operation bucket
- 11 unit tests covering record, classify, guard, stats, violations, clear, eviction

### Priority Inheritance Protocol
- `PriorityInheritance` in `aios-process-mgr/src/priority_inheritance.rs`
- `acquire_lock()` — lock acquisition with priority inheritance for high-priority waiters
- `release_lock()` — release with priority restoration and waiter wakeup chain
- `request_resource()` — resource request with priority inheritance
- `apply_pending_boosts()` — drain accumulated priority boost recommendations
- `release_all()` — release all locks held by a process (for crash cleanup)
- `LockResult` — Acquired/Blocked/AlreadyHeld
- `ResourceResult` — Granted/Blocked/AlreadyHeld
- 12 unit tests covering acquire, contention, release chains, wrong owner, already held, release_all, state, waiters, no-boost-for-lower-priority

### Hardware Detection Expansion
- **Intel Meteor Lake NPU detection** via PCI vendor/device ID (8086:7D0B) on Linux and Windows PnP
- **Qualcomm X Elite NPU detection** via PCI vendor/device ID (17CB:1100) and CPU model matching
- `mock_intel_meteor_lake()` profile with Intel AI Boost NPU (11 TOPS), Arc Graphics, Thunderbolt 4
- `mock_qualcomm_x_elite()` profile with Qualcomm Hexagon NPU (45 TOPS), Adreno GPU, NEON support
- **USB device enumeration** via `lsusb` (Linux) and WMI (Windows) with speed classification (USB 1.1–4.0)
- **Thunderbolt device enumeration** via `/sys/bus/thunderbolt/devices` (Linux) and WMI (Windows) with Tb1–Tb5 speed
- `UsbDevice` struct: name, vendor_id, product_id, speed, is_hub, port
- `ThunderboltDevice` struct: name, vendor_id, device_id, speed, max_power_watts, port
- `UsbSpeed` enum: Usb11, Usb20, Usb30, Usb31, Usb32, Usb40, Unknown
- `ThunderboltSpeed` enum: Tb1, Tb2, Tb3, Tb4, Tb5, Unknown
- `HardwareProfile` extended with `usb_devices` and `thunderbolt_devices` fields
- 11 new tests for NPU mocks, USB/TB types, serialization roundtrips

### WebAssembly Runtime Integration (aios-wasm)
- New crate `aios-wasm` v1.0.0 — Wasmtime v47 embedding for block sandboxing
- `WasmSandbox` — engine creation with fuel consumption and epoch interruption
- `WasmBlock` — WASM block lifecycle: compile (from bytes or WAT), instantiate, call exported functions
- `SandboxConfig` — memory page limits, fuel limits, max instances, timeout
- `MemoryStats` — memory/fuel limits and instantiation status
- `WasiFilter` — WASI syscall filtering with Allow/Deny/Log policies per syscall
- `WasiFilter::permissive()`, `restrictive()`, `no_network()` — preset filter configurations
- `IsolationConfig` — shared-nothing isolation: None/Process/Memory/Network/Full levels
- `ResourceLimits` — max memory, CPU time, storage, network, open files per block
- `IsolationBoundary` — per-block isolation registry with cross-block communication control
- 39 tests covering sandbox lifecycle, WASM compilation, function calls, WASI filtering, isolation boundaries

### Block Marketplace
- `BlockMarketplace` in `aios-block-mgr/src/marketplace.rs` — block registry with repository management
- `BlockMetadata` — name, version, description, author, sha256, tags, download count
- `RepositoryEntry` — metadata, status (Available/Installed/UpdateAvailable/Deprecated), local path
- `BlockStatus` enum: Available, Installed, UpdateAvailable, Deprecated
- `RepositoryType` enum: Local, Remote
- Publish, search (by name/description/tag), install, uninstall, check updates
- Multiple repository support with cross-repo search
- 18 unit tests covering marketplace lifecycle

### Network Stack (aios-net)
- New crate `aios-net` v1.0.0 — TCP/UDP blocks for network communication
- `TcpBlock` — TCP client/server with connection management, send/receive, connection pooling
- `TcpConfig` — bind addr, port, max connections, buffer size, timeout, nodelay
- `TcpConnection` — per-connection state, byte counters
- `TcpMessage` — from/to addresses, data, timestamp
- `UdpBlock` — UDP socket with bind, send, broadcast, receive
- `UdpConfig` — bind addr, port, buffer size, broadcast, multicast TTL
- `UdpPacket` — from/to addresses, data, timestamp
- 27 tests covering TCP lifecycle, connections, send/receive, UDP bind, broadcast, packets

### Filesystem Abstraction
- `FileSystem` in `aios-core/src/filesystem.rs` — unified file access layer
- `FileSystemType` enum: Local, Virtual, Overlay
- Virtual filesystem: in-memory file storage with permission checks
- Local filesystem: read/write through root path with permission enforcement
- Overlay filesystem: virtual layer over local filesystem
- `FilePermissions` — read/write/executable per file
- `FileEntry` — path, size, is_dir, permissions
- Read-only mode enforcement across all operations
- 20 unit tests covering virtual read/write/delete, permissions, read-only, serialization

### GUI Dashboard (aios-gui)
- New crate `aios-gui` v1.0.0 — native graphical dashboard using egui/eframe
- **6 tabs**: Overview, Processes, Blocks, Marketplace, Metrics, Dependencies
- **Overview**: stat cards (RAM, Blocks, Processes, Watchdog), system info (CPU, GPU, storage), RAM sparkline, activity log
- **Processes**: sortable table (PID, Name, Priority, State, RAM, CPU, Crashes), Kill/Suspend/Resume actions, selection navigation (j/k keys)
- **Blocks**: block table with ID, Name, Version, State badge, size, dependencies; Load dialog (2-step wizard), Unload, Hot-Swap
- **Marketplace**: search/filter, install/update/uninstall actions, status badges (Available/Installed/UpdateAvailable/Deprecated)
- **Metrics**: RAM progress bar + sparkline, priority distribution bars, block state statistics, system info
- **Dependencies**: dependency graph table, load order visualization, edge counts
- `AiosTheme` — dark theme with customizable accent/success/warning/danger colors, applied to all egui widgets
- Reusable widgets: `stat_card`, `status_badge`/`badge`, `sparkline`, `progress_bar`, `section`
- Keyboard shortcuts: F1-F6 tabs, j/k navigation
- Quick Actions sidebar: Refresh All, Suspend All
- Top bar: AI tier, watchdog status, RAM, blocks, processes, uptime
- 7 new tests covering app creation, logs, navigation, block ops, marketplace
- Zero egui panics — all rendering is immediate-mode, no retained state

### Code Quality
- Fixed all clippy warnings across workspace (zero warnings)
- Removed `#[cfg(unix)]` gates from persistence tests — all tests now run on Windows
- All 708 tests pass across 20 workspace crates

### Version Bump
- All crates bumped to 1.0.0

## v0.6.0 (Planning) — Advanced Optimization & Hardware Resilience (2026-07-27)

### Phase 20: TEE (Trusted Execution Environment) Integration ✅ IMPLEMENTED
- ✅ TEE platform detection (Intel SGX, ARM TrustZone, AMD SEV) with graceful fallback
- ✅ Data sealing and unsealing with platform binding (`SealingKey`, `SealedData`)
- ✅ Remote attestation framework with PCR (Platform Configuration Register) support
- ✅ Enclave lifecycle management (Created → Initialized → Running → Suspended → Exited/Failed)
- ✅ Enclave configuration with memory size and thread management
- ✅ State preservation and serialization for all TEE types
- ✅ Cross-platform support: Windows (x86-64), Linux, ARM64
- ✅ 28 comprehensive unit tests covering all TEE operations
- ✅ Integration with `aios-security` capability system for access control
- ✅ Full serialization support via `bincode` for IPC transport

### Phase 19: IOMMU Support for DMA Isolation ✅ IMPLEMENTED
- ✅ DMA buffer management with DMA addresses and permissions
- ✅ Page table implementation (entry creation, mapping, unmapping)
- ✅ IOMMU domain management (allocation, deallocation, status tracking)
- ✅ Cross-platform IOMMU detection (Intel VT-d, AMD IOMMU, ARM SMMU)
- ✅ Graceful fallback on unsupported hardware
- ✅ 25 comprehensive unit tests covering all IOMMU operations
- Integration with `aios-security` for DMA access control (ready)

### Phase 15: Zero-Copy IPC Ring Buffers ✅ IMPLEMENTED
- ✅ Lock-free, single-producer/single-consumer ring buffers (`aios-ringbuf` crate)
- ✅ O(1) data transmission efficiency (no kernel copies)
- ✅ Zero-copy read/write pointers for direct memory access
- ✅ Wraparound handling and atomicity guarantees
- ✅ 8 unit tests covering all operations
- Integration with existing `IpcBus` transport (planned for Phase 2 expansion)

### Phase 17: AI KV-Cache & State Compression ✅ IMPLEMENTED
- ✅ FP8 quantization (32 bits → 8 bits) for AI buffers
- ✅ INT4 quantization (32 bits → 4 bits) for memory-heavy state
- ✅ ZSTD compression for system state tables
- ✅ LRU decompression cache with configurable size
- ✅ 16 unit tests for quantization, compression, and caching
- Automatic compression thresholds based on memory pressure (planned)

### Phase 18: Atomic Copy-on-Write Persistence ✅ IMPLEMENTED
- ✅ CoW storage engine with atomic write protocol
- ✅ State snapshots with SHA-256 integrity verification
- ✅ Recovery log for crash resilience during state transfer
- ✅ Atomic rename for power-loss safety (write → fsync → rename)
- ✅ 6 unit tests (Unix) for storage operations
- Rollback support for failed live-updates (ready)

### Phase 16: Hardware-Enforced Memory Protection ✅ IMPLEMENTED
- ✅ Intel MPK (Memory Protection Keys) support via `x86` crate CPUID detection
- ✅ ARM Memory Domains (fallback) with DACR register management
- ✅ Per-block PKEY allocation (max 16 keys on Intel, 4 domains on ARM)
- ✅ MpkSecurityBridge for integration with `aios-security` capability system
- ✅ Hardware detection via `HwMemoryProtection::detect()`
- ✅ 27 comprehensive unit tests covering all operations
- Cross-architecture support with graceful degradation on unsupported hardware
- Integration with block policies and access control ready for Phase 2

## v0.5.0 — RT Scheduler, Stress Tests & Hardware Expansion (2026-07-26)

### TUI Dashboard Enhancement — Full Interactive 4-Tab Dashboard
- Complete rewrite of `dashboard.rs` from 3-zone static layout to 4-tab interactive dashboard
- **Tab 1 (Overview)**: Hardware info panel (CPU, GPU, Storage, System) + Activity Log
- **Tab 2 (Processes)**: Full process table (PID, Name, Priority, State, RAM, CPU, Crashes) with row selection and process detail panel
- **Tab 3 (Blocks)**: Block registry table (ID, Name, Version, State, Size) with summary stats
- **Tab 4 (Metrics)**: RAM usage gauge bar, process priority distribution histogram, RAM history time-series
- `DashboardState` expanded: `processes: Vec<ProcessSnapshot>`, `blocks: Vec<BlockSnapshot>`, `selected_row`, `ram_history`, `process_kill_result`
- Process/Block snapshots taken on every frame for consistent rendering
- `ProcessSnapshot`/`BlockSnapshot` structs for decoupled rendering from scheduler/registry locks
- Color-coded priority styles (Critical=Red, High=Yellow, Normal=Green, Low=Blue, Bg=DarkGray)
- Color-coded state styles (Running=Green, Crashed=Red, Terminated=DarkGray)
- RAM usage gauge with threshold coloring (>85% Red, >60% Yellow, else Green)
- `BlockState` display with Active=Green, Error=Red styling
- 6 new unit tests (add_log_limit, move_selection, selected_process_pid, priority_styles, state_styles, block_state_styles)

### TUI Interactive Keybindings
- `j`/`Down` — move selection down in process/block tables
- `k`/`Up` — move selection up
- `K` — kill selected process (with confirmation display)
- `1`/`2`/`3`/`4` — switch tabs (Overview/Processes/Blocks/Metrics)
- Selection resets on tab switch
- Process detail panel shows selected process info or kill result
- `r` — refresh, `s` — telemetry record, `x` — system status

### TUI Architecture Changes
- `update_from_scheduler()` now takes `&Scheduler` and `&BlockRegistry` (was only `&Scheduler`)
- Tab selection tracked by `selected_tab` (0-3), row selection by `selected_row`
- RAM history ring buffer (60 entries, one per frame)
- Process kill result display (`Option<String>`)

### Process Manager: Real-Time Scheduler
- `SchedulingMode` enum: `Normal` (default weighted round-robin) and `RealTime` (deadline-based)
- `JitterEntry` struct: `pid`, `expected_ms`, `actual_ms`, `timestamp` — tracks scheduling jitter
- `set_scheduling_mode()`, `scheduling_mode()` — switch between Normal and RT mode
- `set_rt_deadline(pid, deadline_ms)` — assign absolute deadline to a process
- `clear_rt_deadline(pid)` — remove deadline from a process
- RT scheduling: picks process with earliest deadline (smallest remaining time)
- Jitter tracking: records entries when scheduling exceeds expected time slice or misses deadline
- `jitter_log()` and `clear_jitter_log()` for jitter audit
- 9 new unit tests: mode default, set mode, deadline management, earliest deadline, skip non-ready, jitter recording, jitter clear, no candidates, mode switch

### Stress Tests & Benchmarks
- 11 stress tests in `tests/stress_test.rs` covering mass operations:
  - `test_stress_mass_spawn_1000` — spawn 1000 processes + schedule loop
  - `test_stress_ipc_bus_throughput` — 10k IPC packet send/receive
  - `test_stress_rt_scheduler_500` — 500 RT deadline tasks scheduling
  - `test_stress_block_registry_500` — register/query 500 blocks
  - `test_stress_context_store_1000` — 1000 telemetry entries
  - `test_stress_hardware_mock_serialize` — 10k HW profile serialize/deserialize
  - `test_stress_heartbeat_1000` — 1000 HMAC heartbeat cycles
  - `test_stress_storage_profiles` — NVMe/SATA mock profile verification
  - `test_stress_message_router_500` — 500 router dispatches
  - `test_stress_live_update_20` — 20 concurrent hot-swap operations
  - `test_stress_persistent_store_batch` — 500 redb telemetry writes

### HAL: Storage Device Detection
- `StorageDevice` struct: `name`, `interface`, `capacity_gb`, `model`
- `StorageInterface` enum: `NVMe`, `SATA`, `USB`, `Unknown`
- `detect_storage()` — Windows `wmic diskdrive` + Linux `/sys/block` detection
- `HardwareProfile::storage_devices: Vec<StorageDevice>` field on all 4 mock profiles
- Mock profiles: modern=2 NVMe, legacy=1 SATA, legacy_2012=1 SATA, nvidia=1 NVMe

### HAL: AMD GPU Detection
- `detect_gpu_amd()` — parses `rocm-smi --showproductname --showmeminfo vram` output
- Added to hardware detection pipeline alongside NVIDIA

### HAL: Storage Unit Tests
- 7 new tests: NVMe/SATA profile verification, StorageDevice serialization roundtrip, full profile serialization with storage

### New Crate: `aios-exec-compat` — Multi-Binary Compatibility Subsystem
- **11th workspace crate** — execution interceptor and syscall translator for foreign binaries
- Non-invasive architecture: connects to AIOS IPC Message Bus like any system module
- Zero-trust sandboxing: foreign executables run with restricted `CapabilityTokens`

#### Binary Header Parser (`format.rs`)
- `ExecutableType` enum: `AiosNative`, `LinuxElf`, `WindowsPe`, `Unknown`
- `HeaderFormat::from_bytes(data: &[u8])` — magic bytes inspection (`MZ` for PE, `\x7fELF` for Linux, `AIOS` for native)
- `ExecutableType::from_extension(path)` — filename-based type detection (.exe/.dll → PE, .so/.elf → ELF, .aib → AIOS)
- `BinaryHeader::parse(data: &[u8])` — full header parsing: entry_point_offset, is_64bit, machine_arch, subsystem
- ELF64/ELF32: e_entry from offset 24, class byte detection
- PE32/PE32+: MZ→PE offset, machine arch (0x8664/0x014C), optional header magic
- `CompatCapability` enum (9 variants): FilesystemRead/Write, ProcessCreate, NetworkAccess, RegistryAccess, WinApiCompat, PosixCompat, MemoryMap, ThreadCreate
- Speed test: <5us overhead for header identification

#### POSIX Syscall Translator (`posix.rs`)
- `PosixSyscall` enum (18 variants): SysOpen, SysRead, SysWrite, SysClose, SysLseek, SysFork, SysExec, SysExit, SysMmap, SysMunmap, SysSocket, SysConnect, SysSend, SysRecv, SysGetpid, SysGetuid, SysStat, SysFstat
- `PosixTranslator` trait: `translate(request)`, `translate_to_ipc(request)`
- `DefaultPosixTranslator` — translates Linux syscall IDs to AIOS IPC packets
- Speed test: <5us per translation

#### Win32 API Translator (`win32.rs`)
- `Win32Api` enum (16 variants): CreateFileW, ReadFile, WriteFile, CloseHandle, GetProcAddress, LoadLibraryW, VirtualAlloc, VirtualFree, CreateThread, ExitProcess, GetLastError, etc.
- Win32 ordinal-based dispatch (standard Windows SSN values)
- `Win32Translator` trait with DLL registration
- Speed test: <5us per translation

#### Dependency Healer (`dependency_healer.rs`)
- Automated detection of missing .dll/.so libraries
- `scan_dependencies()` → `resolve_missing()` → `heal_dependencies()` pipeline
- Configurable search paths per ExecutableType, resolution cache, auto-load into sandbox

#### Sandbox Compatibility (`sandbox_compat.rs`)
- `CompatSandboxConfig` — per-executable-type sandbox settings (memory, files, threads, capabilities)
- `CompatProcess` — isolated process with capability checking, resource limits, syscall blocking
- `CompatSandboxManager` — process lifecycle management (spawn, terminate, cleanup)

#### Integration Tests
- Header parsing (ELF/PE/AIOS), POSIX translation, Win32 translation, dependency healing, sandbox isolation, cross-subsystem lifecycle

### Documentation
- Bilingual documentation maintained for all new features (EN + RU)

## v0.5.0 — GPU Detection, Hot-Reload & Process Groups (2026-07-26)

### HAL: NVIDIA GPU Detection via nvidia-smi
- `GpuInfo` expanded with `driver_version: String`, `cuda_cores: u32`, `compute_capability: String`
- `detect_gpu_nvidia()` — runs `nvidia-smi --query-gpu=name,memory.total,driver_version,compute_cap --format=csv,noheader,nounits` on Windows
- `estimate_cuda_cores(gpu_name)` — maps GPU model names to CUDA core counts (RTX 4090→16384, A100→6912, H100→16896, RTX 3090→10496, etc.)
- `detect_gpu_wmic()` — legacy Windows fallback preserved
- 4 new mock profiles with full GPU info: `mock_modern()` now includes NVIDIA GPU with driver version and CUDA cores
- `mock_nvidia()` — dedicated high-end NVIDIA test profile (RTX 4090 + Ryzen 9 7950X3D)
- 8 new unit tests: GpuInfo fields, mock profiles, CUDA core estimation, serialization roundtrip

### Block Manager: Hot-Reload from Filesystem
- `HotReloader` struct in `hot_reload.rs` — watches a directory for `.bin`/`.aib` file changes
- `scan_and_reload(registry)` — detects new, updated, and removed block files
- `HotReloadConfig`: `watch_dir`, `poll_interval_ms`, `auto_activate`
- `TrackedFile` — tracks `path`, `modified`, `sha256`, `loaded_id` for each watched file
- `ReloadEvent` enum: `NewBlock`, `UpdatedBlock`, `RemovedBlock`, `Error`, `NoChange`
- SHA-256 change detection — files only reloaded when content actually changes
- Auto-creates watch directory if it doesn't exist
- Event log accumulates all reload events for audit trail
- 9 unit tests: creation, directory scanning, file detection, update detection, event logging

### Process Manager: Process Groups & Sessions
- `ProcessGroup` struct: `id`, `name`, `priority`, `member_pids`, `created_at_ms`, `session_id`
- `Process` struct extended with `group_id: Option<u64>` and `with_group()` builder
- `Scheduler` group management: `create_group()`, `create_session()`, `add_to_group()`, `remove_from_group()`
- Group operations: `kill_group()`, `suspend_group()`, `resume_group()`, `set_group_priority()`
- `group_members()`, `all_groups()`, `group_count()`, `get_group()`
- `set_priority()` on individual processes for priority changes
- 10 new unit tests: create group, create session, add/remove members, kill/suspend/resume group, set group priority, error cases

### Documentation
- Bilingual documentation: all 4 doc files (ARCHITECTURE, CHANGELOG, BUGS, TODO) maintained in both English and Russian
- AGENTS.md updated with bilingual documentation policy and documentation structure section

## v0.4.0 — System Hardening & Priority 2 (2026-07-26)

### IPC Bus Improvements
- **Backpressure policies**: `BackpressurePolicy::Reject` (default) and `BackpressurePolicy::DropOldest`
- `IpcBus::with_backpressure()` builder method for both `IpcBus` and `SharedIpcBus`
- Drop-oldest evicts front of queue and removes from dedup set
- **Message deduplication**: `IpcBus::with_dedup()` enables packet_id-based dedup via `HashSet<u64>`
- Duplicate sends silently dropped, counted in `metrics.total_deduplicated`
- **Bus metrics**: `BusMetrics` struct tracking `total_sent`, `total_received`, `total_dropped`, `total_deduplicated`, `peak_queue_depth`, `avg_send_latency_us`
- `metrics()` and `reset_metrics()` methods on `IpcBus`
- 7 unit tests: backpressure (reject + drop-oldest), dedup, metrics, reset, priority with drop-oldest

### Scheduler Enhancements
- **Weighted round-robin**: `priority_weight()` maps Background=1, Low=2, Normal=3, High=4, Critical=5
- Time slice = `default_time_slice_ms * priority_weight` (proportional to priority)
- `round_robin_positions: HashMap<Priority, usize>` tracks position within each priority queue
- Fixed starvation-prevention `break` bug: inner loop now evaluates all processes in a queue (aging can boost later processes above earlier ones)
- **Memory pressure detection**: `memory_pressure_threshold` (default: 0.8)
- `MemoryPressure` enum: `Normal(usage)`, `Warning(usage)`, `Critical(usage)`
- `MemoryPressureEvent` struct with level, usage, used/total MB, callback names
- `register_memory_pressure_callback()` and `check_memory_pressure()` methods
- 5 new unit tests: priority weight, weighted time slice (same + cross priority), memory pressure (normal, warning, critical)

### Block Manager Enhancements
- **Dependency graph** (`dependency.rs`): `DependencyGraph` with `HashMap<String, Vec<String>>` edges
- `add_block()`, `add_dependency()` with cycle detection via DFS
- `load_order()` — topological sort (Kahn's algorithm) for correct initialization order
- `unload_order()` — reverse topological for safe teardown
- `dependencies_of()`, `dependents_of()`, `remove_block()`, `blocks()`, `has_block()`
- **Semantic versioning** (`version.rs`): `SemanticVersion` struct (major, minor, patch)
- `parse()` with optional `v` prefix, `Display` formatting
- `Ord` implementation for version comparison
- `is_compatible_with()` (same major, >= minor), `is_newer_than()`
- `bump_major/minor/patch()` for version incrementing
- 9 dependency tests + 7 version tests

### Bug Fixes
- **BUG-010**: Fixed `schedule_next()` early-break in priority queue inner loop — aging could boost a later process above an earlier one within the same queue, but `break` prevented evaluating all processes. Removed `break` so all Ready processes in a queue are evaluated.
- **BUG-011**: Fixed flaky `test_unload_order_reversed` — topological sort order is non-deterministic for independent nodes (HashMap iteration). Tests now verify dependency constraints rather than absolute positions.

### Additional Integration Tests (21-28)
- `test_ipc_bus_backpressure_dedup_metrics` — DropOldest policy + dedup + metrics reset
- `test_scheduler_weighted_round_robin` — round-robin within same priority level
- `test_scheduler_memory_pressure_detection` — warning and critical pressure levels with callbacks
- `test_block_dependency_graph_ordering` — 6-block dependency graph, load/unload order verification
- `test_semantic_version_with_block_registry` — version comparison + registry integration
- `test_ipc_bus_priority_cross_crates` — priority queue ordering via send_priority
- `test_dependency_graph_complex_cycle` — cycle detection + node removal + re-verification
- `test_cross_subsystem_scheduler_security_ipc` — scheduler + security + IPC bus cross-crate test

## v0.3.0 — System Integration & Scheduling Enhancements (2026-07-26)

### Scheduler Process Aging (Starvation Prevention)
- Added `aging_threshold_ms` and `last_scheduled_ms` to `Scheduler` for tracking wait times
- `schedule_next()` now computes effective priority = base priority + wait time / threshold (capped at +4 boost)
- All processes evaluated globally (not early-break by queue level) for correct aging behavior
- `ProcessTimer::force_expire()` for deterministic testing
- Public API: `force_preempt()`, `set_last_scheduled()`, `is_scheduled()`, `with_aging_threshold()`
- Unit test: `test_aging_boosts_low_priority`

### Watchdog-TUI Integration
- Added `aios-watchdog` and `aios-context` dependencies to `aios-tui`
- Watchdog heartbeat thread runs in background during TUI session
- Dashboard header shows live watchdog state: OK (Green), SUSPENDED (Red), RECOVERING (Yellow), SAFE MODE (Magenta)
- `DashboardState::update_watchdog()` for state synchronization
- New keybindings: `s` = record telemetry, `x` = system status
- `SafeModeShell` integrated into main loop for safe mode command execution

### Context Store ↔ Scheduler Wiring
- `EmbeddedContextStore` and `TelemetryStore` initialized in main loop
- Telemetry recording via `s` key: records process count and RAM metrics
- `TelemetryEntry` API: `with_block()`, `with_process()` builder pattern
- Integration test: `test_context_store_wired_to_scheduler` verifies telemetry-driven priority adjustment

### IPC Bus Priority Queue
- `IpcBus::send_priority()` method for priority-based dequeue ordering
- Higher priority packets are dequeued first, FIFO within same priority level
- 2 unit tests: `test_priority_queue_ordering`, `test_priority_fifo_within_same_level`

### Additional Integration Tests (11-20)
- `test_watchdog_heartbeat_lifecycle` — watchdog + IPC bus coordination
- `test_safe_mode_shell_lifecycle` — safe mode command parsing and execution
- `test_security_sandbox_enforcement` — capability + sandbox cross-module enforcement
- `test_context_store_cross_collection` — telemetry + workflow + stability query API
- `test_watchdog_scheduler_crash_coordination` — watchdog triggers scheduler crash handling
- `test_security_ipc_packet_capability_check` — capability check on IPC packets
- `test_live_update_with_security_revocation` — hot-swap + token revocation coordination
- `test_telemetry_driven_priority_adjustment` — telemetry queries drive scheduler priority changes
- `test_scheduler_aging_starvation_prevention` — aging boosts low-priority process scheduling
- `test_context_store_wired_to_scheduler` — context store telemetry feeds scheduler decisions

### Documentation
- Created: AGENTS.md, README.md, docs/ARCHITECTURE.md, docs/CHANGELOG.md, docs/BUGS.md, docs/TODO.md

## v0.2.0 — Safety, Security & Context Systems (2026-07-25)

### Phase 8: AI Watchdog & Emergency Recovery Engine (`aios-watchdog`)
- `Heartbeat` struct with SHA-256 HMAC authentication
- `Watchdog` with 4-state machine: Monitoring → Suspended → Recovering → SafeMode
- Configurable heartbeat interval, miss threshold, and recovery timeout
- `WatchdogConfig` with sensible defaults (1s interval, 3 misses, 10s recovery)
- `WatchdogAction` enum for kernel response decisions
- `WatchdogEvent` audit trail for all state transitions
- `SafeModeShell` with deterministic CLI commands (ps, blocks, kill, unload, status, logs, restart)
- Restart limiting to prevent infinite loops
- 19 unit tests covering heartbeat auth, watchdog state machine, recovery cycles, safe mode

### Phase 9: Capability-Based Security & Sandboxing (`aios-security`)
- `Capability` enum with 15 specific permissions (network, filesystem, hardware, memory, system)
- `CapabilityToken` with HMAC signatures and time-bounded validity
- `AccessControlLayer` for token issuance, validation, revocation, and violation tracking
- `Sandbox` per-block isolation with syscall validation, memory limits, and syscall count limits
- `Violation` audit trail for unauthorized access attempts
- 20 unit tests covering token lifecycle, access control, sandbox enforcement

### Phase 10: Persistent System Context Store (`aios-context`)
- `EmbeddedContextStore` unifying telemetry, workflows, and stability collections
- `TelemetryStore` with FIFO overflow (10k entries), metric queries, time-range queries, per-block queries
- `WorkflowStore` for learned priority profiles with usage tracking
- `StabilityStore` for block reliability scoring with crash/uptime tracking
- 18 unit tests covering all store operations

## v0.1.0 — Initial System (2026-07-25)

### Phase 1: Workspace + IPC Protocol
- Created flat workspace with 7 crates (aios-core, aios-ipc, aios-hal, aios-block-mgr, aios-live-update, aios-process-mgr, aios-tui)
- Implemented binary IPC protocol using bincode serialization
- `Header` struct with packet_id, source/target blocks, command_id, priority, payload_len, SHA-256 checksum
- `Payload` enum with 15 variants covering all OS operations
- `Response` enum (Success/Failure/Timeout)
- `IpcPacket` with auto-generated packet_id (AtomicU64) and SHA-256 integrity check
- `CommandId` enum (u16) with 13 command types organized by domain (block=0x0001-0x0003, process=0x0010-0x0012, system=0x0020-0x0050)
- Speed tests with dual thresholds (debug: 50us, release: 1us)

### Phase 2: Hardware Abstraction Layer (HAL)
- `HardwareProfile` with CPU, GPU, NPU, Memory, PCI detection
- Real detection via `wmic` (Windows) and `/proc/cpuinfo` + `/proc/meminfo` (Linux)
- `CpuInfo` with AVX-512, AVX2, SSE4.2, NEON flags
- `GpuInfo`, `NpuInfo`, `PciDevice`, `MemoryInfo` structs
- `AiTier` classification: Tier1 (local LLM capable), Tier2 (edge inference), Tier3 (lightweight only)
- 4 mock profiles: legacy, modern, legacy_2012, custom
- `HalBlock` implementing `StatefulBlock` trait (extract/restore profile state)
- 8 unit tests for tier classification logic

### Phase 3: Block Manager
- `BlockRegistry` with register/unload/activate, SHA-256 signature verification
- `BlockEntry` stores manifest + state + binary
- `BlockLoader` for binary validation and one-shot load+activate pipeline
- `MessageRouter` with handler dispatch and route remapping
- `BlockHandler` = `Box<dyn FnMut(&IpcPacket) -> Result<Option<IpcPacket>>>`
- 15 unit tests across registry, loader, and router

### Phase 4: Process Manager
- `ProcessId`, `Priority` (5 levels: Background/Critical), `ProcessState` (5 states)
- `Process` struct with crash_count, max_restarts, parent_pid
- `ProcessTimer` for time-slicing with quota tracking
- `Scheduler` with BTreeMap priority queues, RAM quota enforcement
- Priority-based scheduling with round-robin within same priority
- Crash resilience: auto-restart up to max_restarts, CrashEvent logging
- `handle_process_command()` for IPC-driven spawn/kill/adjust_priority
- 10 unit tests including crash and child process tests

### Phase 5: Live-Update Engine
- `StateTransferManager` for freeze/extract/restore of IPC bus state
- `Snapshot` struct capturing pending packets + state bytes
- `LiveUpdateEngine` with atomic hot-swap (5-step process):
  1. Freeze IPC bus + extract state
  2. Validate new binary SHA-256
  3. Run health check (optional closure)
  4. Store rollback entry
  5. Restore IPC bus
- `HotSwapEntry` for rollback data (old binary, state, version)
- `SwapRecord` audit trail for all swap operations
- Rollback with configurable timeout warning
- 9 unit tests covering success, failure, and rollback scenarios

### Phase 6: AI Orchestrator + TUI
- `IntentEngine` translating natural language to `IpcPacket`
- 8 intent categories: memory, video, block_update, kill, spawn, priority, health_check, topology
- `IntentContext` providing system state for intent translation
- `TranslatedCommand` with explanation, intent description, and IPC packet
- Ratatui dashboard with 3-zone layout: header (tier + metrics), main (system info + log), footer (keybinds)
- `DashboardState` with 100-entry log buffer and scheduler synchronization
- Color-coded display: tier colors (Green/Yellow/Red), log severity colors
- main.rs entry point: hardware detect -> tier classify -> load blocks -> spawn processes -> terminal event loop
- 10 unit tests for intent translation and dashboard state

### Phase 7: Integration Tests
- 10 integration tests covering:
  1. Full system lifecycle (HAL -> tier -> registry -> scheduler -> topology)
  2. IPC serialization speed (50k roundtrips)
  3. 100 concurrent process spawns with RAM tracking
  4. Live-update with 50 in-flight IPC messages
  5. Crash resilience (3 crashes, restart policy enforcement)
  6. Message router integration (direct + redirected dispatch)
  7. Process control IPC lifecycle (spawn -> PID -> adjust -> kill)
  8. AI tier classification across all profiles
  9. Stateful block roundtrip (extract/restore)
  10. Full hotswap lifecycle with bus preservation and rollback

## v1.0.0 — Production Docker-инфраструктура для VM (2026-07-28)

### Docker: Multi-stage production сборка
- Полностью переработан `Dockerfile`: multi-stage сборка (builder → runtime)
- **builder** (rust:1.97-bookworm): компиляция `--release` + прогон `--lib` тестов
- **runtime** (debian:bookworm-slim): минимальный образ ~80MB с бинарником `aios-tui`
- `docker-compose.yml` обновлён: использует `target: runtime`, добавлены `stdin_open`/`tty` для TUI
- Сигнал завершения: `SIGINT` для корректного shutdown через crossterm
- Переменные окружения: `AIOS_DATA_DIR`, `AIOS_BLOCKS_DIR`, `AIOS_MOCK_PROFILE`, `RUST_LOG`

## Debugging Notes
- Speed test thresholds required dual debug/release values due to 10-20x slowdown in unoptimized builds
- Windows PATH must be manually assembled from Machine + User environment variables before cargo commands
- `skip_while().next()` lint replaced with `position().nth()` pattern for cleaner iterator logic

## v1.3.0 — Headless Daemon & Docker Fix (2026-07-29)

### aios-daemon — New Crate: Headless Server Binary
- New crate `aios-daemon` — headless AIOS server for Docker/background deployment
- Minimal dependencies: no ratatui, no crossterm, no egui, no wasmtime
- `aiosd` binary — performs same initialization as `aios-tui` (blocks, scheduler, watchdog, DB) without terminal access
- Background heartbeat loop: logs process count, RAM usage, watchdog state every 10 seconds
- Periodic telemetry persistence to redb every 60 seconds
- Environment config: `AIOS_DATA_DIR`, `AIOS_BLOCKS_DIR`, `AIOS_MOCK_PROFILE`, `RUST_LOG`

### aios-tui: Headless Mode
- Added `--headless` CLI flag and `AIOS_HEADLESS=1` env var support
- When headless, skips TUI initialization (ratatui/crossterm) and runs background loop

### Docker Infrastructure
- Dockerfile simplified: builds only `aios-daemon` (fast build ~2min)
- Uses `aiosd` binary as default CMD — no TTY required
- docker-compose.yml cleaned up: headless daemon by default, `aios-interactive` profile for TUI
- Removed `stdin_open`/`tty` from default service (not needed for headless)
- Final image size: ~120MB (down from ~800MB with full workspace build)

## Total Stats (v0.5.0)
- **12 workspace crates** + integration test crate + stress test file
- **~10,800 lines of Rust** (excluding tests)
- **350 tests** (292 unit + 28 integration + 11 stress + 19 exec-compat)
- **0 clippy warnings**
- **90+ public types**, **320+ public methods**
