use crate::format::ExecutableType;
use aios_core::error::{AIOSException, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissingDependency {
    pub name: String,
    pub dep_type: DependencyType,
    pub required_by: String,
    pub search_paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DependencyType {
    WindowsDll,
    LinuxSo,
    LinuxLib,
}

impl DependencyType {
    pub fn extensions(&self) -> &[&str] {
        match self {
            Self::WindowsDll => &[".dll", ".drv", ".ocx"],
            Self::LinuxSo => &[".so", ".so.1", ".so.2", ".so.0"],
            Self::LinuxLib => &[".a", ".la"],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyResolution {
    pub original_name: String,
    pub resolved_path: String,
    pub resolved_type: DependencyType,
    pub loaded_addr: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyScanResult {
    pub executable_type: ExecutableType,
    pub found: Vec<String>,
    pub missing: Vec<MissingDependency>,
    pub already_satisfied: Vec<String>,
}

pub struct DependencyHealer {
    #[allow(dead_code)]
    block_id: u32,
    resolved_cache: HashMap<String, DependencyResolution>,
    search_paths: HashMap<ExecutableType, Vec<String>>,
    max_search_depth: u32,
    loaded_libraries: HashMap<String, u64>,
    next_load_addr: u64,
}

impl DependencyHealer {
    pub fn new(block_id: u32) -> Self {
        let mut search_paths = HashMap::new();
        search_paths.insert(
            ExecutableType::WindowsPe,
            vec![
                "C:\\Windows\\System32".into(),
                "C:\\Windows\\SysWOW64".into(),
                "C:\\Windows".into(),
            ],
        );
        search_paths.insert(
            ExecutableType::LinuxElf,
            vec![
                "/usr/lib".into(),
                "/usr/lib64".into(),
                "/lib".into(),
                "/lib/x86_64-linux-gnu".into(),
                "/usr/local/lib".into(),
            ],
        );

        Self {
            block_id,
            resolved_cache: HashMap::new(),
            search_paths,
            max_search_depth: 3,
            loaded_libraries: HashMap::new(),
            next_load_addr: 0x70000000,
        }
    }

    pub fn with_search_path(mut self, exe_type: ExecutableType, path: String) -> Self {
        self.search_paths.entry(exe_type).or_default().push(path);
        self
    }

    pub fn with_max_depth(mut self, depth: u32) -> Self {
        self.max_search_depth = depth;
        self
    }

    pub fn add_loaded_library(&mut self, name: &str, addr: u64) {
        self.loaded_libraries.insert(name.to_string(), addr);
        self.resolved_cache.insert(
            name.to_string(),
            DependencyResolution {
                original_name: name.to_string(),
                resolved_path: format!("loaded@0x{:X}", addr),
                resolved_type: DependencyType::LinuxSo,
                loaded_addr: addr,
            },
        );
    }

    pub fn loaded_libraries(&self) -> &HashMap<String, u64> {
        &self.loaded_libraries
    }

    pub fn scan_dependencies(
        &self,
        binary_name: &str,
        exe_type: ExecutableType,
        imported_symbols: &[String],
    ) -> DependencyScanResult {
        let mut found = Vec::new();
        let mut missing = Vec::new();
        let mut already_satisfied = Vec::new();

        for symbol in imported_symbols {
            if self.loaded_libraries.contains_key(symbol) {
                already_satisfied.push(symbol.clone());
                continue;
            }

            let dep_type = match exe_type {
                ExecutableType::WindowsPe => DependencyType::WindowsDll,
                ExecutableType::LinuxElf => DependencyType::LinuxSo,
                _ => DependencyType::LinuxSo,
            };

            let extensions = dep_type.extensions();
            let paths = self
                .search_paths
                .get(&exe_type)
                .cloned()
                .unwrap_or_default();
            let mut found_dep = false;

            for ext in extensions {
                let filename = format!("{}{}", symbol, ext);
                for path in &paths {
                    let full_path = format!("{}/{}", path, filename);
                    if self.simulate_file_exists(&full_path) {
                        found.push(symbol.clone());
                        found_dep = true;
                        break;
                    }
                }
                if found_dep {
                    break;
                }
            }

            if !found_dep {
                missing.push(MissingDependency {
                    name: symbol.clone(),
                    dep_type,
                    required_by: binary_name.to_string(),
                    search_paths: paths,
                });
            }
        }

        DependencyScanResult {
            executable_type: exe_type,
            found,
            missing,
            already_satisfied,
        }
    }

    pub fn resolve_missing(
        &mut self,
        dep: &MissingDependency,
        auto_download: bool,
    ) -> Result<DependencyResolution> {
        if let Some(cached) = self.resolved_cache.get(&dep.name) {
            return Ok(cached.clone());
        }

        if !auto_download {
            return Err(AIOSException::BlockNotFound(format!(
                "Missing dependency: {} (auto-download disabled)",
                dep.name
            )));
        }

        let addr = self.next_load_addr;
        self.next_load_addr += 0x10000;

        let resolution = DependencyResolution {
            original_name: dep.name.clone(),
            resolved_path: format!("sandbox://{}/{}", dep.required_by, dep.name),
            resolved_type: dep.dep_type,
            loaded_addr: addr,
        };

        self.resolved_cache
            .insert(dep.name.clone(), resolution.clone());
        self.loaded_libraries.insert(dep.name.clone(), addr);

        log::info!(
            "DependencyHealer: Auto-loaded '{}' for '{}' at 0x{:X}",
            dep.name,
            dep.required_by,
            addr
        );

        Ok(resolution)
    }

    pub fn heal_dependencies(
        &mut self,
        binary_name: &str,
        exe_type: ExecutableType,
        imported_symbols: &[String],
        auto_download: bool,
    ) -> Result<Vec<DependencyResolution>> {
        let scan = self.scan_dependencies(binary_name, exe_type, imported_symbols);

        let mut resolutions = Vec::new();
        for dep in scan.missing {
            let resolution = self.resolve_missing(&dep, auto_download)?;
            resolutions.push(resolution);
        }

        Ok(resolutions)
    }

    pub fn cached_resolutions(&self) -> &HashMap<String, DependencyResolution> {
        &self.resolved_cache
    }

    pub fn clear_cache(&mut self) {
        self.resolved_cache.clear();
    }

    fn simulate_file_exists(&self, _path: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dependency_type_extensions() {
        assert!(DependencyType::WindowsDll.extensions().contains(&".dll"));
        assert!(DependencyType::LinuxSo.extensions().contains(&".so"));
        assert!(DependencyType::LinuxLib.extensions().contains(&".a"));
    }

    #[test]
    fn test_scan_no_missing() {
        let healer =
            DependencyHealer::new(1).with_search_path(ExecutableType::LinuxElf, "/usr/lib".into());
        let result = healer.scan_dependencies("test", ExecutableType::LinuxElf, &["libc".into()]);
        assert!(result.already_satisfied.is_empty());
        assert!(result.found.is_empty());
        assert_eq!(result.missing.len(), 1);
        assert_eq!(result.missing[0].name, "libc");
    }

    #[test]
    fn test_scan_with_loaded_library() {
        let mut healer = DependencyHealer::new(1);
        healer.add_loaded_library("libc", 0x70000000);
        let result = healer.scan_dependencies("test", ExecutableType::LinuxElf, &["libc".into()]);
        assert_eq!(result.already_satisfied.len(), 1);
        assert!(result.missing.is_empty());
    }

    #[test]
    fn test_heal_auto_download() {
        let mut healer = DependencyHealer::new(1);
        let resolutions = healer
            .heal_dependencies(
                "test.exe",
                ExecutableType::WindowsPe,
                &["user32".into(), "kernel32".into()],
                true,
            )
            .unwrap();
        assert_eq!(resolutions.len(), 2);
        assert!(healer.loaded_libraries().contains_key("user32"));
        assert!(healer.loaded_libraries().contains_key("kernel32"));
    }

    #[test]
    fn test_heal_no_auto_download() {
        let mut healer = DependencyHealer::new(1);
        let result = healer.heal_dependencies(
            "test.exe",
            ExecutableType::WindowsPe,
            &["user32".into()],
            false,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_heal_cached() {
        let mut healer = DependencyHealer::new(1);
        let r1 = healer
            .heal_dependencies(
                "test.exe",
                ExecutableType::WindowsPe,
                &["user32".into()],
                true,
            )
            .unwrap();
        assert_eq!(r1.len(), 1);
        let addr1 = r1[0].loaded_addr;
        let r2 = healer
            .heal_dependencies(
                "test.exe",
                ExecutableType::WindowsPe,
                &["user32".into()],
                true,
            )
            .unwrap();
        assert_eq!(r2.len(), 0);
        assert_eq!(healer.loaded_libraries().get("user32"), Some(&addr1));
    }

    #[test]
    fn test_clear_cache() {
        let mut healer = DependencyHealer::new(1);
        healer
            .heal_dependencies(
                "test.exe",
                ExecutableType::WindowsPe,
                &["user32".into()],
                true,
            )
            .unwrap();
        assert!(!healer.cached_resolutions().is_empty());
        healer.clear_cache();
        assert!(healer.cached_resolutions().is_empty());
    }

    #[test]
    fn test_with_search_path() {
        let healer = DependencyHealer::new(1)
            .with_search_path(ExecutableType::WindowsPe, "D:\\custom".into());
        let paths = healer.search_paths.get(&ExecutableType::WindowsPe).unwrap();
        assert!(paths.contains(&"D:\\custom".to_string()));
    }

    #[test]
    fn test_with_max_depth() {
        let healer = DependencyHealer::new(1).with_max_depth(5);
        assert_eq!(healer.max_search_depth, 5);
    }

    #[test]
    fn test_dependency_scan_result_fields() {
        let mut healer = DependencyHealer::new(1);
        healer.add_loaded_library("existing", 0x70000000);
        let result = healer.scan_dependencies(
            "app.exe",
            ExecutableType::WindowsPe,
            &["existing".into(), "missing1".into(), "missing2".into()],
        );
        assert_eq!(result.already_satisfied.len(), 1);
        assert_eq!(result.missing.len(), 2);
        assert_eq!(result.missing[0].dep_type, DependencyType::WindowsDll);
    }

    #[test]
    fn test_resolve_dependency_address() {
        let mut healer = DependencyHealer::new(1);
        let dep = MissingDependency {
            name: "test.dll".into(),
            dep_type: DependencyType::WindowsDll,
            required_by: "app.exe".into(),
            search_paths: vec![],
        };
        let res = healer.resolve_missing(&dep, true).unwrap();
        assert_eq!(res.loaded_addr, 0x70000000);
        assert!(res.resolved_path.starts_with("sandbox://"));
    }

    #[test]
    fn test_heal_multiple_dependencies() {
        let mut healer = DependencyHealer::new(1);
        let res = healer
            .heal_dependencies(
                "complex.exe",
                ExecutableType::WindowsPe,
                &[
                    "user32".into(),
                    "kernel32".into(),
                    "ntdll".into(),
                    "gdi32".into(),
                ],
                true,
            )
            .unwrap();
        assert_eq!(res.len(), 4);
        let addrs: Vec<u64> = res.iter().map(|r| r.loaded_addr).collect();
        let unique: std::collections::HashSet<u64> = addrs.into_iter().collect();
        assert_eq!(unique.len(), 4);
    }
}
