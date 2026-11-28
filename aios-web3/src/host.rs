use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::RwLock;

use aios_browser::network::NetworkClient;
use aios_browser::types::BrowserConfig;
use aios_core::error::{AIOSException, Result};
use serde_json::{json, Value};
use tokio::runtime::{Builder, Runtime};

use crate::config::{builtin_chains, DEFAULT_CHAIN_ID};
use crate::types::{ChainInfo, ProviderReply, ProviderRequest};
use crate::wallet::Wallet;

const ERR_USER_REJECTED: i64 = 4001;
const ERR_UNAUTHORIZED: i64 = 4100;
const ERR_UNSUPPORTED: i64 = 4200;
const ERR_DISALLOWED: i64 = 32000;
const ERR_CHAIN_NOT_FOUND: i64 = 4902;

/// Backend for the injected `window.ethereum` provider.
///
/// Owns the local wallet, the selected network and per-origin permissions.
/// Read RPC calls go through the SSRF-safe browser network client; dangerous
/// methods require an explicit approval issued by the provider UI.
pub struct Web3Host {
    runtime: Runtime,
    client: NetworkClient,
    wallet: Wallet,
    chains: RwLock<BTreeMap<String, ChainInfo>>,
    chain_id: RwLock<String>,
    approved_origins: RwLock<HashSet<String>>,
    signature_tokens: RwLock<HashMap<String, String>>,
}

impl Web3Host {
    /// Build a host with a wallet derived from the AIOS-managed web3 secret.
    pub fn new() -> Result<Self> {
        let seed =
            aios_core::secrets::resolve_secret_in("web3_wallet", &aios_core::secrets::data_dir())?;
        Self::from_seed(&seed)
    }

    /// Build a host with an explicit seed (used by tests and custom setups).
    pub fn from_seed(seed: &[u8]) -> Result<Self> {
        let runtime = Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("aios-web3")
            .enable_all()
            .build()
            .map_err(|e| AIOSException::Generic(format!("web3 runtime: {e}")))?;
        Ok(Self {
            runtime,
            client: NetworkClient::new(&BrowserConfig::for_untrusted()),
            wallet: Wallet::from_seed(seed)?,
            chains: RwLock::new(builtin_chains()),
            chain_id: RwLock::new(DEFAULT_CHAIN_ID.to_string()),
            approved_origins: RwLock::new(HashSet::new()),
            signature_tokens: RwLock::new(HashMap::new()),
        })
    }

    /// Address of the local account.
    pub fn address(&self) -> &str {
        self.wallet.address()
    }

    /// Currently selected hex chain id.
    pub fn chain_id(&self) -> String {
        read(&self.chain_id).clone()
    }

    /// Snapshot of all known networks, including user-added chains.
    pub fn chains(&self) -> Vec<ChainInfo> {
        read(&self.chains).values().cloned().collect()
    }

    /// Select a network from the wallet UI.
    ///
    /// Returns `false` when the chain is not registered.
    pub fn select_chain(&self, chain_id: &str) -> bool {
        if !read(&self.chains).contains_key(chain_id) {
            return false;
        }
        *write(&self.chain_id) = chain_id.to_string();
        true
    }

    /// Handle a raw JSON message from a WebView/IPC bridge.
    ///
    /// Returns a serialized [`ProviderReply`] suitable for
    /// `window.__aiosWeb3Resolve(id, ok, value)`.
    pub fn handle_wire(&self, wire: &str) -> String {
        let reply = match serde_json::from_str::<ProviderRequest>(wire) {
            Ok(request) => self.dispatch(&request),
            Err(error) => ProviderReply::err(
                "unknown".into(),
                ERR_DISALLOWED,
                format!("invalid web3 request: {error}"),
            ),
        };
        serde_json::to_string(&reply).unwrap_or_else(|_| {
            r#"{"id":"unknown","error":{"code":32000,"message":"serialize failed"}}"#.into()
        })
    }

