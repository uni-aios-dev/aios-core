mod app_state;
mod ui;

pub use app_state::TuiApp;
pub use ui::draw;

use crate::orchestrator::OrchestratorState;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
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

fn handle_key(app: &mut TuiApp, key: event::KeyEvent) {
    match key.code {
        KeyCode::Char('q') if app.ai_input.is_empty() => {
            app.running = false;
        }
        KeyCode::Char('g') if app.ai_input.is_empty() => {
            let url = format!("http://localhost:{}", app.bridge_port);
            let _ = open::that(&url);
        }
        KeyCode::Char('r') if app.ai_input.is_empty() => {
            app.refresh_hw();
        }
        KeyCode::Char(' ') if app.ai_input.is_empty() => {
            app.log_paused = !app.log_paused;
        }
        KeyCode::Tab | KeyCode::F(1) => {
            app.current_tab = (app.current_tab + 1) % 4;
        }
        KeyCode::Char(ch) if ch.is_ascii_digit() && ch >= '1' && ch <= '4' => {
            app.current_tab = (ch as u8 - b'1') as usize;
        }
        KeyCode::Char(c) if app.current_tab == 2 && app.ai_mode => {
            app.ai_input.push(c);
        }
        KeyCode::Backspace if app.current_tab == 2 && app.ai_mode && !app.ai_input.is_empty() => {
            app.ai_input.pop();
        }
        KeyCode::Enter if app.current_tab == 2 && app.ai_mode && !app.ai_input.is_empty() => {
            let input = app.ai_input.clone();
            app.ai_input.clear();
            {
                let mut ai_guard = app.ai_output.lock().unwrap();
                ai_guard.push_back(format!("> {}", input));
                if ai_guard.len() > 100 {
                    ai_guard.pop_front();
                }
            }
            let state = app.state.lock().unwrap();
            let logs = app.logs.clone();
            let ai_out = app.ai_output.clone();
            let bridge = state.bridge.clone();
            let prompt = input.clone();
            drop(state);
            tokio::spawn(async move {
                let req = aios_llm::types::LlmRequest {
                    system_prompt: "You are a helpful AI assistant.".into(),
                    user_prompt: prompt.clone(),
                    max_tokens: 1024,
                    temperature: 0.7,
                };
                let result = match bridge.llm.lock().await.query(&req).await {
                    Ok(resp) => resp.text,
                    Err(e) => format!("Error: {}", e),
                };
                let mut ai_guard = ai_out.lock().unwrap();
                ai_guard.push_back(result);
                let len = ai_guard.len();
                if len > 100 {
                    ai_guard.drain(0..len - 50);
                }
                let mut l = logs.lock().unwrap();
                l.push(format!("AI query: {} (done)", prompt));
            });
        }
        KeyCode::Esc => {
            if app.ai_mode {
                app.ai_mode = false;
            }
        }
        KeyCode::Char('i') if app.current_tab == 2 && !app.ai_mode => {
            app.ai_mode = true;
        }
        _ => {}
    }
}
