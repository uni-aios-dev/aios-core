use crate::orchestrator::OrchestratorState;
use aios_llm::{default_config, LlmConfig};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

pub struct TuiApp {
    pub running: bool,
    pub current_tab: usize,
    pub state: Arc<Mutex<OrchestratorState>>,
    pub logs: Arc<Mutex<Vec<String>>>,
    pub log_paused: bool,
    pub displayed_logs: VecDeque<String>,
    pub ai_input: String,
    pub ai_mode: bool,
    pub browser_url: String,
    pub browser_mode: bool,
    pub net_input: String,
    pub net_mode: bool,
    pub ai_output: Arc<Mutex<VecDeque<String>>>,
    pub bridge_port: u16,
    pub ai_system_prompt: String,
    pub ai_config: LlmConfig,
    pub ai_history: VecDeque<String>,
    pub ai_history_index: Option<usize>,
    pub ai_show_help: bool,
    pub ai_status: Arc<Mutex<String>>,
}

impl TuiApp {
    pub fn new(state: Arc<Mutex<OrchestratorState>>) -> Self {
        let logs = {
            let s = state.lock().unwrap();
            s.logs.clone()
        };
        let bridge_port = 8080;
        Self {
            running: true,
            current_tab: 0,
            state,
            logs,
            log_paused: false,
            displayed_logs: VecDeque::new(),
            ai_input: String::new(),
            ai_mode: false,
            browser_url: String::new(),
            browser_mode: false,
            net_input: String::new(),
            net_mode: false,
            ai_output: Arc::new(Mutex::new(VecDeque::new())),
            bridge_port,
            ai_system_prompt: "You are a helpful AI assistant.".into(),
            ai_config: default_config(),
            ai_history: VecDeque::new(),
            ai_history_index: None,
            ai_show_help: false,
            ai_status: Arc::new(Mutex::new("ready".into())),
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
}
