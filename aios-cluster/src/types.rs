//! Core cluster types: node identity, metrics, process references and placement.
use serde::{Deserialize, Serialize};

/// Stable identifier of a node in the cluster.
pub type NodeId = u64;

/// Liveness state of a peer node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeStatus {
    /// Never seen or state lost.
    Unknown,
    /// Heartbeats are arriving within the failover threshold.
    Online,
    /// Heartbeats stopped; the node is treated as failed.
    Offline,
    /// Graceful shutdown in progress.
    Leaving,
}

impl NodeStatus {
    /// Short human-readable label.
    pub fn label(&self) -> &'static str {
        match self {
            NodeStatus::Unknown => "Unknown",
            NodeStatus::Online => "Online",
            NodeStatus::Offline => "Offline",
            NodeStatus::Leaving => "Leaving",
        }
    }
}

/// Resource load snapshot of a node, exchanged on heartbeats.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NodeMetrics {
    /// Reported CPU utilization 0.0..=1.0.
    pub cpu_percent: f64,
    /// Currently used RAM in MiB.
    pub ram_used_mb: u64,
    /// Total RAM in MiB.
    pub ram_total_mb: u64,
    /// Number of processes this node hosts.
    pub process_count: u64,
    /// Monotonic timestamp (ms) when the snapshot was taken.
    pub updated_at_ms: u64,
}

impl NodeMetrics {
    /// Create a metrics snapshot with the current timestamp.
    pub fn new(cpu_percent: f64, ram_used_mb: u64, ram_total_mb: u64, process_count: u64) -> Self {
        Self {
            cpu_percent: cpu_percent.clamp(0.0, 1.0),
            ram_used_mb,
            ram_total_mb,
            process_count,
            updated_at_ms: now_ms(),
        }
    }

    /// Idle snapshot (used by nodes without a process executor).
    pub fn idle() -> Self {
        Self::new(0.0, 0, 0, 0)
    }

    /// Fraction of RAM in use; 1.0 when the total is unknown.
    pub fn load_fraction(&self) -> f64 {
        if self.ram_total_mb == 0 {
            1.0
        } else {
            self.ram_used_mb as f64 / self.ram_total_mb as f64
        }
    }
}

/// Static description of a node that is exchanged during discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    /// Unique node id.
    pub id: NodeId,
    /// Human-readable node name.
    pub name: String,
    /// Transport address peers use to reach this node (`host:port` or a
    /// memory-transport address).
    pub addr: String,
    /// Hardware tier (1 = most capable, 3 = low-spec), used by tier-aware
    /// placement.
    pub tier: u8,
    /// Current liveness.
    pub status: NodeStatus,
    /// Latest reported load.
    pub metrics: NodeMetrics,
}

/// Identity of a process running on a remote node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RemoteProcessId {
    /// Node hosting the process.
    pub node: NodeId,
    /// Local process id on that node.
    pub pid: u64,
}

impl std::fmt::Display for RemoteProcessId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "node_{}:pid_{}", self.node, self.pid)
    }
}

/// A request to run a process on a remote node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteProcessSpec {
    /// Process name (block name).
    pub name: String,
    /// Scheduling priority 0..=4 (matches `aios-process-mgr::Priority`).
    pub priority: u8,
    /// RAM quota in MiB.
    pub ram_mb: u64,
    /// Optional registered block id to bind the process to.
    pub block_id: Option<u32>,
    /// Optional init payload delivered to the remote executor.
    pub payload: Vec<u8>,
    /// Placement filters (None = no constraint).
    pub min_tier: Option<u8>,
    /// Placement filters (None = no constraint).
    pub max_tier: Option<u8>,
}

impl RemoteProcessSpec {
    /// Create a basic remote process spec.
    pub fn new(name: &str, priority: u8, ram_mb: u64) -> Self {
        Self {
            name: name.to_string(),
            priority: priority.clamp(0, 4),
            ram_mb,
            block_id: None,
            payload: Vec::new(),
            min_tier: None,
            max_tier: None,
        }
    }

    /// Bind the process to a registered block id.
    pub fn with_block_id(mut self, id: u32) -> Self {
        self.block_id = Some(id);
        self
    }

    /// Attach an init payload for the remote executor.
    pub fn with_payload(mut self, payload: Vec<u8>) -> Self {
        self.payload = payload;
        self
    }

    /// Constrain placement to nodes within `[min, max]` hardware tiers.
    pub fn with_tier_range(mut self, min: u8, max: u8) -> Self {
        self.min_tier = Some(min);
        self.max_tier = Some(max);
        self
    }
}

/// Snapshot of a remotely scheduled process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteProcessStatus {
    /// Remote identity.
    pub id: RemoteProcessId,
    /// Process name.
    pub name: String,
    /// Coarse state string (`Running`, `Suspended`, `Terminated`, `Crashed`).
    pub state: String,
    /// RAM quota in MiB.
    pub ram_mb: u64,
}

/// Placement policy used to pick a target node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlacementStrategy {
    /// Cycle through online nodes.
    RoundRobin,
    /// Pick the node with the lowest RAM load fraction.
    LeastLoaded,
    /// Pick the most capable (lowest tier number) online node.
    ByTier,
}

/// Monotonic millisecond timestamp.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
