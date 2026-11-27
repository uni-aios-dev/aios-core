//! Compiles a workflow into a WASM module that only calls guarded host imports.

use crate::workflow::{SystemCommand, Workflow};

pub const AIOS_MODULE: &str = "aios";

pub struct WorkflowCompiler;

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Escape a UTF-8 prompt as a WAT string literal (byte-wise, no overlap).
fn wat_string(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\\""),
            0x20..=0x7e => out.push(byte as char),
            other => out.push_str(&format!("\\{other:02x}")),
        }
    }
    out
}

impl WorkflowCompiler {
    /// Compile a workflow to a validated WASM binary.
    pub fn compile_to_wasm(workflow: &Workflow) -> Result<Vec<u8>, String> {
        let wat = Self::generate_wat(workflow);
        wat::parse_str(&wat).map_err(|e| format!("WAT compile error: {e}"))
    }

    /// Generate textual WASM with one import per distinct system command.
    pub fn generate_wat(workflow: &Workflow) -> String {
        let mut wat = String::from("(module\n");
        wat.push_str(&format!(
            "  (import \"{AIOS_MODULE}\" \"log\" (func $log (param i32 i32) (result i32)))\n"
        ));

        let mut commands: Vec<SystemCommand> = Vec::new();
        for step in &workflow.steps {
            if let Some(command) = step.system_command() {
                if !commands.contains(&command) {
                    commands.push(command);
                }
            }
        }
        for command in &commands {
            let name = command.import_name();
            wat.push_str(&format!(
                "  (import \"{AIOS_MODULE}\" \"{name}\" (func ${name} (param i32 i32) (result i32)))\n"
            ));
        }

        wat.push_str("  (memory (export \"memory\") 1)\n");

        let mut offset = 0usize;
        let mut used_labels: Vec<String> = Vec::new();
        for (i, step) in workflow.steps.iter().enumerate() {
            let prompt = &step.prompt;
            let len = prompt.len();
            wat.push_str(&format!(
                "  (data (i32.const {offset}) \"{}\\00\")\n",
                wat_string(prompt)
            ));

            let base_label = sanitize_name(&step.label);
            let mut label = base_label.clone();
            if used_labels.contains(&label) {
                label = format!("{base_label}_{i}");
            }
            used_labels.push(label.clone());

            wat.push_str(&format!(
                "  (func $step_{i} (export \"step_{i}\") (export \"step_{label}\") (result i32)\n"
            ));
            wat.push_str(&format!(
                "    i32.const {offset}\n    i32.const {len}\n    call $log\n    drop\n"
            ));
            if let Some(command) = step.system_command() {
                wat.push_str(&format!(
                    "    i32.const {offset}\n    i32.const {len}\n    call ${}\n    drop\n",
                    command.import_name()
                ));
            }
            wat.push_str("    i32.const 0\n  )\n");
            offset += len + 1;
        }

        wat.push_str(&format!(
            "  (func (export \"step_count\") (result i32)\n    i32.const {}\n  )\n",
            workflow.steps.len()
        ));
        wat.push_str("  (func (export \"init\"))\n");
        wat.push_str("  (func (export \"start\"))\n");
        wat.push_str(")\n");
        wat
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::WorkflowStep;

    fn verify_wasm(wasm: &[u8]) {
        assert_eq!(&wasm[0..4], b"\x00asm", "Invalid WASM magic");
    }

    fn imports(wasm: &[u8]) -> Vec<String> {
        let mut names = Vec::new();
        for payload in wasmparser::Parser::new(0).parse_all(wasm) {
            if let Ok(wasmparser::Payload::ImportSection(reader)) = payload {
                for import in reader.into_iter().flatten() {
                    names.push(format!("{}.{}", import.module, import.name));
                }
            }
        }
        names
    }

    #[test]
    fn test_compile_empty_workflow() {
        let wasm = WorkflowCompiler::compile_to_wasm(&Workflow::new("empty_test")).unwrap();
        verify_wasm(&wasm);
    }

    #[test]
    fn test_compile_with_steps() {
        let mut wf = Workflow::new("test_wf");
        wf.add_step("Spawn", "spawn process browser");
        wf.add_step("Compact", "compact memory");
        let wasm = WorkflowCompiler::compile_to_wasm(&wf).unwrap();
        verify_wasm(&wasm);
    }

    #[test]
    fn test_module_has_aios_import() {
        let mut wf = Workflow::new("import_test");
        wf.add_step("Query", "system status");
        let wasm = WorkflowCompiler::compile_to_wasm(&wf).unwrap();
        assert!(imports(&wasm).contains(&"aios.log".to_string()));
    }

    #[test]
    fn test_module_exports_lifecycle() {
        let mut wf = Workflow::new("lifecycle_test");
        wf.add_step("Query", "system status");
        let wasm = WorkflowCompiler::compile_to_wasm(&wf).unwrap();
        let mut names = Vec::new();
        for payload in wasmparser::Parser::new(0).parse_all(&wasm) {
            if let Ok(wasmparser::Payload::ExportSection(reader)) = payload {
                for export in reader.into_iter().flatten() {
                    names.push(export.name.to_string());
                }
            }
        }
        assert!(names.contains(&"init".to_string()));
        assert!(names.contains(&"memory".to_string()));
        assert!(names.contains(&"step_count".to_string()));
    }

    #[test]
    fn test_module_validates() {
        let mut wf = Workflow::new("valid_test");
        wf.add_step("Spawn", "spawn process test");
        let wasm = WorkflowCompiler::compile_to_wasm(&wf).unwrap();
        assert!(wasmparser::Validator::new().validate_all(&wasm).is_ok());
    }

    #[test]
    fn test_step_exports_correct_count() {
        let mut wf = Workflow::new("step_export");
        wf.add_step("A", "a");
        wf.add_step("B", "b");
        wf.add_step("C", "c");
        let wasm = WorkflowCompiler::compile_to_wasm(&wf).unwrap();
        let mut names = Vec::new();
        for payload in wasmparser::Parser::new(0).parse_all(&wasm) {
            if let Ok(wasmparser::Payload::ExportSection(reader)) = payload {
                for export in reader.into_iter().flatten() {
                    names.push(export.name.to_string());
                }
            }
        }
        assert!(names.contains(&"step_0".to_string()));
        assert!(names.contains(&"step_1".to_string()));
        assert!(names.contains(&"step_2".to_string()));
    }

    #[test]
    fn test_generate_wat_output() {
        let mut wf = Workflow::new("wat_test");
        wf.add_step("Kill", "kill process 3");
        let wat = WorkflowCompiler::generate_wat(&wf);
        assert!(wat.contains("step_0"));
        assert!(wat.contains("init"));
        assert!(wat.contains("memory"));
        assert!(wat.contains(AIOS_MODULE));
    }

    #[test]
    fn test_system_commands_become_guarded_imports() {
        let mut wf = Workflow::new("system");
        wf.steps.push(WorkflowStep {
            label: "net".into(),
            prompt: "net_check https://example.com".into(),
        });
        wf.steps.push(WorkflowStep {
            label: "note".into(),
            prompt: "notify: done".into(),
        });
        let wasm = WorkflowCompiler::compile_to_wasm(&wf).unwrap();
        let found = imports(&wasm);
        assert!(found.contains(&"aios.net_check".to_string()));
        assert!(found.contains(&"aios.notify".to_string()));
        assert!(!found.contains(&"aios.console_cmd".to_string()));
    }

    #[test]
    fn test_long_and_unicode_prompts_do_not_overlap() {
        let mut wf = Workflow::new("unicode");
        wf.add_step("A", &"привет\n\"мир\"\\".repeat(80));
        wf.add_step("B", "second step");
        let wasm = WorkflowCompiler::compile_to_wasm(&wf).unwrap();
        assert!(wasmparser::Validator::new().validate_all(&wasm).is_ok());
    }

    #[test]
    fn test_module_roundtrip() {
        let mut wf = Workflow::new("roundtrip");
        wf.add_step("Test", "test prompt");
        let wasm = WorkflowCompiler::compile_to_wasm(&wf).unwrap();
        assert!(wasmparser::Validator::new().validate_all(&wasm).is_ok());
    }
}
