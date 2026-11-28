//! AIOS Web3 host: EIP-1193 provider backend, wallet and JSON-RPC transport.
//!
//! The injected provider (`aios-browser::web3::PROVIDER_JS`) sends requests to
//! this host. The host enforces a permission policy, signs with a local key
//! derived from an AIOS-managed secret, and forwards read RPC calls through the
//! SSRF-safe browser network client.

pub mod config;
pub mod host;
pub mod types;
pub mod wallet;

pub use config::{builtin_chains, DEFAULT_CHAIN_ID};
pub use host::Web3Host;
pub use types::{ChainInfo, ProviderError, ProviderReply, ProviderRequest};
pub use wallet::Wallet;
