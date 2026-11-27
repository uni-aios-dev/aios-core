use serde::{Deserialize, Serialize};

/// Host-backed EasyLang commands compiled into guarded `aios` imports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemCommand {
    /// Check a URL against the host's SSRF policy (audit-only host call).
    NetCheck,
    /// Request a browser navigation through the host (audit-only host call).
    Browse,
    /// Request a console command through the host (audit-only host call).
    ConsoleCmd,
    /// Show a desktop notification through the host.
    Notify,
}

impl SystemCommand {
    /// Recognize a leading system command token in a step prompt.
    pub fn from_prompt(prompt: &str) -> Option<Self> {
        let first = prompt.split_whitespace().next()?.trim_end_matches(':');
        match first.to_ascii_lowercase().as_str() {
            "net_check" => Some(Self::NetCheck),
            "browse" => Some(Self::Browse),
            "console_cmd" => Some(Self::ConsoleCmd),
            "notify" => Some(Self::Notify),
            _ => None,
        }
    }

    /// WASM import name of the guarded host function.
    pub fn import_name(self) -> &'static str {
        match self {
            Self::NetCheck => "net_check",
            Self::Browse => "browse",
            Self::ConsoleCmd => "console_cmd",
            Self::Notify => "notify",
        }
    }

    /// Step prompt with the command token removed.
    pub fn argument(self, prompt: &str) -> &str {
        let trimmed = prompt.trim_start();
        let rest = trimmed
            .find(char::is_whitespace)
            .map(|pos| &trimmed[pos..])
            .unwrap_or_default();
        let _ = self;
        rest.trim()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStep {
    pub label: String,
    pub prompt: String,
}

impl WorkflowStep {
    /// System command this step maps to, if any.
    pub fn system_command(&self) -> Option<SystemCommand> {
        SystemCommand::from_prompt(&self.prompt)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    pub name: String,
    pub steps: Vec<WorkflowStep>,
}

impl Workflow {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            steps: Vec::new(),
        }
    }

    pub fn add_step(&mut self, label: &str, prompt: &str) {
        self.steps.push(WorkflowStep {
            label: label.to_string(),
            prompt: prompt.to_string(),
        });
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        if self.name.is_empty() {
            errors.push("Workflow name cannot be empty".into());
        }
        if self.steps.is_empty() {
            errors.push("Workflow must have at least one step".into());
        }
        for (i, step) in self.steps.iter().enumerate() {
            if step.prompt.is_empty() {
                errors.push(format!("Step {}: prompt cannot be empty", i + 1));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}
