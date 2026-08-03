//! AIOS network configuration model, persistence and a `StatefulBlock`.
//!
//! The crate provides a `NetworkConfig` value type describing the system's
//! network settings (proxy, DNS, interfaces, ports), a `NetworkConfigStore`
//! that persists it as JSON, and a `NetSettingsBlock` that exposes the
//! configuration to the IPC bus of the kernel.

pub mod block;
pub mod config;
pub mod store;
pub mod validation;

pub use block::NetSettingsBlock;
pub use config::{DnsConfig, InterfaceConfig, NetworkConfig, ProxyConfig, ProxyProtocol};
pub use store::NetworkConfigStore;
pub use validation::{validate_ip, validate_port, validate_proxy, validate_url};
