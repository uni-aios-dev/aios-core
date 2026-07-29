use crate::html_parser::HtmlParser;
use crate::network::NetworkClient;
use crate::types::{BrowserConfig, BrowserError, Page};

pub struct BrowserEngine {
    config: BrowserConfig,
    network: NetworkClient,
}

impl BrowserEngine {
    pub fn new(config: BrowserConfig) -> Self {
        let network = NetworkClient::new(&config);
        Self { config, network }
    }

    pub async fn navigate(&self, url: &str) -> Result<Page, BrowserError> {
        let html = self.network.fetch(url).await?;

        let title = HtmlParser::extract_title(&html);
        let text_content = HtmlParser::extract_text(&html);
        let links = HtmlParser::extract_links(&html, url);

        Ok(Page {
            url: url.to_string(),
            title,
            text_content,
            html,
            links,
        })
    }

    pub fn config(&self) -> &BrowserConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_browser_engine_creation() {
        let config = BrowserConfig::default();
        let engine = BrowserEngine::new(config);
        assert_eq!(engine.config().user_agent, "AIOS-Browser/0.1");
    }

    #[test]
    fn test_browser_config_defaults() {
        let config = BrowserConfig::default();
        assert_eq!(config.timeout_secs, 30);
        assert!(config.sandbox_enabled);
    }

    #[tokio::test]
    async fn test_navigate_invalid_url() {
        let config = BrowserConfig::default();
        let engine = BrowserEngine::new(config);
        let result = engine.navigate("http://invalid.nonexistent.domain").await;
        assert!(result.is_err());
    }
}
