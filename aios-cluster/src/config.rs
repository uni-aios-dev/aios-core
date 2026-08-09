//! Cluster bootstrap configuration.
use crate::types::{NodeId, PlacementStrategy};
use serde::{Deserialize, Serialize};

/// Static cluster configuration, read from the environment or a JSON file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterConfig {
    /// This node's id.
    pub node_id: NodeId,
    /// This node's display name.
    pub node_name: String,
    /// This node's listen address (`host:port`).
    pub addr: String,
    /// Hardware tier (1 = most capable, 3 = low-spec).
    pub tier: u8,
    /// Peer addresses to announce to (`host:port` each).
    pub peers: Vec<String>,
    /// Heartbeat announce interval in ms.
    pub heartbeat_ms: u64,
    /// Failover threshold in ms.
    pub failover_threshold_ms: u64,
    /// Respawn processes from failed nodes elsewhere.
    pub failover_respawn: bool,
    /// Placement strategy.
    pub strategy: PlacementStrategy,
    /// How long replicated checkpoints stay usable before pruning (ms).
    #[serde(default = "default_checkpoint_ttl_ms")]
    pub checkpoint_ttl_ms: u64,
}

fn default_checkpoint_ttl_ms() -> u64 {
    15000
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            node_id: 1,
            node_name: "aios-node".into(),
            addr: "127.0.0.1:9000".into(),
            tier: 2,
            peers: Vec::new(),
            heartbeat_ms: 1000,
            failover_threshold_ms: 3000,
            failover_respawn: true,
            strategy: PlacementStrategy::LeastLoaded,
            checkpoint_ttl_ms: default_checkpoint_ttl_ms(),
        }
    }
}

impl ClusterConfig {
    /// Derive the configuration from the standard AIOS environment:
    /// - `AIOS_CLUSTER_ID` — node id (default 1)
    /// - `AIOS_CLUSTER_NAME` — node name (default `aios-node`)
    /// - `AIOS_CLUSTER_ADDR` — listen address (default `127.0.0.1:9000`)
    /// - `AIOS_CLUSTER_PEERS` — comma-separated peer addresses
    /// - `AIOS_CLUSTER_TIER` — hardware tier
    /// - `AIOS_CLUSTER_STRATEGY` — `roundrobin|leastloaded|bytier`
    /// - `AIOS_CLUSTER_HEARTBEAT_MS`, `AIOS_CLUSTER_FAILOVER_MS`,
    ///   `AIOS_CLUSTER_FAILOVER_RESPAWN`, `AIOS_CLUSTER_CHECKPOINT_TTL_MS`
    ///
    /// Returns `None` when clustering is not requested (`AIOS_CLUSTER_PEERS`
    /// is unset) or the node address is missing.
    pub fn from_env() -> Option<Self> {
        let peers_raw = std::env::var("AIOS_CLUSTER_PEERS").unwrap_or_default();
        if peers_raw.trim().is_empty() {
            return None;
        }
        let peers: Vec<String> = peers_raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if peers.is_empty() {
            return None;
        }
        let addr = std::env::var("AIOS_CLUSTER_ADDR").unwrap_or_else(|_| "127.0.0.1:9000".into());
        let node_id = std::env::var("AIOS_CLUSTER_ID")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);
        let tier = std::env::var("AIOS_CLUSTER_TIER")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2);
        let strategy = match std::env::var("AIOS_CLUSTER_STRATEGY")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "roundrobin" | "round-robin" => PlacementStrategy::RoundRobin,
            "bytier" | "by-tier" => PlacementStrategy::ByTier,
            _ => PlacementStrategy::LeastLoaded,
        };
        Some(Self {
            node_id,
            node_name: std::env::var("AIOS_CLUSTER_NAME")
                .unwrap_or_else(|_| format!("aios-node-{node_id}")),
            addr,
            tier,
            peers,
            heartbeat_ms: env_u64("AIOS_CLUSTER_HEARTBEAT_MS", 1000),
            failover_threshold_ms: env_u64("AIOS_CLUSTER_FAILOVER_MS", 3000),
            failover_respawn: std::env::var("AIOS_CLUSTER_FAILOVER_RESPAWN")
                .map(|v| v != "0" && v.to_lowercase() != "false")
                .unwrap_or(true),
            strategy,
            checkpoint_ttl_ms: env_u64(
                "AIOS_CLUSTER_CHECKPOINT_TTL_MS",
                default_checkpoint_ttl_ms(),
            ),
        })
    }

    /// Parse a cluster configuration from a JSON string.
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| format!("cluster config parse failed: {e}"))
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_json_roundtrip() {
        let json = r#"{
            "node_id": 3, "node_name": "edge", "addr": "10.0.0.3:9000",
            "tier": 3, "peers": ["10.0.0.1:9000", "10.0.0.2:9000"],
            "heartbeat_ms": 500, "failover_threshold_ms": 1500,
            "failover_respawn": true, "strategy": "RoundRobin"
        }"#;
        let cfg = ClusterConfig::from_json(json).unwrap();
        assert_eq!(cfg.node_id, 3);
        assert_eq!(cfg.tier, 3);
        assert_eq!(cfg.peers.len(), 2);
        assert_eq!(cfg.strategy, PlacementStrategy::RoundRobin);
        assert_eq!(cfg.checkpoint_ttl_ms, default_checkpoint_ttl_ms());
    }

    #[test]
    fn from_json_rejects_garbage() {
        assert!(ClusterConfig::from_json("not json").is_err());
    }
}
