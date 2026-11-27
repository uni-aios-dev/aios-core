use crate::types::{BrowserConfig, BrowserError, FetchResponse};
use reqwest::{redirect::Policy, Response};
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use url::Url;

/// HTTP client with an explicit SSRF policy.
///
/// Only `http` and `https` are accepted. Every hop (initial URL and each
/// redirect) is resolved and rejected when it points at loopback, private,
/// link-local or otherwise non-global addresses, unless private targets are
/// explicitly enabled in [`BrowserConfig`]. DNS names are pinned to the
/// validated addresses for the request, so a second resolution cannot redirect
/// the connection to a private host. Automatic redirects are disabled so the
/// policy can be applied to every hop. Response bodies are streamed with a
/// hard size cap.
pub struct NetworkClient {
    client: reqwest::Client,
    user_agent: String,
    timeout: Duration,
    max_redirects: usize,
    max_response_bytes: usize,
    allow_private_targets: bool,
}

struct PinnedDns {
    host: String,
    addresses: Vec<SocketAddr>,
}

fn build_client(
    user_agent: &str,
    timeout: Duration,
    pinned: Option<(&str, &[SocketAddr])>,
) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(timeout)
        .redirect(Policy::none());
    if let Some((host, addresses)) = pinned {
        builder = builder.resolve_to_addrs(host, addresses);
    }
    builder
        .build()
        .unwrap_or_else(|e| panic!("cannot build browser HTTP client: {e}"))
}

impl NetworkClient {
    /// Build a client from the browser configuration.
    pub fn new(config: &BrowserConfig) -> Self {
        let timeout = Duration::from_secs(config.timeout_secs);
        let client = build_client(&config.user_agent, timeout, None);

        let allow_private_targets = config.allow_private_targets
            || std::env::var("AIOS_BROWSER_ALLOW_PRIVATE")
                .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
                .unwrap_or(false);

        Self {
            client,
            user_agent: config.user_agent.clone(),
            timeout,
            max_redirects: config.max_redirects,
            max_response_bytes: config.max_response_bytes,
            allow_private_targets,
        }
    }

