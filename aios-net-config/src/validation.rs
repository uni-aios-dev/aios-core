/// Validation helpers for network configuration values.
use crate::config::{ProxyConfig, ProxyProtocol};

/// Return `true` if `ip` is a syntactically valid IPv4 or IPv6 address.
pub fn validate_ip(ip: &str) -> bool {
    ip.parse::<std::net::IpAddr>().is_ok()
}

/// Validate a TCP/UDP port number (1..=65535, 0 reserved for ephemeral).
pub fn validate_port(port: u16) -> Result<(), String> {
    if port == 0 {
        return Err("port must be in 1..=65535".to_string());
    }
    Ok(())
}

/// Validate a proxy configuration: host non-empty, port in range, credentials consistent.
pub fn validate_proxy(proxy: &ProxyConfig) -> Result<(), String> {
    if proxy.host.trim().is_empty() {
        return Err("proxy host must not be empty".to_string());
    }
    validate_port(proxy.port)?;
    if proxy.username.is_none() != proxy.password.is_none() {
        return Err("proxy username and password must be set together".to_string());
    }
    Ok(())
}

/// Validate that `url` is a well-formed `http://` or `https://` URL.
pub fn validate_url(url: &str) -> Result<(), String> {
    let parsed = url
        .parse::<url::Url>()
        .map_err(|e| format!("Invalid URL: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => Ok(()),
        s => Err(format!("Unsupported URL scheme: {s}")),
    }
}

/// Build a validated `ProxyConfig` from a `protocol://host:port` string.
pub fn parse_proxy_url(input: &str) -> Result<ProxyConfig, String> {
    let s = input.trim();
    let (protocol, rest) = match s.split_once("://") {
        Some((p, r)) => {
            let proto = match p.to_ascii_lowercase().as_str() {
                "http" => ProxyProtocol::Http,
                "https" => ProxyProtocol::Https,
                "socks5" | "socks" => ProxyProtocol::Socks5,
                other => return Err(format!("Unsupported proxy protocol: {other}")),
            };
            (proto, r)
        }
        None => (ProxyProtocol::Http, s),
    };
    let host;
    let port;
    if let Some((h, p)) = rest.rsplit_once(':') {
        host = h.to_string();
        port = p
            .parse::<u16>()
            .map_err(|_| format!("Invalid proxy port: {p}"))?;
    } else {
        host = rest.to_string();
        port = 3128;
    }
    if host.is_empty() {
        return Err("proxy host must not be empty".to_string());
    }
    let proxy = ProxyConfig {
        protocol,
        host,
        port,
        username: None,
        password: None,
    };
    validate_proxy(&proxy)?;
    Ok(proxy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_ipv4() {
        assert!(validate_ip("192.168.1.1"));
    }

    #[test]
    fn test_valid_ipv6() {
        assert!(validate_ip("::1"));
        assert!(validate_ip("2001:db8::1"));
    }

    #[test]
    fn test_invalid_ip() {
        assert!(!validate_ip("999.999.999.999"));
        assert!(!validate_ip("not-an-ip"));
        assert!(!validate_ip(""));
    }

    #[test]
    fn test_port_zero_rejected() {
        assert!(validate_port(0).is_err());
        assert!(validate_port(8080).is_ok());
    }

    #[test]
    fn test_proxy_missing_host() {
        let p = ProxyConfig {
            protocol: ProxyProtocol::Http,
            host: "  ".into(),
            port: 3128,
            username: None,
            password: None,
        };
        assert!(validate_proxy(&p).is_err());
    }

    #[test]
    fn test_proxy_credentials_consistent() {
        let p = ProxyConfig {
            protocol: ProxyProtocol::Socks5,
            host: "x".into(),
            port: 1080,
            username: Some("u".into()),
            password: None,
        };
        assert!(validate_proxy(&p).is_err());
    }

    #[test]
    fn test_parse_full_proxy_url() {
        let p = parse_proxy_url("socks5://127.0.0.1:9050").unwrap();
        assert_eq!(p.protocol, ProxyProtocol::Socks5);
        assert_eq!(p.host, "127.0.0.1");
        assert_eq!(p.port, 9050);
    }

    #[test]
    fn test_parse_default_scheme_and_port() {
        let p = parse_proxy_url("proxy.local").unwrap();
        assert_eq!(p.protocol, ProxyProtocol::Http);
        assert_eq!(p.host, "proxy.local");
        assert_eq!(p.port, 3128);
    }

    #[test]
    fn test_parse_bad_protocol() {
        assert!(parse_proxy_url("ftp://x:1").is_err());
    }
}
