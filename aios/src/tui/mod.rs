mod app_state;
mod ui;

pub use app_state::TuiApp;
pub use ui::draw;

use self::app_state::{AiMessage, WebBookmark, WebTab};

use crate::orchestrator::{push_log, OrchestratorState};
use aios_block_mgr::loader::BlockLoader;
use aios_browser::engine::BrowserEngine;
use aios_browser::types::BrowserConfig;
use aios_cluster::scheduler::DistributedScheduler;
use aios_cluster::types::{RemoteProcessId, RemoteProcessSpec};
use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};
use aios_llm::{provider_name, BackendKind, CloudProvider, LlmConfig, LlmEngine};
use aios_process_mgr::task::ProcessId;
use aios_store::manager::StoreManager;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::collections::{BTreeMap, VecDeque};
use std::io::stdout;
use std::path::PathBuf;
#[cfg(feature = "webview")]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
#[cfg(feature = "webview")]
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const DESKTOP_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

pub fn run_tui(state: Arc<Mutex<OrchestratorState>>) -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let mut app = TuiApp::new(state.clone());
    load_presets(&mut app);
    load_chat(&app);
    load_bookmarks(&mut app);

    let res = run(&mut terminal, app);

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    mut app: TuiApp,
) -> Result<(), Box<dyn std::error::Error>> {
    while app.running {
        terminal.draw(|f| draw(f, &mut app))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    handle_key(&mut app, key);
                }
            }
        }

        app.update_logs();
        web_poll(&mut app);
        app.hw_poll_hotplug();
        app.hw_refresh();
    }
    save_chat(&app);
    save_bookmarks(&app);
    Ok(())
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn blocks_dir() -> PathBuf {
    env_or("AIOS_BLOCKS_DIR", "blocks").into()
}

fn store_manager() -> StoreManager {
    StoreManager::new(blocks_dir())
}

fn dispatch_net_get(app: &mut TuiApp) -> Option<String> {
    let result = {
        let mut state = app.state.lock().unwrap();
        let packet = IpcPacket::new(
            0,
            state.net_block_id.0,
            CommandId::Custom,
            Payload::Custom("net_get".into(), Vec::new()),
        );
        state.router.dispatch(&packet)
    };
    match result {
        Ok(Some(resp)) => match resp.payload {
            Payload::Text(text) => Some(text),
            _ => Some("net settings block: OK".into()),
        },
        Ok(None) => Some("net: no response".into()),
        Err(e) => Some(format!("net ERROR: {e}")),
    }
}

fn dispatch_net_set(app: &mut TuiApp, raw: &str) -> String {
    let mut updates = serde_json::Map::new();
    for token in raw.split_whitespace() {
        let (k, v) = match token.split_once('=') {
            Some(kv) => kv,
            None => {
                return format!("AIOS: net: bad token '{token}' (use key=value)");
            }
        };
        let value = serde_json::from_str::<serde_json::Value>(v)
            .unwrap_or_else(|_| serde_json::Value::String(v.to_string()));
        updates.insert(k.to_string(), value);
    }
    if updates.is_empty() {
        return "AIOS: net: usage: key=value ...".into();
    }
    let body = serde_json::Value::Object(updates).to_string();
    let result = {
        let mut state = app.state.lock().unwrap();
        let packet = IpcPacket::new(
            0,
            state.net_block_id.0,
            CommandId::Custom,
            Payload::Custom("net_set".into(), body.into_bytes()),
        );
        state.router.dispatch(&packet)
    };
    match result {
        Ok(Some(resp)) => match resp.payload {
            Payload::Text(text) => text,
            _ => "net settings block: OK".to_string(),
        },
        Ok(None) => "AIOS: net: no response".into(),
        Err(e) => format!("AIOS: net ERROR: {e}"),
    }
}

fn push_out(ai_out: &Arc<Mutex<VecDeque<String>>>, line: String) {
    let mut ai_guard = ai_out.lock().unwrap();
    ai_guard.push_back(line);
    let len = ai_guard.len();
    if len > 200 {
        ai_guard.drain(0..len - 150);
    }
}

fn push_ai_line(app: &TuiApp, line: String) {
    push_out(&app.ai_output, line);
}

/// Path of the persisted chat log (JSON Lines under AIOS_DATA_DIR).
fn chat_path() -> PathBuf {
    PathBuf::from(env_or("AIOS_DATA_DIR", "aios_data")).join("chat.jsonl")
}

/// Path of the persisted prompt templates (JSON object under AIOS_DATA_DIR).
fn presets_path() -> PathBuf {
    PathBuf::from(env_or("AIOS_DATA_DIR", "aios_data")).join("presets.json")
}

/// Writes the preset map as a JSON object; missing parent dirs are created.
fn save_presets(app: &TuiApp) {
    let path = presets_path();
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(json) = serde_json::to_string_pretty(&app.ai_presets) else {
        return;
    };
    let _ = std::fs::write(path, json);
}

/// Overlays persisted presets over the built-in seeds at boot.
fn load_presets(app: &mut TuiApp) {
    let Ok(content) = std::fs::read_to_string(presets_path()) else {
        return;
    };
    let Ok(saved) = serde_json::from_str::<BTreeMap<String, String>>(&content) else {
        return;
    };
    for (name, text) in saved {
        if !name.trim().is_empty() && !text.trim().is_empty() {
            app.ai_presets.insert(name, text);
        }
    }
}

/// Path of the persisted Web tab bookmarks (JSON array under AIOS_DATA_DIR).
fn bookmarks_path() -> PathBuf {
    PathBuf::from(env_or("AIOS_DATA_DIR", "aios_data")).join("web_bookmarks.json")
}

/// Writes the bookmark list as a JSON array; missing parent dirs are created.
fn save_bookmarks(app: &TuiApp) {
    let path = bookmarks_path();
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(json) = serde_json::to_string_pretty(&app.web.bookmarks) else {
        return;
    };
    let _ = std::fs::write(path, json);
}

/// Restores previously saved Web tab bookmarks at boot.
fn load_bookmarks(app: &mut TuiApp) {
    let Ok(content) = std::fs::read_to_string(bookmarks_path()) else {
        return;
    };
    let Ok(saved) = serde_json::from_str::<Vec<WebBookmark>>(&content) else {
        return;
    };
    app.web.bookmarks = saved;
    app.web.bookmarks_sel = 0;
}

fn save_chat_to(path: &std::path::Path, ai_log: &Arc<Mutex<Vec<AiMessage>>>) {
    let Ok(guard) = ai_log.lock() else {
        return;
    };
    let mut buf = String::new();
    for msg in guard.iter() {
        if let Ok(line) = serde_json::to_string(msg) {
            buf.push_str(&line);
            buf.push('\n');
        }
    }
    drop(guard);
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let _ = std::fs::write(path, buf);
}

fn save_chat(app: &TuiApp) {
    save_chat_to(&chat_path(), &app.ai_log);
}

/// Restores a previously saved chat into the AI Console output at boot.
fn load_chat(app: &TuiApp) {
    let path = chat_path();
    let Ok(content) = std::fs::read_to_string(&path) else {
        return;
    };
    let mut messages: Vec<AiMessage> = Vec::new();
    for line in content.lines() {
        if let Ok(msg) = serde_json::from_str::<AiMessage>(line) {
            messages.push(msg);
        }
    }
    if messages.is_empty() {
        return;
    }
    if let Ok(mut guard) = app.ai_log.lock() {
        guard.extend(messages.clone());
    }
    for msg in messages {
        if msg.role == "user" {
            push_ai_line(app, format!("> {}", msg.text));
        } else {
            push_ai_line(app, msg.text);
        }
    }
    *app.ai_status.lock().unwrap() = "chat restored from disk".into();
}

