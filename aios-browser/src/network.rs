use crate::types::{BrowserConfig, BrowserError};

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
