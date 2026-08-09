use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use aios_block_mgr::loader::BlockLoader;
use aios_block_mgr::registry::BlockRegistry;
use aios_bridge::dto::{StorePublishRequest, StorePublishResponse};
use aios_browser::html_parser::HtmlParser;
use aios_context::persistence::PersistentStore;
use aios_context::store::EmbeddedContextStore;
use aios_context::telemetry::{TelemetryEntry, TelemetryStore};
use aios_core::block::BlockId;
use aios_fm::commands::Command;
use aios_fm::engine::FileManager;
use aios_fm::state::PanelSide;
use aios_fm::ui_tui::{key_to_action, TuiAction};
use aios_hal::ai_tier::AiTier;
use aios_hal::hardware::HardwareProfile;
use aios_net_config::block::NetSettingsBlock;
use aios_net_config::config::NetworkConfig;
use aios_process_mgr::scheduler::Scheduler;
use aios_process_mgr::task::Priority;
use aios_store::manager::StoreManager;
use aios_store::manifest::{sign_manifest, ManifestInfo, ManifestValidator};
use aios_tui::dashboard::{self, DashboardState, PageContent};
use aios_vfs::security::AclContext;
use aios_vfs::vfs::{AiosVfs, VirtualFileSystem};
use aios_watchdog::heartbeat::Heartbeat;
use aios_watchdog::safe_mode::SafeModeShell;
use aios_watchdog::watchdog::{Watchdog, WatchdogConfig, WatchdogState};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn is_headless() -> bool {
    std::env::var("AIOS_HEADLESS").as_deref() == Ok("1")
        || std::env::args().any(|a| a == "--headless")
}

/// HTTP client with a desktop-browser User-Agent so real sites respond
/// (default `reqwest` UA is often blocked) and a 15s timeout so a stuck host
/// cannot hang a fetch forever.
fn http_client() -> Result<reqwest::blocking::Client, reqwest::Error> {
    reqwest::blocking::Client::builder()
        .user_agent(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
        )
        .timeout(Duration::from_secs(15))
        .build()
}

fn fetch_url(url: &str) -> Result<PageContent, Box<dyn std::error::Error>> {
    let resp = http_client()?
        .get(url)
        .header(reqwest::header::ACCEPT, "text/html,application/xhtml+xml")
        .send()?;
    let html = resp.text()?;
    let title = HtmlParser::extract_title(&html);
    let text = HtmlParser::extract_text(&html);
    let links = HtmlParser::extract_links(&html, url)
        .into_iter()
        .map(|l| (l.text, l.href))
        .collect();
    Ok(PageContent {
        url: url.to_string(),
        title,
        text,
        links,
    })
}

fn search_web(query: &str) -> Result<PageContent, Box<dyn std::error::Error>> {
    let url = format!("https://html.duckduckgo.com/html/?q={}", urlencoding(query));
    let resp = http_client()?.get(&url).send()?;
    let html = resp.text()?;
    let title = format!("Search results: {query}");
    let text = HtmlParser::extract_text(&html);
    let links = HtmlParser::extract_links(&html, &url)
        .into_iter()
        .map(|l| (l.text, l.href))
        .collect();
    Ok(PageContent {
        url,
        title,
        text,
        links,
    })
}

fn is_url_input(s: &str) -> bool {
    let s = s.trim();
    s.starts_with("http://")
        || s.starts_with("https://")
        || (s.contains('.') && !s.contains(|c: char| c.is_whitespace()))
}

fn load_url(state: &mut DashboardState, url: &str, push_history: bool) {
    let url = url.trim().to_string();
    if url.is_empty() {
        return;
    }
    let prev = state.web_state.current_url.clone();
    if push_history
        && !prev.is_empty()
        && prev != url
        && state.web_state.history.last().map(String::as_str) != Some(url.as_str())
    {
        state.web_state.history.push(prev);
    }
    if let Some(page) = state.web_state.cached_page(&url) {
        state.web_state.page = Some(page.clone());
        state.web_state.current_url = url.clone();
        state.web_state.url_input.clear();
        state.web_state.search_query.clear();
        state.web_state.loading = false;
        state.web_state.error = None;
        state.web_state.scroll = 0;
        state.web_state.links_scroll = 0;
        state.selected_row = 0;
        state.add_log(format!("Web: loaded {url} (cached)"));
        return;
    }
    state.web_state.loading = true;
    state.web_state.page = None;
    state.web_state.error = None;
    state.web_state.scroll = 0;
    state.web_state.web_fetch_gen += 1;
    let gen = state.web_state.web_fetch_gen;
    let outbox = state.page_cache.clone();
    let url_fetch = url.clone();
    state.add_log(format!("Navigating to: {url}"));
    std::thread::spawn(move || {
        let result = fetch_url(&url_fetch)
            .map(|p| (p, None))
            .map_err(|e| e.to_string());
        if let Ok(mut slot) = outbox.lock() {
            *slot = Some((gen, result));
        }
    });
}

fn navigate_web(state: &mut DashboardState, raw: &str) {
    let raw = raw.trim();
    if raw.is_empty() {
        return;
    }
    if is_url_input(raw) {
        let url = if raw.starts_with("http://") || raw.starts_with("https://") {
            raw.to_string()
        } else {
            format!("https://{raw}")
        };
        load_url(state, &url, true);
    } else {
        let prev = state.web_state.current_url.clone();
        if !prev.is_empty() && prev != raw {
            state.web_state.history.push(prev);
        }
        state.web_state.loading = true;
        state.web_state.page = None;
        state.web_state.error = None;
        state.web_state.url_input.clear();
        state.web_state.web_fetch_gen += 1;
        let gen = state.web_state.web_fetch_gen;
        let outbox = state.page_cache.clone();
        let query = raw.to_string();
        state.add_log(format!("Searching for: {raw}"));
        std::thread::spawn(move || {
            let result = search_web(&query)
                .map(|p| (p, Some(query)))
                .map_err(|e| e.to_string());
            if let Ok(mut slot) = outbox.lock() {
                *slot = Some((gen, result));
            }
        });
    }
}

fn open_selected_link(state: &mut DashboardState) {
    let href = state.web_state.page.as_ref().and_then(|page| {
        page.links
            .get(state.selected_row)
            .map(|(_text, href)| href.clone())
    });
    if let Some(href) = href {
        load_url(state, &href, true);
    }
}

/// Pop the last visited page from the web tab history and navigate back to it.
/// The page is restored without pushing anything back, so repeated presses
/// drain the history instead of ping-ponging between the last two pages.
fn web_go_back(state: &mut DashboardState) {
    match state.web_state.history.pop() {
        Some(prev) => {
            state.add_log(format!("Web: back to {prev}"));
            load_url(state, &prev, false);
        }
        None => state.add_log("Web: no history to go back to".into()),
    }
}

/// Scroll the web page content one line (`dir` = +1 down, -1 up).
fn web_scroll(state: &mut DashboardState, dir: isize) {
    let max = state
        .web_state
        .page
        .as_ref()
        .map(|p| {
            dashboard::wrap_text(&p.text, state.web_state.wrap_width)
                .len()
                .saturating_sub(2)
        })
        .unwrap_or(0);
    let next = state.web_state.scroll as isize + dir;
    state.web_state.scroll = next.clamp(0, max as isize) as usize;
}

/// Move the navigation sidebar selection (`dir` = +1 down, -1 up), wrapping
/// around the entry list.
fn web_sidebar_move(state: &mut DashboardState, dir: isize) {
    let len = dashboard::web_nav_entries(&state.web_state).len();
    if len == 0 {
        state.web_state.history_sel = 0;
        return;
    }
    let next = state.web_state.history_sel as isize + dir;
    state.web_state.history_sel = next.rem_euclid(len as isize) as usize;
}