fn apply_config_async(app: &TuiApp, config: LlmConfig) {
    let state = app.state.lock().unwrap();
    let bridge = state.bridge.clone();
    drop(state);
    tokio::spawn(async move {
        let mut eng = bridge.llm.lock().await;
        *eng = LlmEngine::from_config(config);
    });
}

fn submit_ai_query(app: &mut TuiApp, prompt: String) {
    let system = app.ai_system_prompt.clone();
    let config = app.ai_config.clone();
    let state = app.state.lock().unwrap();
    let logs = app.logs.clone();
    let ai_out = app.ai_output.clone();
    let status = app.ai_status.clone();
    let stream = app.ai_stream.clone();
    let streaming = app.ai_streaming.clone();
    let ai_log = app.ai_log.clone();
    let path = chat_path();
    let bridge = state.bridge.clone();
    drop(state);
    if let Ok(mut guard) = ai_log.lock() {
        guard.push(AiMessage {
            role: "user".into(),
            text: prompt.clone(),
        });
    }
    *status.lock().unwrap() = "streaming...".into();
    *streaming.lock().unwrap() = true;
    *stream.lock().unwrap() = String::new();
    tokio::spawn(async move {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let worker = {
            let mut eng = bridge.llm.lock().await;
            let engine = std::mem::replace(
                &mut *eng,
                LlmEngine::from_config(aios_llm::default_config()),
            );
            let req = aios_llm::LlmRequest {
                system_prompt: system,
                user_prompt: prompt.clone(),
                max_tokens: config.max_tokens,
                temperature: config.temperature,
            };
            tokio::spawn(async move { engine.query_stream(&req, tx).await })
        };
        let start = std::time::Instant::now();
        let mut full = String::new();
        while let Some(item) = rx.recv().await {
            match item {
                Ok(delta) => {
                    full.push_str(&delta);
                    if let Ok(mut s) = stream.lock() {
                        s.push_str(&delta);
                    }
                }
                Err(e) => {
                    *streaming.lock().unwrap() = false;
                    *status.lock().unwrap() = format!("error: {e}");
                    push_out(&ai_out, format!("[error] {e}"));
                    save_chat_to(&path, &ai_log);
                    let mut l = logs.lock().unwrap();
                    l.push(format!("AI query: {} (error: {e})", prompt));
                    return;
                }
            }
        }
        let _ = worker.await;
        *streaming.lock().unwrap() = false;
        let tail = {
            let mut s = stream.lock().unwrap();
            let tail = s.clone();
            s.clear();
            tail
        };
        *status.lock().unwrap() = format!("done: {} ms", start.elapsed().as_millis());
        if !tail.trim().is_empty() {
            push_out(&ai_out, tail.clone());
            if let Ok(mut guard) = ai_log.lock() {
                guard.push(AiMessage {
                    role: "assistant".into(),
                    text: tail,
                });
            }
        }
        save_chat_to(&path, &ai_log);
        let mut l = logs.lock().unwrap();
        l.push(format!("AI query: {} (done, {} chars)", prompt, full.len()));
    });
}

fn handle_ai_command(app: &mut TuiApp, cmd: &str) {
    let trimmed = cmd.trim();
    let (name, rest) = match trimmed.split_once(char::is_whitespace) {
        Some((n, r)) => (n, r.trim()),
        None => (trimmed, ""),
    };
    let mut replies: Vec<String> = Vec::new();
    match name {
        "help" | "?" => {
            app.ai_show_help = true;
            replies.push("Help panel opened — press h or Esc to close".into());
            replies.push(
                "Commands: /help /status /clear /history /system /model /backend /key /temp \
                 /tokens /preset /save /load"
                    .into(),
            );
        }
        "clear" => {
            if let Ok(mut g) = app.ai_output.lock() {
                g.clear();
            }
            *app.ai_status.lock().unwrap() = "cleared".into();
            return;
        }
        "status" => {
            let cfg = app.ai_config.clone();
            let backend = match cfg.backend {
                BackendKind::Cloud(ref p) => format!("cloud/{}", provider_name(p)),
                BackendKind::MicroLocal => "local/micro".into(),
                BackendKind::FullLocal => "local/full".into(),
            };
            let key_state = if cfg.api_key.as_deref().is_some_and(|k| !k.is_empty()) {
                "set"
            } else {
                "NOT set"
            };
            replies.push(format!("Backend : {backend}"));
            replies.push(format!("Model   : {}", cfg.model));
            replies.push(format!("API key : {key_state}"));
            replies.push(format!("Temp    : {}", cfg.temperature));
            replies.push(format!("Tokens  : {}", cfg.max_tokens));
            replies.push(format!("System  : {}", app.ai_system_prompt));
            let models = aios_llm::local::detect_local_models();
            if models.is_empty() {
                replies.push("Local GGUF models: none found (check AIOS_MODELS_DIR)".into());
            } else {
                replies.push(format!("Local GGUF models: {}", models.join(", ")));
            }
        }
        "system" => {
            if rest.is_empty() {
                replies.push(format!("System prompt: {}", app.ai_system_prompt));
            } else {
                app.ai_system_prompt = rest.to_string();
                replies.push("System prompt updated.".into());
            }
        }
        "model" => {
            if rest.is_empty() {
                replies.push(format!("Model: {}", app.ai_config.model));
            } else {
                app.ai_config.model = rest.to_string();
                apply_config_async(app, app.ai_config.clone());
                replies.push(format!("Model set: {rest}"));
            }
        }
        "backend" => {
            let target = rest.to_ascii_lowercase();
            let mut cfg = app.ai_config.clone();
            let mut label = String::new();
            match target.as_str() {
                "groq" => {
                    cfg.backend = BackendKind::Cloud(CloudProvider::Groq);
                    cfg.model = CloudProvider::Groq.default_model().into();
                    label = "cloud/groq".into();
                }
                "openrouter" => {
                    cfg.backend = BackendKind::Cloud(CloudProvider::OpenRouter);
                    cfg.model = CloudProvider::OpenRouter.default_model().into();
                    label = "cloud/openrouter".into();
                }
                "google" | "googleai" | "google-ai-studio" => {
                    cfg.backend = BackendKind::Cloud(CloudProvider::GoogleAiStudio);
                    cfg.model = CloudProvider::GoogleAiStudio.default_model().into();
                    label = "cloud/google".into();
                }
                "micro" | "micro-local" => {
                    cfg.backend = BackendKind::MicroLocal;
                    cfg.model = "qwen2.5-0.5b".into();
                    cfg.api_key = None;
                    label = "local/micro".into();
                }
                "full" | "full-local" => {
                    cfg.backend = BackendKind::FullLocal;
                    cfg.model = "qwen2.5-7b".into();
                    cfg.api_key = None;
                    label = "local/full".into();
                }
                _ => {
                    replies.push("Usage: /backend <groq|openrouter|google|micro|full>".into());
                    replies.push("Local backends need a GGUF model (AIOS_MODEL_PATH).".into());
                }
            }
            if !label.is_empty() {
                app.ai_config = cfg.clone();
                apply_config_async(app, cfg);
                replies.push(format!("Backend switched: {label}"));
            }
        }
        "key" => {
            if rest.is_empty() {
                app.ai_config.api_key = None;
                replies.push("API key cleared.".into());
            } else {
                app.ai_config.api_key = Some(rest.to_string());
                replies.push("API key set.".into());
            }
            apply_config_async(app, app.ai_config.clone());
        }
        "temp" => match rest.parse::<f32>() {
            Ok(v) if (0.0..=2.0).contains(&v) => {
                app.ai_config.temperature = v;
                replies.push(format!("Temperature set: {v}"));
            }
            _ => replies.push("Usage: /temp <0.0-2.0>".into()),
        },
        "tokens" => match rest.parse::<u32>() {
            Ok(v) if (1..=8192).contains(&v) => {
                app.ai_config.max_tokens = v;
                replies.push(format!("Max tokens set: {v}"));
            }
            _ => replies.push("Usage: /tokens <1-8192>".into()),
        },
        "history" => {
            if app.ai_history.is_empty() {
                replies.push("History is empty.".into());
            } else {
                for (i, h) in app.ai_history.iter().enumerate() {
                    replies.push(format!("{:>3}: {}", i + 1, h));
                }
            }
        }
        "preset" => {
            let (pname, ptext) = match rest.split_once(char::is_whitespace) {
                Some((n, t)) => (n, t.trim()),
                None => (rest, ""),
            };
            if pname == "list" || pname.is_empty() {
                if app.ai_presets.is_empty() {
                    replies.push("No presets defined.".into());
                } else {
                    replies.push(format!("Presets ({}):", app.ai_presets.len()));
                    for (name, text) in app.ai_presets.iter() {
                        let preview: String = text.chars().take(60).collect();
                        replies.push(format!("  /preset {name}  —  {preview}"));
                    }
                }
            } else if pname == "del" && !ptext.is_empty() {
                if app.ai_presets.remove(ptext).is_some() {
                    save_presets(app);
                    replies.push(format!("Preset '{ptext}' deleted."));
                } else {
                    replies.push(format!("Preset '{ptext}' not found."));
                }
            } else if !ptext.is_empty() {
                app.ai_presets.insert(pname.to_string(), ptext.to_string());
                save_presets(app);
                replies.push(format!("Preset '{pname}' saved."));
            } else if let Some(text) = app.ai_presets.get(pname) {
                app.ai_system_prompt = text.clone();
                replies.push(format!("Preset '{pname}' applied as system prompt."));
            } else {
                replies.push(format!(
                    "Preset '{pname}' not found. Define it: /preset {pname} <text>"
                ));
            }
        }
        "save" => {
            save_chat(app);
            replies.push(format!("Chat saved to {}", chat_path().display()));
        }
        "load" => {
            let path = chat_path();
            let Ok(content) = std::fs::read_to_string(&path) else {
                replies.push(format!("No saved chat found at {}", path.display()));
                return;
            };
            let mut messages: Vec<AiMessage> = Vec::new();
            for line in content.lines() {
                if let Ok(msg) = serde_json::from_str::<AiMessage>(line) {
                    messages.push(msg);
                }
            }
            if let Ok(mut guard) = app.ai_log.lock() {
                guard.clear();
                guard.extend(messages.clone());
            }
            if let Ok(mut output) = app.ai_output.lock() {
                output.clear();
            }
            for msg in messages {
                if msg.role == "user" {
                    push_ai_line(app, format!("> {}", msg.text));
                } else {
                    push_ai_line(app, msg.text);
                }
            }
            replies.push(format!("Chat restored from {}", path.display()));
        }
        _ => {
            replies.push(format!("Unknown command '/{name}'. Type /help for usage."));
        }
    }
    for r in replies {
        push_ai_line(app, r);
    }
}

