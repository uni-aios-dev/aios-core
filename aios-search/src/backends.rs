use crate::types::{SearchBackend, SearchConfig, SearchError, SearchResult};

pub struct DuckDuckGoBackend;
pub struct SearXngBackend;
pub struct BraveBackend;

pub enum SearchBackendImpl {
    DuckDuckGo(DuckDuckGoBackend),
    SearXNG(SearXngBackend),
    Brave(BraveBackend),
}

impl SearchBackendImpl {
    pub fn from_config(config: &SearchConfig) -> Self {
        match config.backend {
            SearchBackend::DuckDuckGo => Self::DuckDuckGo(DuckDuckGoBackend),
            SearchBackend::SearXNG => Self::SearXNG(SearXngBackend),
            SearchBackend::Brave => Self::Brave(BraveBackend),
        }
    }

    pub async fn search(&self, query: &str, config: &SearchConfig) -> Result<Vec<SearchResult>, SearchError> {
        match self {
            Self::DuckDuckGo(b) => b.search(query, config).await,
            Self::SearXNG(b) => b.search(query, config).await,
            Self::Brave(b) => b.search(query, config).await,
        }
    }
}

impl DuckDuckGoBackend {
    pub async fn search(&self, query: &str, config: &SearchConfig) -> Result<Vec<SearchResult>, SearchError> {
        let url = config.api_url.as_deref().unwrap_or(SearchBackend::DuckDuckGo.default_url());
        let client = reqwest::Client::new();
        let params = [("q", query)];

        let response = client
            .post(url)
            .form(&params)
            .header("User-Agent", "AIOS-Search/0.1")
            .send()
            .await?;

        let body = response.text().await?;
        let results = Self::parse_html_response(&body);
        Ok(results)
    }

    pub fn parse_html_response(body: &str) -> Vec<SearchResult> {
        let mut results = Vec::new();
        let mut pos = 0;

        while let Some(start) = body[pos..].find(r#"class="result__a"#) {
            let abs_start = pos + start;
            let href_start = body[abs_start..].find(r#"href=""#).map(|i| abs_start + i + 7);
            let href_end = href_start.and_then(|s| body[s..].find('"').map(|e| s + e));
            let href = href_end.map(|e| &body[href_start.unwrap()..e]).unwrap_or("");

            let title_start = body[abs_start..].find(r#"class="result__a"#).map(|i| abs_start + i);
            let title_begin = title_start.and_then(|s| body[s..].find(r#">"#).map(|i| s + i + 1));
            let title_end = title_begin.and_then(|s| body[s..].find("</a>").map(|e| s + e));
            let title = title_end.map(|e| &body[title_begin.unwrap()..e]).unwrap_or("");

            let snippet_start = body[abs_start..].find(r#"class="result__snippet"#).map(|i| abs_start + i);
            let snippet_begin = snippet_start.and_then(|s| body[s..].find(r#">"#).map(|i| s + i + 1));
            let snippet_end = snippet_begin.and_then(|s| body[s..].find("</a>").map(|e| s + e));
            let snippet = snippet_end.map(|e| &body[snippet_begin.unwrap()..e]).unwrap_or("");

            if !href.is_empty() {
                results.push(SearchResult {
                    title: Self::clean_text(title),
                    url: href.to_string(),
                    snippet: Self::clean_text(snippet),
                    source: "duckduckgo".into(),
                });
            }

            pos = abs_start + 1;
        }

        results
    }

    fn clean_text(text: &str) -> String {
        text.replace("&amp;", "&")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
    }
}

impl SearXngBackend {
    pub async fn search(&self, query: &str, config: &SearchConfig) -> Result<Vec<SearchResult>, SearchError> {
        let url = config.api_url.as_deref().unwrap_or(SearchBackend::SearXNG.default_url());
        let client = reqwest::Client::new();

        let response = client
            .get(url)
            .query(&[("q", query), ("format", "json")])
            .send()
            .await?;

        let bytes = response.bytes().await?;
        Self::parse_json_response(&bytes)
    }

    pub fn parse_json_response(body: &[u8]) -> Result<Vec<SearchResult>, SearchError> {
        let value: serde_json::Value =
            serde_json::from_slice(body).map_err(|e| SearchError::ParseError(e.to_string()))?;

        let mut results = Vec::new();

        if let Some(results_arr) = value.get("results").and_then(|r| r.as_array()) {
            for item in results_arr {
                let title = item.get("title").and_then(|t| t.as_str()).unwrap_or("");
                let url = item.get("url").and_then(|u| u.as_str()).unwrap_or("");
                let content = item.get("content").and_then(|c| c.as_str()).unwrap_or("");

                if !url.is_empty() {
                    results.push(SearchResult {
                        title: title.to_string(),
                        url: url.to_string(),
                        snippet: content.to_string(),
                        source: "searxng".into(),
                    });
                }
            }
        }

        Ok(results)
    }
}

impl BraveBackend {
    pub async fn search(&self, query: &str, config: &SearchConfig) -> Result<Vec<SearchResult>, SearchError> {
        let api_key = config.api_key.as_deref().ok_or_else(|| SearchError::BackendUnavailable("Brave requires API key".into()))?;
        let url = config.api_url.as_deref().unwrap_or(SearchBackend::Brave.default_url());
        let client = reqwest::Client::new();

        let response = client
            .get(url)
            .query(&[("q", query)])
            .header("Accept", "application/json")
            .header("X-Subscription-Token", api_key)
            .send()
            .await?;

        let bytes = response.bytes().await?;
        Self::parse_json_response(&bytes)
    }

    pub fn parse_json_response(body: &[u8]) -> Result<Vec<SearchResult>, SearchError> {
        let value: serde_json::Value =
            serde_json::from_slice(body).map_err(|e| SearchError::ParseError(e.to_string()))?;

        let mut results = Vec::new();

        if let Some(web) = value.get("web") {
            if let Some(results_arr) = web.get("results").and_then(|r| r.as_array()) {
                for item in results_arr {
                    let title = item.get("title").and_then(|t| t.as_str()).unwrap_or("");
                    let url = item.get("url").and_then(|u| u.as_str()).unwrap_or("");
                    let description = item.get("description").and_then(|d| d.as_str()).unwrap_or("");

                    if !url.is_empty() {
                        results.push(SearchResult {
                            title: title.to_string(),
                            url: url.to_string(),
                            snippet: description.to_string(),
                            source: "brave".into(),
                        });
                    }
                }
            }
        }

        Ok(results)
    }
}
