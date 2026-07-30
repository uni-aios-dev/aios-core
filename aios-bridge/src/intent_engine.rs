use aios_llm::{LlmEngine, LlmRequest};
use aios_security::capability::Capability;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub enum ProcessAction {
    List,
    Kill,
    Spawn,
    AdjustPriority,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BlockAction {
    List,
    Load,
    Unload,
    HotSwap,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MetricType {
    Cpu,
    Memory,
    Processes,
    Blocks,
    All,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UserIntent {
    ProcessControl {
        action: ProcessAction,
        target: String,
    },
    BlockManagement {
        action: BlockAction,
        wasm_path: Option<PathBuf>,
        block_name: Option<String>,
    },
    SystemQuery {
        metric: MetricType,
    },
    MemoryCompaction,
    WorkflowExecution {
        steps: Vec<UserIntent>,
    },
    Unknown {
        raw_prompt: String,
    },
}

pub struct ExecutionPlan {
    pub intent: UserIntent,
    pub required_capabilities: Vec<Capability>,
    pub steps: Vec<String>,
}

pub struct IntentParser {
    rules: Vec<ParseRule>,
}

#[allow(dead_code)]
struct ParseRule {
    patterns: Vec<String>,
    intent_fn: fn(&str, &[String]) -> Option<UserIntent>,
    capabilities: Vec<Capability>,
}

impl IntentParser {
    pub fn new() -> Self {
        Self {
            rules: Self::default_rules(),
        }
    }

    fn default_rules() -> Vec<ParseRule> {
        vec![
            // Process: show/list processes (RU/EN)
            ParseRule {
                patterns: vec![
                    "show processes".into(),
                    "list processes".into(),
                    "покажи процессы".into(),
                    "список процессов".into(),
                    "процессы".into(),
                ],
                intent_fn: |_, _| {
                    Some(UserIntent::ProcessControl {
                        action: ProcessAction::List,
                        target: String::new(),
                    })
                },
                capabilities: vec![],
            },
            // Process: kill (RU/EN)
            ParseRule {
                patterns: vec![
                    "kill process".into(),
                    "ubey process".into(),
                    "убей процесс".into(),
                    "убить процесс".into(),
                    "terminate".into(),
                    "завершить".into(),
                ],
                intent_fn: |prompt, _| {
                    let target = extract_number(prompt)
                        .map(|n| n.to_string())
                        .or_else(|| extract_name(prompt));
                    target.map(|t| UserIntent::ProcessControl {
                        action: ProcessAction::Kill,
                        target: t,
                    })
                },
                capabilities: vec![Capability::ProcessKill],
            },
            // Process: spawn (RU/EN)
            ParseRule {
                patterns: vec![
                    "spawn".into(),
                    "start".into(),
                    "launch".into(),
                    "запусти".into(),
                    "запустить".into(),
                    "старт".into(),
                ],
                intent_fn: |prompt, _| {
                    extract_name(prompt).map(|name| UserIntent::ProcessControl {
                        action: ProcessAction::Spawn,
                        target: name,
                    })
                },
                capabilities: vec![Capability::ProcessSpawn],
            },
            // Block: list (RU/EN)
            ParseRule {
                patterns: vec![
                    "show blocks".into(),
                    "list blocks".into(),
                    "покажи блоки".into(),
                    "список блоков".into(),
                    "блоки".into(),
                ],
                intent_fn: |_, _| {
                    Some(UserIntent::BlockManagement {
                        action: BlockAction::List,
                        wasm_path: None,
                        block_name: None,
                    })
                },
                capabilities: vec![],
            },
            // Block: unload (RU/EN) — before load to prevent "unload" matching "load"
            ParseRule {
                patterns: vec![
                    "unload block".into(),
                    "unload".into(),
                    "выгрузи блок".into(),
                    "выгрузить".into(),
                    "удали блок".into(),
                ],
                intent_fn: |prompt, _| {
                    extract_name(prompt).map(|name| UserIntent::BlockManagement {
                        action: BlockAction::Unload,
                        wasm_path: None,
                        block_name: Some(name),
                    })
                },
                capabilities: vec![Capability::BlockUnload],
            },
            // Block: load (RU/EN)
            ParseRule {
                patterns: vec![
                    "load block".into(),
                    "load".into(),
                    "загрузи блок".into(),
                    "загрузить".into(),
                ],
                intent_fn: |prompt, _| {
                    extract_name(prompt).map(|name| UserIntent::BlockManagement {
                        action: BlockAction::Load,
                        wasm_path: Some(PathBuf::from(&name)),
                        block_name: Some(name),
                    })
                },
                capabilities: vec![Capability::BlockLoad],
            },
            // System: status (RU/EN)
            ParseRule {
                patterns: vec![
                    "system status".into(),
                    "status".into(),
                    "статус системы".into(),
                    "статус".into(),
                    "system query".into(),
                ],
                intent_fn: |_, _| {
                    Some(UserIntent::SystemQuery {
                        metric: MetricType::All,
                    })
                },
                capabilities: vec![],
            },
            // System: CPU
            ParseRule {
                patterns: vec![
                    "cpu".into(),
                    "show cpu".into(),
                    "cpu usage".into(),
                    "процессор".into(),
                    "загрузка процессора".into(),
                ],
                intent_fn: |_, _| {
                    Some(UserIntent::SystemQuery {
                        metric: MetricType::Cpu,
                    })
                },
                capabilities: vec![],
            },
            // Memory compaction — before "memory" to prevent "compact memory" matching SystemQuery/Memory
            ParseRule {
                patterns: vec![
                    "compress memory".into(),
                    "compact memory".into(),
                    "сожми память".into(),
                    "сжать память".into(),
                    "compact".into(),
                    "compress".into(),
                ],
                intent_fn: |_, _| Some(UserIntent::MemoryCompaction),
                capabilities: vec![Capability::MemAlloc],
            },
            // System: memory
            ParseRule {
                patterns: vec![
                    "memory".into(),
                    "ram".into(),
                    "show memory".into(),
                    "память".into(),
                    "оперативная память".into(),
                    "ram".into(),
                ],
                intent_fn: |_, _| {
                    Some(UserIntent::SystemQuery {
                        metric: MetricType::Memory,
                    })
                },
                capabilities: vec![],
            },
        ]
    }

    pub fn parse(&self, prompt: &str) -> UserIntent {
        let lower = prompt.to_lowercase().trim().to_string();

        for rule in &self.rules {
            if let Some(intent) = self.match_rule(&lower, rule) {
                return intent;
            }
        }

        UserIntent::Unknown {
            raw_prompt: prompt.to_string(),
        }
    }

    pub async fn parse_with_llm_fallback(&self, prompt: &str, llm: &LlmEngine) -> UserIntent {
        let rule_result = self.parse(prompt);

        match &rule_result {
            UserIntent::Unknown { raw_prompt } => {
                match self.classify_with_llm(raw_prompt, llm).await {
                    Ok(Some(intent)) => intent,
                    _ => rule_result,
                }
            }
            _ => rule_result,
        }
    }

    async fn classify_with_llm(
        &self,
        prompt: &str,
        llm: &LlmEngine,
    ) -> Result<Option<UserIntent>, String> {
        let system_prompt = "\
You are an intent classifier for an AI operating system. \
Given a user's natural language request, classify it into one of these intent types \
and return a JSON object.

Available intents:
- ProcessControl: \
{\"intent\":\"ProcessControl\",\"action\":\"List|Kill|Spawn|AdjustPriority\",\"target\":\"...\"}
- BlockManagement: \
{\"intent\":\"BlockManagement\",\"action\":\"List|Load|Unload|HotSwap\",\"block_name\":\"...\"}
- SystemQuery: \
{\"intent\":\"SystemQuery\",\"metric\":\"Cpu|Memory|Processes|Blocks|All\"}
- MemoryCompaction: \
{\"intent\":\"MemoryCompaction\"}

Return ONLY valid JSON, no other text.";

        let request = LlmRequest {
            system_prompt: system_prompt.to_string(),
            user_prompt: prompt.to_string(),
            max_tokens: 256,
            temperature: 0.2,
        };

        let response = llm.query(&request).await.map_err(|e| e.to_string())?;
        let text = response.text.trim().to_string();

        Self::parse_llm_response(&text)
    }

    fn parse_llm_response(text: &str) -> Result<Option<UserIntent>, String> {
        let cleaned = text
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();

        let parsed: serde_json::Value =
            serde_json::from_str(cleaned).map_err(|e| format!("JSON parse error: {e}"))?;

        let intent_type = parsed["intent"]
            .as_str()
            .ok_or_else(|| "Missing 'intent' field".to_string())?;

        match intent_type {
            "ProcessControl" => {
                let action_str = parsed["action"].as_str().unwrap_or("List");
                let action = match action_str {
                    "Kill" => ProcessAction::Kill,
                    "Spawn" => ProcessAction::Spawn,
                    "AdjustPriority" => ProcessAction::AdjustPriority,
                    _ => ProcessAction::List,
                };
                let target = parsed["target"].as_str().unwrap_or("").to_string();
                Ok(Some(UserIntent::ProcessControl { action, target }))
            }
            "BlockManagement" => {
                let action_str = parsed["action"].as_str().unwrap_or("List");
                let action = match action_str {
                    "Load" => BlockAction::Load,
                    "Unload" => BlockAction::Unload,
                    "HotSwap" => BlockAction::HotSwap,
                    _ => BlockAction::List,
                };
                let block_name = parsed["block_name"].as_str().map(|s| s.to_string());
                Ok(Some(UserIntent::BlockManagement {
                    action,
                    wasm_path: None,
                    block_name,
                }))
            }
            "SystemQuery" => {
                let metric_str = parsed["metric"].as_str().unwrap_or("All");
                let metric = match metric_str {
                    "Cpu" => MetricType::Cpu,
                    "Memory" => MetricType::Memory,
                    "Processes" => MetricType::Processes,
                    "Blocks" => MetricType::Blocks,
                    _ => MetricType::All,
                };
                Ok(Some(UserIntent::SystemQuery { metric }))
            }
            "MemoryCompaction" => Ok(Some(UserIntent::MemoryCompaction)),
            _ => Ok(None),
        }
    }

    fn match_rule(&self, prompt: &str, rule: &ParseRule) -> Option<UserIntent> {
        for pattern in &rule.patterns {
            if prompt.contains(pattern) {
                return (rule.intent_fn)(prompt, &rule.patterns);
            }
        }
        None
    }

    pub fn create_execution_plan(&self, intent: &UserIntent) -> ExecutionPlan {
        let (required_capabilities, steps) = match intent {
            UserIntent::ProcessControl { action, target } => {
                let (caps, steps) = match action {
                    ProcessAction::List => {
                        (vec![], vec!["Query scheduler for process list".into()])
                    }
                    ProcessAction::Kill => (
                        vec![Capability::ProcessKill],
                        vec![format!("Kill process {target}")],
                    ),
                    ProcessAction::Spawn => (
                        vec![Capability::ProcessSpawn],
                        vec![format!("Spawn process {target}")],
                    ),
                    ProcessAction::AdjustPriority => (
                        vec![Capability::SchedModify],
                        vec![format!("Adjust priority of {target}")],
                    ),
                };
                (caps, steps)
            }
            UserIntent::BlockManagement {
                action, block_name, ..
            } => {
                let (caps, steps) = match action {
                    BlockAction::List => (vec![], vec!["Query block registry".into()]),
                    BlockAction::Load => {
                        let name = block_name.as_deref().unwrap_or("unknown");
                        (
                            vec![Capability::BlockLoad],
                            vec![format!("Load block {name}")],
                        )
                    }
                    BlockAction::Unload => {
                        let name = block_name.as_deref().unwrap_or("unknown");
                        (
                            vec![Capability::BlockUnload],
                            vec![format!("Unload block {name}")],
                        )
                    }
                    BlockAction::HotSwap => {
                        let name = block_name.as_deref().unwrap_or("unknown");
                        (
                            vec![Capability::BlockLoad, Capability::BlockUnload],
                            vec![format!("Hot-swap block {name}")],
                        )
                    }
                };
                (caps, steps)
            }
            UserIntent::SystemQuery { .. } => (vec![], vec!["Collect system metrics".into()]),
            UserIntent::MemoryCompaction => (
                vec![Capability::MemAlloc],
                vec!["Trigger memory compaction".into()],
            ),
            UserIntent::WorkflowExecution { steps } => {
                let mut all_caps = Vec::new();
                let mut all_steps = Vec::new();
                for s in steps {
                    let plan = self.create_execution_plan(s);
                    all_caps.extend(plan.required_capabilities);
                    all_steps.extend(plan.steps);
                }
                (all_caps, all_steps)
            }
            UserIntent::Unknown { .. } => (vec![], vec!["Unable to determine action".into()]),
        };

        ExecutionPlan {
            intent: intent.clone(),
            required_capabilities,
            steps,
        }
    }
}

fn extract_number(prompt: &str) -> Option<u64> {
    prompt
        .split_whitespace()
        .find_map(|w| w.parse::<u64>().ok())
}

fn extract_name(prompt: &str) -> Option<String> {
    let words: Vec<&str> = prompt.split_whitespace().collect();
    if words.len() <= 1 {
        return None;
    }
    let last = words.last()?;
    if last.parse::<u64>().is_ok() {
        if words.len() >= 2 {
            return Some(words[words.len() - 2].to_string());
        }
        return None;
    }
    Some(last.to_string())
}

impl Default for IntentParser {
    fn default() -> Self {
        Self::new()
    }
}