fn is_url_input(s: &str) -> bool {
    let s = s.trim();
    s.starts_with("http://")
        || s.starts_with("https://")
        || (s.contains('.') && !s.contains(|c: char| c.is_whitespace()))
}

fn web_sync_tab(app: &mut TuiApp) {
    if let Some(tab) = app.web.tabs.get_mut(app.web.active_tab) {
        tab.url = app.web.current_url.clone();
        tab.page = app.web.page.clone();
        tab.scroll = app.web.scroll;
        tab.selected_link = app.web.selected_link;
        tab.history.clone_from(&app.web.history);
        tab.error.clone_from(&app.web.error);
    }
}

fn web_load_tab(app: &mut TuiApp, idx: usize) {
    if let Some(tab) = app.web.tabs.get(idx) {
        app.web.current_url = tab.url.clone();
        app.web.page = tab.page.clone();
        app.web.scroll = tab.scroll;
        app.web.selected_link = tab.selected_link;
        app.web.history.clone_from(&tab.history);
        app.web.error.clone_from(&tab.error);
        app.web.loading = false;
        app.web.history_sel = 0;
    }
}

fn web_new_tab(app: &mut TuiApp) {
    web_sync_tab(app);
    app.web.tabs.push(WebTab::default());
    app.web.active_tab = app.web.tabs.len() - 1;
    app.web.current_url.clear();
    app.web.page = None;
    app.web.loading = false;
    app.web.error = None;
    app.web.scroll = 0;
    app.web.selected_link = 0;
    app.web.history.clear();
    push_log(&app.logs, "AIOS: web: opened a new tab".into());
}

fn web_close_tab(app: &mut TuiApp) {
    if app.web.tabs.len() <= 1 {
        push_log(&app.logs, "AIOS: web: cannot close the last tab".into());
        return;
    }
    web_sync_tab(app);
    app.web.tabs.remove(app.web.active_tab);
    if app.web.active_tab >= app.web.tabs.len() {
        app.web.active_tab = app.web.tabs.len() - 1;
    }
    web_load_tab(app, app.web.active_tab);
    push_log(
        &app.logs,
        format!("AIOS: web: closed tab, {} left", app.web.tabs.len()),
    );
}

fn web_switch_tab(app: &mut TuiApp, dir: isize) {
    if app.web.tabs.len() < 2 {
        return;
    }
    web_sync_tab(app);
    let len = app.web.tabs.len() as isize;
    let next = (app.web.active_tab as isize + dir).rem_euclid(len) as usize;
    app.web.active_tab = next;
    web_load_tab(app, next);
}

fn web_load(app: &mut TuiApp, url: &str, push_history: bool) {
    let url = url.trim().to_string();
    if url.is_empty() {
        return;
    }
    web_sync_tab(app);
    let tab = app.web.active_tab;
    let prev = app.web.current_url.clone();
    if push_history && !prev.is_empty() && prev != url && !app.web.history.iter().any(|h| h == &url)
    {
        app.web.history.push(prev);
    }
    app.web.loading = true;
    app.web.page = None;
    app.web.error = None;
    app.web.scroll = 0;
    app.web.input_focused = false;
    app.web.fetch_gen += 1;
    let gen = app.web.fetch_gen;
    let out = app.web.fetch_out.clone();
    let logs = app.logs.clone();
    let target = url.clone();
    web_sync_tab(app);
    push_log(&logs, format!("AIOS: web: navigating to {url}"));
    let handle = match tokio::runtime::Handle::try_current() {
        Ok(h) => h,
        Err(_) => {
            app.web.loading = false;
            app.web.error = Some("no tokio runtime for web fetch".into());
            return;
        }
    };
    handle.spawn(async move {
        let engine = BrowserEngine::new(BrowserConfig {
            user_agent: DESKTOP_UA.into(),
            timeout_secs: 15,
            ..BrowserConfig::default()
        });
        let result = engine.navigate(&target).await.map_err(|e| e.to_string());
        if let Ok(mut slot) = out.lock() {
            *slot = Some((gen, tab, result));
        }
    });
}

