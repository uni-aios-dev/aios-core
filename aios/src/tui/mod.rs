mod app_state;
mod ui;

pub use app_state::TuiApp;
pub use ui::draw;

use crate::orchestrator::{push_log, OrchestratorState};
use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};
use aios_llm::{provider_name, BackendKind, CloudProvider, LlmConfig, LlmEngine};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::stdout;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub fn run_tui(state: Arc<Mutex<OrchestratorState>>) -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let app = TuiApp::new(state.clone());

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
    }
    Ok(())
}

fn dispatch_open_url(app: &mut TuiApp, url: &str) {
    let raw = url.trim();
    let target = if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else if raw.contains('.') && !raw.contains(|c: char| c.is_whitespace()) {
        format!("https://{raw}")
    } else {
        format!(
            "https://html.duckduckgo.com/html/?q={}",
            url::form_urlencoded::byte_serialize(raw.as_bytes()).collect::<String>()
        )
    };
    let result = {
        let mut state = app.state.lock().unwrap();
        let packet = IpcPacket::new(
            0,
            state.browser_block_id.0,
            CommandId::Custom,
            Payload::Custom("open_native".into(), target.as_bytes().to_vec()),
        );
        state.router.dispatch(&packet)
    };
    match result {
        Ok(Some(resp)) => {
            let msg = match resp.payload {
                Payload::Text(text) => text,
                _ => "browser block: OK".to_string(),
            };
            push_log(&app.logs, format!("AIOS: browser: {msg}"));
        }
        Ok(None) => push_log(&app.logs, "AIOS: browser: no response".into()),
        Err(e) => push_log(&app.logs, format!("AIOS: browser ERROR: {e}")),
    }
}

fn dispatch_net_set(app: &mut TuiApp, raw: &str) {
    let mut updates = serde_json::Map::new();
    for token in raw.split_whitespace() {
        let (k, v) = match token.split_once('=') {
            Some(kv) => kv,
            None => {
                push_log(
                    &app.logs,
                    format!("AIOS: net: bad token '{token}' (use key=value)"),
                );
                return;
            }
        };
        let value = serde_json::from_str::<serde_json::Value>(v)
            .unwrap_or_else(|_| serde_json::Value::String(v.to_string()));
        updates.insert(k.to_string(), value);
    }
    if updates.is_empty() {
        push_log(&app.logs, "AIOS: net: usage: key=value ...".into());
        return;
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
        Ok(Some(resp)) => {
            let msg = match resp.payload {
                Payload::Text(text) => text,
                _ => "net settings block: OK".to_string(),
            };
            push_log(&app.logs, format!("AIOS: net: {msg}"));
        }
        Ok(None) => push_log(&app.logs, "AIOS: net: no response".into()),
        Err(e) => push_log(&app.logs, format!("AIOS: net ERROR: {e}")),
    }
}

