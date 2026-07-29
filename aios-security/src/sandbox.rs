use crate::capability::{Capability, CapabilityToken};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SandboxState {
    Created,
    Running,
    Violated,
    Terminated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyscallAttempt {
    pub name: String,
    pub timestamp_ms: u64,
    pub allowed: bool,
}

pub struct Sandbox {
    pub block_id: u32,
    state: SandboxState,
    allowed_capabilities: Vec<Capability>,
    memory_limit_bytes: u64,
    used_memory_bytes: u64,
    syscall_log: Vec<SyscallAttempt>,
    max_syscalls: u64,
    syscall_count: u64,
}

impl Sandbox {
    pub fn new(
        block_id: u32,
        capabilities: Vec<Capability>,
        memory_limit_bytes: u64,
        max_syscalls: u64,
    ) -> Self {
        Self {
            block_id,
            state: SandboxState::Created,
            allowed_capabilities: capabilities,
            memory_limit_bytes,
            used_memory_bytes: 0,
            syscall_log: Vec::new(),
            max_syscalls,
            syscall_count: 0,
        }
    }

    pub fn start(&mut self) {
        self.state = SandboxState::Running;
    }

    pub fn check_syscall(&mut self, syscall_name: &str, required_cap: &Capability) -> bool {
        if self.state != SandboxState::Running {
            return false;
        }

        self.syscall_count += 1;

        if self.syscall_count > self.max_syscalls {
            self.state = SandboxState::Violated;
            self.syscall_log.push(SyscallAttempt {
                name: syscall_name.to_string(),
                timestamp_ms: crate::capability::now_ms(),
                allowed: false,
            });
            return false;
        }

        let allowed = self.allowed_capabilities.contains(required_cap)
            || self.allowed_capabilities.contains(&Capability::All);

        self.syscall_log.push(SyscallAttempt {
            name: syscall_name.to_string(),
            timestamp_ms: crate::capability::now_ms(),
            allowed,
        });

        if !allowed {
            self.state = SandboxState::Violated;
        }

        allowed
    }

    pub fn allocate_memory(&mut self, bytes: u64) -> bool {
        if self.used_memory_bytes + bytes > self.memory_limit_bytes {
            self.state = SandboxState::Violated;
            return false;
        }
        self.used_memory_bytes += bytes;
        true
    }

    pub fn release_memory(&mut self, bytes: u64) {
        self.used_memory_bytes = self.used_memory_bytes.saturating_sub(bytes);
    }

    pub fn terminate(&mut self) {
        self.state = SandboxState::Terminated;
    }

    pub fn state(&self) -> SandboxState {
        self.state.clone()
    }

    pub fn memory_usage(&self) -> (u64, u64) {
        (self.used_memory_bytes, self.memory_limit_bytes)
    }

    pub fn syscall_log(&self) -> &[SyscallAttempt] {
        &self.syscall_log
    }

    pub fn syscall_count(&self) -> u64 {
        self.syscall_count
    }

    pub fn is_violated(&self) -> bool {
        self.state == SandboxState::Violated
    }

    pub fn from_token(token: &CapabilityToken, memory_limit_bytes: u64, max_syscalls: u64) -> Self {
        Self::new(
            token.block_id,
            token.capabilities.clone(),
            memory_limit_bytes,
            max_syscalls,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_sandbox() -> Sandbox {
        Sandbox::new(
            1,
            vec![Capability::FsRead, Capability::FsWrite],
            1024 * 1024,
            100,
        )
    }

    #[test]
    fn test_sandbox_lifecycle() {
        let mut sb = test_sandbox();
        assert_eq!(sb.state(), SandboxState::Created);
        sb.start();
        assert_eq!(sb.state(), SandboxState::Running);
        sb.terminate();
        assert_eq!(sb.state(), SandboxState::Terminated);
    }

    #[test]
    fn test_allowed_syscall() {
        let mut sb = test_sandbox();
        sb.start();
        assert!(sb.check_syscall("open", &Capability::FsRead));
        assert!(sb.check_syscall("write", &Capability::FsWrite));
        assert_eq!(sb.state(), SandboxState::Running);
    }

    #[test]
    fn test_blocked_syscall() {
        let mut sb = test_sandbox();
        sb.start();
        assert!(!sb.check_syscall("bind", &Capability::NetBind));
        assert_eq!(sb.state(), SandboxState::Violated);
    }

    #[test]
    fn test_memory_limit() {
        let mut sb = Sandbox::new(1, vec![Capability::FsRead, Capability::FsWrite], 1000, 100);
        sb.start();
        assert!(sb.allocate_memory(500));
        assert!(sb.allocate_memory(500));
        assert!(!sb.allocate_memory(1));
        assert_eq!(sb.state(), SandboxState::Violated);
    }

    #[test]
    fn test_syscall_limit() {
        let mut sb = Sandbox::new(1, vec![Capability::FsRead], 1024, 3);
        sb.start();
        assert!(sb.check_syscall("read", &Capability::FsRead));
        assert!(sb.check_syscall("read", &Capability::FsRead));
        assert!(sb.check_syscall("read", &Capability::FsRead));
        assert!(!sb.check_syscall("read", &Capability::FsRead));
        assert_eq!(sb.state(), SandboxState::Violated);
    }

    #[test]
    fn test_from_token() {
        use crate::capability::CapabilityToken;
        let token = CapabilityToken::new(42, vec![Capability::FsRead], 60_000, b"secret");
        let sb = Sandbox::from_token(&token, 4096, 50);
        assert_eq!(sb.block_id, 42);
        assert!(sb.allowed_capabilities.contains(&Capability::FsRead));
    }

    #[test]
    fn test_memory_release() {
        let mut sb = test_sandbox();
        sb.start();
        sb.allocate_memory(500);
        sb.release_memory(200);
        assert_eq!(sb.used_memory_bytes, 300);
    }
}