fn web_navigate(app: &mut TuiApp, raw: &str) {
    let raw = raw.trim();
    if raw.is_empty() {
        return;
    }
    let url = if is_url_input(raw) {
        if raw.starts_with("http://") || raw.starts_with("https://") {
            raw.to_string()
        } else {
            format!("https://{raw}")
        }
    } else {
        app.web.search_query = raw.to_string();
        format!(
            "https://html.duckduckgo.com/html/?q={}",
            url::form_urlencoded::byte_serialize(raw.as_bytes()).collect::<String>()
        )
    };
    app.web.url_input.clear();
    web_load(app, &url, true);
}

fn web_poll(app: &mut TuiApp) {
    let got = {
        let mut slot = app.web.fetch_out.lock().unwrap();
        slot.take()
    };
    if let Some((gen, tab_idx, result)) = got {
        if gen != app.web.fetch_gen {
            return;
        }
        app.web.loading = false;
        if let Some(tab) = app.web.tabs.get_mut(tab_idx) {
            match result {
                Ok(page) => {
                    let url = page.url.clone();
                    let is_active = tab_idx == app.web.active_tab;
                    tab.url = url.clone();
                    tab.page = Some(page);
                    tab.scroll = 0;
                    tab.selected_link = 0;
                    tab.error = None;
                    if is_active {
                        app.web.page = tab.page.clone();
                        app.web.current_url = url;
                        app.web.history_sel = 0;
                    }
                }
                Err(e) => {
                    tab.error = Some(e.clone());
                    if tab_idx == app.web.active_tab {
                        app.web.error = Some(e);
                    }
                }
            }
        }
    }
}

fn web_back(app: &mut TuiApp) {
    if let Some(prev) = app.web.history.pop() {
        web_load(app, &prev, false);
    } else {
        push_log(&app.logs, "AIOS: web: no history to go back to".into());
    }
}

fn web_scroll(app: &mut TuiApp, dir: isize) {
    let max = app
        .web
        .page
        .as_ref()
        .map(|p| {
            wrap_text(&p.text_content, app.web.wrap_width)
                .len()
                .saturating_sub(2)
        })
        .unwrap_or(0);
    let next = app.web.scroll as isize + dir;
    app.web.scroll = next.clamp(0, max as isize) as usize;
}

fn web_link_move(app: &mut TuiApp, dir: isize) {
    let len = app.web.page.as_ref().map(|p| p.links.len()).unwrap_or(0);
    if len == 0 {
        app.web.selected_link = 0;
        return;
    }
    let next = app.web.selected_link as isize + dir;
    app.web.selected_link = next.rem_euclid(len as isize) as usize;
}

fn web_open_selected(app: &mut TuiApp) {
    let href = app
        .web
        .page
        .as_ref()
        .and_then(|p| p.links.get(app.web.selected_link))
        .map(|l| l.href.clone());
    if let Some(href) = href {
        web_load(app, &href, true);
    }
}

pub(crate) fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for raw in text.lines() {
        if raw.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let indent = &raw[..raw.len() - raw.trim_start().len()];
        let mut line = String::new();
        let mut start_of_line = true;
        let mut has_content = false;
        for word in raw.split_whitespace() {
            if has_content && line.chars().count() + 1 + word.chars().count() > width {
                out.push(std::mem::take(&mut line));
                has_content = false;
            }
            if has_content {
                line.push(' ');
            } else if start_of_line {
                line.push_str(indent);
                start_of_line = false;
            }
            let mut rest = word.to_string();
            while !rest.is_empty() && line.chars().count() + rest.chars().count() > width {
                let avail = width.saturating_sub(line.chars().count()).max(1);
                let cut: String = rest.chars().take(avail).collect();
                line.push_str(&cut);
                out.push(std::mem::take(&mut line));
                rest = rest.chars().skip(avail).collect();
            }
            line.push_str(&rest);
            has_content = !line.is_empty();
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

#[cfg(feature = "webview")]
static WEB_BROWSER: OnceLock<Mutex<Option<aios_webview::WebBrowser>>> = OnceLock::new();
#[cfg(feature = "webview")]
static WEB_BROWSER_SPAWNING: OnceLock<AtomicBool> = OnceLock::new();

#[cfg(feature = "webview")]
fn web_browser_handle() -> &'static Mutex<Option<aios_webview::WebBrowser>> {
    WEB_BROWSER.get_or_init(|| Mutex::new(None))
}

#[cfg(feature = "webview")]
fn web_browser_spawning() -> &'static AtomicBool {
    WEB_BROWSER_SPAWNING.get_or_init(|| AtomicBool::new(false))
}

#[cfg(feature = "webview")]
fn web_open_native(app: &mut TuiApp, target: Option<String>) {
    let target = match target {
        Some(t) => t,
        None => match app.web.page.as_ref().map(|p| p.url.clone()) {
            Some(u) => u,
            None => {
                push_log(
                    &app.logs,
                    "AIOS: web: nothing to open — load a page first".into(),
                );
                return;
            }
        },
    };
    let handle = web_browser_handle();
    let mut guard = handle.lock().unwrap_or_else(|p| p.into_inner());
    match guard.as_mut() {
        Some(browser) => match browser.navigate(&target) {
            Ok(()) => push_log(&app.logs, format!("AIOS: browser: navigating to {target}")),
            Err(_) => {
                *guard = None;
                push_log(&app.logs, format!("AIOS: browser: reopening {target}"));
                web_browser_spawn(&app.logs, target);
            }
        },
        None => {
            push_log(&app.logs, format!("AIOS: browser: opening {target}"));
            web_browser_spawn(&app.logs, target);
        }
    }
}

#[cfg(feature = "webview")]
fn web_browser_spawn(logs: &Arc<Mutex<Vec<String>>>, target: String) {
    let handle = web_browser_handle();
    let spawning = web_browser_spawning();
    if spawning.swap(true, Ordering::SeqCst) {
        return;
    }
    let logs = logs.clone();
    std::thread::spawn(move || {
        match aios_webview::WebBrowser::open(&target) {
            Ok(browser) => {
                if let Ok(mut guard) = handle.lock() {
                    *guard = Some(browser);
                }
            }
            Err(e) => push_log(&logs, format!("AIOS: browser failed to open: {e}")),
        }
        spawning.store(false, Ordering::SeqCst);
    });
}

fn block_restart(app: &mut TuiApp) {
    let (name, id) = selected_block(app);
    let Some(name) = name else {
        return;
    };
    let state = app.state.lock().unwrap();
    let mut registry = state.bridge.registry.lock().unwrap();
    if let Ok(_entry) = registry.unload_block(id) {
        let _ = registry.register_block(&name, "1.0.0", b"block".to_vec());
        let _ = registry.activate_block(id);
    }
    push_log(&app.logs, format!("AIOS: blocks: restarted '{name}'"));
}