/// Open the currently selected navigation sidebar entry. Selecting the current
/// page reloads it; selecting a history entry navigates back to it.
fn web_sidebar_open(state: &mut DashboardState) {
    let entries = dashboard::web_nav_entries(&state.web_state);
    let sel = state.web_state.history_sel;
    if let Some(entry) = entries.get(sel) {
        if entry.is_current {
            state.add_log("Web: reloading current page".into());
        } else {
            state.add_log(format!("Web: history → {}", entry.url));
        }
        load_url(state, &entry.url, false);
    }
}

/// Live native browser window handle (aios-webview), kept alive across key
/// presses. Lives outside `DashboardState` so a background thread can create
/// the window without blocking the TUI.
static WEB_BROWSER: OnceLock<Mutex<Option<aios_webview::WebBrowser>>> = OnceLock::new();

/// Raised while a background thread is still opening the native browser, so
/// rapid repeated key presses do not spawn a second window.
static WEB_BROWSER_SPAWNING: OnceLock<AtomicBool> = OnceLock::new();

fn web_browser_handle() -> &'static Mutex<Option<aios_webview::WebBrowser>> {
    WEB_BROWSER.get_or_init(|| Mutex::new(None))
}

fn web_browser_spawning() -> &'static AtomicBool {
    WEB_BROWSER_SPAWNING.get_or_init(|| AtomicBool::new(false))
}

/// Open `target` in the full native browser window (`B` on the Web tab). If a
/// window already exists it is reused and just navigated; otherwise one is
/// created on a background thread so the TUI stays responsive.
fn web_open_native(state: &mut DashboardState, target: Option<String>) {
    let target = match target {
        Some(t) => t,
        None => match web_current_page_url(state) {
            Some(u) => u,
            None => {
                state.add_log("Web: nothing to open — load a page first".into());
                return;
            }
        },
    };
    let handle = web_browser_handle();
    let mut guard = handle.lock().unwrap_or_else(|p| p.into_inner());
    match guard.as_mut() {
        Some(browser) => match browser.navigate(&target) {
            Ok(()) => state.add_log(format!("Browser: navigating to {target}")),
            Err(_) => {
                *guard = None;
                state.add_log(format!("Browser: reopening {target}"));
                web_browser_spawn(target);
            }
        },
        None => {
            state.add_log(format!("Browser: opening {target}"));
            web_browser_spawn(target);
        }
    }
}

/// URL of the currently loaded page, if any.
fn web_current_page_url(state: &DashboardState) -> Option<String> {
    state.web_state.page.as_ref().map(|p| p.url.clone())
}

fn web_browser_spawn(target: String) {
    let handle = web_browser_handle();
    let spawning = web_browser_spawning();
    if spawning.swap(true, Ordering::SeqCst) {
        log::debug!("native browser already being opened, ignoring");
        return;
    }
    std::thread::spawn(move || {
        match aios_webview::WebBrowser::open(&target) {
            Ok(browser) => {
                if let Ok(mut guard) = handle.lock() {
                    *guard = Some(browser);
                }
            }
            Err(e) => log::warn!("native browser failed to open: {e}"),
        }
        spawning.store(false, Ordering::SeqCst);
    });
}

