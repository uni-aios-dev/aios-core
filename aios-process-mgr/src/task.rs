use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProcessId(pub u64);

impl ProcessId {
    pub const fn new(id: u64) -> Self {
        Self(id)
    }
}

impl fmt::Display for ProcessId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pid_{}", self.0)
    }
}

#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub enum Priority {
    Background = 0,
    Low = 1,
    #[default]
    Normal = 2,
    High = 3,
    Critical = 4,
}

impl Priority {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Background,
            1 => Self::Low,
            2 => Self::Normal,
            3 => Self::High,
            4..=255 => Self::Critical,
        }
    }
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Background => write!(f, "Background"),
            Self::Low => write!(f, "Low"),
            Self::Normal => write!(f, "Normal"),
            Self::High => write!(f, "High"),
            Self::Critical => write!(f, "Critical"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProcessState {
    Ready,
    Running,
    Suspended,
    Terminated,
    Crashed,
}

impl fmt::Display for ProcessState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready => write!(f, "Ready"),
            Self::Running => write!(f, "Running"),
            Self::Suspended => write!(f, "Suspended"),
            Self::Terminated => write!(f, "Terminated"),
            Self::Crashed => write!(f, "Crashed"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Process {
    pub pid: ProcessId,
    pub name: String,
    pub priority: Priority,
    pub state: ProcessState,
    pub ram_quota_mb: u64,
    pub cpu_time_ms: u64,
    pub block_id: Option<u32>,
    pub parent_pid: Option<ProcessId>,
    pub group_id: Option<u64>,
    pub crash_count: u32,
    pub max_restarts: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessGroup {
    pub id: u64,
    pub name: String,
    pub priority: Priority,
    pub member_pids: Vec<ProcessId>,
    pub created_at_ms: u64,
    pub session_id: Option<u64>,
}

impl Process {
    pub fn new(pid: ProcessId, name: String, priority: Priority, ram_mb: u64) -> Self {
        Self {
            pid,
            name,
            priority,
            state: ProcessState::Ready,
            ram_quota_mb: ram_mb,
            cpu_time_ms: 0,
            block_id: None,
            parent_pid: None,
            group_id: None,
            crash_count: 0,
            max_restarts: 3,
        }
    }

    pub fn with_parent(mut self, parent: ProcessId) -> Self {
        self.parent_pid = Some(parent);
        self
    }

    pub fn with_group(mut self, group_id: u64) -> Self {
        self.group_id = Some(group_id);
        self
    }

    pub fn with_max_restarts(mut self, max: u32) -> Self {
        self.max_restarts = max;
        self
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl ProcessGroup {
    pub fn new(id: u64, name: String, priority: Priority) -> Self {
        Self {
            id,
            name,
            priority,
            member_pids: Vec::new(),
            created_at_ms: now_ms(),
            session_id: None,
        }
    }

    pub fn with_session(mut self, session_id: u64) -> Self {
        self.session_id = Some(session_id);
        self
    }

    pub fn add_member(&mut self, pid: ProcessId) {
        if !self.member_pids.contains(&pid) {
            self.member_pids.push(pid);
        }
    }

    pub fn remove_member(&mut self, pid: ProcessId) {
        self.member_pids.retain(|p| *p != pid);
    }

    pub fn member_count(&self) -> usize {
        self.member_pids.len()
    }

    pub fn contains(&self, pid: ProcessId) -> bool {
        self.member_pids.contains(&pid)
    }
}

pub struct ProcessTimer {
    pub pid: ProcessId,
    started: Instant,
    pub quota_ms: u64,
}

impl ProcessTimer {
    pub fn new(pid: ProcessId, quota_ms: u64) -> Self {
        Self {
            pid,
            started: Instant::now(),
            quota_ms,
        }
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    pub fn quota_exceeded(&self) -> bool {
        self.elapsed_ms() >= self.quota_ms
    }

    pub fn remaining_ms(&self) -> u64 {
        self.quota_ms.saturating_sub(self.elapsed_ms())
    }

    pub fn force_expire(&mut self) {
        self.started = Instant::now() - std::time::Duration::from_millis(self.quota_ms + 1);
    }
}
