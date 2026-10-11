use crate::types::{BrowserConfig, BrowserError, FetchResponse};
use std::collections::BTreeMap;

pub struct NetworkClient {
    client: reqwest::Client,
}

impl NetworkClient {
    pub fn new(config: &BrowserConfig) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(&config.user_agent)
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .redirect(reqwest::redirect::Policy::limited(config.max_redirects))
            .build()
            .unwrap_or_default();

        Self { client }
    }

    pub async fn fetch(&self, url: &str) -> Result<String, BrowserError> {
        let response = self.client.get(url).send().await?;

        if !response.status().is_success() {
            return Err(BrowserError::NetworkError(format!(
                "HTTP {}",
                response.status()
            )));
        }

        let body = response.text().await?;
        Ok(body)
    }

    /// Perform a page script's request and return a structured response for
    /// the JS bridge. Unlike [`NetworkClient::fetch`] a non-2xx status is not
    /// an error here — the status is handed to the script (`response.ok`),
    /// mirroring the browser `fetch` contract. Only transport failures carry
    /// `FetchResponse::error`.
    pub async fn request(&self, method: &str, url: &str, body: Option<&str>) -> FetchResponse {
        let mut builder = match method {
            "POST" => self.client.post(url),
            "PUT" => self.client.put(url),
            "DELETE" => self.client.delete(url),
            "HEAD" => self.client.head(url),
            _ => self.client.get(url),
        };
        if let Some(body) = body {
            builder = builder.body(body.to_string());
        }
        let response = match builder.send().await {
            Ok(r) => r,
            Err(e) => return FetchResponse::failure(url, e.to_string()),
        };
        let status = response.status();
        let final_url = response.url().to_string();
        let headers: BTreeMap<String, String> = response
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_ascii_lowercase(),
                    v.to_str().unwrap_or("").to_string(),
                )
            })
            .collect();
        let body = if method == "HEAD" {
            String::new()
        } else {
            match response.text().await {
                Ok(t) => t,
                Err(e) => return FetchResponse::failure(url, e.to_string()),
            }
        };
        FetchResponse {
            status: status.as_u16(),
            status_text: status.canonical_reason().unwrap_or("").to_string(),
            url: final_url,
            body,
            headers,
            error: None,
        }
    }

    pub async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, BrowserError> {
        let response = self.client.get(url).send().await?;

        if !response.status().is_success() {
            return Err(BrowserError::NetworkError(format!(
                "HTTP {}",
                response.status()
            )));
        }

        let bytes = response.bytes().await?;
        Ok(bytes.to_vec())
    }
}
