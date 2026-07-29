use aios_core::ipc_protocol::{CommandId, IpcPacket, Payload};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentContext {
    pub active_processes: Vec<String>,
    pub loaded_blocks: Vec<String>,
    pub current_tier: String,
    pub ram_used_mb: u64,
    pub ram_total_mb: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslatedCommand {
    pub intent: String,
    pub packet: IpcPacket,
    pub explanation: String,
}

pub struct IntentEngine;

impl IntentEngine {
    pub fn translate(intent: &str, context: &IntentContext) -> Option<TranslatedCommand> {
        let lower = intent.to_lowercase();

        if lower.contains("free up memory")
            || lower.contains("reduce memory")
            || lower.contains("clear memory")
        {
            return Some(Self::translate_memory_optimize(context));
        }

        if lower.contains("video editing")
            || lower.contains("optimize for video")
            || lower.contains("video production")
        {
            return Some(Self::translate_video_optimize(context));
        }

        if lower.contains("update") && lower.contains("block") {
            return Some(Self::translate_block_update(&lower));
        }

        if lower.contains("kill") || lower.contains("stop") {
            return Some(Self::translate_kill_process(&lower, context));
        }

        if lower.contains("spawn") || lower.contains("start") || lower.contains("run") {
            return Some(Self::translate_spawn_process(&lower));
        }

        if lower.contains("priority") || lower.contains("boost") || lower.contains("throttle") {
            return Some(Self::translate_priority_adjust(&lower, context));
        }

        if lower.contains("status") || lower.contains("health") || lower.contains("diagnostic") {
            return Some(Self::translate_health_check());
        }

        if lower.contains("topology") || lower.contains("blocks") || lower.contains("modules") {
            return Some(Self::translate_topology(context));
        }

        None
    }

    fn translate_memory_optimize(context: &IntentContext) -> TranslatedCommand {
        let explanation = format!(
            "Optimizing memory: currently {}MB/{}MB used. Throttling background processes and freeing caches.",
            context.ram_used_mb, context.ram_total_mb
        );

        TranslatedCommand {
            intent: "Free up memory".into(),
            packet: IpcPacket::new(
                0,
                3,
                CommandId::AdjustPriority,
                Payload::AdjustPriority {
                    pid: 0,
                    new_priority: 0,
                },
            ),
            explanation,
        }
    }

    fn translate_video_optimize(context: &IntentContext) -> TranslatedCommand {
        let explanation = format!(
            "Optimizing for video production: boosting encoding processes to Critical priority, \
             throttle background tasks. Current tier: {}.",
            context.current_tier
        );

        TranslatedCommand {
            intent: "Optimize for video editing".into(),
            packet: IpcPacket::new(
                0,
                3,
                CommandId::AdjustPriority,
                Payload::AdjustPriority {
                    pid: 0,
                    new_priority: 4,
                },
            ),
            explanation,
        }
    }

    fn translate_block_update(lower: &str) -> TranslatedCommand {
        let block_name = lower
            .split_whitespace()
            .skip_while(|w| *w != "update")
            .skip(1)
            .take_while(|w| *w != "block")
            .collect::<Vec<_>>()
            .join(" ");

        let name = if block_name.is_empty() {
            "unknown".to_string()
        } else {
            block_name
        };

        TranslatedCommand {
            intent: format!("Update block '{name}'"),
            packet: IpcPacket::new(0, 2, CommandId::HotSwap, Payload::HotSwap {
                block_id: 0,
                new_binary: Vec::new(),
                new_version: "latest".into(),
            }),
            explanation: format!("Hot-swapping block '{name}' with latest version. Queue will be frozen during swap."),
        }
    }

    fn translate_kill_process(lower: &str, context: &IntentContext) -> TranslatedCommand {
        let target_name = context
            .active_processes
            .iter()
            .find(|name| lower.contains(&name.to_lowercase()))
            .cloned()
            .unwrap_or_else(|| "unknown".into());

        TranslatedCommand {
            intent: format!("Kill process '{target_name}'"),
            packet: IpcPacket::new(
                0,
                3,
                CommandId::KillProcess,
                Payload::KillProcess { pid: 0 },
            ),
            explanation: format!(
                "Terminating process '{target_name}' and reclaiming its resources."
            ),
        }
    }

    fn translate_spawn_process(lower: &str) -> TranslatedCommand {
        let trigger_words = ["spawn", "start", "run"];
        let name = lower
            .split_whitespace()
            .position(|w| trigger_words.contains(&w))
            .and_then(|pos| lower.split_whitespace().nth(pos + 1))
            .unwrap_or("new_process")
            .to_string();

        TranslatedCommand {
            intent: format!("Spawn process '{name}'"),
            packet: IpcPacket::new(
                0,
                3,
                CommandId::SpawnProcess,
                Payload::SpawnProcess {
                    name,
                    priority: 2,
                    ram_mb: 256,
                },
            ),
            explanation: "Spawning new process with Normal priority and 256MB RAM quota.".into(),
        }
    }

    fn translate_priority_adjust(lower: &str, _context: &IntentContext) -> TranslatedCommand {
        let (priority, action) = if lower.contains("boost")
            || lower.contains("high")
            || lower.contains("critical")
        {
            (4u8, "boosted to Critical")
        } else if lower.contains("throttle") || lower.contains("low") || lower.contains("reduce") {
            (1u8, "throttled to Low")
        } else {
            (2u8, "set to Normal")
        };

        TranslatedCommand {
            intent: format!("Adjust priority: {action}"),
            packet: IpcPacket::new(
                0,
                3,
                CommandId::AdjustPriority,
                Payload::AdjustPriority {
                    pid: 0,
                    new_priority: priority,
                },
            ),
            explanation: format!(
                "Priority {action} for targeted processes. Active: {} processes.",
                _context.active_processes.len()
            ),
        }
    }

    fn translate_health_check() -> TranslatedCommand {
        TranslatedCommand {
            intent: "System health check".into(),
            packet: IpcPacket::new(0, 0, CommandId::HealthCheck, Payload::HealthCheck),
            explanation: "Running health check on all loaded blocks and active processes.".into(),
        }
    }

    fn translate_topology(context: &IntentContext) -> TranslatedCommand {
        TranslatedCommand {
            intent: "Get block topology".into(),
            packet: IpcPacket::new(0, 2, CommandId::GetTopology, Payload::GetTopology),
            explanation: format!(
                "Querying topology of {} loaded blocks.",
                context.loaded_blocks.len()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_context() -> IntentContext {
        IntentContext {
            active_processes: vec!["encoder".into(), "monitor".into()],
            loaded_blocks: vec!["hal".into(), "scheduler".into()],
            current_tier: "Tier 2".into(),
            ram_used_mb: 4096,
            ram_total_mb: 8192,
        }
    }

    #[test]
    fn test_translate_memory_optimize() {
        let ctx = test_context();
        let cmd = IntentEngine::translate("free up memory", &ctx).unwrap();
        assert!(cmd.explanation.contains("4096"));
    }

    #[test]
    fn test_translate_video_optimize() {
        let ctx = test_context();
        let cmd = IntentEngine::translate("optimize for video editing", &ctx).unwrap();
        assert!(cmd.explanation.contains("Tier 2"));
    }

    #[test]
    fn test_translate_block_update() {
        let ctx = test_context();
        let cmd = IntentEngine::translate("update network block", &ctx).unwrap();
        assert!(cmd.explanation.contains("network"));
    }

    #[test]
    fn test_translate_kill() {
        let ctx = test_context();
        let cmd = IntentEngine::translate("kill encoder", &ctx).unwrap();
        assert!(cmd.explanation.contains("encoder"));
    }

    #[test]
    fn test_translate_spawn() {
        let ctx = test_context();
        let cmd = IntentEngine::translate("start inference", &ctx).unwrap();
        assert!(cmd.explanation.contains("Spawning"));
    }

    #[test]
    fn test_translate_health_check() {
        let ctx = test_context();
        let cmd = IntentEngine::translate("check system health", &ctx).unwrap();
        assert!(cmd.explanation.contains("health check"));
    }

    #[test]
    fn test_translate_topology() {
        let ctx = test_context();
        let cmd = IntentEngine::translate("show loaded blocks", &ctx).unwrap();
        assert!(cmd.explanation.contains("2 loaded blocks"));
    }

    #[test]
    fn test_unknown_intent() {
        let ctx = test_context();
        assert!(IntentEngine::translate("xyzzy foobar", &ctx).is_none());
    }
}
