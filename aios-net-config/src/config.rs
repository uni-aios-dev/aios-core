use serde::{Deserialize, Serialize};

use crate::validation::{validate_ip, validate_port, validate_proxy};

/// The proxy protocol family supported by the network configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyProtocol {
    /// Plain HTTP proxy.
    Http,
    /// HTTPS (CONNECT-tunnelling) proxy.
    Https,
    /// SOCKS5 proxy.
    Socks5,
}

impl ProxyProtocol {
    /// Stable scheme prefix used when building a proxy URL.
    pub fn scheme(&self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::Socks5 => "socks5",
        }
    }
}

impl std::fmt::Display for ProxyProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.scheme())
    }
}

/// An HTTP/HTTPS/SOCKS proxy entry point.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProxyConfig {
    /// Proxy protocol (http, https or socks5).
    pub protocol: ProxyProtocol,
    /// Host name or IP address of the proxy server.
    pub host: String,
    /// TCP port of the proxy server.
    pub port: u16,
    /// Optional user name for authenticated proxies.
    pub username: Option<String>,
    /// Optional password for authenticated proxies.
    pub password: Option<String>,
}

impl ProxyConfig {
    /// Build a human-readable `host:port` address.
    pub fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Build a full `scheme://host:port` proxy URL.
    pub fn url(&self) -> String {
        format!("{}://{}", self.protocol.scheme(), self.authority())
    }
}

/// DNS resolution settings used by network clients.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DnsConfig {
    /// Preferred DNS server.
    pub primary: Option<String>,
    /// Fallback DNS server.
    pub secondary: Option<String>,
    /// DNS search domains appended to bare host names.
    pub search_domains: Vec<String>,
}

/// A single network interface entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InterfaceConfig {
    /// Interface name (e.g. `eth0` or `WLAN`).
    pub name: String,
    /// Assigned IP address, if any.
    pub ip: Option<String>,
    /// Subnet mask, if any.
    pub netmask: Option<String>,
    /// Default gateway, if any.
    pub gateway: Option<String>,
    /// MTU in bytes, if set.
    pub mtu: Option<u32>,
    /// Whether the interface uses DHCP for auto-configuration.
    pub dhcp: bool,
}

/// Complete set of network settings consumed by the network stack.
///
/// Serialized as JSON by `NetworkConfigStore`. Only the fields present in an
/// update payload are changed by `NetSettingsBlock::apply_updates`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkConfig {
    /// Schema version of the configuration.
    pub version: u32,
    /// Machine host name.
    pub hostname: String,
    /// Default TCP port the bridge/daemon listens on.
    pub listen_port: u16,
    /// Connection establishment timeout in milliseconds.
    pub connect_timeout_ms: u64,
    /// Maximum number of concurrent outbound connections.
    pub max_connections: u32,
    /// User-Agent used for outbound HTTP requests.
    pub user_agent: String,
    /// Optional outbound proxy.
    pub proxy: Option<ProxyConfig>,
    /// DNS settings.
    pub dns: DnsConfig,
    /// Known network interfaces.
    pub interfaces: Vec<InterfaceConfig>,
    /// Whether access to private/LAN addresses is allowed.
    pub allow_private_access: bool,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            version: 1,
            hostname: "aios-host".into(),
            listen_port: 8080,
            connect_timeout_ms: 15_000,
            max_connections: 64,
            user_agent: "AIOS/1.0 (net-config; +https://github.com/uni-aios-dev/aios-core)".into(),
            proxy: None,
            dns: DnsConfig::default(),
            interfaces: Vec::new(),
            allow_private_access: false,
        }
    }
}

