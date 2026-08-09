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
        self.build_page(url, html).await
    }

    /// Build a [`Page`] from fetched HTML, falling back to a headless render
    /// when the plain fetch produced no readable text (JS-heavy sites).
    async fn build_page(&self, url: &str, html: String) -> Result<Page, BrowserError> {
        let text_content = HtmlParser::extract_text(&html);
        if self.config.headless_fallback && crate::headless::looks_like_js_shell(&text_content) {
            if let Ok(dumped) = crate::headless::render_to_html(url).await {
                if crate::headless::has_more_content(&text_content, &dumped) {
                    let title = HtmlParser::extract_title(&dumped);
                    let dumped_text = HtmlParser::extract_text(&dumped);
                    let links = HtmlParser::extract_links(&dumped, url);
                    return Ok(Page {
                        url: url.to_string(),
                        title,
                        text_content: dumped_text,
                        html: dumped,
                        links,
                    });
                }
            }
        }
        let title = HtmlParser::extract_title(&html);
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
        assert!(config.headless_fallback);
    }

    #[tokio::test]
    async fn test_build_page_rich_text_skips_fallback() {
        let html = format!(
            "<html><head><title>T</title></head><body><p>{}</p></body></html>",
            "word ".repeat(300)
        );
        let engine = BrowserEngine::new(BrowserConfig::default());
        let page = engine
            .build_page("https://example.com/", html.to_string())
            .await
            .unwrap();
        assert_eq!(page.title, "T");
        assert!(page.text_content.contains("word"));
        assert_eq!(page.url, "https://example.com/");
    }

    #[tokio::test]
    async fn test_build_page_shell_no_crash_with_fallback_disabled() {
        let html = "<html><body><div id=\"app\">Loading...</div></body></html>";
        let mut cfg = BrowserConfig::default();
        cfg.headless_fallback = false;
        let engine = BrowserEngine::new(cfg);
        let page = engine
            .build_page("https://example.com/", html.to_string())
            .await
            .unwrap();
        assert_eq!(page.text_content, "Loading...");
    }

    #[tokio::test]
    async fn test_navigate_invalid_url() {
        let config = BrowserConfig::default();
        let engine = BrowserEngine::new(config);
        let result = engine.navigate("http://invalid.nonexistent.domain").await;
        assert!(result.is_err());
    }
}
