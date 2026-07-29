use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyscallPolicy {
    Allow,
    Deny,
    Log,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasiFilter {
    pub allowed_syscalls: HashSet<String>,
    pub denied_syscalls: HashSet<String>,
    pub logged_syscalls: HashSet<String>,
    pub default_policy: SyscallPolicy,
}

impl Default for WasiFilter {
    fn default() -> Self {
        let mut allowed = HashSet::new();
        allowed.insert("fd_close".into());
        allowed.insert("fd_read".into());
        allowed.insert("fd_write".into());
        allowed.insert("fd_seek".into());
        allowed.insert(" environ_get".into());
        allowed.insert("environ_sizes_get".into());
        allowed.insert("clock_time_get".into());
        allowed.insert("proc_exit".into());

        Self {
            allowed_syscalls: allowed,
            denied_syscalls: HashSet::new(),
            logged_syscalls: HashSet::new(),
            default_policy: SyscallPolicy::Deny,
        }
    }
}

impl WasiFilter {
    pub fn new(default_policy: SyscallPolicy) -> Self {
        Self {
            allowed_syscalls: HashSet::new(),
            denied_syscalls: HashSet::new(),
            logged_syscalls: HashSet::new(),
            default_policy,
        }
    }

    pub fn allow(mut self, syscall: &str) -> Self {
        self.allowed_syscalls.insert(syscall.to_string());
        self.denied_syscalls.remove(syscall);
        self
    }

    pub fn deny(mut self, syscall: &str) -> Self {
        self.denied_syscalls.insert(syscall.to_string());
        self.allowed_syscalls.remove(syscall);
        self
    }

    pub fn log(mut self, syscall: &str) -> Self {
        self.logged_syscalls.insert(syscall.to_string());
        self
    }

    pub fn is_allowed(&self) -> bool {
        !self.denied_syscalls.is_empty() || self.default_policy != SyscallPolicy::Allow
    }

    pub fn check_syscall(&self, syscall: &str) -> SyscallPolicy {
        if self.allowed_syscalls.contains(syscall) {
            return SyscallPolicy::Allow;
        }
        if self.denied_syscalls.contains(syscall) {
            return SyscallPolicy::Deny;
        }
        if self.logged_syscalls.contains(syscall) {
            return SyscallPolicy::Log;
        }
        self.default_policy
    }

    pub fn total_allowed(&self) -> usize {
        self.allowed_syscalls.len()
    }

    pub fn total_denied(&self) -> usize {
        self.denied_syscalls.len()
    }

    pub fn total_logged(&self) -> usize {
        self.logged_syscalls.len()
    }

    pub fn permissive() -> Self {
        Self::new(SyscallPolicy::Allow)
    }

    pub fn restrictive() -> Self {
        let mut filter = Self::new(SyscallPolicy::Deny);
        filter = filter.allow("fd_close");
        filter = filter.allow("fd_write");
        filter = filter.allow("proc_exit");
        filter = filter.allow("clock_time_get");
        filter
    }

    pub fn no_network() -> Self {
        let mut filter = Self::permissive();
        filter = filter.deny("sock_connect");
        filter = filter.deny("sock_bind");
        filter = filter.deny("sock_listen");
        filter = filter.deny("sock_accept");
        filter = filter.deny("sock_send");
        filter = filter.deny("sock_recv");
        filter
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_filter() {
        let filter = WasiFilter::default();
        assert!(filter.is_allowed());
        assert_eq!(filter.check_syscall("fd_read"), SyscallPolicy::Allow);
        assert_eq!(filter.check_syscall("proc_exit"), SyscallPolicy::Allow);
        assert_eq!(filter.check_syscall("sock_connect"), SyscallPolicy::Deny);
    }

    #[test]
    fn test_custom_filter_allow_deny() {
        let filter = WasiFilter::new(SyscallPolicy::Deny)
            .allow("fd_write")
            .allow("fd_read")
            .deny("proc_exit");
        assert_eq!(filter.check_syscall("fd_write"), SyscallPolicy::Allow);
        assert_eq!(filter.check_syscall("fd_read"), SyscallPolicy::Allow);
        assert_eq!(filter.check_syscall("proc_exit"), SyscallPolicy::Deny);
        assert_eq!(filter.check_syscall("unknown"), SyscallPolicy::Deny);
    }

    #[test]
    fn test_permissive_filter() {
        let filter = WasiFilter::permissive();
        assert_eq!(filter.check_syscall("anything"), SyscallPolicy::Allow);
        assert_eq!(filter.check_syscall("sock_connect"), SyscallPolicy::Allow);
    }

    #[test]
    fn test_restrictive_filter() {
        let filter = WasiFilter::restrictive();
        assert_eq!(filter.check_syscall("fd_close"), SyscallPolicy::Allow);
        assert_eq!(filter.check_syscall("fd_write"), SyscallPolicy::Allow);
        assert_eq!(filter.check_syscall("proc_exit"), SyscallPolicy::Allow);
        assert_eq!(filter.check_syscall("clock_time_get"), SyscallPolicy::Allow);
        assert_eq!(filter.check_syscall("fd_read"), SyscallPolicy::Deny);
        assert_eq!(filter.check_syscall("memory_grow"), SyscallPolicy::Deny);
    }

    #[test]
    fn test_no_network_filter() {
        let filter = WasiFilter::no_network();
        assert_eq!(filter.check_syscall("sock_connect"), SyscallPolicy::Deny);
        assert_eq!(filter.check_syscall("sock_bind"), SyscallPolicy::Deny);
        assert_eq!(filter.check_syscall("fd_read"), SyscallPolicy::Allow);
    }

    #[test]
    fn test_log_policy() {
        let filter = WasiFilter::new(SyscallPolicy::Deny)
            .log("fd_write")
            .allow("fd_read");
        assert_eq!(filter.check_syscall("fd_write"), SyscallPolicy::Log);
        assert_eq!(filter.check_syscall("fd_read"), SyscallPolicy::Allow);
        assert_eq!(filter.check_syscall("unknown"), SyscallPolicy::Deny);
    }

    #[test]
    fn test_filter_counts() {
        let filter = WasiFilter::new(SyscallPolicy::Deny)
            .allow("a")
            .allow("b")
            .deny("c")
            .log("d");
        assert_eq!(filter.total_allowed(), 2);
        assert_eq!(filter.total_denied(), 1);
        assert_eq!(filter.total_logged(), 1);
    }

    #[test]
    fn test_allow_overrides_deny() {
        let filter = WasiFilter::new(SyscallPolicy::Deny)
            .deny("fd_read")
            .allow("fd_read");
        assert_eq!(filter.check_syscall("fd_read"), SyscallPolicy::Allow);
    }

    #[test]
    fn test_deny_overrides_allow() {
        let filter = WasiFilter::new(SyscallPolicy::Allow)
            .allow("fd_read")
            .deny("fd_read");
        assert_eq!(filter.check_syscall("fd_read"), SyscallPolicy::Deny);
    }

    #[test]
    fn test_filter_serialization_roundtrip() {
        let filter = WasiFilter::restrictive();
        let bytes = bincode::serialize(&filter).unwrap();
        let restored: WasiFilter = bincode::deserialize(&bytes).unwrap();
        assert_eq!(restored.total_allowed(), 4);
        assert_eq!(restored.default_policy, SyscallPolicy::Deny);
    }
}
