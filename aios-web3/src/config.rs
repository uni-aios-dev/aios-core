use std::collections::BTreeMap;

use crate::types::ChainInfo;

/// Built-in Ethereum chain selected on first launch.
pub const DEFAULT_CHAIN_ID: &str = "0x1";

/// Return the built-in network table.
///
/// Ethereum mainnet is the default; the remaining entries cover the most
/// commonly requested EVM networks. Users can add more chains from the wallet
/// UI through `wallet_addEthereumChain`.
pub fn builtin_chains() -> BTreeMap<String, ChainInfo> {
    let mut chains = BTreeMap::new();
    chains.insert(
        "0x1".into(),
        ChainInfo {
            chain_id: "0x1".into(),
            name: "Ethereum Mainnet".into(),
            rpc_url: "https://cloudflare-eth.com".into(),
            explorer: "https://etherscan.io".into(),
            symbol: "ETH".into(),
        },
    );
    chains.insert(
        "0xaa36a7".into(),
        ChainInfo {
            chain_id: "0xaa36a7".into(),
            name: "Sepolia".into(),
            rpc_url: "https://rpc.sepolia.org".into(),
            explorer: "https://sepolia.etherscan.io".into(),
            symbol: "ETH".into(),
        },
    );
    chains.insert(
        "0x89".into(),
        ChainInfo {
            chain_id: "0x89".into(),
            name: "Polygon".into(),
            rpc_url: "https://polygon-rpc.com".into(),
            explorer: "https://polygonscan.com".into(),
            symbol: "POL".into(),
        },
    );
    chains.insert(
        "0xa4b1".into(),
        ChainInfo {
            chain_id: "0xa4b1".into(),
            name: "Arbitrum One".into(),
            rpc_url: "https://arb1.arbitrum.io/rpc".into(),
            explorer: "https://arbiscan.io".into(),
            symbol: "ETH".into(),
        },
    );
    chains.insert(
        "0xa".into(),
        ChainInfo {
            chain_id: "0xa".into(),
            name: "Optimism".into(),
            rpc_url: "https://mainnet.optimism.io".into(),
            explorer: "https://optimistic.etherscan.io".into(),
            symbol: "ETH".into(),
        },
    );
    chains.insert(
        "0x2105".into(),
        ChainInfo {
            chain_id: "0x2105".into(),
            name: "Base".into(),
            rpc_url: "https://mainnet.base.org".into(),
            explorer: "https://basescan.org".into(),
            symbol: "ETH".into(),
        },
    );
    chains
}
