use crate::workflow::Workflow;

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

impl WorkflowCompiler {
    pub fn compile_to_wasm(workflow: &Workflow) -> Result<Vec<u8>, String> {
        let wat = Self::generate_wat(workflow);
        wat::parse_str(&wat).map_err(|e| format!("WAT compile error: {e}"))
    }

    pub fn generate_wat(workflow: &Workflow) -> String {
        let mut wat = String::new();
        wat.push_str("(module\n");

        wat.push_str(&format!(
            "  (import \"{AIOS_MODULE}\" \"log\" (func $log (param i32 i32) (result i32)))\n"
        ));

        wat.push_str("  (memory (export \"memory\") 1)\n");

        for (i, step) in workflow.steps.iter().enumerate() {
            let prompt = &step.prompt;
            let label = sanitize_name(&step.label);
            let offset = i * 256;
            let safe_prompt = prompt.replace('\\', "\\\\").replace('"', "\\\"");

            wat.push_str(&format!(
                "  (data (i32.const {offset}) \"{safe_prompt}\\00\")\n"
            ));

            wat.push_str(&format!(
                "  (func $step_{i} (export \"step_{i}\") (export \"step_{label}\") (result i32)\n"
            ));
            wat.push_str(&format!(
                "    i32.const {offset}\n    i32.const {}\n    call $log\n    drop\n",
                prompt.len()
            ));
            wat.push_str("    i32.const 0\n  )\n");
        }

        wat.push_str(&format!(
            "  (func (export \"step_count\") (result i32)\n    i32.const {}\n  )\n",
            workflow.steps.len()
        ));

        wat.push_str("  (func (export \"init\") (result i32)\n    i32.const 0\n  )\n");

        wat.push_str("  (func (export \"start\") (result i32)\n    i32.const 1\n  )\n");

        wat.push_str(")\n");
        wat
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verify_wasm(wasm: &[u8]) {
        assert_eq!(&wasm[0..4], b"\x00asm", "Invalid WASM magic");
    }

    #[test]
    fn test_compile_empty_workflow() {
        let wf = Workflow::new("empty_test");
        let wasm = WorkflowCompiler::compile_to_wasm(&wf).unwrap();
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

        let mut has_import = false;
        for payload in wasmparser::Parser::new(0).parse_all(&wasm) {
            if let Ok(wasmparser::Payload::ImportSection(reader)) = payload {
                for import in reader {
                    let i = import.unwrap();
                    if i.module == AIOS_MODULE {
                        has_import = true;
                    }
                }
            }
        }
        assert!(has_import, "Missing aios import");
    }

    #[test]
    fn test_module_exports_lifecycle() {
        let mut wf = Workflow::new("lifecycle_test");
        wf.add_step("Query", "system status");
        let wasm = WorkflowCompiler::compile_to_wasm(&wf).unwrap();

        let mut has_init = false;
        let mut has_memory = false;
        let mut has_step_cnt = false;
        for payload in wasmparser::Parser::new(0).parse_all(&wasm) {
            if let Ok(wasmparser::Payload::ExportSection(reader)) = payload {
                for export in reader {
                    let e = export.unwrap();
                    match e.name {
                        "init" => has_init = true,
                        "memory" => has_memory = true,
                        "step_count" => has_step_cnt = true,
                        _ => {}
                    }
                }
            }
        }
        assert!(has_init, "Missing 'init' export");
        assert!(has_memory, "Missing 'memory' export");
        assert!(has_step_cnt, "Missing 'step_count' export");
    }

    #[test]
    fn test_module_validates() {
        let mut wf = Workflow::new("valid_test");
        wf.add_step("Spawn", "spawn process test");
        let wasm = WorkflowCompiler::compile_to_wasm(&wf).unwrap();
        assert!(
            wasmparser::Validator::new().validate_all(&wasm).is_ok(),
            "WASM validation failed"
        );
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
                for export in reader {
                    names.push(export.unwrap().name.to_string());
                }
            }
        }
        assert!(names.contains(&"step_0".into()), "Missing step_0 export");
        assert!(names.contains(&"step_1".into()), "Missing step_1 export");
        assert!(names.contains(&"step_2".into()), "Missing step_2 export");
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
    fn test_module_roundtrip() {
        let mut wf = Workflow::new("roundtrip");
        wf.add_step("Test", "test prompt");
        let wasm = WorkflowCompiler::compile_to_wasm(&wf).unwrap();
        let re_validated = wasmparser::Validator::new().validate_all(&wasm);
        assert!(re_validated.is_ok());
    }
}