fn handle_key(app: &mut TuiApp, key: event::KeyEvent) {
    if key.modifiers.contains(KeyModifiers::ALT) {
        if let KeyCode::Char(d) = key.code {
            if let Some(d) = d.to_digit(10) {
                if (1..=4).contains(&d) {
                    app.current_tab = (d - 1) as usize;
                    app.ai_mode = false;
                    app.browser_mode = false;
                    app.net_mode = false;
                    return;
                }
            }
        }
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

    if app.browser_mode {
        match key.code {
            KeyCode::Esc => app.browser_mode = false,
            KeyCode::Enter if !app.browser_url.is_empty() => {
                let url = app.browser_url.clone();
                app.browser_mode = false;
                app.browser_url.clear();
                dispatch_open_url(app, &url);
            }
            KeyCode::Char(c) => app.browser_url.push(c),
            KeyCode::Backspace => {
                app.browser_url.pop();
            }
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
                dispatch_net_set(app, &input);
            }
            KeyCode::Char(c) => app.net_input.push(c),
            KeyCode::Backspace => {
                app.net_input.pop();
            }
            _ => {}
        }
        return;
    }

    match key.code {
        KeyCode::Char('q') if app.ai_input.is_empty() => {
            app.running = false;
        }
        KeyCode::Char('g') if app.ai_input.is_empty() => {
            let url = format!("http://localhost:{}", app.bridge_port);
            let _ = open::that(&url);
        }
        KeyCode::Char('b') if app.ai_input.is_empty() && !app.ai_mode => {
            app.browser_mode = true;
            app.browser_url.clear();
        }
        KeyCode::Char('n') if app.ai_input.is_empty() && !app.ai_mode => {
            app.net_mode = true;
            app.net_input.clear();
        }
        KeyCode::Char('r') if app.ai_input.is_empty() => {
            app.refresh_hw();
        }
        KeyCode::Char('W') if app.ai_input.is_empty() => {
            match aios_webview::launcher::launch_gui() {
                Ok(path) => push_log(
                    &app.logs,
                    format!("AIOS: GUI dashboard launched: {}", path.display()),
                ),
                Err(e) => push_log(&app.logs, format!("AIOS: GUI launch failed: {e}")),
            }
        }
        KeyCode::Char(' ') if app.ai_input.is_empty() => {
            app.log_paused = !app.log_paused;
        }
        KeyCode::Tab | KeyCode::F(1) => {
            app.current_tab = (app.current_tab + 1) % 4;
        }
        KeyCode::Char(ch) if ch.is_ascii_digit() && ('1'..='4').contains(&ch) && !app.ai_mode => {
            app.current_tab = (ch as u8 - b'1') as usize;
        }
        KeyCode::Char(c) if app.current_tab == 2 && app.ai_mode => {
            app.ai_input.push(c);
        }
        KeyCode::Backspace if app.current_tab == 2 && app.ai_mode && !app.ai_input.is_empty() => {
            app.ai_input.pop();
        }
        KeyCode::Up if app.current_tab == 2 && app.ai_mode => {
            app.history_up();
        }
        KeyCode::Down if app.current_tab == 2 && app.ai_mode => {
            app.history_down();
        }
        KeyCode::Enter if app.current_tab == 2 && app.ai_mode && !app.ai_input.is_empty() => {
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
        KeyCode::Esc => {
            if app.ai_mode {
                app.ai_mode = false;
            }
        }
        KeyCode::Char('i') if app.current_tab == 2 && !app.ai_mode => {
            app.ai_mode = true;
        }
        KeyCode::Char('h') if app.current_tab == 2 && !app.ai_mode => {
            app.ai_show_help = !app.ai_show_help;
        }
        _ => {}
    }
}

fn push_ai_line(app: &TuiApp, line: String) {
    let mut ai_guard = app.ai_output.lock().unwrap();
    ai_guard.push_back(line);
    let len = ai_guard.len();
    if len > 200 {
        ai_guard.drain(0..len - 150);
    }
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
    let bridge = state.bridge.clone();
    drop(state);
    *status.lock().unwrap() = "thinking...".into();
    tokio::spawn(async move {
        let result = {
            let mut eng = bridge.llm.lock().await;
            *eng = LlmEngine::from_config(config.clone());
            let req = aios_llm::LlmRequest {
                system_prompt: system,
                user_prompt: prompt.clone(),
                max_tokens: config.max_tokens,
                temperature: config.temperature,
            };
            eng.query(&req).await
        };
        match result {
            Ok(resp) => {
                let tokens = if resp.tokens_used > 0 {
                    format!(", {} tokens", resp.tokens_used)
                } else {
                    String::new()
                };
                *status.lock().unwrap() = format!("done: {} ms{}", resp.duration_ms, tokens);
                let mut ai_guard = ai_out.lock().unwrap();
                ai_guard.push_back(resp.text);
                let len = ai_guard.len();
                if len > 200 {
                    ai_guard.drain(0..len - 150);
                }
            }
            Err(e) => {
                *status.lock().unwrap() = format!("error: {e}");
                let mut ai_guard = ai_out.lock().unwrap();
                ai_guard.push_back(format!("[error] {e}"));
            }
        }
        let mut l = logs.lock().unwrap();
        l.push(format!("AI query: {} (done)", prompt));
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
                "Commands: /help /status /clear /history /system /model /backend /key /temp /tokens"
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
        _ => {
            replies.push(format!("Unknown command '/{name}'. Type /help for usage."));
        }
    }
    for r in replies {
        push_ai_line(app, r);
    }
}
