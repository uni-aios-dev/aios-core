use crate::orchestrator::OrchestratorState;
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
    pub ai_output: Arc<Mutex<VecDeque<String>>>,
    pub bridge_port: u16,
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
            ai_output: Arc::new(Mutex::new(VecDeque::new())),
            bridge_port,
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
}