fn parse_search_domains(v: &serde_json::Value) -> Result<Vec<String>, String> {
    let domains = v
        .as_array()
        .ok_or_else(|| "dns.search_domains must be an array".to_string())?
        .iter()
        .map(|d| {
            let domain = d
                .as_str()
                .ok_or_else(|| "search domain must be a string".to_string())?;
            if domain.trim().is_empty() {
                return Err("search domain must not be empty".to_string());
            }
            Ok(domain.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(domains)
}

fn parse_optional_ip(v: &serde_json::Value, field: &str) -> Result<Option<String>, String> {
    if v.is_null() {
        return Ok(None);
    }
    let ip = v
        .as_str()
        .ok_or_else(|| format!("{field} must be a string"))?;
    if !validate_ip(ip) {
        return Err(format!("Invalid {field} address: {ip}"));
    }
    Ok(Some(ip.to_string()))
}

impl NetworkConfig {
    /// Serialize the whole configuration to a JSON string.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Parse a configuration from a JSON string.
    pub fn from_json(data: &str) -> Result<Self, String> {
        serde_json::from_str(data).map_err(|e| format!("Invalid network config JSON: {e}"))
    }

    /// Apply a partial JSON update (only the keys present in `updates` change).
    ///
    /// Supported keys: `hostname`, `listen_port`, `connect_timeout_ms`,
    /// `max_connections`, `user_agent`, `allow_private_access`, `proxy`
    /// (object or `null`), `dns` (object, primary-IP string or search-domain
    /// array), `dns.primary`, `dns.secondary`, `dns.search_domains` and
    /// `interfaces` (array of [`InterfaceConfig`] objects).
    pub fn apply_updates(&mut self, updates: &serde_json::Value) -> Result<(), String> {
        if let Some(v) = updates.get("hostname") {
            let name = v
                .as_str()
                .ok_or_else(|| "hostname must be a string".to_string())?;
            if name.trim().is_empty() {
                return Err("hostname must not be empty".into());
            }
            self.hostname = name.to_string();
        }
        if let Some(v) = updates.get("listen_port") {
            let port = v
                .as_u64()
                .ok_or_else(|| "listen_port must be a number".to_string())?
                as u16;
            validate_port(port).map_err(|e| e.to_string())?;
            self.listen_port = port;
        }
        if let Some(v) = updates.get("connect_timeout_ms") {
            self.connect_timeout_ms = v
                .as_u64()
                .ok_or_else(|| "connect_timeout_ms must be a number".to_string())?;
        }
        if let Some(v) = updates.get("max_connections") {
            self.max_connections = v
                .as_u64()
                .ok_or_else(|| "max_connections must be a number".to_string())?
                as u32;
        }
        if let Some(v) = updates.get("user_agent") {
            self.user_agent = v
                .as_str()
                .ok_or_else(|| "user_agent must be a string".to_string())?
                .to_string();
        }
        if let Some(v) = updates.get("allow_private_access") {
            self.allow_private_access = v
                .as_bool()
                .ok_or_else(|| "allow_private_access must be a boolean".to_string())?;
        }
        if let Some(v) = updates.get("proxy") {
            if v.is_null() {
                self.proxy = None;
            } else {
                let proxy: ProxyConfig =
                    serde_json::from_value(v.clone()).map_err(|e| format!("Invalid proxy: {e}"))?;
                validate_proxy(&proxy)?;
                self.proxy = Some(proxy);
            }
        }
        if let Some(dns) = updates.get("dns") {
            match dns {
                serde_json::Value::Object(_) => {
                    if let Some(v) = dns.get("primary") {
                        self.dns.primary = parse_optional_ip(v, "dns.primary")?;
                    }
                    if let Some(v) = dns.get("secondary") {
                        self.dns.secondary = parse_optional_ip(v, "dns.secondary")?;
                    }
                    if let Some(v) = dns.get("search_domains") {
                        self.dns.search_domains = parse_search_domains(v)?;
                    }
                }
                serde_json::Value::String(_) => {
                    self.dns.primary = parse_optional_ip(dns, "dns.primary")?;
                }
                serde_json::Value::Array(_) => {
                    self.dns.search_domains = parse_search_domains(dns)?;
                }
                serde_json::Value::Null => self.dns = DnsConfig::default(),
                _ => return Err("dns must be an object, address string, array or null".into()),
            }
        }
        if let Some(v) = updates.get("interfaces") {
            let list = v
                .as_array()
                .ok_or_else(|| "interfaces must be an array".to_string())?;
            let mut interfaces = Vec::with_capacity(list.len());
            for entry in list {
                let interface: InterfaceConfig = serde_json::from_value(entry.clone())
                    .map_err(|e| format!("Invalid interface: {e}"))?;
                if interface.name.trim().is_empty() {
                    return Err("interface name must not be empty".into());
                }
                for (field, value) in [
                    ("ip", &interface.ip),
                    ("netmask", &interface.netmask),
                    ("gateway", &interface.gateway),
                ] {
                    if let Some(ip) = value {
                        if !validate_ip(ip) {
                            return Err(format!("Invalid interface {field} address: {ip}"));
                        }
                    }
                }
                if interface.mtu == Some(0) {
                    return Err("interface mtu must be greater than zero".into());
                }
                interfaces.push(interface);
            }
            self.interfaces = interfaces;
        }
        self.version = self.version.max(2);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> NetworkConfig {
        NetworkConfig::default()
    }

    #[test]
    fn test_default_values() {
        let c = base();
        assert_eq!(c.hostname, "aios-host");
        assert_eq!(c.listen_port, 8080);
        assert_eq!(c.max_connections, 64);
        assert_eq!(c.version, 1);
    }

    #[test]
    fn test_json_roundtrip() {
        let mut c = base();
        c.proxy = Some(ProxyConfig {
            protocol: ProxyProtocol::Socks5,
            host: "127.0.0.1".into(),
            port: 9050,
            username: None,
            password: None,
        });
        let json = c.to_json();
        let restored = NetworkConfig::from_json(&json).unwrap();
        assert_eq!(restored, c);
    }

    #[test]
    fn test_apply_hostname() {
        let mut c = base();
        let u = serde_json::json!({ "hostname": "workstation" });
        c.apply_updates(&u).unwrap();
        assert_eq!(c.hostname, "workstation");
    }

    #[test]
    fn test_apply_port_validation() {
        let mut c = base();
        let u = serde_json::json!({ "listen_port": 0 });
        assert!(c.apply_updates(&u).is_err());
    }

    #[test]
    fn test_apply_proxy_object() {
        let mut c = base();
        let u = serde_json::json!({
            "proxy": { "protocol": "http", "host": "proxy.local", "port": 3128 }
        });
        c.apply_updates(&u).unwrap();
        let p = c.proxy.unwrap();
        assert_eq!(p.authority(), "proxy.local:3128");
        assert_eq!(p.url(), "http://proxy.local:3128");
    }

    #[test]
    fn test_apply_proxy_clear() {
        let mut c = base();
        c.proxy = Some(ProxyConfig {
            protocol: ProxyProtocol::Http,
            host: "x".into(),
            port: 1,
            username: None,
            password: None,
        });
        let u = serde_json::json!({ "proxy": null });
        c.apply_updates(&u).unwrap();
        assert!(c.proxy.is_none());
    }

    #[test]
    fn test_apply_dns() {
        let mut c = base();
        let u = serde_json::json!({
            "dns": { "primary": "1.1.1.1", "secondary": "8.8.8.8", "search_domains": ["lan"] }
        });
        c.apply_updates(&u).unwrap();
        assert_eq!(c.dns.primary.as_deref(), Some("1.1.1.1"));
        assert_eq!(c.dns.secondary.as_deref(), Some("8.8.8.8"));
        assert_eq!(c.dns.search_domains, vec!["lan"]);
    }

    #[test]
    fn test_apply_dns_shorthands() {
        let mut c = base();
        c.apply_updates(&serde_json::json!({ "dns": "1.1.1.1" }))
            .unwrap();
        assert_eq!(c.dns.primary.as_deref(), Some("1.1.1.1"));
        c.apply_updates(&serde_json::json!({ "dns": ["lan", "example.test"] }))
            .unwrap();
        assert_eq!(c.dns.search_domains, vec!["lan", "example.test"]);
        assert_eq!(c.dns.primary.as_deref(), Some("1.1.1.1"));
        c.apply_updates(&serde_json::json!({ "dns": null }))
            .unwrap();
        assert_eq!(c.dns, DnsConfig::default());
    }

    #[test]
    fn test_apply_interfaces() {
        let mut c = base();
        let u = serde_json::json!({
            "interfaces": [{
                "name": "eth0",
                "ip": "10.0.0.5",
                "netmask": "255.255.255.0",
                "gateway": "10.0.0.1",
                "mtu": 1500,
                "dhcp": false
            }]
        });
        c.apply_updates(&u).unwrap();
        assert_eq!(c.interfaces.len(), 1);
        assert_eq!(c.interfaces[0].name, "eth0");
        assert_eq!(c.interfaces[0].ip.as_deref(), Some("10.0.0.5"));
        assert_eq!(c.version, 2);
    }

    #[test]
    fn test_apply_rejects_invalid_interface() {
        let mut c = base();
        let u = serde_json::json!({
            "interfaces": [{ "name": "eth0", "ip": "not-an-ip", "netmask": null, "gateway": null, "mtu": null, "dhcp": true }]
        });
        assert!(c.apply_updates(&u).is_err());
        assert!(c.interfaces.is_empty());
    }

    #[test]
    fn test_apply_invalid_dns_rejected() {
        let mut c = base();
        let u = serde_json::json!({ "dns": { "primary": "not-an-ip" } });
        assert!(c.apply_updates(&u).is_err());
    }

    #[test]
    fn test_partial_update_keeps_rest() {
        let mut c = base();
        c.max_connections = 8;
        let u = serde_json::json!({ "user_agent": "custom" });
        c.apply_updates(&u).unwrap();
        assert_eq!(c.user_agent, "custom");
        assert_eq!(c.max_connections, 8);
        assert_eq!(c.listen_port, 8080);
    }
}
