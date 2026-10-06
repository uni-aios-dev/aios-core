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
    /// Execute the page's `<script>` elements (and engine user scripts)
    /// through the embedded JS engine before rendering. Off = classic
    /// text-only fetch.
    pub execute_scripts: bool,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            user_agent: "AIOS-Browser/0.1".into(),
            timeout_secs: 30,
            max_redirects: 5,
            sandbox_enabled: true,
            headless_fallback: true,
            execute_scripts: true,
        }
    }
}

/// A script injected into every matching page after its own scripts ran
/// (Tampermonkey-style userscript).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserScript {
    /// Glob-ish URL filter: `"*"` matches everything, a plain substring
    /// matches any URL containing it, `*` acts as the wildcard.
    pub pattern: String,
    /// JavaScript source, evaluated document-end in the page context.
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub url: String,
    pub title: String,
    pub text_content: String,
    pub html: String,
    pub links: Vec<Link>,
    /// `console.*` lines emitted by the page's scripts (capped).
    #[serde(default)]
    pub console: Vec<String>,
    /// How many scripts completed without throwing (page + user scripts).
    #[serde(default)]
    pub scripts_executed: usize,
    /// Per-script failures: fetch errors, thrown exceptions, engine faults.
    #[serde(default)]
    pub script_errors: Vec<String>,
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
    #[error("IO error: {0}")]
    Io(String),
    #[error("Script error: {0}")]
    ScriptError(String),
}