    /// Fetch a URL as text, rejecting HTTP error statuses.
    pub async fn fetch(&self, url: &str) -> Result<String, BrowserError> {
        let response = self.send_checked("GET", url, None).await?;
        if !response.status().is_success() {
            return Err(BrowserError::NetworkError(format!(
                "HTTP {}",
                response.status()
            )));
        }
        let bytes = self.limited_bytes(response).await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Perform a page script's request and return a structured response for
    /// the JS bridge. Unlike [`NetworkClient::fetch`] a non-2xx status is not
    /// an error here — the status is handed to the script (`response.ok`),
    /// mirroring the browser `fetch` contract. Only transport and policy
    /// failures carry `FetchResponse::error`.
    pub async fn request(&self, method: &str, url: &str, body: Option<&str>) -> FetchResponse {
        let response = match self.send_checked(method, url, body).await {
            Ok(response) => response,
            Err(error) => return FetchResponse::failure(url, error.to_string()),
        };
        let status = response.status();
        let final_url = response.url().to_string();
        let headers: BTreeMap<String, String> = response
            .headers()
            .iter()
            .map(|(key, value)| {
                (
                    key.as_str().to_ascii_lowercase(),
                    value.to_str().unwrap_or("").to_string(),
                )
            })
            .collect();
        let bytes = if method == "HEAD" {
            Vec::new()
        } else {
            match self.limited_bytes(response).await {
                Ok(bytes) => bytes,
                Err(error) => return FetchResponse::failure(url, error.to_string()),
            }
        };
        FetchResponse {
            status: status.as_u16(),
            status_text: status.canonical_reason().unwrap_or("").to_string(),
            url: final_url,
            body: String::from_utf8_lossy(&bytes).into_owned(),
            headers,
            error: None,
        }
    }

    /// Fetch a URL as bytes, rejecting HTTP error statuses.
    pub async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, BrowserError> {
        let response = self.send_checked("GET", url, None).await?;
        if !response.status().is_success() {
            return Err(BrowserError::NetworkError(format!(
                "HTTP {}",
                response.status()
            )));
        }
        self.limited_bytes(response).await
    }

    async fn send_checked(
        &self,
        method: &str,
        url: &str,
        body: Option<&str>,
    ) -> Result<Response, BrowserError> {
        let mut current = Url::parse(url)
            .map_err(|e| BrowserError::NetworkError(format!("Invalid URL '{url}': {e}")))?;
        let mut current_method = method.to_string();
        let mut current_body = body.map(str::to_string);

        for hop in 0..=self.max_redirects {
            let pinned = self.validate(&current).await?;
            let client = match &pinned {
                Some(pin) => build_client(
                    &self.user_agent,
                    self.timeout,
                    Some((&pin.host, &pin.addresses)),
                ),
                None => self.client.clone(),
            };
            let mut builder = match current_method.as_str() {
                "POST" => client.post(current.clone()),
                "PUT" => client.put(current.clone()),
                "DELETE" => client.delete(current.clone()),
                "HEAD" => client.head(current.clone()),
                _ => client.get(current.clone()),
            };
            if let Some(payload) = current_body.as_deref() {
                builder = builder.body(payload.to_string());
            }
            let response = builder.send().await?;
            let redirect = response.status().is_redirection();
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            if !redirect {
                return Ok(response);
            }
            if hop == self.max_redirects {
                return Err(BrowserError::NetworkError("Too many redirects".to_string()));
            }
            let Some(location) = location else {
                return Ok(response);
            };
            current = current.join(&location).map_err(|e| {
                BrowserError::NetworkError(format!("Invalid redirect URL '{location}': {e}"))
            })?;
            if response.status() == reqwest::StatusCode::SEE_OTHER {
                current_method = "GET".to_string();
                current_body = None;
            }
        }
        Err(BrowserError::NetworkError("Too many redirects".to_string()))
    }

    async fn validate(&self, url: &Url) -> Result<Option<PinnedDns>, BrowserError> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(BrowserError::NetworkError(format!(
                "URL scheme '{}' is not allowed",
                url.scheme()
            )));
        }
        if self.allow_private_targets {
            return Ok(None);
        }
        let Some(host) = url.host_str() else {
            return Err(BrowserError::NetworkError("URL has no host".into()));
        };
        if let Ok(ip) = host.parse::<IpAddr>() {
            public_ip(ip).ok_or_else(|| {
                BrowserError::NetworkError(format!("Private or non-global host '{host}' blocked"))
            })?;
            return Ok(None);
        }
        let port = url.port_or_known_default().unwrap_or(80);
        let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
            .await
            .map_err(|e| BrowserError::NetworkError(format!("Cannot resolve '{host}': {e}")))?
            .collect();
        if addresses.is_empty() {
            return Err(BrowserError::NetworkError(format!(
                "Cannot resolve '{host}'"
            )));
        }
        for address in &addresses {
            public_ip(address.ip()).ok_or_else(|| {
                BrowserError::NetworkError(format!("Host '{host}' resolves to a private address"))
            })?;
        }
        Ok(Some(PinnedDns {
            host: host.to_string(),
            addresses,
        }))
    }

    async fn limited_bytes(&self, response: Response) -> Result<Vec<u8>, BrowserError> {
        if let Some(length) = response.content_length() {
            if length as usize > self.max_response_bytes {
                return Err(BrowserError::NetworkError(format!(
                    "Response too large: {length} bytes"
                )));
            }
        }
        let mut response = response;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len().saturating_add(chunk.len()) > self.max_response_bytes {
                return Err(BrowserError::NetworkError(format!(
                    "Response exceeds {} bytes",
                    self.max_response_bytes
                )));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

fn public_ip(ip: IpAddr) -> Option<()> {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4.octets()).then_some(()),
        IpAddr::V6(v6) => is_public_v6(v6.segments()).then_some(()),
    }
}

