use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserConfig {
    pub user_agent: String,
    pub timeout_secs: u64,
    pub max_redirects: usize,
    pub sandbox_enabled: bool,
    /// Fall back to a headless Chromium-class browser when a page's plain
    /// text fetch returns no meaningful content (JS-rendered SPA shells).
    pub headless_fallback: bool,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            user_agent: "AIOS-Browser/0.1".into(),
            timeout_secs: 30,
            max_redirects: 5,
            sandbox_enabled: true,
            headless_fallback: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub url: String,
    pub title: String,
    pub text_content: String,
    pub html: String,
    pub links: Vec<Link>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Link {
    pub href: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomNode {
    pub tag: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<DomNode>,
    pub text: String,
}

#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    #[error("Network error: {0}")]
    NetworkError(String),
    #[error("Parse error: {0}")]
    ParseError(String),
    #[error("Capability denied: {0}")]
    CapabilityDenied(String),
    #[error("Timeout")]
    Timeout,
    #[error("HTTP error: {0}")]
    HttpError(#[from] reqwest::Error),
}
