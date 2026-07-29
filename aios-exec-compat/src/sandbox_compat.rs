use crate::format::{CompatCapability, ExecutableType};
use aios_core::error::{AIOSException, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompatSandboxConfig {
    pub max_memory_mb: u64,
    pub max_cpu_time_ms: u64,
    pub max_open_files: u32,
    pub max_threads: u32,
    pub allowed_capabilities: Vec<CompatCapability>,
    pub blocked_syscalls: Vec<String>,
    pub network_sandboxed: bool,
    pub filesystem_root: String,
}

impl Default for CompatSandboxConfig {
    fn default() -> Self {
        Self {
            max_memory_mb: 512,
            max_cpu_time_ms: 10_000,
            max_open_files: 256,
            max_threads: 16,
            allowed_capabilities: vec![CompatCapability::FilesystemRead],
            blocked_syscalls: Vec::new(),
            network_sandboxed: true,
            filesystem_root: "/sandbox".into(),
        }
    }
}

impl CompatSandboxConfig {
    pub fn for_executable_type(exe_type: ExecutableType) -> Self {
        match exe_type {
            ExecutableType::AiosNative => Self {
                max_memory_mb: 1024,
                max_cpu_time_ms: 30_000,
                max_open_files: 512,
                max_threads: 32,
                allowed_capabilities: vec![
                    CompatCapability::FilesystemRead,
                    CompatCapability::FilesystemWrite,
                    CompatCapability::ProcessCreate,
                    CompatCapability::NetworkAccess,
                ],
                network_sandboxed: false,
                filesystem_root: "/".into(),
                ..Default::default()
            },
            ExecutableType::LinuxElf => Self {
                max_memory_mb: 512,
                max_cpu_time_ms: 10_000,
                max_open_files: 256,
                max_threads: 16,
                allowed_capabilities: vec![
                    CompatCapability::FilesystemRead,
                    CompatCapability::FilesystemWrite,
                    CompatCapability::ProcessCreate,
                    CompatCapability::NetworkAccess,
                ],
                network_sandboxed: true,
                filesystem_root: "/sandbox/posix".into(),
                ..Default::default()
            },
            ExecutableType::WindowsPe => Self {
                max_memory_mb: 512,
                max_cpu_time_ms: 10_000,
                max_open_files: 256,
                max_threads: 16,
                allowed_capabilities: vec![
                    CompatCapability::FilesystemRead,
                    CompatCapability::FilesystemWrite,
                    CompatCapability::ProcessCreate,
                    CompatCapability::NetworkAccess,
                    CompatCapability::RegistryAccess,
                    CompatCapability::WinApiCompat,
                ],
                network_sandboxed: true,
                filesystem_root: "/sandbox/win32".into(),
                ..Default::default()
            },
            ExecutableType::Unknown => Self::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompatProcess {
    pub pid: u64,
    pub name: String,
    pub exe_type: ExecutableType,
    pub config: CompatSandboxConfig,
    pub granted_capabilities: HashSet<CompatCapability>,
    pub memory_used_mb: u64,
    pub open_files: u32,
    pub active_threads: u32,
    pub syscall_count: u64,
    pub is_terminated: bool,
}

impl CompatProcess {
    pub fn new(pid: u64, name: &str, exe_type: ExecutableType) -> Self {
        let config = CompatSandboxConfig::for_executable_type(exe_type);
        let granted_capabilities: HashSet<CompatCapability> =
            config.allowed_capabilities.iter().copied().collect();
        Self {
            pid,
            name: name.to_string(),
            exe_type,
            config,
            granted_capabilities,
            memory_used_mb: 0,
            open_files: 0,
            active_threads: 1,
            syscall_count: 0,
            is_terminated: false,
        }
    }

    pub fn check_capability(&self, cap: &CompatCapability) -> bool {
        self.granted_capabilities.contains(cap)
    }

    pub fn check_memory_limit(&self) -> Result<()> {
        if self.memory_used_mb > self.config.max_memory_mb {
            return Err(AIOSException::IPCError(format!(
                "Process '{}' exceeded memory limit: {}/{} MB",
                self.name, self.memory_used_mb, self.config.max_memory_mb,
            )));
        }
        Ok(())
    }

    pub fn check_file_limit(&self) -> Result<()> {
        if self.open_files > self.config.max_open_files {
            return Err(AIOSException::IPCError(format!(
                "Process '{}' exceeded open file limit: {}/{}",
                self.name, self.open_files, self.config.max_open_files,
            )));
        }
        Ok(())
    }

    pub fn check_thread_limit(&self) -> Result<()> {
        if self.active_threads > self.config.max_threads {
            return Err(AIOSException::IPCError(format!(
                "Process '{}' exceeded thread limit: {}/{}",
                self.name, self.active_threads, self.config.max_threads,
            )));
        }
        Ok(())
    }

    pub fn check_syscall(&self, syscall_name: &str) -> Result<()> {
        if self
            .config
            .blocked_syscalls
            .iter()
            .any(|s| s == syscall_name)
        {
            return Err(AIOSException::IPCError(format!(
                "Process '{}' blocked syscall: {}",
                self.name, syscall_name,
            )));
        }
        Ok(())
    }

    pub fn allocate_memory(&mut self, mb: u64) -> Result<()> {
        self.memory_used_mb += mb;
        self.check_memory_limit()
    }

    pub fn open_file(&mut self) -> Result<()> {
        self.open_files += 1;
        self.check_file_limit()
    }

    pub fn close_file(&mut self) {
        self.open_files = self.open_files.saturating_sub(1);
    }

    pub fn spawn_thread(&mut self) -> Result<()> {
        self.active_threads += 1;
        self.check_thread_limit()
    }

    pub fn terminate(&mut self) {
        self.is_terminated = true;
    }
}

pub struct CompatSandboxManager {
    processes: Vec<CompatProcess>,
    next_pid: u64,
    max_processes: u32,
}

impl CompatSandboxManager {
    pub fn new() -> Self {
        Self {
            processes: Vec::new(),
            next_pid: 1000,
            max_processes: 64,
        }
    }

    pub fn with_max_processes(mut self, max: u32) -> Self {
        self.max_processes = max;
        self
    }

    pub fn spawn_process(&mut self, name: &str, exe_type: ExecutableType) -> Result<u64> {
        if self.processes.len() >= self.max_processes as usize {
            return Err(AIOSException::IPCError(format!(
                "Max compat processes reached: {}",
                self.max_processes,
            )));
        }

        let pid = self.next_pid;
        self.next_pid += 1;

        let proc = CompatProcess::new(pid, name, exe_type);
        self.processes.push(proc);

        log::info!(
            "CompatSandbox: Spawned '{}' ({:?}) pid={}",
            name,
            exe_type,
            pid
        );
        Ok(pid)
    }

    pub fn get_process(&self, pid: u64) -> Option<&CompatProcess> {
        self.processes.iter().find(|p| p.pid == pid)
    }

    pub fn get_process_mut(&mut self, pid: u64) -> Option<&mut CompatProcess> {
        self.processes.iter_mut().find(|p| p.pid == pid)
    }

    pub fn terminate_process(&mut self, pid: u64) -> Result<()> {
        let proc = self
            .get_process_mut(pid)
            .ok_or_else(|| AIOSException::BlockNotFound(format!("Compat process {}", pid)))?;
        proc.terminate();
        Ok(())
    }

    pub fn active_processes(&self) -> Vec<&CompatProcess> {
        self.processes.iter().filter(|p| !p.is_terminated).collect()
    }

    pub fn all_processes(&self) -> &[CompatProcess] {
        &self.processes
    }

    pub fn process_count(&self) -> usize {
        self.processes.iter().filter(|p| !p.is_terminated).count()
    }

    pub fn total_memory_used(&self) -> u64 {
        self.processes
            .iter()
            .filter(|p| !p.is_terminated)
            .map(|p| p.memory_used_mb)
            .sum()
    }

    pub fn cleanup_terminated(&mut self) {
        self.processes.retain(|p| !p.is_terminated);
    }
}

impl Default for CompatSandboxManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sandbox_config_default() {
        let cfg = CompatSandboxConfig::default();
        assert_eq!(cfg.max_memory_mb, 512);
        assert!(cfg.network_sandboxed);
        assert_eq!(cfg.filesystem_root, "/sandbox");
    }

    #[test]
    fn test_sandbox_config_for_pe() {
        let cfg = CompatSandboxConfig::for_executable_type(ExecutableType::WindowsPe);
        assert!(cfg
            .allowed_capabilities
            .contains(&CompatCapability::WinApiCompat));
        assert!(cfg
            .allowed_capabilities
            .contains(&CompatCapability::RegistryAccess));
        assert_eq!(cfg.filesystem_root, "/sandbox/win32");
    }

    #[test]
    fn test_sandbox_config_for_elf() {
        let cfg = CompatSandboxConfig::for_executable_type(ExecutableType::LinuxElf);
        assert!(cfg.network_sandboxed);
        assert_eq!(cfg.filesystem_root, "/sandbox/posix");
    }

    #[test]
    fn test_sandbox_config_for_native() {
        let cfg = CompatSandboxConfig::for_executable_type(ExecutableType::AiosNative);
        assert!(!cfg.network_sandboxed);
        assert_eq!(cfg.filesystem_root, "/");
    }

    #[test]
    fn test_compat_process_create() {
        let proc = CompatProcess::new(1, "test.exe", ExecutableType::WindowsPe);
        assert_eq!(proc.pid, 1);
        assert_eq!(proc.exe_type, ExecutableType::WindowsPe);
        assert!(!proc.is_terminated);
        assert_eq!(proc.active_threads, 1);
    }

    #[test]
    fn test_check_capability() {
        let proc = CompatProcess::new(1, "test.exe", ExecutableType::WindowsPe);
        assert!(proc.check_capability(&CompatCapability::WinApiCompat));
        assert!(proc.check_capability(&CompatCapability::FilesystemRead));
    }

    #[test]
    fn test_check_memory_limit() {
        let mut proc = CompatProcess::new(1, "test.exe", ExecutableType::WindowsPe);
        assert!(proc.check_memory_limit().is_ok());
        proc.memory_used_mb = 513;
        assert!(proc.check_memory_limit().is_err());
    }

    #[test]
    fn test_allocate_memory_within_limit() {
        let mut proc = CompatProcess::new(1, "test.exe", ExecutableType::WindowsPe);
        assert!(proc.allocate_memory(256).is_ok());
        assert_eq!(proc.memory_used_mb, 256);
    }

    #[test]
    fn test_allocate_memory_exceeds_limit() {
        let mut proc = CompatProcess::new(1, "test.exe", ExecutableType::WindowsPe);
        assert!(proc.allocate_memory(512).is_ok());
        assert!(proc.allocate_memory(1).is_err());
    }

    #[test]
    fn test_check_file_limit() {
        let mut proc = CompatProcess::new(1, "test.exe", ExecutableType::WindowsPe);
        for _ in 0..256 {
            assert!(proc.open_file().is_ok());
        }
        assert!(proc.open_file().is_err());
    }

    #[test]
    fn test_close_file() {
        let mut proc = CompatProcess::new(1, "test.exe", ExecutableType::WindowsPe);
        proc.open_file().unwrap();
        assert_eq!(proc.open_files, 1);
        proc.close_file();
        assert_eq!(proc.open_files, 0);
    }

    #[test]
    fn test_check_thread_limit() {
        let mut proc = CompatProcess::new(1, "test.exe", ExecutableType::WindowsPe);
        for _ in 0..15 {
            assert!(proc.spawn_thread().is_ok());
        }
        assert!(proc.spawn_thread().is_err());
    }

    #[test]
    fn test_check_syscall_blocked() {
        let mut config = CompatSandboxConfig::default();
        config.blocked_syscalls.push("sys_fork".into());
        let proc = CompatProcess {
            config,
            ..CompatProcess::new(1, "test", ExecutableType::LinuxElf)
        };
        assert!(proc.check_syscall("sys_fork").is_err());
        assert!(proc.check_syscall("sys_read").is_ok());
    }

    #[test]
    fn test_terminate() {
        let mut proc = CompatProcess::new(1, "test.exe", ExecutableType::WindowsPe);
        assert!(!proc.is_terminated);
        proc.terminate();
        assert!(proc.is_terminated);
    }

    #[test]
    fn test_sandbox_manager_spawn() {
        let mut mgr = CompatSandboxManager::new();
        let pid = mgr
            .spawn_process("test.exe", ExecutableType::WindowsPe)
            .unwrap();
        assert_eq!(pid, 1000);
        assert_eq!(mgr.process_count(), 1);
    }

    #[test]
    fn test_sandbox_manager_max_processes() {
        let mut mgr = CompatSandboxManager::new().with_max_processes(2);
        mgr.spawn_process("p1.exe", ExecutableType::WindowsPe)
            .unwrap();
        mgr.spawn_process("p2.exe", ExecutableType::WindowsPe)
            .unwrap();
        assert!(mgr
            .spawn_process("p3.exe", ExecutableType::WindowsPe)
            .is_err());
    }

    #[test]
    fn test_sandbox_manager_get_process() {
        let mut mgr = CompatSandboxManager::new();
        let pid = mgr
            .spawn_process("test.exe", ExecutableType::WindowsPe)
            .unwrap();
        assert!(mgr.get_process(pid).is_some());
        assert!(mgr.get_process(999).is_none());
    }

    #[test]
    fn test_sandbox_manager_terminate() {
        let mut mgr = CompatSandboxManager::new();
        let pid = mgr
            .spawn_process("test.exe", ExecutableType::WindowsPe)
            .unwrap();
        mgr.terminate_process(pid).unwrap();
        assert!(mgr.get_process(pid).unwrap().is_terminated);
    }

    #[test]
    fn test_sandbox_manager_total_memory() {
        let mut mgr = CompatSandboxManager::new();
        let pid = mgr
            .spawn_process("test.exe", ExecutableType::WindowsPe)
            .unwrap();
        mgr.get_process_mut(pid)
            .unwrap()
            .allocate_memory(256)
            .unwrap();
        assert_eq!(mgr.total_memory_used(), 256);
    }

    #[test]
    fn test_cleanup_terminated() {
        let mut mgr = CompatSandboxManager::new();
        let p1 = mgr
            .spawn_process("p1.exe", ExecutableType::WindowsPe)
            .unwrap();
        let _p2 = mgr
            .spawn_process("p2.exe", ExecutableType::WindowsPe)
            .unwrap();
        mgr.terminate_process(p1).unwrap();
        assert_eq!(mgr.all_processes().len(), 2);
        mgr.cleanup_terminated();
        assert_eq!(mgr.all_processes().len(), 1);
    }

    #[test]
    fn test_active_processes() {
        let mut mgr = CompatSandboxManager::new();
        let p1 = mgr
            .spawn_process("p1.exe", ExecutableType::WindowsPe)
            .unwrap();
        let _p2 = mgr
            .spawn_process("p2.exe", ExecutableType::WindowsPe)
            .unwrap();
        mgr.terminate_process(p1).unwrap();
        assert_eq!(mgr.active_processes().len(), 1);
    }

    #[test]
    fn test_sandbox_manager_default() {
        let mgr = CompatSandboxManager::default();
        assert_eq!(mgr.process_count(), 0);
    }
}