fn block_kill(app: &mut TuiApp) {
    let (name, id) = selected_block(app);
    let Some(name) = name else {
        return;
    };
    let state = app.state.lock().unwrap();
    let mut registry = state.bridge.registry.lock().unwrap();
    match registry.unload_block(id) {
        Ok(_) => push_log(
            &app.logs,
            format!("AIOS: blocks: unloaded '{name}' ({})", id.0),
        ),
        Err(e) => push_log(
            &app.logs,
            format!("AIOS: blocks: unload '{name}' failed: {e}"),
        ),
    }
}

fn selected_block(app: &TuiApp) -> (Option<String>, aios_core::block::BlockId) {
    let state = app.state.lock().unwrap();
    let registry = state.bridge.registry.lock().unwrap();
    let mut ids = registry.all_ids();
    ids.sort_by_key(|id| id.0);
    let sel = app.blocks_selected.min(ids.len().saturating_sub(1));
    match ids.get(sel) {
        Some(id) => {
            let name = registry
                .get(*id)
                .map(|e| e.manifest.name.clone())
                .unwrap_or_default();
            (Some(name), *id)
        }
        None => (None, aios_core::block::BlockId::new(0)),
    }
}

fn block_load_path(app: &mut TuiApp, path: &str) {
    let path = path.trim();
    if path.is_empty() {
        push_log(&app.logs, "AIOS: blocks: usage: load <path-to.wasm>".into());
        return;
    }
    let state = app.state.lock().unwrap();
    let mut registry = state.bridge.registry.lock().unwrap();
    match BlockLoader::load_from_directory(&mut registry, PathBuf::from(path).as_path()) {
        results if results.is_empty() => match BlockLoader::load_from_binary(
            &mut registry,
            "loaded_block",
            "1.0.0",
            match std::fs::read(path) {
                Ok(b) => b,
                Err(e) => {
                    push_log(
                        &app.logs,
                        format!("AIOS: blocks: read '{path}' failed: {e}"),
                    );
                    return;
                }
            },
        ) {
            Ok(m) => push_log(
                &app.logs,
                format!("AIOS: blocks: loaded '{}' {}", m.name, m.id),
            ),
            Err(e) => push_log(&app.logs, format!("AIOS: blocks: load failed: {e}")),
        },
        results => {
            for r in results {
                match r {
                    Ok(m) => push_log(
                        &app.logs,
                        format!("AIOS: blocks: loaded '{}' {}", m.name, m.id),
                    ),
                    Err(e) => push_log(&app.logs, format!("AIOS: blocks: load failed: {e}")),
                }
            }
        }
    }
}

fn store_refresh(app: &mut TuiApp) {
    let manager = store_manager();
    let installed = manager.list_installed();
    app.store_installed = installed
        .iter()
        .map(|b| format!("{} {}", b.manifest.name, b.manifest.version))
        .collect();
    app.store_status = format!("{} installed block(s)", app.store_installed.len());
}

fn shell_execute(app: &mut TuiApp, line: &str) {
    let lower = line.trim().to_lowercase();
    let parts: Vec<&str> = lower.split_whitespace().collect();
    let command = match parts.first().copied() {
        Some("") | None => return,
        Some(c) => c,
    };
    let mut out: Vec<String> = Vec::new();
    match command {
        "help" | "?" => {
            out.push("Commands: ps | blocks | kill <pid> | spawn <wasm> | store list | store search <q> | store install <name> | net get | net set k=v ... | cluster status | cluster spawn <name> | cluster kill <node> <pid> | cluster migrate <node> <pid> [target] | status | logs | restart | clear".into());
        }
        "clear" | "cls" => {
            app.shell_output.clear();
        }
        "ps" => {
            let state = app.state.lock().unwrap();
            let scheduler = state.bridge.scheduler.lock().unwrap();
            for proc in scheduler.all_processes() {
                let st = match proc.state {
                    aios_process_mgr::task::ProcessState::Running => "Running",
                    aios_process_mgr::task::ProcessState::Ready => "Ready",
                    aios_process_mgr::task::ProcessState::Suspended => "Suspended",
                    aios_process_mgr::task::ProcessState::Terminated => "Terminated",
                    aios_process_mgr::task::ProcessState::Crashed => "Crashed",
                };
                out.push(format!("  pid_{:<4} {} [{}]", proc.pid.0, proc.name, st));
            }
            let (used, total) = scheduler.ram_usage();
            out.push(format!(
                "  total={} running={} ram={}MB/{}MB",
                scheduler.process_count(),
                scheduler.running_count(),
                used,
                total
            ));
        }
        "blocks" => {
            let state = app.state.lock().unwrap();
            let registry = state.bridge.registry.lock().unwrap();
            let mut ids = registry.all_ids();
            ids.sort_by_key(|id| id.0);
            for id in ids {
                if let Ok(e) = registry.get(id) {
                    out.push(format!(
                        "  [{}] {} v{} ({:?})",
                        id, e.manifest.name, e.manifest.version, e.state
                    ));
                }
            }
        }
        "kill" => match parts.get(1).and_then(|p| p.parse::<u64>().ok()) {
            Some(pid) => {
                let state = app.state.lock().unwrap();
                let mut scheduler = state.bridge.scheduler.lock().unwrap();
                match scheduler.kill_process(ProcessId(pid)) {
                    Ok(p) => out.push(format!("  killed '{}' ({})", p.name, p.pid.0)),
                    Err(e) => out.push(format!("  kill failed: {e}")),
                }
            }
            None => out.push("Usage: kill <pid>".into()),
        },
        "spawn" => {
            let name = parts.get(1).copied().unwrap_or("");
            if name.is_empty() {
                out.push("Usage: spawn <wasm-path-or-file>".into());
            } else {
                let before = app.shell_output.len();
                block_load_path(app, name);
                let added: Vec<String> = app.shell_output.iter().skip(before).cloned().collect();
                out.extend(added);
            }
        }
        "store" => {
            let sub = parts.get(1).copied().unwrap_or("");
            let mut manager = store_manager();
            match sub {
                "list" => {
                    let installed = manager.list_installed();
                    if installed.is_empty() {
                        out.push("  no blocks installed".into());
                    } else {
                        for b in &installed {
                            out.push(format!("  {} {}", b.manifest.name, b.manifest.version));
                        }
                    }
                }
                "search" => {
                    let query = parts.get(2..).map(|p| p.join(" ")).unwrap_or_default();
                    if query.is_empty() {
                        out.push("Usage: store search <query>".into());
                    } else {
                        match StoreManager::block_on(manager.search(&query, None)) {
                            Ok(results) if results.is_empty() => out.push("  no matches".into()),
                            Ok(results) => {
                                out.push(format!("  {} result(s):", results.len()));
                                for m in results {
                                    out.push(format!(
                                        "  {} {} — {}",
                                        m.name, m.version, m.description
                                    ));
                                }
                            }
                            Err(e) => out.push(format!("  search failed: {e}")),
                        }
                    }
                }
                "install" => {
                    let name = parts.get(2).copied().unwrap_or("");
                    if name.is_empty() {
                        out.push("Usage: store install <name>".into());
                    } else {
                        match StoreManager::block_on(manager.install(None, name, None)) {
                            Ok(b) => out.push(format!(
                                "  installed {} {}",
                                b.manifest.name, b.manifest.version
                            )),
                            Err(e) => out.push(format!("  install failed: {e}")),
                        }
                    }
                }
                _ => out.push("Usage: store list | store search <q> | store install <name>".into()),
            }
        }
        "net" => {
            let sub = parts.get(1).copied().unwrap_or("");
            match sub {
                "get" => {
                    if let Some(json) = dispatch_net_get(app) {
                        out.push(format!("  {json}"));
                    }
                }
                "set" => {
                    let rest = parts.get(2..).map(|p| p.join(" ")).unwrap_or_default();
                    let msg = dispatch_net_set(app, &rest);
                    out.push(format!("  {msg}"));
                }
                _ => out.push("Usage: net get | net set key=value ...".into()),
            }
        }
        "cluster" => {
            cluster_execute(app, &parts, &mut out);
        }
        "status" => {
            let state = app.state.lock().unwrap();
            let up = state.start_time.elapsed().as_secs();
            let bridge = state.bridge_running.load(Ordering::SeqCst);
            let n_blocks = state.bridge.registry.lock().unwrap().count();
            out.push(format!(
                "  uptime={up}s bridge={} tier={}",
                if bridge { "online" } else { "starting" },
                state.hw_profile.ai_tier
            ));
            out.push(format!(
                "  RAM {:.1}GB used/{:.1}GB total",
                state.hw_profile.memory.used_gb, state.hw_profile.memory.total_gb
            ));
            out.push(format!("  blocks={n_blocks}"));
        }
        "logs" => {
            let lines: Vec<String> = app
                .logs
                .lock()
                .unwrap()
                .iter()
                .rev()
                .take(20)
                .rev()
                .cloned()
                .collect();
            out.extend(lines.into_iter().map(|l| format!("  {l}")));
        }
        "restart" => {
            app.refresh_hw();
            out.push("  subsystems re-initialized (hardware re-probed)".into());
            push_log(&app.logs, "AIOS: restart requested via shell".into());
        }
        _ => {
            out.push(format!("  unknown command '{command}' — type help"));
        }
    }
    for l in out {
        app.shell_push(l);
    }
}

