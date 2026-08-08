//! Multi-node distributed scheduling for AIOS.
//!
//! This crate provides the `aios-cluster` distributed scheduler: nodes
//! discover each other over a pluggable transport (real TCP or in-process),
//! exchange load snapshots, place processes with load-aware/tier-aware/round-
//! robin strategies and recover from node failures by respawning its processes
//! elsewhere.
//!
//! # Roles
//!
//! - A node with [`DistributedScheduler`] attached to a [`ProcessExecutor`]
//!   is a **worker** — it runs processes requested by peers.
//! - A node without an executor is a pure **coordinator** — it schedules
//!   processes onto workers.
//!
//! The same type fulfils both roles; attach an executor to host processes.
//!
//! # Example (in-memory, two workers)
//!
//! ```no_run
//! use aios_cluster::types::*;
//! use aios_cluster::executor::{MockProcessExecutor, ProcessExecutor};
//! use aios_cluster::scheduler::DistributedScheduler;
//! use aios_cluster::transport::{ClusterTransport, InMemoryClusterTransport, MemoryRegistry};
//! use std::sync::Arc;
//!
//! let registry = MemoryRegistry::new();
//! let transport_a = Arc::new(InMemoryClusterTransport::new("mem://a", registry.clone_arc()));
//! let transport_b = Arc::new(InMemoryClusterTransport::new("mem://b", registry.clone_arc()));
//!
//! let mut a = DistributedScheduler::new(
//!     NodeInfo {
//!         id: 1, name: "a".into(), addr: "mem://a".into(), tier: 2,
//!         status: NodeStatus::Online, metrics: NodeMetrics::idle(),
//!     },
//!     transport_a,
//!     PlacementStrategy::LeastLoaded,
//! );
//! a.set_executor(Arc::new(MockProcessExecutor::new(1)));
//! a.start(&["mem://b".into()]);
//! ```
//!
//! See `aios-cluster` unit tests for a full two-node spawn/kill/failover flow.
pub mod config;
pub mod executor;
pub mod protocol;
pub mod scheduler;
pub mod transport;
pub mod types;

pub use config::ClusterConfig;
pub use executor::{MockProcessExecutor, ProcessExecutor, SchedulerProcessExecutor};
pub use scheduler::DistributedScheduler;
pub use transport::{
    ClusterTransport, InMemoryClusterTransport, MemoryRegistry, TcpClusterTransport,
};
pub use types::{
    now_ms, NodeId, NodeInfo, NodeMetrics, NodeStatus, PlacementStrategy, RemoteProcessId,
    RemoteProcessSpec, RemoteProcessStatus,
};
