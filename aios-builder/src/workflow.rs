use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStep {
    pub label: String,
    pub prompt: String,
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