    /// Dispatch a parsed provider request and produce a reply.
    ///
    /// Must be called outside a Tokio runtime (GUI/WebView event loops are).
    pub fn dispatch(&self, request: &ProviderRequest) -> ProviderReply {
        let id = request.id.clone();
        if request.embedded && is_sensitive(&request.method) {
            return ProviderReply::err(
                id,
                ERR_DISALLOWED,
                "method is unavailable in the embedded browser; use the native WebView browser",
            );
        }
        self.runtime.block_on(self.handle(request))
    }

    async fn handle(&self, request: &ProviderRequest) -> ProviderReply {
        let id = request.id.clone();
        let method = request.method.as_str();
        let params = &request.params;
        let origin = normalized_origin(request);

        match method {
            "wallet_approveAccountAccess" => {
                write(&self.approved_origins).insert(origin);
                ProviderReply::ok(id, json!(true))
            }
            "wallet_revokeAccountAccess" => {
                write(&self.approved_origins).remove(&origin);
                ProviderReply::ok(id, json!(true))
            }
            "wallet_getPermissions" => {
                let permitted = read(&self.approved_origins).contains(&origin);
                let permissions = if permitted {
                    json!([{"origin": origin, "method": "eth_accounts"}])
                } else {
                    json!([])
                };
                ProviderReply::ok(id, permissions)
            }
            "wallet_approveSignature" => {
                let message = param_str(params, 0).unwrap_or_default();
                let token = hex::encode(aios_core::secrets::random_secret());
                write(&self.signature_tokens).insert(format!("{origin}:{message}"), token.clone());
                ProviderReply::ok(id, json!(token))
            }
            "wallet_getChains" => {
                let chains: Vec<ChainInfo> = read(&self.chains).values().cloned().collect();
                ProviderReply::ok(id, serde_json::to_value(chains).unwrap_or(json!([])))
            }
            "wallet_getCurrentChain" => {
                let chain_id = read(&self.chain_id).clone();
                let chain = read(&self.chains).get(&chain_id).cloned();
                ProviderReply::ok(
                    id,
                    chain
                        .map(|c| serde_json::to_value(c).unwrap_or(json!(null)))
                        .unwrap_or(json!(null)),
                )
            }
            "wallet_switchEthereumChain" => {
                let target = param_str(params, 0).unwrap_or_default();
                if !read(&self.chains).contains_key(&target) {
                    return ProviderReply::err(
                        id,
                        ERR_CHAIN_NOT_FOUND,
                        format!("unregistered chain {target}"),
                    );
                }
                *write(&self.chain_id) = target.clone();
                ProviderReply::ok(id, json!(target))
            }
            "wallet_addEthereumChain" => {
                let Some(chain) = params.first() else {
                    return ProviderReply::err(id, ERR_DISALLOWED, "missing chain description");
                };
                let Some(chain_id) = chain.get("chainId").and_then(Value::as_str) else {
                    return ProviderReply::err(id, ERR_DISALLOWED, "missing chainId");
                };
                let rpc_url = chain
                    .get("rpcUrls")
                    .and_then(Value::as_array)
                    .and_then(|urls| urls.first())
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !valid_rpc_url(rpc_url) {
                    return ProviderReply::err(id, ERR_DISALLOWED, "invalid RPC URL");
                }
                let info = ChainInfo {
                    chain_id: chain_id.to_string(),
                    name: chain
                        .get("chainName")
                        .and_then(Value::as_str)
                        .unwrap_or(chain_id)
                        .to_string(),
                    rpc_url: rpc_url.to_string(),
                    explorer: chain
                        .get("blockExplorerUrls")
                        .and_then(Value::as_array)
                        .and_then(|urls| urls.first())
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    symbol: chain
                        .get("nativeCurrency")
                        .and_then(|c| c.get("symbol"))
                        .and_then(Value::as_str)
                        .unwrap_or("ETH")
                        .to_string(),
                };
                write(&self.chains).insert(chain_id.to_string(), info);
                *write(&self.chain_id) = chain_id.to_string();
                ProviderReply::ok(id, json!(null))
            }
            "eth_accounts" => {
                let accounts = if read(&self.approved_origins).contains(&origin) {
                    json!([self.wallet.address()])
                } else {
                    json!([])
                };
                ProviderReply::ok(id, accounts)
            }
            "eth_requestAccounts" => {
                if !read(&self.approved_origins).contains(&origin) {
                    return ProviderReply::err(
                        id,
                        ERR_UNAUTHORIZED,
                        "origin is not authorized to access accounts",
                    );
                }
                ProviderReply::ok(id, json!([self.wallet.address()]))
            }
            "eth_chainId" => ProviderReply::ok(id, json!(read(&self.chain_id).clone())),
            "net_version" => {
                let chain_id = read(&self.chain_id).clone();
                ProviderReply::ok(id, json!(parse_chain_decimal(&chain_id)))
            }
            "personal_sign" | "eth_sign" => {
                if !read(&self.approved_origins).contains(&origin) {
                    return ProviderReply::err(id, ERR_UNAUTHORIZED, "account access not approved");
                }
                let message = param_str(params, 1).unwrap_or_default();
                let token = param_str(params, 0).unwrap_or_default();
                let key = format!("{origin}:{message}");
                let expected = write(&self.signature_tokens).remove(&key);
                if expected.as_deref() != Some(token.as_str()) || token.is_empty() {
                    return ProviderReply::err(
                        id,
                        ERR_USER_REJECTED,
                        "missing or expired signature approval",
                    );
                }
                let decoded = decode_message(&message);
                match self.wallet.sign_message(&decoded) {
                    Ok(signature) => ProviderReply::ok(id, json!(signature)),
                    Err(error) => ProviderReply::err(id, ERR_DISALLOWED, error.to_string()),
                }
            }
            method if is_rpc_method(method) => self.rpc(id, method, params.clone()).await,
            _ => ProviderReply::err(
                id,
                ERR_UNSUPPORTED,
                format!("method {method} is not supported"),
            ),
        }
    }