fn is_public_v4(o: [u8; 4]) -> bool {
    let (a, b, c) = (o[0], o[1], o[2]);
    if a == 0 || a == 10 || a == 127 || a >= 224 || a == 169 && b == 254 {
        return false;
    }
    if a == 100 && (64..=127).contains(&b) {
        return false;
    }
    if a == 172 && (16..=31).contains(&b) {
        return false;
    }
    if a == 192 && (b == 168 || b == 0 || b == 88 && c == 99) {
        return false;
    }
    if a == 198 && (b == 18 || b == 19 || b == 51 && c == 100) {
        return false;
    }
    if a == 203 && b == 0 && c == 113 {
        return false;
    }
    true
}

fn is_public_v6(s: [u16; 8]) -> bool {
    if s[0] == 0 && s[1] == 0 && s[2] == 0 && s[3] == 0 && s[4] == 0 {
        if s[5] == 0 {
            return false;
        }
        if s[5] == 0xffff {
            let a = (s[6] >> 8) as u8;
            let b = s[6] as u8;
            let c = (s[7] >> 8) as u8;
            let d = s[7] as u8;
            return is_public_v4([a, b, c, d]);
        }
    }
    if s[0] & 0xfe00 == 0xfc00 || s[0] & 0xffc0 == 0xfe80 || s[0] & 0xff00 == 0xff00 {
        return false;
    }
    if s[0] == 0x2001 && ((s[1] & 0xfe00) == 0 || s[1] == 0x0db8) {
        return false;
    }
    if s[0] == 0x2002 {
        let a = (s[1] >> 8) as u8;
        let b = s[1] as u8;
        let c = (s[2] >> 8) as u8;
        let d = s[2] as u8;
        return is_public_v4([a, b, c, d]);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_non_http_schemes() {
        let client = NetworkClient::new(&BrowserConfig::default());
        for url in [
            "file:///etc/passwd",
            "ftp://example.com/a",
            "data:text/plain,x",
        ] {
            let parsed = Url::parse(url).unwrap();
            assert!(client.validate(&parsed).await.is_err());
        }
    }

    #[tokio::test]
    async fn rejects_private_literals() {
        let client = NetworkClient::new(&BrowserConfig::default());
        for url in [
            "http://127.0.0.1:8080/",
            "http://10.0.0.1/",
            "http://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/",
            "http://0.0.0.0/",
        ] {
            let parsed = Url::parse(url).unwrap();
            assert!(client.validate(&parsed).await.is_err(), "{url}");
        }
    }

    #[tokio::test]
    async fn allows_public_literals() {
        let client = NetworkClient::new(&BrowserConfig::default());
        for url in ["http://93.184.215.14/", "https://example.com/"] {
            let parsed = Url::parse(url).unwrap();
            assert!(client.validate(&parsed).await.is_ok(), "{url}");
        }
    }

    #[tokio::test]
    async fn explicit_opt_in_allows_loopback() {
        let config = BrowserConfig {
            allow_private_targets: true,
            ..Default::default()
        };
        let client = NetworkClient::new(&config);
        let parsed = Url::parse("http://127.0.0.1:1/").unwrap();
        assert!(client.validate(&parsed).await.is_ok());
    }

    #[tokio::test]
    async fn dns_names_are_checked() {
        let client = NetworkClient::new(&BrowserConfig::default());
        let parsed = Url::parse("http://localhost/").unwrap();
        assert!(client.validate(&parsed).await.is_err());
    }

    #[tokio::test]
    async fn dns_names_are_pinned_to_validated_addresses() {
        let client = NetworkClient::new(&BrowserConfig::default());
        let parsed = Url::parse("https://example.com/").unwrap();
        let pin = client
            .validate(&parsed)
            .await
            .unwrap()
            .expect("public DNS name");
        assert_eq!(pin.host, "example.com");
        assert!(!pin.addresses.is_empty());
        assert!(pin
            .addresses
            .iter()
            .all(|addr| public_ip(addr.ip()).is_some()));
    }

    #[tokio::test]
    async fn ip_literals_need_no_dns_pin() {
        let client = NetworkClient::new(&BrowserConfig::default());
        let parsed = Url::parse("http://93.184.215.14/").unwrap();
        assert!(client.validate(&parsed).await.unwrap().is_none());
    }
}
