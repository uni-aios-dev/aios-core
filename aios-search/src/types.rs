use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    pub backend: SearchBackend,
    pub api_key: Option<String>,
    pub api_url: Option<String>,
    pub max_results: usize,
    pub enable_summary: bool,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            backend: SearchBackend::DuckDuckGo,
            api_key: None,
            api_url: None,
            max_results: 10,
            enable_summary: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SearchBackend {
    DuckDuckGo,
    SearXNG,
    Brave,
}

impl SearchBackend {
    pub fn default_url(&self) -> &str {
        match self {
            SearchBackend::DuckDuckGo => "https://html.duckduckgo.com/html/",
            SearchBackend::SearXNG => "http://localhost:8888/search",
            SearchBackend::Brave => "https://api.search.brave.com/res/v1/web/search",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchSummary {
    pub query: String,
    pub results: Vec<SearchResult>,
    pub total_results: usize,
    pub summary: Option<String>,
    pub duration_ms: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("Network error: {0}")]
    NetworkError(String),
    #[error("Parse error: {0}")]
    ParseError(String),
    #[error("Backend unavailable: {0}")]
    BackendUnavailable(String),
    #[error("No results")]
    NoResults,
    #[error("HTTP error: {0}")]
    HttpError(#[from] reqwest::Error),
    #[error("LLM error: {0}")]
    LlmError(String),
}