    async fn rpc(&self, id: String, method: &str, params: Vec<Value>) -> ProviderReply {
        let chain_id = read(&self.chain_id).clone();
        let url = read(&self.chains)
            .get(&chain_id)
            .map(|chain| chain.rpc_url.clone())
            .unwrap_or_default();
        if url.is_empty() {
            return ProviderReply::err(id, ERR_CHAIN_NOT_FOUND, "no RPC endpoint for chain");
        }
        let body = json!({"jsonrpc": "2.0", "id": "aios", "method": method, "params": params});
        let response = self
            .client
            .request("POST", &url, Some(&body.to_string()))
            .await;
        if let Some(error) = response.error {
            return ProviderReply::err(id, ERR_DISALLOWED, error);
        }
        match serde_json::from_str::<Value>(&response.body) {
            Ok(Value::Object(object)) => {
                if let Some(error) = object.get("error") {
                    let code = error
                        .get("code")
                        .and_then(Value::as_i64)
                        .unwrap_or(ERR_DISALLOWED);
                    let message = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("RPC error")
                        .to_string();
                    return ProviderReply::err(id, code, message);
                }
                ProviderReply::ok(id, object.get("result").cloned().unwrap_or(Value::Null))
            }
            Ok(other) => ProviderReply::ok(id, other),
            Err(error) => ProviderReply::err(id, ERR_DISALLOWED, format!("bad RPC reply: {error}")),
        }
    }
}

fn read<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn normalized_origin(request: &ProviderRequest) -> String {
    if request.origin.is_empty() {
        "null".to_string()
    } else {
        request.origin.clone()
    }
}

