use crate::orchestrator::OrchestratorState;
use aios_browser::types::Page;
use aios_llm::{default_config, LlmConfig};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

/// One persisted chat entry of the AI Console.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AiMessage {
    pub role: String,
    pub text: String,
}

/// Outbox slot for a background web fetch result (generation, target tab, outcome).
pub type FetchOut = Arc<Mutex<Option<(u64, usize, Result<Page, String>)>>>;

/// A single persisted Web tab bookmark: a display name plus the target URL.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WebBookmark {
    pub name: String,
    pub url: String,
}

/// A single open Web tab: URL, loaded page and the per-tab view state.
#[derive(Debug, Clone, Default)]
pub struct WebTab {
    pub url: String,
    pub page: Option<Page>,
    pub scroll: usize,
    pub selected_link: usize,
    pub history: Vec<String>,
    pub error: Option<String>,
}

/// State of the built-in text web browser tab.
pub struct WebState {
    pub current_url: String,
    pub url_input: String,
    pub search_query: String,
    pub page: Option<Page>,
    pub loading: bool,
    pub error: Option<String>,
    pub input_focused: bool,
    pub scroll: usize,
    pub history: Vec<String>,
    pub wrap_width: usize,
    pub history_sel: usize,
    pub selected_link: usize,
    pub fetch_gen: u64,
    pub fetch_out: FetchOut,
    /// Persisted bookmarks (name + URL), shown in the `m` panel.
    pub bookmarks: Vec<WebBookmark>,
    /// Selected bookmark index in the bookmarks panel.
    pub bookmarks_sel: usize,
    /// Whether the bookmarks panel is open instead of the links list.
    pub show_bookmarks: bool,
    /// Whether the bookmark-name input line is active (`a` after a page load).
    pub bookmark_naming: bool,
    /// Buffer for the bookmark name being typed.
    pub bookmark_name: String,
    /// Open Web tabs; index `active_tab` is the one currently rendered.
    pub tabs: Vec<WebTab>,
    /// Index of the rendered tab inside `tabs`.
    pub active_tab: usize,
}

impl Default for WebState {
    fn default() -> Self {
        Self {
            current_url: String::new(),
            url_input: String::new(),
            search_query: String::new(),
            page: None,
            loading: false,
            error: None,
            input_focused: false,
            scroll: 0,
            history: Vec::new(),
            wrap_width: 80,
            history_sel: 0,
            selected_link: 0,
            fetch_gen: 0,
            fetch_out: Arc::new(Mutex::new(None)),
            bookmarks: Vec::new(),
            bookmarks_sel: 0,
            show_bookmarks: false,
            bookmark_naming: false,
            bookmark_name: String::new(),
            tabs: vec![WebTab::default()],
            active_tab: 0,
        }
    }
}

pub struct TuiApp {
    pub running: bool,
    pub current_tab: usize,
    pub state: Arc<Mutex<OrchestratorState>>,
    pub logs: Arc<Mutex<Vec<String>>>,
    pub log_paused: bool,
    pub displayed_logs: VecDeque<String>,
    pub ai_input: String,
    pub ai_mode: bool,
    pub ai_output: Arc<Mutex<VecDeque<String>>>,
    pub bridge_port: u16,
    pub ai_system_prompt: String,
    pub ai_config: LlmConfig,
    pub ai_history: VecDeque<String>,
    pub ai_history_index: Option<usize>,
    pub ai_show_help: bool,
    pub ai_status: Arc<Mutex<String>>,
    pub ai_stream: Arc<Mutex<String>>,
    pub ai_streaming: Arc<Mutex<bool>>,
    pub ai_presets: BTreeMap<String, String>,
    pub ai_log: Arc<Mutex<Vec<AiMessage>>>,
    pub web: WebState,
    pub shell_input: String,
    pub shell_output: VecDeque<String>,
    pub net_input: String,
    pub net_mode: bool,
    pub net_status: String,
    pub show_help: bool,
    pub blocks_selected: usize,
    pub store_installed: Vec<String>,
    pub store_status: String,
    pub load_mode: bool,
    pub load_input: String,
}