fn cluster_execute(app: &TuiApp, parts: &[&str], out: &mut Vec<String>) {
    let cluster = app.state.lock().unwrap().cluster.clone();
    out.extend(cluster_run(cluster, parts));
}

fn cluster_run(cluster: Option<Arc<Mutex<DistributedScheduler>>>, parts: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let Some(cluster) = cluster else {
        out.push("  clustering disabled (set AIOS_CLUSTER_PEERS to enable)".into());
        return out;
    };
    let sub = parts.get(1).copied().unwrap_or("");
    match sub {
        "" | "help" => {
            out.push("Usage: cluster status | cluster nodes | cluster spawn <name> [ram_mb] [priority] [target_node] | cluster kill <node> <pid> | cluster migrate <node> <pid> [target_node]".into());
        }
        "status" => {
            let s = cluster.lock().unwrap();
            out.push(format!(
                "  self: [{}] {} tier={}",
                s.self_info().id,
                s.self_info().name,
                s.self_info().tier
            ));
            for n in s.nodes() {
                out.push(format!(
                    "  [{}] {} tier={} {:?} load={:.1}%",
                    n.id,
                    n.name,
                    n.tier,
                    n.status,
                    n.metrics.load_fraction() * 100.0
                ));
            }
            let remote = s.processes();
            if remote.is_empty() {
                out.push("  remote processes: none".into());
            } else {
                out.push(format!("  remote processes ({}):", remote.len()));
                for p in remote {
                    out.push(format!(
                        "    {} {} [{}] {}MB",
                        p.id, p.name, p.state, p.ram_mb
                    ));
                }
            }
            out.push(format!(
                "  local processes hosted: {}",
                s.local_processes().len()
            ));
        }
        "nodes" => {
            let s = cluster.lock().unwrap();
            for n in s.nodes() {
                out.push(format!(
                    "  [{}] {} tier={} {:?} load={:.1}%",
                    n.id,
                    n.name,
                    n.tier,
                    n.status,
                    n.metrics.load_fraction() * 100.0
                ));
            }
        }
        "spawn" => {
            let name = parts.get(2).copied().unwrap_or("");
            if name.is_empty() {
                out.push("Usage: cluster spawn <name> [ram_mb] [priority] [target_node]".into());
                return out;
            }
            let ram = parts
                .get(3)
                .and_then(|p| p.parse::<u64>().ok())
                .unwrap_or(128);
            let prio = parts.get(4).and_then(|p| p.parse::<u8>().ok()).unwrap_or(2);
            let target = parts.get(5).and_then(|p| p.parse::<u64>().ok());
            let spec = RemoteProcessSpec::new(name, prio, ram);
            match cluster.lock().unwrap().spawn(spec, target) {
                Ok(rid) => out.push(format!("  spawned {rid} on node {}", rid.node)),
                Err(e) => out.push(format!("  spawn failed: {e}")),
            }
        }
        "kill" => {
            let node = parts.get(2).and_then(|p| p.parse::<u64>().ok());
            let pid = parts.get(3).and_then(|p| p.parse::<u64>().ok());
            let (Some(node), Some(pid)) = (node, pid) else {
                out.push("Usage: cluster kill <node> <pid>".into());
                return out;
            };
            let rid = RemoteProcessId { node, pid };
            match cluster.lock().unwrap().kill(rid) {
                Ok(()) => out.push(format!("  killed {rid}")),
                Err(e) => out.push(format!("  kill failed: {e}")),
            }
        }
        "migrate" => {
            let node = parts.get(2).and_then(|p| p.parse::<u64>().ok());
            let pid = parts.get(3).and_then(|p| p.parse::<u64>().ok());
            let (Some(node), Some(pid)) = (node, pid) else {
                out.push("Usage: cluster migrate <node> <pid> [target_node]".into());
                return out;
            };
            let rid = RemoteProcessId { node, pid };
            let target = parts.get(4).and_then(|p| p.parse::<u64>().ok());
            match cluster.lock().unwrap().migrate(rid, target) {
                Ok(new_rid) => out.push(format!("  migrated {rid} -> {new_rid}")),
                Err(e) => out.push(format!("  migrate failed: {e}")),
            }
        }
        _ => {
            out.push("Usage: cluster status | cluster nodes | cluster spawn ... | cluster kill <node> <pid> | cluster migrate <node> <pid> [target_node]".into());
        }
    }
    out
}

