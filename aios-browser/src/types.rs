use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A page script's queued network request (`fetch` / `XMLHttpRequest`).
///
/// The embedded engine has no host functions, so scripts cannot call into
/// Rust directly. Instead each request is parked in a JS queue, the engine
/// drains the queue between script phases, performs the fetch through the
/// [`NetworkClient`](crate::network::NetworkClient), and resolves the JS
/// promise/callback with the response — a cooperative two-phase model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchRequest {
    /// Correlates the request with its pending promise/callback in JS.
    pub id: String,
    /// The (possibly relative) request URL.
    pub url: String,
    /// HTTP method (upper-case); `GET` when omitted.
    #[serde(default)]
    pub method: Option<String>,
    /// Request body for `POST`/`PUT`.
    #[serde(default)]
    pub body: Option<String>,
    /// `"xhr"` for an `XMLHttpRequest`, absent for `fetch`.
    #[serde(default)]
    pub kind: Option<String>,
}

/// A fetched response handed back to a parked JS request.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FetchResponse {
    /// HTTP status code (0 when the request failed before a response).
    pub status: u16,
    /// Reason phrase (`"OK"`, `"Not Found"`, ...).
    #[serde(default)]
    pub status_text: String,
    /// Final URL after redirects.
    #[serde(default)]
    pub url: String,
    /// Response body as text.
    #[serde(default)]
    pub body: String,
    /// Response headers, lower-cased keys.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Transport error message (`None` on success); when set, JS rejects.
    #[serde(default)]
    pub error: Option<String>,
}

impl FetchResponse {
    /// Build a failed response carrying a transport error message.
    pub fn failure(url: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status: 0,
            status_text: String::new(),
            url: url.into(),
            body: String::new(),
            headers: BTreeMap::new(),
            error: Some(message.into()),
        }
    }
}

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