fn execute_shell_cmd(
    state: &mut DashboardState,
    cmd: &str,
    scheduler: &mut Scheduler,
    registry: &mut BlockRegistry,
    safe_shell: &mut SafeModeShell,
) {
    let lower = cmd.trim().to_lowercase();
    let parts: Vec<&str> = lower.split_whitespace().collect();
    let command = match parts.first().copied() {
        Some("clear") | Some("cls") => {
            state.shell_state.output.clear();
            return;
        }
        Some("fetch") => {
            let url = *parts.get(1).unwrap_or(&"");
            if url.is_empty() {
                state.shell_state.add_output("Usage: fetch <url>".into());
                return;
            }
            state
                .shell_state
                .add_output(format!("Fetching block from: {url}..."));
            match reqwest::blocking::get(url) {
                Ok(resp) => match resp.bytes() {
                    Ok(binary) => {
                        let name = url.split('/').next_back().unwrap_or("block");
                        match BlockLoader::load_from_binary(
                            registry,
                            name,
                            "1.0.0",
                            binary.to_vec(),
                        ) {
                            Ok(m) => state
                                .shell_state
                                .add_output(format!("Loaded block '{}' ID {}", m.name, m.id)),
                            Err(e) => state.shell_state.add_output(format!("Load failed: {e}")),
                        }
                    }
                    Err(e) => state.shell_state.add_output(format!("Read failed: {e}")),
                },
                Err(e) => state.shell_state.add_output(format!("Fetch failed: {e}")),
            }
            return;
        }
        Some("search") => {
            let query = parts.get(1..).map(|p| p.join(" ")).unwrap_or_default();
            if query.is_empty() {
                state.shell_state.add_output("Usage: search <query>".into());
                return;
            }
            state
                .shell_state
                .add_output(format!("Searching for: {query}..."));
            let url = format!(
                "https://html.duckduckgo.com/html/?q={}",
                urlencoding(&query)
            );
            match reqwest::blocking::get(&url) {
                Ok(resp) => {
                    if let Ok(html) = resp.text() {
                        let links = HtmlParser::extract_links(&html, &url);
                        state
                            .shell_state
                            .add_output(format!("Found {} results:", links.len()));
                        for (i, link) in links.iter().take(20).enumerate() {
                            let text = if link.text.is_empty() {
                                &link.href
                            } else {
                                &link.text
                            };
                            state.shell_state.add_output(format!(
                                "  {}. {} — {}",
                                i + 1,
                                text,
                                link.href
                            ));
                        }
                    }
                }
                Err(e) => state.shell_state.add_output(format!("Search failed: {e}")),
            }
            return;
        }
        Some("open") => {
            let url = (*parts.get(1).unwrap_or(&"")).to_string();
            if url.is_empty() {
                state.shell_state.add_output("Usage: open <url>".into());
                return;
            }
            state.selected_tab = 5;
            state.selected_row = 0;
            navigate_web(state, &url);
            return;
        }
        Some("net") => {
            let sub = parts.get(1).copied().unwrap_or("");
            let mut block =
                NetSettingsBlock::with_default_store(BlockId::new(9), NetworkConfig::default());
            match sub {
                "get" => state.shell_state.add_output(block.config().to_json()),
                "set" => {
                    let mut updates = serde_json::Map::new();
                    for kv in parts.iter().skip(2) {
                        let (k, v) = match kv.split_once('=') {
                            Some(kv) => kv,
                            None => {
                                state.shell_state.add_output(format!(
                                    "Usage: net set key=value ... (bad token: {kv})"
                                ));
                                return;
                            }
                        };
                        let value = serde_json::from_str::<serde_json::Value>(v)
                            .unwrap_or_else(|_| serde_json::Value::String(v.to_string()));
                        updates.insert(k.to_string(), value);
                    }
                    if updates.is_empty() {
                        state
                            .shell_state
                            .add_output("Usage: net set hostname=myhost listen_port=9090".into());
                        return;
                    }
                    match block.apply(&serde_json::Value::Object(updates)) {
                        Ok(()) => state.shell_state.add_output(block.config().to_json()),
                        Err(e) => state.shell_state.add_output(format!("Error: {e}")),
                    }
                }
                "reset" => match block.reset() {
                    Ok(()) => state.shell_state.add_output(block.config().to_json()),
                    Err(e) => state.shell_state.add_output(format!("Error: {e}")),
                },
                _ => state
                    .shell_state
                    .add_output("Usage: net get | net set key=value ... | net reset".into()),
            }
            return;
        }
        Some("store") => {
            let blocks_dir = env_or("AIOS_BLOCKS_DIR", "/app/blocks");
            let store_cfg =
                PathBuf::from(env_or("AIOS_DATA_DIR", "/app/data")).join("store_config.json");
            let mut manager = match StoreManager::load_config(&store_cfg, &blocks_dir) {
                Ok(m) => m,
                Err(_) => StoreManager::new(&blocks_dir),
            };
            let sub = parts.get(1).copied().unwrap_or("");
            match sub {
                "list" => {
                    let installed = manager.list_installed();
                    if installed.is_empty() {
                        state.shell_state.add_output("No blocks installed.".into());
                    } else {
                        state
                            .shell_state
                            .add_output(format!("Installed blocks ({}):", installed.len()));
                        for b in &installed {
                            state.shell_state.add_output(format!(
                                "  {} {}",
                                b.manifest.name, b.manifest.version
                            ));
                        }
                    }
                }
                "sources" => {
                    for s in &manager.sources {
                        state.shell_state.add_output(format!(
                            "  {} — {} trusted key(s)",
                            s.display(),
                            s.trusted_public_keys.len()
                        ));
                    }
                }
                "add-source" => {
                    let spec = parts.get(2).copied().unwrap_or("");
                    if spec.is_empty() {
                        state.shell_state.add_output(
                            "Usage: store add-source <github:owner/repo|local:path|http://url>"
                                .into(),
                        );
                        return;
                    }
                    match StoreManager::parse_source_spec(spec) {
                        Ok(source) => match manager.add_source(source) {
                            Ok(()) => {
                                let _ = manager.save_config(&store_cfg);
                                state
                                    .shell_state
                                    .add_output(format!("Added source: {spec}"))
                            }
                            Err(e) => state.shell_state.add_output(format!("Error: {e}")),
                        },
                        Err(e) => state.shell_state.add_output(format!("Error: {e}")),
                    }
                }
                "trust" => {
                    let mut source_name = None;
                    let mut key_hex = None;
                    let mut clear = false;
                    let mut i = 2;
                    while i < parts.len() {
                        match parts[i] {
                            "--key" => {
                                key_hex = parts.get(i + 1).map(|s| s.to_string());
                                i += 2;
                            }
                            "--clear" => {
                                clear = true;
                                i += 1;
                            }
                            _ if source_name.is_none() => {
                                source_name = Some(parts[i].to_string());
                                i += 1;
                            }
                            _ => {
                                i += 1;
                            }
                        }
                    }
                    let source_name = match source_name {
                        Some(s) => s,
                        None => {
                            state.shell_state.add_output(
                                "Usage: store trust <source> [--key <public_hex>] [--clear]".into(),
                            );
                            return;
                        }
                    };
                    if clear {
                        match manager.clear_source_trust(&source_name) {
                            Ok(removed) => {
                                let _ = manager.save_config(&store_cfg);
                                state.shell_state.add_output(format!(
                                    "Cleared {removed} trusted key(s) from '{source_name}'"
                                ));
                            }
                            Err(e) => state.shell_state.add_output(format!("Error: {e}")),
                        }
                        return;
                    }
                    match key_hex {
                        Some(key) => {
                            let bytes = match hex::decode(&key) {
                                Ok(b) => b,
                                Err(e) => {
                                    state
                                        .shell_state
                                        .add_output(format!("Invalid key hex: {e}"));
                                    return;
                                }
                            };
                            let arr: [u8; 32] = match <[u8; 32]>::try_from(bytes.as_slice()) {
                                Ok(a) => a,
                                Err(_) => {
                                    state.shell_state.add_output(
                                        "Trusted key must be 32 bytes (64 hex chars)".into(),
                                    );
                                    return;
                                }
                            };
                            if ed25519_dalek::VerifyingKey::from_bytes(&arr).is_err() {
                                state
                                    .shell_state
                                    .add_output("Invalid Ed25519 public key".into());
                                return;
                            }
                            match manager.trust_source(&source_name, std::slice::from_ref(&key)) {
                                Ok(()) => {
                                    let _ = manager.save_config(&store_cfg);
                                    state.shell_state.add_output(format!(
                                        "Trusted key {key} for source '{source_name}'"
                                    ));
                                }
                                Err(e) => state.shell_state.add_output(format!("Error: {e}")),
                            }
                        }
                        None => match manager.source(Some(&source_name)) {
                            Ok(src) => {
                                if src.trusted_public_keys.is_empty() {
                                    state.shell_state.add_output(format!(
                                        "Source '{source_name}' trusts no keys (unsigned allowed)"
                                    ));
                                } else {
                                    state
                                        .shell_state
                                        .add_output(format!("Trusted keys for '{source_name}':"));
                                    for k in &src.trusted_public_keys {
                                        state.shell_state.add_output(format!("  {k}"));
                                    }
                                }
                            }
                            Err(e) => state.shell_state.add_output(format!("Error: {e}")),
                        },
                    }
                }
                "search" => {
                    let (query, source_name) = split_store_args(parts.get(2..).unwrap_or(&[]));
                    if query.is_empty() {
                        state
                            .shell_state
                            .add_output("Usage: store search <query> [--source NAME]".into());
                        return;
                    }
                    state
                        .shell_state
                        .add_output(format!("Searching store for '{query}'..."));
                    match StoreManager::block_on(manager.search(&query, source_name.as_deref())) {
                        Ok(results) => {
                            if results.is_empty() {
                                state.shell_state.add_output("No matches.".into());
                            } else {
                                state
                                    .shell_state
                                    .add_output(format!("Found {} result(s):", results.len()));
                                for m in &results {
                                    state.shell_state.add_output(format!(
                                        "  {} {} — {}",
                                        m.name, m.version, m.description
                                    ));
                                }
                            }
                        }
                        Err(e) => state.shell_state.add_output(format!("Search failed: {e}")),
                    }
                }
                "install" => {
                    let (name, source_name) = split_store_args(parts.get(2..).unwrap_or(&[]));
                    if name.is_empty() {
                        state
                            .shell_state
                            .add_output("Usage: store install <name> [--source NAME]".into());
                        return;
                    }
                    state
                        .shell_state
                        .add_output(format!("Installing '{name}'..."));
                    match StoreManager::block_on(manager.install(
                        source_name.as_deref(),
                        &name,
                        None,
                    )) {
                        Ok(b) => state.shell_state.add_output(format!(
                            "Installed {} {}",
                            b.manifest.name, b.manifest.version
                        )),
                        Err(e) => state.shell_state.add_output(format!("Install failed: {e}")),
                    }
                }
                "update" => {
                    let (name, source_name) = split_store_args(parts.get(2..).unwrap_or(&[]));
                    state
                        .shell_state
                        .add_output("Checking for updates...".into());
                    let name_arg = if name.is_empty() {
                        None
                    } else {
                        Some(name.as_str())
                    };
                    match StoreManager::block_on(manager.update(source_name.as_deref(), name_arg)) {
                        Ok(updated) => {
                            if updated.is_empty() {
                                state.shell_state.add_output("Already up to date.".into());
                            } else {
                                for b in &updated {
                                    state.shell_state.add_output(format!(
                                        "Updated {} -> {}",
                                        b.manifest.name, b.manifest.version
                                    ));
                                }
                            }
                        }
                        Err(e) => state.shell_state.add_output(format!("Update failed: {e}")),
                    }
                }
                "uninstall" => {
                    let name = parts.get(2).copied().unwrap_or("");
                    if name.is_empty() {
                        state
                            .shell_state
                            .add_output("Usage: store uninstall <name>".into());
                        return;
                    }
                    match manager.uninstall(name) {
                        Ok(removed) => state.shell_state.add_output(format!(
                            "Uninstalled {} version(s) of '{name}'",
                            removed.len()
                        )),
                        Err(e) => state
                            .shell_state
                            .add_output(format!("Uninstall failed: {e}")),
                    }
                }
                "rollback" => {
                    let name = parts.get(2).copied().unwrap_or("");
                    if name.is_empty() {
                        state
                            .shell_state
                            .add_output("Usage: store rollback <name>".into());
                        return;
                    }
                    match manager.rollback(name) {
                        Ok(b) => state.shell_state.add_output(format!(
                            "Rolled back '{}' to {}",
                            b.manifest.name, b.manifest.version
                        )),
                        Err(e) => state
                            .shell_state
                            .add_output(format!("Rollback failed: {e}")),
                    }
                }
                "publish" => {
                    let mut file = None;
                    let mut name = None;
                    let mut version = None;
                    let mut key_hex = std::env::var("AIOS_STORE_SIGNING_KEY").ok();
                    let mut i = 2;
                    while i < parts.len() {
                        if parts[i] == "--key" {
                            key_hex = parts.get(i + 1).map(|s| s.to_string());
                            i += 2;
                        } else if file.is_none() {
                            file = Some(parts[i].to_string());
                            i += 1;
                        } else if name.is_none() {
                            name = Some(parts[i].to_string());
                            i += 1;
                        } else if version.is_none() {
                            version = Some(parts[i].to_string());
                            i += 1;
                        } else {
                            i += 1;
                        }
                    }
                    let file = match file {
                        Some(f) => f,
                        None => {
                            state.shell_state.add_output(
                                "Usage: store publish <file.wasm> [name] [version] [--key <secret_hex>]"
                                    .into(),
                            );
                            return;
                        }
                    };
                    let path = std::path::Path::new(&file);
                    let binary = match std::fs::read(path) {
                        Ok(b) => b,
                        Err(e) => {
                            state.shell_state.add_output(format!("Read failed: {e}"));
                            return;
                        }
                    };
                    let default_name = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "block".to_string());
                    let name = name.unwrap_or(default_name);
                    let version = version.unwrap_or_else(|| "1.0.0".to_string());
                    let sha = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&binary));
                    let wasm_base64 =
                        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &binary);
                    let mut signature = None;
                    if let Some(key_hex) = key_hex {
                        let secret = match hex::decode(&key_hex) {
                            Ok(bytes) => match <[u8; 32]>::try_from(bytes.as_slice()) {
                                Ok(arr) => arr,
                                Err(_) => {
                                    state.shell_state.add_output(
                                        "Signing key must be 32 bytes (64 hex chars)".into(),
                                    );
                                    return;
                                }
                            },
                            Err(e) => {
                                state
                                    .shell_state
                                    .add_output(format!("Invalid key hex: {e}"));
                                return;
                            }
                        };
                        let manifest = ManifestInfo {
                            name: name.clone(),
                            version: version.clone(),
                            description: "Published from TUI shell".into(),
                            author: "local-user".into(),
                            capabilities: std::collections::HashSet::new(),
                            wasm_size_bytes: binary.len() as u64,
                            wasm_sha256: sha.clone(),
                            signature: None,
                            store_url: None,
                        };
                        let signing_key = ed25519_dalek::SigningKey::from_bytes(&secret);
                        signature = Some(sign_manifest(&manifest, &signing_key));
                    }
                    let req = StorePublishRequest {
                        name: name.clone(),
                        version: version.clone(),
                        description: "Published from TUI shell".into(),
                        author: "local-user".into(),
                        capabilities: Vec::new(),
                        checksum_sha256: sha,
                        wasm_base64,
                        signature,
                    };
                    let port = env_or("AIOS_BRIDGE_PORT", "8080");
                    let url = format!("http://localhost:{port}/api/v1/store/publish");
                    state.shell_state.add_output(format!(
                        "Publishing '{}' v{} ({} bytes) to {url}...",
                        name,
                        version,
                        binary.len()
                    ));
                    match http_client().map(|client| client.post(&url).json(&req).send()) {
                        Ok(Ok(resp)) => match resp.json::<StorePublishResponse>() {
                            Ok(pub_resp) => {
                                if pub_resp.success {
                                    state.shell_state.add_output(format!(
                                        "Published {} {}",
                                        pub_resp.name, pub_resp.version
                                    ));
                                } else {
                                    state.shell_state.add_output(format!(
                                        "Publish failed: {}",
                                        pub_resp.error.unwrap_or_default()
                                    ));
                                }
                            }
                            Err(e) => {
                                state
                                    .shell_state
                                    .add_output(format!("Response parse failed: {e}"));
                            }
                        },
                        Ok(Err(e)) => {
                            state.shell_state.add_output(format!("Publish failed: {e}"));
                        }
                        Err(e) => {
                            state.shell_state.add_output(format!("Publish failed: {e}"));
                        }
                    }
                }
                "sign" => {
                    let mut name = None;
                    let mut version = None;
                    let mut key_hex = std::env::var("AIOS_STORE_SIGNING_KEY").ok();
                    let mut file = None;
                    let mut i = 2;
                    while i < parts.len() {
                        if parts[i] == "--key" {
                            key_hex = parts.get(i + 1).map(|s| s.to_string());
                            i += 2;
                        } else if file.is_none() {
                            file = Some(parts[i].to_string());
                            i += 1;
                        } else if name.is_none() {
                            name = Some(parts[i].to_string());
                            i += 1;
                        } else if version.is_none() {
                            version = Some(parts[i].to_string());
                            i += 1;
                        } else {
                            i += 1;
                        }
                    }
                    let file = match file {
                        Some(f) => f,
                        None => {
                            state.shell_state.add_output(
                                "Usage: store sign <file.wasm> [name] [version] [--key <secret_hex>]"
                                    .into(),
                            );
                            return;
                        }
                    };
                    let key_hex = match key_hex {
                        Some(k) => k,
                        None => {
                            state.shell_state.add_output(
                                "No signing key: pass --key <secret_hex> or set AIOS_STORE_SIGNING_KEY"
                                    .into(),
                            );
                            return;
                        }
                    };
                    let secret = match hex::decode(&key_hex) {
                        Ok(bytes) => match <[u8; 32]>::try_from(bytes.as_slice()) {
                            Ok(arr) => arr,
                            Err(_) => {
                                state.shell_state.add_output(
                                    "Signing key must be 32 bytes (64 hex chars)".into(),
                                );
                                return;
                            }
                        },
                        Err(e) => {
                            state
                                .shell_state
                                .add_output(format!("Invalid key hex: {e}"));
                            return;
                        }
                    };
                    let path = std::path::Path::new(&file);
                    let binary = match std::fs::read(path) {
                        Ok(b) => b,
                        Err(e) => {
                            state.shell_state.add_output(format!("Read failed: {e}"));
                            return;
                        }
                    };
                    let default_name = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "block".to_string());
                    let name = name.unwrap_or(default_name);
                    let version = version.unwrap_or_else(|| "1.0.0".to_string());
                    let mut manifest = ManifestInfo {
                        name: name.clone(),
                        version: version.clone(),
                        description: "Published from TUI shell".into(),
                        author: "local-user".into(),
                        capabilities: std::collections::HashSet::new(),
                        wasm_size_bytes: binary.len() as u64,
                        wasm_sha256: hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&binary)),
                        signature: None,
                        store_url: None,
                    };
                    let signing_key = ed25519_dalek::SigningKey::from_bytes(&secret);
                    manifest.signature = Some(sign_manifest(&manifest, &signing_key));
                    let sidecar = path.with_file_name(format!("{name}_{version}.json"));
                    match std::fs::write(
                        &sidecar,
                        serde_json::to_string_pretty(&manifest).unwrap_or_default(),
                    ) {
                        Ok(()) => state.shell_state.add_output(format!(
                            "Signed '{}' v{} (key {}) -> {}",
                            name,
                            version,
                            hex::encode(signing_key.verifying_key().to_bytes()),
                            sidecar.display()
                        )),
                        Err(e) => state.shell_state.add_output(format!("Write failed: {e}")),
                    }
                }
                "verify" => {
                    let name = parts.get(2).copied().unwrap_or("");
                    if name.is_empty() {
                        state
                            .shell_state
                            .add_output("Usage: store verify <name>".into());
                        return;
                    }
                    let block = match manager.find_installed(name) {
                        Some(b) => b,
                        None => {
                            state
                                .shell_state
                                .add_output(format!("Block '{name}' is not installed"));
                            return;
                        }
                    };
                    let binary = match std::fs::read(&block.path) {
                        Ok(b) => b,
                        Err(e) => {
                            state.shell_state.add_output(format!("Read failed: {e}"));
                            return;
                        }
                    };
                    let sha_ok = ManifestValidator::validate_sha256(&block.manifest, &binary)
                        .unwrap_or(false);
                    state.shell_state.add_output(format!(
                        "Block '{}' v{} — {} bytes",
                        block.manifest.name,
                        block.manifest.version,
                        binary.len()
                    ));
                    state.shell_state.add_output(if sha_ok {
                        "SHA-256: OK".into()
                    } else {
                        "SHA-256: MISMATCH".into()
                    });
                    match &block.manifest.signature {
                        None => state.shell_state.add_output("Signature: none".into()),
                        Some(_) => match ManifestValidator::verify_signature(&block.manifest) {
                            Ok(true) => {
                                state
                                    .shell_state
                                    .add_output("Signature: OK (Ed25519)".into());
                            }
                            Ok(false) => {
                                state.shell_state.add_output("Signature: INVALID".into());
                            }
                            Err(e) => {
                                state
                                    .shell_state
                                    .add_output(format!("Signature: error — {e}"));
                            }
                        },
                    }
                }
                _ => state.shell_state.add_output(
                    "Usage: store list | sources | search <q> [--source N] | \
                     add-source <github:owner/repo|local:path|http://url> | \
                     trust <source> [--key <public_hex>] [--clear] | \
                     install <name> [--source N] | update [name] [--source N] | \
                     uninstall <name> | rollback <name> | \
                     publish <file.wasm> [name] [version] [--key <secret_hex>] | \
                     sign <file.wasm> [name] [version] [--key <secret_hex>] | verify <name>"
                        .into(),
                ),
            }
            return;
        }
        Some("help") | Some("?") => {
            let resp = safe_shell.execute(
                aios_watchdog::safe_mode::ShellCommand::Help,
                scheduler,
                registry,
            );
            state.shell_state.add_output(resp.output);
            state.shell_state.add_output(
                "TUI extras:\n  fetch <url>     — download and load a block from a URL\n  search <query>  — DuckDuckGo web search\n  open <query|url> — open/search on the Web tab\n  clear           — clear shell output"
                    .into(),
            );
            return;
        }
        _ => aios_watchdog::safe_mode::SafeModeShell::parse_command(cmd),
    };
    let resp = safe_shell.execute(command, scheduler, registry);
    if resp.success {
        for line in resp.output.lines() {
            state.shell_state.add_output(line.to_string());
        }
    } else {
        state
            .shell_state
            .add_output(format!("Error: {}", resp.output));
    }
}