fn handle_key(app: &mut TuiApp, key: event::KeyEvent) {
    if key.modifiers.contains(KeyModifiers::ALT) {
        if let KeyCode::Char(d) = key.code {
            if let Some(d) = d.to_digit(10) {
                if (1..=7).contains(&d) {
                    app.current_tab = (d - 1) as usize;
                    app.ai_mode = false;
                    app.web.input_focused = false;
                    app.net_mode = false;
                    return;
                }
            }
        }
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.running = false;
        return;
    }

    if app.show_help {
        match key.code {
            KeyCode::Esc | KeyCode::Char('h') => app.show_help = false,
            KeyCode::Char('q') => app.running = false,
            _ => {}
        }
        return;
    }

    if app.load_mode {
        match key.code {
            KeyCode::Esc => app.load_mode = false,
            KeyCode::Enter if !app.load_input.is_empty() => {
                let path = app.load_input.clone();
                app.load_mode = false;
                app.load_input.clear();
                block_load_path(app, &path);
            }
            KeyCode::Char(c) => app.load_input.push(c),
            KeyCode::Backspace => {
                app.load_input.pop();
            }
            _ => {}
        }
        return;
    }

    if app.ai_show_help {
        match key.code {
            KeyCode::Esc => app.ai_show_help = false,
            KeyCode::Char('h') if app.ai_input.is_empty() => app.ai_show_help = false,
            KeyCode::Char('q') if app.ai_input.is_empty() => app.running = false,
            _ => {}
        }
        return;
    }

    if app.net_mode {
        match key.code {
            KeyCode::Esc => app.net_mode = false,
            KeyCode::Enter if !app.net_input.is_empty() => {
                let input = app.net_input.clone();
                app.net_mode = false;
                app.net_input.clear();
                let msg = dispatch_net_set(app, &input);
                push_log(&app.logs, format!("AIOS: net: {msg}"));
            }
            KeyCode::Char(c) => app.net_input.push(c),
            KeyCode::Backspace => {
                app.net_input.pop();
            }
            _ => {}
        }
        return;
    }

    if app.web.input_focused {
        match key.code {
            KeyCode::Esc => app.web.input_focused = false,
            KeyCode::Enter if !app.web.url_input.is_empty() => {
                let input = app.web.url_input.clone();
                app.web.url_input.clear();
                web_navigate(app, &input);
            }
            KeyCode::Char(c) => app.web.url_input.push(c),
            KeyCode::Backspace => {
                app.web.url_input.pop();
            }
            _ => {}
        }
        return;
    }

    if app.current_tab == 2 && app.ai_mode {
        match key.code {
            KeyCode::Char(c) => app.ai_input.push(c),
            KeyCode::Backspace => {
                app.ai_input.pop();
            }
            KeyCode::Up => app.history_up(),
            KeyCode::Down => app.history_down(),
            KeyCode::Enter if !app.ai_input.is_empty() => {
                let input = app.ai_input.clone();
                app.ai_input.clear();
                app.push_history(input.clone());
                let mut ai_guard = app.ai_output.lock().unwrap();
                ai_guard.push_back(format!("> {}", input));
                let len = ai_guard.len();
                if len > 200 {
                    ai_guard.drain(0..len - 150);
                }
                drop(ai_guard);
                if let Some(cmd) = input.strip_prefix('/') {
                    handle_ai_command(app, cmd);
                } else {
                    submit_ai_query(app, input);
                }
            }
            KeyCode::Esc => app.ai_mode = false,
            _ => {}
        }
        return;
    }

    if app.current_tab == 6 {
        match key.code {
            KeyCode::Char(c) => app.shell_input.push(c),
            KeyCode::Backspace => {
                app.shell_input.pop();
            }
            KeyCode::Enter => {
                let input = app.shell_input.clone();
                app.shell_input.clear();
                app.shell_push(format!("$ {input}"));
                shell_execute(app, &input);
            }
            KeyCode::Esc => app.shell_input.clear(),
            _ => {}
        }
        return;
    }

    match key.code {
        KeyCode::Char('q') => {
            app.running = false;
        }
        KeyCode::F(1) | KeyCode::Char('?') => {
            app.show_help = !app.show_help;
        }
        KeyCode::Tab => {
            app.current_tab = (app.current_tab + 1) % 7;
        }
        KeyCode::Char(ch) if ch.is_ascii_digit() && ('1'..='7').contains(&ch) => {
            app.current_tab = (ch as u8 - b'1') as usize;
            app.ai_mode = false;
            app.web.input_focused = false;
            app.net_mode = false;
        }
        #[cfg(feature = "webview")]
        KeyCode::Char('W') => match aios_webview::launcher::launch_gui() {
            Ok(path) => push_log(
                &app.logs,
                format!("AIOS: GUI dashboard launched: {}", path.display()),
            ),
            Err(e) => push_log(&app.logs, format!("AIOS: GUI launch failed: {e}")),
        },
        KeyCode::Char(' ') => {
            app.log_paused = !app.log_paused;
        }
        KeyCode::Char('g') if app.current_tab == 5 => {
            app.web.url_input.clear();
            app.web.input_focused = true;
        }
        KeyCode::Char('g') => {
            let url = format!("http://localhost:{}", app.bridge_port);
            let _ = open::that(&url);
        }
        _ => match app.current_tab {
            1 => handle_blocks_key(app, key),
            2 => handle_ai_key(app, key),
            4 => handle_net_store_key(app, key),
            5 => handle_web_key(app, key),
            _ => {}
        },
    }
}

fn handle_blocks_key(app: &mut TuiApp, key: event::KeyEvent) {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            let count = {
                let state = app.state.lock().unwrap();
                let reg = state.bridge.registry.lock().unwrap();
                reg.count()
            };
            if count > 0 && app.blocks_selected + 1 < count {
                app.blocks_selected += 1;
            }
        }
        KeyCode::Up => {
            if app.blocks_selected > 0 {
                app.blocks_selected -= 1;
            }
        }
        KeyCode::Char('r') => block_restart(app),
        KeyCode::Char('k') => block_kill(app),
        KeyCode::Char('l') => {
            app.load_mode = true;
            app.load_input.clear();
        }
        _ => {}
    }
}

fn handle_ai_key(app: &mut TuiApp, key: event::KeyEvent) {
    match key.code {
        KeyCode::Char('i') => app.ai_mode = true,
        KeyCode::Char('h') => app.ai_show_help = !app.ai_show_help,
        _ => {}
    }
}

fn handle_net_store_key(app: &mut TuiApp, key: event::KeyEvent) {
    match key.code {
        KeyCode::Char('n') => {
            app.net_mode = true;
            app.net_input.clear();
        }
        KeyCode::Char('s') => {
            store_refresh(app);
        }
        KeyCode::Char('g') => {
            if let Some(json) = dispatch_net_get(app) {
                app.net_status = json.clone();
                push_log(&app.logs, format!("AIOS: net: {json}"));
            }
        }
        _ => {}
    }
}

