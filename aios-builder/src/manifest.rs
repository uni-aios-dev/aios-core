use aios_security::capability::Capability;

pub struct AutoManifestGenerator {
    capability_map: Vec<(&'static str, Capability)>,
}

impl Default for AutoManifestGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl AutoManifestGenerator {
    pub fn new() -> Self {
        Self {
            capability_map: vec![
                ("net_bind", Capability::NetBind),
                ("net_connect", Capability::NetConnect),
                ("net_listen", Capability::NetListen),
                ("fs_read", Capability::FsRead),
                ("fs_write", Capability::FsWrite),
                ("fs_delete", Capability::FsDelete),
                ("hw_access", Capability::HwAccess),
                ("mem_alloc", Capability::MemAlloc),
                ("mem_share", Capability::MemShare),
                ("sched_modify", Capability::SchedModify),
                ("block_load", Capability::BlockLoad),
                ("block_unload", Capability::BlockUnload),
                ("process_spawn", Capability::ProcessSpawn),
                ("process_kill", Capability::ProcessKill),
                ("system_config", Capability::SystemConfig),
            ],
        }
    }

    pub fn from_wasm_binary(&self, wasm_bytes: &[u8]) -> Result<Vec<Capability>, String> {
        let mut caps = Vec::new();

        let validator = wasmparser::Validator::new();
        let parser = wasmparser::Parser::new(0);

        for payload in parser.parse_all(wasm_bytes) {
            match payload {
                Ok(wasmparser::Payload::ImportSection(reader)) => {
                    for import in reader {
                        let import = import.map_err(|e| format!("Import parse error: {e}"))?;
                        if let Some(cap) = self.match_import_name(import.module, import.name) {
                            if !caps.contains(&cap) {
                                caps.push(cap);
                            }
                        }
                    }
                }
                Ok(wasmparser::Payload::ExportSection(reader)) => {
                    for export in reader {
                        let export = export.map_err(|e| format!("Export parse error: {e}"))?;
                        if let Some(cap) = self.match_export_name(export.name) {
                            if !caps.contains(&cap) {
                                caps.push(cap);
                            }
                        }
                    }
                }
                Err(e) => return Err(format!("WASM parse error: {e}")),
                _ => {}
            }
        }

        let _ = validator;
        Ok(caps)
    }

    pub fn generate_json_manifest(
        &self,
        wasm_bytes: &[u8],
        name: &str,
        version: &str,
    ) -> Result<String, String> {
        let caps = self.from_wasm_binary(wasm_bytes)?;
        let cap_names: Vec<String> = caps.iter().map(|c| c.name().to_string()).collect();

        let manifest = serde_json::json!({
            "name": name,
            "version": version,
            "capabilities": cap_names,
        });

        serde_json::to_string_pretty(&manifest).map_err(|e| format!("Serialization error: {e}"))
    }

    fn match_import_name(&self, module: &str, name: &str) -> Option<Capability> {
        let full = format!("{module}.{name}");
        for (prefix, cap) in &self.capability_map {
            if module == *prefix || full.starts_with(*prefix) || name == *prefix {
                return Some(*cap);
            }
        }
        None
    }

    fn match_export_name(&self, name: &str) -> Option<Capability> {
        for (prefix, cap) in &self.capability_map {
            if name == *prefix || name.starts_with(*prefix) || name.contains(*prefix) {
                return Some(*cap);
            }
        }
        None
    }

    pub fn from_workflow_intents(&self, steps: &[(&str, &str)]) -> Vec<Capability> {
        let mut caps = Vec::new();
        let mut push = |c: Capability| {
            if !caps.contains(&c) {
                caps.push(c);
            }
        };
        for (_label, prompt) in steps {
            let lower = prompt.to_lowercase();
            if lower.contains("spawn") || lower.contains("create") {
                push(Capability::ProcessSpawn);
            }
            if lower.contains("kill") || lower.contains("stop") || lower.contains("terminate") {
                push(Capability::ProcessKill);
            }
            if lower.contains("load") && (lower.contains("block") || lower.contains("wasm")) {
                push(Capability::BlockLoad);
            }
            if lower.contains("unload") {
                push(Capability::BlockUnload);
            }
            if lower.contains("compact") || lower.contains("memory") {
                push(Capability::MemAlloc);
            }
            if lower.contains("net") || lower.contains("network") || lower.contains("connect") {
                push(Capability::NetConnect);
            }
            if lower.contains("bind") || lower.contains("listen") {
                push(Capability::NetBind);
            }
            if lower.contains("fs")
                || lower.contains("file")
                || lower.contains("read")
                || lower.contains("write")
            {
                push(Capability::FsRead);
                push(Capability::FsWrite);
            }
            if lower.contains("config") || lower.contains("setting") {
                push(Capability::SystemConfig);
            }
        }
        caps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_wasm() -> Vec<u8> {
        wat::parse_str(
            r#"(module
                (func (export "init") (result i32) i32.const 0)
                (func (export "process_spawn") (result i32) i32.const 1)
                (memory (export "memory") 1)
            )"#,
        )
        .expect("WAT parse failed")
    }

    fn wasm_with_imports() -> Vec<u8> {
        wat::parse_str(
            r#"(module
                (import "net_bind" "bind" (func (param i32 i32) (result i32)))
                (func (export "start") (result i32) i32.const 0)
                (memory (export "memory") 1)
            )"#,
        )
        .expect("WAT parse failed")
    }

    #[test]
    fn test_detect_cap_from_export_name() {
        let gen = AutoManifestGenerator::new();
        let wasm = minimal_wasm();
        let caps = gen.from_wasm_binary(&wasm).unwrap();
        assert!(caps.contains(&Capability::ProcessSpawn));
    }

    #[test]
    fn test_detect_cap_from_import() {
        let gen = AutoManifestGenerator::new();
        let wasm = wasm_with_imports();
        let caps = gen.from_wasm_binary(&wasm).unwrap();
        assert!(caps.contains(&Capability::NetBind));
    }

    #[test]
    fn test_generate_json_manifest() {
        let gen = AutoManifestGenerator::new();
        let wasm = minimal_wasm();
        let json = gen
            .generate_json_manifest(&wasm, "test_block", "1.0.0")
            .unwrap();
        assert!(json.contains("test_block"));
        assert!(json.contains("CAP_PROCESS_SPAWN"));
    }

    #[test]
    fn test_workflow_detect_caps() {
        let gen = AutoManifestGenerator::new();
        let steps = vec![
            ("Spawn Process", "spawn process browser"),
            ("Compact Memory", "compact memory now"),
        ];
        let caps = gen.from_workflow_intents(&steps);
        assert!(caps.contains(&Capability::ProcessSpawn));
        assert!(caps.contains(&Capability::MemAlloc));
        assert_eq!(caps.len(), 2);
    }

    #[test]
    fn test_workflow_detect_multiple_caps() {
        let gen = AutoManifestGenerator::new();
        let steps = vec![
            ("Kill", "kill process 5"),
            ("Network", "listen on port 8080"),
            ("File Read", "read file /tmp/test"),
        ];
        let caps = gen.from_workflow_intents(&steps);
        assert!(caps.contains(&Capability::ProcessKill));
        assert!(caps.contains(&Capability::NetBind));
        assert!(caps.contains(&Capability::FsRead));
        assert!(caps.contains(&Capability::FsWrite));
    }
}