fn urlencoding(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Split trailing `store` subcommand args into `(query/name, source_name)`,
/// extracting any `--source NAME` token.
fn split_store_args(args: &[&str]) -> (String, Option<String>) {
    let mut query = String::new();
    let mut source_name = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--source" {
            source_name = args.get(i + 1).map(|s| s.to_string());
            i += 2;
        } else {
            if !query.is_empty() {
                query.push(' ');
            }
            query.push_str(args[i]);
            i += 1;
        }
    }
    (query, source_name)
}

fn switch_tab(state: &mut DashboardState, tab: usize) {
    state.selected_tab = tab;
    state.selected_row = 0;
    if tab == 5 {
        state.web_state.links_scroll = 0;
    }
    if tab == 7 {
        if let Some(fm) = state.fm.as_ref() {
            fm.send(Command::Refresh {
                side: PanelSide::Left,
            });
            fm.send(Command::Refresh {
                side: PanelSide::Right,
            });
        }
    }
    state.process_kill_result = None;
    state.block_operation_result = None;
}

fn fm_handle_action(state: &mut DashboardState, fm: &FileManager, action: TuiAction) {
    match action {
        TuiAction::MoveUp { side } => fm.move_cursor(side, -1),
        TuiAction::MoveDown { side } => fm.move_cursor(side, 1),
        TuiAction::Enter { side } => {
            if fm.selected_is_dir(side) == Some(true) {
                if let Some(path) = fm.selected(side) {
                    fm.send(Command::Navigate { side, path });
                }
            } else if fm.selected(side).is_some() {
                if let Some(path) = fm.selected(side) {
                    fm.send(Command::View { path });
                    state.add_log("FM: AI preview...".into());
                }
            }
        }
        TuiAction::GoUp { side } => {
            let parent = fm.panel_path(side).parent();
            fm.send(Command::Navigate { side, path: parent });
        }
        TuiAction::SwitchPanel => fm.switch_panel(),
        TuiAction::CopySelected => {
            let side = fm.active_side();
            if let (Some(src), Some(dst)) = (fm.selected(side), fm.default_target(side)) {
                fm.send(Command::Copy { src, dst });
                state.add_log("FM: copying...".into());
            }
        }
        TuiAction::MoveSelected => {
            let side = fm.active_side();
            if let (Some(src), Some(dst)) = (fm.selected(side), fm.default_target(side)) {
                fm.send(Command::Move { src, dst });
                state.add_log("FM: moving...".into());
            }
        }
        TuiAction::DeleteSelected => {
            let side = fm.active_side();
            if let Some(path) = fm.selected(side) {
                fm.send(Command::Delete { path });
                state.add_log("FM: deleting...".into());
            }
        }
        TuiAction::Mkdir { .. } => {
            state.fm_input_mode = dashboard::FmInputMode::Mkdir;
            state.fm_input_buffer.clear();
            state.add_log("FM: mkdir — enter directory name".into());
        }
        TuiAction::Rename { .. } => {
            let side = fm.active_side();
            if fm.selected(side).is_some() {
                state.fm_input_mode = dashboard::FmInputMode::Rename;
                state.fm_input_buffer.clear();
                state.add_log("FM: rename — enter new name".into());
            }
        }
        TuiAction::ViewSelected => {
            let side = fm.active_side();
            if let Some(path) = fm.selected(side) {
                fm.send(Command::View { path });
                state.add_log("FM: AI preview...".into());
            }
        }
        TuiAction::ToggleSort { side } => fm.toggle_sort(side),
        TuiAction::GrantHostRead => {
            fm.send(Command::GrantHostRead);
            state.add_log("FM: granted vfs:host:read".into());
        }
        TuiAction::GrantHostWrite => {
            fm.send(Command::GrantHostWrite);
            state.add_log("FM: granted vfs:host:write".into());
        }
        TuiAction::Refresh { side } => fm.send(Command::Refresh { side }),
        TuiAction::Close => {
            if state.fm_preview.is_some() {
                state.fm_preview = None;
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let data_dir = PathBuf::from(env_or("AIOS_DATA_DIR", "/app/data"));
    let blocks_dir = PathBuf::from(env_or("AIOS_BLOCKS_DIR", "/app/blocks"));
    let mock_profile = env_or("AIOS_MOCK_PROFILE", "modern");

    log::info!("AIOS: data_dir={:?}, blocks_dir={:?}", data_dir, blocks_dir);

    let _ = std::fs::create_dir_all(&data_dir);
    let _ = std::fs::create_dir_all(&blocks_dir);

    let profile = if mock_profile != "none" {
        log::info!("AIOS: using mock profile '{}'", mock_profile);
        match mock_profile.as_str() {
            "legacy" => HardwareProfile::mock_legacy(),
            _ => HardwareProfile::mock_modern(),
        }
    } else {
        HardwareProfile::detect()
    };

    let ai_tier = AiTier::from_profile(&profile);
    log::info!("AIOS: AI tier = {:?}", ai_tier);

    let mut registry = BlockRegistry::new();

    let hal_data = b"hal-native-module";
    let _ = BlockLoader::load_from_binary(&mut registry, "hal", "1.0.0", hal_data.to_vec());
    let _ = BlockLoader::load_from_binary(&mut registry, "ipc_bus", "1.0.0", b"ipc_bus".to_vec());
    let _ =
        BlockLoader::load_from_binary(&mut registry, "scheduler", "1.0.0", b"scheduler".to_vec());
    let _ = BlockLoader::load_from_binary(
        &mut registry,
        "browser",
        "0.1.0",
        b"browser-native".to_vec(),
    );

    let disk_results = BlockLoader::load_from_directory(&mut registry, &blocks_dir);
    let disk_loaded = disk_results.iter().filter(|r| r.is_ok()).count();
    let disk_failed = disk_results.iter().filter(|r| r.is_err()).count();
    log::info!(
        "AIOS: disk blocks loaded={}, failed={}",
        disk_loaded,
        disk_failed
    );

    registry.set_block_dependencies("ipc_bus", vec!["hal".into()]);
    registry.set_block_dependencies("scheduler", vec!["ipc_bus".into()]);

    let mut context_store = EmbeddedContextStore::new(10_000);
    if context_store.should_compact() {
        let report = context_store.compact();
        log::info!(
            "AIOS: auto-compact telemetry={}, workflows={}",
            report.telemetry_pruned,
            report.workflows_pruned
        );
    }

    let persistent = PersistentStore::new(data_dir.join("context.redb"));
    if persistent.is_available() {
        if let Some(version) = persistent.load_version() {
            log::info!("AIOS: recovered DB version={}", version);
        }
        if let Ok(telemetry) = persistent.load_telemetry() {
            log::info!("AIOS: recovered {} telemetry entries", telemetry.len());
            for entry in telemetry {
                context_store.telemetry_mut().record(entry);
            }
        }
    }

    let mut scheduler = Scheduler::new(profile.memory.total_mb);
    let _ = scheduler.spawn_process("ai_orchestrator", Priority::High, 512);
    let _ = scheduler.spawn_process("io_handler", Priority::Normal, 128);
    let _ = scheduler.spawn_process("health_monitor", Priority::Low, 64);

    let watchdog_config = WatchdogConfig {
        heartbeat_interval_ms: 2000,
        max_missed_heartbeats: 3,
        secret: b"aios_heartbeat_secret".to_vec(),
        ..Default::default()
    };
    let mut watchdog = Watchdog::new(watchdog_config.clone());
    watchdog
        .receive_heartbeat(&Heartbeat::new(0, &watchdog_config.secret))
        .ok();

    let watchdog_state = Arc::new(Mutex::new(watchdog.state()));
    let watchdog_state_clone = watchdog_state.clone();
    let hb_secret = watchdog_config.secret.clone();
    let hb_interval = watchdog_config.heartbeat_interval_ms;

    std::thread::spawn(move || {
        let mut seq: u64 = 1;
        loop {
            std::thread::sleep(Duration::from_millis(hb_interval / 2));
            let hb = Heartbeat::new(seq, &hb_secret);
            seq += 1;

            let state = if hb.verify(&hb_secret) {
                if seq.is_multiple_of(10) {
                    WatchdogState::Suspended
                } else {
                    WatchdogState::Monitoring
                }
            } else {
                WatchdogState::SafeMode
            };

            if let Ok(mut s) = watchdog_state_clone.lock() {
                *s = state;
            }
        }
    });

    let mut telemetry = TelemetryStore::new();
    let mut safe_shell = SafeModeShell::new(3);

    if is_headless() {
        log::info!("AIOS: headless mode — running without TUI");
        log::info!("AIOS: system initialized, entering background loop");
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut state = DashboardState::new(ai_tier, profile, &registry, &scheduler);

    let rt = tokio::runtime::Runtime::new()?;
    let fm_root = data_dir.join("vfs_sandbox");
    let (fm, fm_ack) = {
        let vfs: Arc<dyn VirtualFileSystem> = Arc::new(AiosVfs::new(fm_root.clone())?);
        rt.block_on(async move { FileManager::new(vfs, Arc::new(AclContext::new())) })
    };
    state.fm = Some(fm);
    state.fm_ack = Some(fm_ack);
    log::info!("AIOS: file manager started on {}", fm_root.display());

    if let Ok((w, _)) = crossterm::terminal::size() {
        state.web_state.wrap_width = dashboard::web_page_width(w as usize);
    }

    log::info!("AIOS: TUI started — press q to quit");

    loop {
        let wd_state = watchdog_state
            .lock()
            .map(|s| *s)
            .unwrap_or(WatchdogState::Monitoring);
        state.update_watchdog(wd_state);
        state.check_page_cache();
        state.poll_fm_acks();

        terminal.draw(|f| {
            state.update_from_scheduler(&scheduler, &registry);
            dashboard::draw_dashboard(f, &state);
        })?;

        if event::poll(std::time::Duration::from_millis(100))? {
            match event::read()? {
                Event::Resize(w, h) => {
                    let _ = h;
                    state.web_state.wrap_width = dashboard::web_page_width(w as usize);
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if key.modifiers.contains(KeyModifiers::ALT) {
                        if let KeyCode::Char(d) = key.code {
                            if let Some(d) = d.to_digit(10) {
                                if (1..=8).contains(&d) {
                                    switch_tab(&mut state, (d - 1) as usize);
                                    continue;
                                }
                            }
                        }
                    }

                    if state.show_help {
                        match key.code {
                            KeyCode::F(1) | KeyCode::Char('?') | KeyCode::Esc => {
                                state.show_help = false;
                            }
                            _ => {}
                        }
                        continue;
                    }

                    if state.selected_tab == 5 && state.web_state.input_focused {
                        match key.code {
                            KeyCode::Esc => {
                                state.web_state.input_focused = false;
                            }
                            KeyCode::Enter => {
                                let input = state.web_state.url_input.clone();
                                state.web_state.input_focused = false;
                                if !input.trim().is_empty() {
                                    navigate_web(&mut state, &input);
                                }
                            }
                            KeyCode::Char(c) => {
                                state.web_state.url_input.push(c);
                            }
                            KeyCode::Backspace => {
                                state.web_state.url_input.pop();
                            }
                            _ => {}
                        }
                        continue;
                    }

                    if state.selected_tab == 5 && state.web_state.sidebar_focused {
                        match key.code {
                            KeyCode::Esc | KeyCode::Char('\\') => {
                                state.web_state.sidebar_focused = false;
                            }
                            KeyCode::Char('j') | KeyCode::Down => {
                                web_sidebar_move(&mut state, 1);
                            }
                            KeyCode::Char('k') | KeyCode::Up => {
                                web_sidebar_move(&mut state, -1);
                            }
                            KeyCode::Enter | KeyCode::Char('o') => {
                                web_sidebar_open(&mut state);
                            }
                            _ => {}
                        }
                        continue;
                    }

                    if state.block_input_mode != dashboard::BlockInputMode::None {
                        match key.code {
                            KeyCode::Esc => {
                                state.cancel_block_input();
                            }
                            KeyCode::Enter => {
                                let step = state.confirm_block_load();
                                if let Some((label, value)) = step {
                                    if label == "__name__" {
                                        state.add_log(format!("Load: entering name '{value}'"));
                                    } else if label == "__version__" {
                                        let path = std::path::PathBuf::from(format!(
                                            "{}/{}_{}.bin",
                                            std::env::var("AIOS_BLOCKS_DIR")
                                                .unwrap_or_else(|_| "/app/blocks".into()),
                                            state
                                                .blocks
                                                .get(state.selected_row)
                                                .map(|b| b.name.clone())
                                                .unwrap_or_default(),
                                            value
                                        ));
                                        if path.exists() {
                                            match std::fs::read(&path) {
                                                Ok(binary) => {
                                                    let name = state
                                                        .blocks
                                                        .get(state.selected_row)
                                                        .map(|b| b.name.clone())
                                                        .unwrap_or_else(|| "unknown".into());
                                                    match BlockLoader::load_from_binary(
                                                        &mut registry,
                                                        &name,
                                                        &value,
                                                        binary,
                                                    ) {
                                                        Ok(manifest) => {
                                                            state.add_log(format!(
                                                                "Loaded block '{}' ({})",
                                                                manifest.name, manifest.id
                                                            ));
                                                            state.block_operation_result =
                                                                Some(format!(
                                                                    "Loaded '{}' v{}",
                                                                    name, value
                                                                ));
                                                        }
                                                        Err(e) => {
                                                            state.add_log(format!(
                                                                "Load failed: {e}"
                                                            ));
                                                            state.block_operation_result =
                                                                Some(format!("Load failed: {e}"));
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    state.add_log(format!(
                                                        "Failed to read {}: {e}",
                                                        path.display()
                                                    ));
                                                    state.block_operation_result =
                                                        Some(format!("Read failed: {e}"));
                                                }
                                            }
                                        } else {
                                            state.add_log(format!(
                                                "Block file not found: {}",
                                                path.display()
                                            ));
                                            state.block_operation_result =
                                                Some(format!("File not found: {}", path.display()));
                                        }
                                    }
                                }
                            }
                            KeyCode::Char(c) => {
                                state.push_char_to_block_input(c);
                            }
                            KeyCode::Backspace => {
                                state.pop_char_from_block_input();
                            }
                            _ => {}
                        }
                        continue;
                    }

                    if state.selected_tab == 6 {
                        match key.code {
                            KeyCode::Enter => {
                                let cmd = state.shell_state.input_buffer.trim().to_string();
                                if !cmd.is_empty() {
                                    state.shell_state.add_output(format!("$ {cmd}"));
                                    state.shell_state.push_history(cmd.clone());
                                    execute_shell_cmd(
                                        &mut state,
                                        &cmd,
                                        &mut scheduler,
                                        &mut registry,
                                        &mut safe_shell,
                                    );
                                    state.shell_state.input_buffer.clear();
                                }
                            }
                            KeyCode::Backspace => {
                                state.shell_state.input_buffer.pop();
                            }
                            KeyCode::Up => {
                                if state.shell_state.history_pos > 0 {
                                    state.shell_state.history_pos -= 1;
                                    state.shell_state.input_buffer = state
                                        .shell_state
                                        .command_history[state.shell_state.history_pos]
                                        .clone();
                                }
                            }
                            KeyCode::Down => {
                                let len = state.shell_state.command_history.len();
                                if state.shell_state.history_pos < len {
                                    state.shell_state.history_pos += 1;
                                    state.shell_state.input_buffer =
                                        if state.shell_state.history_pos < len {
                                            state.shell_state.command_history
                                                [state.shell_state.history_pos]
                                                .clone()
                                        } else {
                                            String::new()
                                        };
                                }
                            }
                            KeyCode::Esc => {
                                state.shell_state.input_buffer.clear();
                            }
                            KeyCode::Char(c) => {
                                state.shell_state.input_buffer.push(c);
                            }
                            _ => {}
                        }
                        continue;
                    }

                    if state.selected_tab == 7 {
                        if let Some(fm) = state.fm.clone() {
                            if state.fm_input_mode != dashboard::FmInputMode::None {
                                match key.code {
                                    KeyCode::Esc => {
                                        state.fm_input_mode = dashboard::FmInputMode::None;
                                        state.fm_input_buffer.clear();
                                    }
                                    KeyCode::Enter => {
                                        state.fm_confirm_input();
                                    }
                                    KeyCode::Char(c) => {
                                        state.fm_input_buffer.push(c);
                                    }
                                    KeyCode::Backspace => {
                                        state.fm_input_buffer.pop();
                                    }
                                    _ => {}
                                }
                                continue;
                            }
                            if key.code == KeyCode::Esc && state.fm_preview.is_some() {
                                state.fm_preview = None;
                                continue;
                            }
                            let side = fm.active_side();
                            if let Some(action) = key_to_action(key, side) {
                                fm_handle_action(&mut state, &fm, action);
                            }
                        }
                        continue;
                    }

                    match key.code {
                        KeyCode::F(1) | KeyCode::Char('?') => {
                            state.show_help = true;
                        }
                        KeyCode::Char('q') => break,
                        KeyCode::Esc => {
                            if state.show_help {
                                state.show_help = false;
                            } else {
                                break;
                            }
                        }
                        KeyCode::Char('1') => switch_tab(&mut state, 0),
                        KeyCode::Char('2') => switch_tab(&mut state, 1),
                        KeyCode::Char('3') => switch_tab(&mut state, 2),
                        KeyCode::Char('4') => switch_tab(&mut state, 3),
                        KeyCode::Char('5') => switch_tab(&mut state, 4),
                        KeyCode::Char('6') => switch_tab(&mut state, 5),
                        KeyCode::Char('7') => switch_tab(&mut state, 6),
                        KeyCode::Char('8') => switch_tab(&mut state, 7),
                        KeyCode::Char('j') | KeyCode::Down => {
                            state.move_selection_down();
                            state.process_kill_result = None;
                            state.block_operation_result = None;
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            state.move_selection_up();
                            state.process_kill_result = None;
                            state.block_operation_result = None;
                        }
                        KeyCode::Char('K') => {
                            if state.selected_tab == 1 {
                                if let Some(pid) = state.selected_process_pid() {
                                    use aios_process_mgr::task::ProcessId;
                                    let result = scheduler.kill_process(ProcessId(pid));
                                    match result {
                                        Ok(proc) => {
                                            state.add_log(format!(
                                                "Killed process '{}' (PID {})",
                                                proc.name, pid
                                            ));
                                            state.process_kill_result = Some(format!(
                                                "Killed '{}' (PID {})",
                                                proc.name, pid
                                            ));
                                        }
                                        Err(e) => {
                                            state.add_log(format!("Kill failed: {e}"));
                                            state.process_kill_result =
                                                Some(format!("Kill failed: {e}"));
                                        }
                                    }
                                }
                            }
                        }
                        KeyCode::Char('U') => {
                            if state.selected_tab == 2 {
                                if let Some((name, version)) = state.selected_block_name_version() {
                                    let selected_id =
                                        state.blocks.get(state.selected_row).map(|b| BlockId(b.id));
                                    if let Some(id) = selected_id {
                                        match registry.unload_block(id) {
                                            Ok(entry) => {
                                                state.add_log(format!(
                                                    "Unloaded block '{}' ({})",
                                                    entry.manifest.name, id
                                                ));
                                                state.block_operation_result =
                                                    Some(format!("Unloaded '{name}@{version}'"));
                                            }
                                            Err(e) => {
                                                state.add_log(format!("Unload failed: {e}"));
                                                state.block_operation_result =
                                                    Some(format!("Unload failed: {e}"));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        KeyCode::Char('L') => {
                            if state.selected_tab == 2 {
                                state.start_load_block();
                                state.add_log("Loading block: enter name...".into());
                            }
                        }
                        KeyCode::Char('H') => {
                            if state.selected_tab == 2 {
                                if let Some((name, version)) = state.selected_block_name_version() {
                                    let selected_id =
                                        state.blocks.get(state.selected_row).map(|b| BlockId(b.id));
                                    let path = std::path::PathBuf::from(format!(
                                        "{}/{}_{}.bin",
                                        std::env::var("AIOS_BLOCKS_DIR")
                                            .unwrap_or_else(|_| "/app/blocks".into()),
                                        name,
                                        version
                                    ));
                                    if path.exists() {
                                        if let Some(id) = selected_id {
                                            let _ = registry.unload_block(id);
                                        }
                                        match std::fs::read(&path) {
                                            Ok(binary) => {
                                                match BlockLoader::load_from_binary(
                                                    &mut registry,
                                                    &name,
                                                    &version,
                                                    binary,
                                                ) {
                                                    Ok(manifest) => {
                                                        state.add_log(format!(
                                                            "Hot-swap: reloaded '{}' ({})",
                                                            manifest.name, manifest.id
                                                        ));
                                                        state.block_operation_result =
                                                            Some(format!(
                                                                "Hot-swap OK: '{name}@{version}'"
                                                            ));
                                                    }
                                                    Err(e) => {
                                                        state.block_operation_result =
                                                            Some(format!("Hot-swap failed: {e}"));
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                state
                                                    .add_log(format!("Hot-swap: read failed: {e}"));
                                                state.block_operation_result =
                                                    Some(format!("Hot-swap read failed: {e}"));
                                            }
                                        }
                                    } else {
                                        state.add_log(format!(
                                            "Hot-swap: binary not found at {}",
                                            path.display()
                                        ));
                                        state.block_operation_result =
                                            Some(format!("Binary not found: {}", path.display()));
                                    }
                                }
                            }
                        }
                        KeyCode::Char('g') => {
                            if state.selected_tab == 5 {
                                state.web_state.input_focused = true;
                                state.add_log(
                                    "Omnibox focused — type a search query or URL, Enter to go"
                                        .into(),
                                );
                            }
                        }
                        KeyCode::Char('o') | KeyCode::Enter => {
                            if state.selected_tab == 5 {
                                open_selected_link(&mut state);
                            }
                        }
                        KeyCode::Char('b') => {
                            if state.selected_tab == 5 {
                                web_go_back(&mut state);
                            }
                        }
                        KeyCode::Char('B') => {
                            if state.selected_tab == 5 {
                                web_open_native(&mut state, None);
                            }
                        }
                        KeyCode::Char('n') => {
                            if state.selected_tab == 5 {
                                if let Some(href) = state.web_state.page.as_ref().and_then(|p| {
                                    p.links
                                        .get(state.selected_row)
                                        .map(|(_text, href)| href.clone())
                                }) {
                                    web_open_native(&mut state, Some(href));
                                }
                            }
                        }
                        KeyCode::Char('\\') => {
                            if state.selected_tab == 5 && !state.web_state.input_focused {
                                state.web_state.sidebar_focused = !state.web_state.sidebar_focused;
                                state.web_state.history_sel = 0;
                            }
                        }
                        KeyCode::Char('u') => {
                            if state.selected_tab == 5 {
                                web_scroll(&mut state, -1);
                            }
                        }
                        KeyCode::Char('d') => {
                            if state.selected_tab == 5 {
                                web_scroll(&mut state, 1);
                            }
                        }
                        KeyCode::PageUp => {
                            if state.selected_tab == 5 {
                                web_scroll(&mut state, -20);
                            }
                        }
                        KeyCode::PageDown => {
                            if state.selected_tab == 5 {
                                web_scroll(&mut state, 20);
                            }
                        }
                        KeyCode::Char('r') => {
                            state.add_log("System refreshed".into());
                        }
                        KeyCode::Char('W') => match aios_webview::launcher::launch_gui() {
                            Ok(path) => {
                                state.add_log(format!("GUI dashboard launched: {}", path.display()))
                            }
                            Err(e) => state.add_log(format!("GUI launch failed: {e}")),
                        },
                        KeyCode::Char('s') => {
                            let entry = TelemetryEntry::new(
                                "process_count",
                                scheduler.process_count() as f64,
                                scheduler.ram_usage().0,
                            )
                            .with_process("scheduler");
                            telemetry.record(entry);
                            let avg = telemetry.average_value("process_count").unwrap_or(0.0);
                            context_store.telemetry_mut().record(
                                TelemetryEntry::new("process_count", avg, 0)
                                    .with_process("scheduler"),
                            );
                            state.add_log(format!(
                                "Telemetry: {} proc, avg={:.1}",
                                scheduler.process_count(),
                                avg,
                            ));
                        }
                        KeyCode::Char('x') => {
                            let resp = safe_shell.execute(
                                aios_watchdog::safe_mode::ShellCommand::SystemStatus,
                                &mut scheduler,
                                &mut registry,
                            );
                            state.add_log(format!("Status: {}", resp.output));
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    log::info!("AIOS: saving state before shutdown...");

    let telemetry_entries: Vec<TelemetryEntry> = context_store.telemetry().entries.to_vec();
    if !telemetry_entries.is_empty() {
        match persistent.save_telemetry(&telemetry_entries) {
            Ok(n) => log::info!("AIOS: persisted {} telemetry entries", n),
            Err(e) => log::error!("AIOS: failed to persist telemetry: {}", e),
        }
    }
    match persistent.save_version("1.0.0") {
        Ok(_) => log::info!("AIOS: saved DB version"),
        Err(e) => log::error!("AIOS: failed to save version: {}", e),
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    log::info!("AIOS: shutdown complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{is_url_input, web_current_page_url, web_sidebar_move};
    use aios_tui::dashboard::DashboardState;

    #[test]
    fn test_is_url_input_scheme_urls() {
        assert!(is_url_input("https://example.com"));
        assert!(is_url_input("http://localhost:8080"));
    }

    #[test]
    fn test_is_url_input_bare_host() {
        assert!(is_url_input("example.com"));
        assert!(is_url_input("duckduckgo.com"));
    }

    #[test]
    fn test_is_url_input_plain_query() {
        assert!(!is_url_input("how does AIOS work"));
        assert!(!is_url_input("  rust scheduler tutorial "));
        assert!(!is_url_input(""));
    }

    #[test]
    fn test_is_url_input_whitespace_padded_host() {
        assert!(is_url_input("  example.com  "));
    }

    fn make_dashboard_state() -> DashboardState {
        use aios_block_mgr::registry::BlockRegistry;
        use aios_hal::ai_tier::AiTier;
        use aios_hal::hardware::HardwareProfile;
        use aios_process_mgr::scheduler::Scheduler;
        let profile = HardwareProfile::mock_modern();
        let mut reg = BlockRegistry::new();
        reg.register_block("test", "0.1.0", b"t".to_vec()).unwrap();
        let sched = Scheduler::new(65536);
        DashboardState::new(AiTier::Tier1, profile, &reg, &sched)
    }

    #[test]
    fn test_web_sidebar_move_wraps_around() {
        let mut state = make_dashboard_state();
        state.web_state.current_url = "https://b".into();
        state.web_state.history = vec!["https://a".into()];
        assert_eq!(state.web_state.history_sel, 0);
        web_sidebar_move(&mut state, 1);
        assert_eq!(state.web_state.history_sel, 1);
        web_sidebar_move(&mut state, 1);
        assert_eq!(state.web_state.history_sel, 0);
        web_sidebar_move(&mut state, -1);
        assert_eq!(state.web_state.history_sel, 1);
    }

    #[test]
    fn test_web_current_page_url_none_when_no_page() {
        let state = make_dashboard_state();
        assert_eq!(web_current_page_url(&state), None);
    }

    #[test]
    fn test_web_current_page_url_returns_page_url() {
        let mut state = make_dashboard_state();
        state.web_state.page = Some(aios_tui::dashboard::PageContent {
            url: "https://x".into(),
            title: "X".into(),
            text: "t".into(),
            links: vec![],
        });
        assert_eq!(web_current_page_url(&state), Some("https://x".into()));
    }
}