fn handle_web_key(app: &mut TuiApp, key: event::KeyEvent) {
    if app.web.bookmark_naming {
        match key.code {
            KeyCode::Esc => app.web.bookmark_naming = false,
            KeyCode::Enter if !app.web.bookmark_name.trim().is_empty() => {
                let name = app.web.bookmark_name.trim().to_string();
                let url = app.web.current_url.clone();
                app.web.bookmark_naming = false;
                app.web.bookmark_name.clear();
                if !url.is_empty() {
                    if let Some(b) = app.web.bookmarks.iter_mut().find(|b| b.url == url) {
                        b.name = name;
                    } else {
                        app.web.bookmarks.push(WebBookmark {
                            name,
                            url: url.clone(),
                        });
                    }
                    save_bookmarks(app);
                    push_log(&app.logs, format!("AIOS: web: bookmarked {url}"));
                }
            }
            KeyCode::Char(c) => app.web.bookmark_name.push(c),
            KeyCode::Backspace => {
                app.web.bookmark_name.pop();
            }
            _ => {}
        }
        return;
    }

    if app.web.show_bookmarks {
        match key.code {
            KeyCode::Esc => app.web.show_bookmarks = false,
            KeyCode::Char('j') | KeyCode::Down => {
                if !app.web.bookmarks.is_empty() {
                    app.web.bookmarks_sel = (app.web.bookmarks_sel + 1) % app.web.bookmarks.len();
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if !app.web.bookmarks.is_empty() {
                    app.web.bookmarks_sel = app
                        .web
                        .bookmarks_sel
                        .checked_sub(1)
                        .unwrap_or(app.web.bookmarks.len() - 1);
                }
            }
            KeyCode::Enter | KeyCode::Char('o') => {
                let url = app
                    .web
                    .bookmarks
                    .get(app.web.bookmarks_sel)
                    .map(|b| b.url.clone());
                if let Some(url) = url {
                    web_load(app, &url, true);
                }
            }
            KeyCode::Char('d') if app.web.bookmarks_sel < app.web.bookmarks.len() => {
                let removed = app.web.bookmarks.remove(app.web.bookmarks_sel);
                if app.web.bookmarks_sel >= app.web.bookmarks.len() && !app.web.bookmarks.is_empty()
                {
                    app.web.bookmarks_sel = app.web.bookmarks.len() - 1;
                }
                save_bookmarks(app);
                push_log(
                    &app.logs,
                    format!("AIOS: web: removed bookmark '{}'", removed.name),
                );
            }
            _ => {}
        }
        return;
    }

    match key.code {
        KeyCode::Char('g') => {
            app.web.url_input.clear();
            app.web.input_focused = true;
        }
        KeyCode::Char('a') => {
            if app.web.current_url.is_empty() {
                push_log(&app.logs, "AIOS: web: no page loaded to bookmark".into());
            } else {
                app.web.bookmark_name = app
                    .web
                    .page
                    .as_ref()
                    .map(|p| p.title.clone())
                    .unwrap_or_default();
                app.web.bookmark_naming = true;
            }
        }
        KeyCode::Char('m') => {
            app.web.show_bookmarks = !app.web.show_bookmarks;
            app.web.bookmarks_sel = app
                .web
                .bookmarks_sel
                .min(app.web.bookmarks.len().saturating_sub(1));
        }
        KeyCode::Char('j') | KeyCode::Down => web_link_move(app, 1),
        KeyCode::Char('k') | KeyCode::Up => web_link_move(app, -1),
        KeyCode::Enter | KeyCode::Char('o') => web_open_selected(app),
        KeyCode::Char('u') | KeyCode::PageUp => web_scroll(app, -1),
        KeyCode::Char('d') | KeyCode::PageDown => web_scroll(app, 1),
        KeyCode::Char('b') => web_back(app),
        #[cfg(feature = "webview")]
        KeyCode::Char('B') => web_open_native(app, None),
        #[cfg(feature = "webview")]
        KeyCode::Char('n') => {
            let href = app
                .web
                .page
                .as_ref()
                .and_then(|p| p.links.get(app.web.selected_link))
                .map(|l| l.href.clone());
            web_open_native(app, href);
        }
        KeyCode::Char('t') => web_new_tab(app),
        KeyCode::Char('x') => web_close_tab(app),
        KeyCode::Char(']') => web_switch_tab(app, 1),
        KeyCode::Char('[') => web_switch_tab(app, -1),
        KeyCode::Esc => app.web.input_focused = false,
        _ => {}
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use aios_cluster::executor::MockProcessExecutor;
    use aios_cluster::scheduler::DistributedScheduler;
    use aios_cluster::transport::{ClusterTransport, InMemoryClusterTransport, MemoryRegistry};
    use aios_cluster::types::{NodeInfo, NodeMetrics, NodeStatus, PlacementStrategy};
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    fn test_node(id: u64, name: &str, addr: &str) -> NodeInfo {
        NodeInfo {
            id,
            name: name.to_string(),
            addr: addr.to_string(),
            tier: 2,
            status: NodeStatus::Online,
            metrics: NodeMetrics::idle(),
        }
    }

    #[test]
    fn cluster_disabled_prints_hint() {
        let out = cluster_run(None, &["cluster", "status"]);
        assert!(out[0].contains("clustering disabled"), "got {out:?}");
    }

    #[test]
    fn cluster_shell_spawn_kill_migrate() {
        let registry = MemoryRegistry::new();
        let transports: Vec<Arc<dyn ClusterTransport>> = ["mem://sh1", "mem://sh2", "mem://sh3"]
            .iter()
            .map(|addr| {
                Arc::from(InMemoryClusterTransport::new(addr, registry.clone_arc()))
                    as Arc<dyn ClusterTransport>
            })
            .collect();
        let addrs: Vec<String> = transports.iter().map(|t| t.addr().to_string()).collect();

        let mut schedulers = Vec::new();
        for (idx, id) in [1u64, 2, 3].iter().enumerate() {
            let mut s = DistributedScheduler::new(
                test_node(*id, &format!("s{id}"), &addrs[idx]),
                transports[idx].clone(),
                PlacementStrategy::LeastLoaded,
            )
            .with_heartbeat(Duration::from_millis(20))
            .with_failover_threshold(Duration::from_millis(500))
            .with_ack_timeout(Duration::from_secs(2));
            s.set_executor(Arc::new(MockProcessExecutor::new(*id)));
            schedulers.push(Arc::new(Mutex::new(s)));
        }

        let stops: Vec<Arc<AtomicBool>> =
            (0..3).map(|_| Arc::new(AtomicBool::new(false))).collect();
        for (idx, s) in schedulers.iter().enumerate() {
            let peers: Vec<String> = addrs
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != idx)
                .map(|(_, a)| a.clone())
                .collect();
            s.lock().unwrap().start(&peers).unwrap();
            let s = s.clone();
            let stop = stops[idx].clone();
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = s.lock().unwrap().process_events();
                    std::thread::sleep(Duration::from_millis(2));
                }
            });
        }

        // Wait until the coordinator discovers both peers.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while schedulers[0].lock().unwrap().nodes().len() < 2
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(schedulers[0].lock().unwrap().nodes().len(), 2);

        // Spawn through the shell handler onto node 2.
        let out = cluster_run(
            Some(schedulers[0].clone()),
            &["cluster", "spawn", "svc", "256", "2", "2"],
        );
        assert!(
            out.iter().any(|l| l.contains("spawned")),
            "spawn output: {out:?}"
        );
        let rid = schedulers[0].lock().unwrap().processes()[0].id;
        assert_eq!(rid.node, 2);

        let out = cluster_run(Some(schedulers[0].clone()), &["cluster", "status"]);
        assert!(
            out.iter().any(|l| l.contains("remote processes (1)")),
            "status output: {out:?}"
        );

        // Migrate to node 3 through the shell handler.
        let out = cluster_run(
            Some(schedulers[0].clone()),
            &[
                "cluster",
                "migrate",
                &rid.node.to_string(),
                &rid.pid.to_string(),
                "3",
            ],
        );
        assert!(
            out.iter().any(|l| l.contains("migrated")),
            "migrate output: {out:?}"
        );
        let new_rid = schedulers[0].lock().unwrap().processes()[0].id;
        assert_eq!(new_rid.node, 3);

        // Kill through the shell handler.
        let out = cluster_run(
            Some(schedulers[0].clone()),
            &[
                "cluster",
                "kill",
                &new_rid.node.to_string(),
                &new_rid.pid.to_string(),
            ],
        );
        assert!(
            out.iter().any(|l| l.contains("killed")),
            "kill output: {out:?}"
        );
        assert!(schedulers[0].lock().unwrap().processes().is_empty());

        for stop in &stops {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        for s in &schedulers {
            s.lock().unwrap().shutdown();
        }
    }
}
