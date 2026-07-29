use aios_bridge::intent_engine::{
    BlockAction, IntentParser, MetricType, ProcessAction, UserIntent,
};

#[test]
fn test_parse_show_processes_en() {
    let parser = IntentParser::new();
    let intent = parser.parse("show processes");
    assert_eq!(
        intent,
        UserIntent::ProcessControl {
            action: ProcessAction::List,
            target: String::new(),
        }
    );
}

#[test]
fn test_parse_show_processes_ru() {
    let parser = IntentParser::new();
    let intent = parser.parse("покажи процессы");
    assert_eq!(
        intent,
        UserIntent::ProcessControl {
            action: ProcessAction::List,
            target: String::new(),
        }
    );
}

#[test]
fn test_parse_kill_process_en() {
    let parser = IntentParser::new();
    let intent = parser.parse("kill process 42");
    assert_eq!(
        intent,
        UserIntent::ProcessControl {
            action: ProcessAction::Kill,
            target: "42".into(),
        }
    );
}

#[test]
fn test_parse_kill_process_ru() {
    let parser = IntentParser::new();
    let intent = parser.parse("убей процесс 7");
    assert_eq!(
        intent,
        UserIntent::ProcessControl {
            action: ProcessAction::Kill,
            target: "7".into(),
        }
    );
}

#[test]
fn test_parse_spawn_process_en() {
    let parser = IntentParser::new();
    let intent = parser.parse("spawn net_watcher");
    assert_eq!(
        intent,
        UserIntent::ProcessControl {
            action: ProcessAction::Spawn,
            target: "net_watcher".into(),
        }
    );
}

#[test]
fn test_parse_spawn_process_ru() {
    let parser = IntentParser::new();
    let intent = parser.parse("запусти монитор");
    match intent {
        UserIntent::ProcessControl {
            action: ProcessAction::Spawn,
            target,
        } => {
            assert_eq!(target, "монитор");
        }
        other => panic!("Expected Spawn, got {other:?}"),
    }
}

#[test]
fn test_parse_system_status_en() {
    let parser = IntentParser::new();
    let intent = parser.parse("system status");
    assert_eq!(
        intent,
        UserIntent::SystemQuery {
            metric: MetricType::All
        }
    );
}

#[test]
fn test_parse_system_status_ru() {
    let parser = IntentParser::new();
    let intent = parser.parse("статус системы");
    assert_eq!(
        intent,
        UserIntent::SystemQuery {
            metric: MetricType::All
        }
    );
}

#[test]
fn test_parse_show_blocks_en() {
    let parser = IntentParser::new();
    let intent = parser.parse("show blocks");
    assert_eq!(
        intent,
        UserIntent::BlockManagement {
            action: BlockAction::List,
            wasm_path: None,
            block_name: None,
        }
    );
}

#[test]
fn test_parse_show_blocks_ru() {
    let parser = IntentParser::new();
    let intent = parser.parse("покажи блоки");
    assert_eq!(
        intent,
        UserIntent::BlockManagement {
            action: BlockAction::List,
            wasm_path: None,
            block_name: None,
        }
    );
}

#[test]
fn test_parse_load_block_en() {
    let parser = IntentParser::new();
    let intent = parser.parse("load block my_block");
    match intent {
        UserIntent::BlockManagement {
            action: BlockAction::Load,
            block_name: Some(name),
            ..
        } => {
            assert_eq!(name, "my_block");
        }
        other => panic!("Expected Block Load, got {other:?}"),
    }
}

#[test]
fn test_parse_load_block_ru() {
    let parser = IntentParser::new();
    let intent = parser.parse("загрузи блок net_watcher");
    match intent {
        UserIntent::BlockManagement {
            action: BlockAction::Load,
            block_name: Some(name),
            ..
        } => {
            assert_eq!(name, "net_watcher");
        }
        other => panic!("Expected Block Load, got {other:?}"),
    }
}

#[test]
fn test_parse_unload_block_en() {
    let parser = IntentParser::new();
    let intent = parser.parse("unload block old_block");
    match intent {
        UserIntent::BlockManagement {
            action: BlockAction::Unload,
            block_name: Some(name),
            ..
        } => {
            assert_eq!(name, "old_block");
        }
        other => panic!("Expected Block Unload, got {other:?}"),
    }
}

#[test]
fn test_parse_memory_compaction_en() {
    let parser = IntentParser::new();
    let intent = parser.parse("compress memory");
    assert_eq!(intent, UserIntent::MemoryCompaction);
}

#[test]
fn test_parse_memory_compaction_ru() {
    let parser = IntentParser::new();
    let intent = parser.parse("сожми память");
    assert_eq!(intent, UserIntent::MemoryCompaction);
}

#[test]
fn test_parse_unknown() {
    let parser = IntentParser::new();
    let intent = parser.parse("do something completely different");
    assert!(matches!(intent, UserIntent::Unknown { .. }));
}

#[test]
fn test_execution_plan_kill_requires_process_kill() {
    let parser = IntentParser::new();
    let intent = parser.parse("kill process 5");
    let plan = parser.create_execution_plan(&intent);
    assert!(plan
        .required_capabilities
        .iter()
        .any(|c| c.name() == "CAP_PROCESS_KILL"));
    assert!(!plan.steps.is_empty());
}

#[test]
fn test_execution_plan_load_requires_block_load() {
    let parser = IntentParser::new();
    let intent = parser.parse("load block test");
    let plan = parser.create_execution_plan(&intent);
    assert!(plan
        .required_capabilities
        .iter()
        .any(|c| c.name() == "CAP_BLOCK_LOAD"));
}

#[test]
fn test_execution_plan_query_no_caps() {
    let parser = IntentParser::new();
    let intent = parser.parse("status");
    let plan = parser.create_execution_plan(&intent);
    assert!(plan.required_capabilities.is_empty());
}

#[test]
fn test_parse_list_processes_alt() {
    let parser = IntentParser::new();
    assert!(matches!(
        parser.parse("list processes"),
        UserIntent::ProcessControl {
            action: ProcessAction::List,
            ..
        }
    ));
}

#[test]
fn test_parse_cpu_query() {
    let parser = IntentParser::new();
    assert_eq!(
        parser.parse("cpu usage"),
        UserIntent::SystemQuery {
            metric: MetricType::Cpu
        }
    );
}

#[test]
fn test_parse_memory_query_ru() {
    let parser = IntentParser::new();
    assert_eq!(
        parser.parse("память"),
        UserIntent::SystemQuery {
            metric: MetricType::Memory
        }
    );
}

#[test]
fn test_parse_empty_string() {
    let parser = IntentParser::new();
    let intent = parser.parse("");
    assert!(matches!(intent, UserIntent::Unknown { .. }));
}

#[test]
fn test_execution_plan_spawn_requires_process_spawn() {
    let parser = IntentParser::new();
    let intent = parser.parse("spawn worker");
    let plan = parser.create_execution_plan(&intent);
    assert!(plan
        .required_capabilities
        .iter()
        .any(|c| c.name() == "CAP_PROCESS_SPAWN"));
}