impl TuiApp {
    pub fn new(state: Arc<Mutex<OrchestratorState>>) -> Self {
        let logs = {
            let s = state.lock().unwrap();
            s.logs.clone()
        };
        Self {
            running: true,
            current_tab: 0,
            state,
            logs,
            log_paused: false,
            displayed_logs: VecDeque::new(),
            ai_input: String::new(),
            ai_mode: false,
            ai_output: Arc::new(Mutex::new(VecDeque::new())),
            bridge_port: 8080,
            ai_system_prompt: "You are a helpful AI assistant.".into(),
            ai_config: default_config(),
            ai_history: VecDeque::new(),
            ai_history_index: None,
            ai_show_help: false,
            ai_status: Arc::new(Mutex::new("ready".into())),
            ai_stream: Arc::new(Mutex::new(String::new())),
            ai_streaming: Arc::new(Mutex::new(false)),
            ai_presets: seed_presets(),
            ai_log: Arc::new(Mutex::new(Vec::new())),
            web: WebState::default(),
            shell_input: String::new(),
            shell_output: VecDeque::new(),
            net_input: String::new(),
            net_mode: false,
            net_status: String::new(),
            show_help: false,
            blocks_selected: 0,
            store_installed: Vec::new(),
            store_status: String::new(),
            load_mode: false,
            load_input: String::new(),
        }
    }

    pub fn update_logs(&mut self) {
        if let Ok(guard) = self.logs.lock() {
            while self.displayed_logs.len() < guard.len() {
                let idx = self.displayed_logs.len();
                if let Some(msg) = guard.get(idx) {
                    self.displayed_logs.push_back(msg.clone());
                } else {
                    break;
                }
            }
            while self.displayed_logs.len() > 200 {
                self.displayed_logs.pop_front();
            }
        }
    }

    pub fn refresh_hw(&mut self) {
        let new_profile = crate::hw_probe::probe();
        if let Ok(mut state) = self.state.lock() {
            state.hw_profile = new_profile;
            let mut logs = state.logs.lock().unwrap();
            logs.push("AIOS: hardware re-probed.".into());
        }
    }

    pub fn push_history(&mut self, entry: String) {
        self.ai_history.push_back(entry);
        while self.ai_history.len() > 50 {
            self.ai_history.pop_front();
        }
        self.ai_history_index = None;
    }

    pub fn history_up(&mut self) {
        if self.ai_history.is_empty() {
            return;
        }
        let len = self.ai_history.len();
        let idx = match self.ai_history_index {
            Some(i) if i > 0 => i - 1,
            _ => len - 1,
        };
        self.ai_history_index = Some(idx);
        if let Some(s) = self.ai_history.get(idx) {
            self.ai_input = s.clone();
        }
    }

    pub fn history_down(&mut self) {
        let idx = match self.ai_history_index {
            Some(i) if i + 1 < self.ai_history.len() => Some(i + 1),
            _ => None,
        };
        self.ai_history_index = idx;
        self.ai_input = idx.map(|i| self.ai_history[i].clone()).unwrap_or_default();
    }

    pub fn shell_push(&mut self, line: String) {
        self.shell_output.push_back(line);
        while self.shell_output.len() > 300 {
            self.shell_output.pop_front();
        }
    }
}

fn seed_presets() -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("assistant".into(), "You are a helpful AI assistant.".into());
    m.insert(
        "code".into(),
        "You are an expert senior software engineer. Give concise, idiomatic \
         code with brief explanations. Prefer standard library solutions."
            .into(),
    );
    m.insert(
        "translator".into(),
        "You translate text between languages accurately, preserving meaning \
         and tone. Output only the translation."
            .into(),
    );
    m.insert(
        "explainer".into(),
        "You explain complex topics in simple terms with concrete examples.".into(),
    );
    m
}