fn param_str(params: &[Value], index: usize) -> Option<String> {
    params
        .get(index)
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn is_sensitive(method: &str) -> bool {
    matches!(
        method,
        "eth_requestAccounts"
            | "personal_sign"
            | "eth_sign"
            | "wallet_switchEthereumChain"
            | "wallet_addEthereumChain"
            | "wallet_approveSignature"
            | "wallet_approveAccountAccess"
    )
}

fn is_rpc_method(method: &str) -> bool {
    method.starts_with("eth_")
        || method.starts_with("net_")
        || method.starts_with("web3_")
        || method.starts_with("txpool_")
}

fn valid_rpc_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

fn decode_message(message: &str) -> String {
    if let Some(hex_part) = message.strip_prefix("0x") {
        if let Ok(bytes) = hex::decode(hex_part) {
            return String::from_utf8_lossy(&bytes).into_owned();
        }
    }
    message.to_string()
}

fn parse_chain_decimal(chain_id: &str) -> String {
    u64::from_str_radix(chain_id.trim_start_matches("0x"), 16)
        .map(|value| value.to_string())
        .unwrap_or_else(|_| chain_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::{decode_message, is_rpc_method, parse_chain_decimal, valid_rpc_url, Web3Host};
    use crate::types::ProviderRequest;

    fn request(method: &str, params: Vec<serde_json::Value>, embedded: bool) -> ProviderRequest {
        ProviderRequest {
            id: "test".into(),
            method: method.into(),
            params,
            origin: "https://example.com".into(),
            embedded,
        }
    }

    #[test]
    fn chain_and_accounts_policy() {
        let host = Web3Host::from_seed(b"aios-web3-host-test").expect("host");
        let chain = host.dispatch(&request("eth_chainId", vec![], false));
        assert_eq!(chain.result, Some(serde_json::json!("0x1")));
        let accounts = host.dispatch(&request("eth_accounts", vec![], false));
        assert_eq!(accounts.result, Some(serde_json::json!([])));
        let denied = host.dispatch(&request("eth_requestAccounts", vec![], false));
        assert_eq!(denied.error.expect("error").code, 4100);
        host.dispatch(&request(
            "wallet_approveAccountAccess",
            vec![serde_json::json!("https://example.com")],
            false,
        ));
        let allowed = host.dispatch(&request("eth_requestAccounts", vec![], false));
        assert_eq!(
            allowed
                .result
                .expect("accounts")
                .as_array()
                .expect("array")
                .len(),
            1
        );
    }

    #[test]
    fn embedded_signing_is_rejected() {
        let host = Web3Host::from_seed(b"aios-web3-embedded-test").expect("host");
        let reply = host.dispatch(&request("personal_sign", vec![], true));
        assert_eq!(reply.error.expect("error").code, 32000);
    }

    #[test]
    fn signature_requires_one_time_approval() {
        let host = Web3Host::from_seed(b"aios-web3-sign-test").expect("host");
        host.dispatch(&request(
            "wallet_approveAccountAccess",
            vec![serde_json::json!("https://example.com")],
            false,
        ));
        let missing = host.dispatch(&request(
            "personal_sign",
            vec![serde_json::json!("token"), serde_json::json!("hello")],
            false,
        ));
        assert_eq!(missing.error.expect("error").code, 4001);
    }

    #[test]
    fn chain_meta_helpers() {
        assert!(is_rpc_method("eth_getBalance"));
        assert!(!is_rpc_method("personal_sign"));
        assert!(valid_rpc_url("https://rpc.example.com"));
        assert!(!valid_rpc_url("file:///etc/passwd"));
        assert_eq!(parse_chain_decimal("0xaa36a7"), "11155111");
        assert_eq!(decode_message("0x68656c6c6f"), "hello");
    }

    #[test]
    fn wire_roundtrip() {
        let host = Web3Host::from_seed(b"aios-web3-wire-test").expect("host");
        let wire = r#"{"id":"x","method":"eth_chainId","params":[],"origin":"https://a.example"}"#;
        let reply = host.handle_wire(wire);
        assert!(reply.contains("\"id\":\"x\""));
        assert!(reply.contains("0x1"));
    }
}
