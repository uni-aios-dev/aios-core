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

    pub async fn search(
        &self,
        query: &str,
        config: &SearchConfig,
    ) -> Result<Vec<SearchResult>, SearchError> {
        match self {
            Self::DuckDuckGo(b) => b.search(query, config).await,
            Self::SearXNG(b) => b.search(query, config).await,
            Self::Brave(b) => b.search(query, config).await,
        }
    }
}

impl DuckDuckGoBackend {
    pub async fn search(
        &self,
        query: &str,
        config: &SearchConfig,
    ) -> Result<Vec<SearchResult>, SearchError> {
        let url = config
            .api_url
            .as_deref()
            .unwrap_or(SearchBackend::DuckDuckGo.default_url());
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
            let href_start = body[abs_start..]
                .find(r#"href=""#)
                .map(|i| abs_start + i + 6);
            let href_end = href_start.and_then(|s| body[s..].find('"').map(|e| s + e));
            let href = href_end
                .map(|e| &body[href_start.unwrap()..e])
                .unwrap_or("");

            let title_start = body[abs_start..]
                .find(r#"class="result__a"#)
                .map(|i| abs_start + i);
            let title_begin = title_start.and_then(|s| body[s..].find(r#">"#).map(|i| s + i + 1));
            let title_end = title_begin.and_then(|s| body[s..].find("</a>").map(|e| s + e));
            let title = title_end
                .map(|e| &body[title_begin.unwrap()..e])
                .unwrap_or("");

            let snippet_start = body[abs_start..]
                .find(r#"class="result__snippet"#)
                .map(|i| abs_start + i);
            let snippet_begin =
                snippet_start.and_then(|s| body[s..].find(r#">"#).map(|i| s + i + 1));
            let snippet_end = snippet_begin.and_then(|s| body[s..].find("</a>").map(|e| s + e));
            let snippet = snippet_end
                .map(|e| &body[snippet_begin.unwrap()..e])
                .unwrap_or("");

            if !href.is_empty() {
                results.push(SearchResult {
                    title: Self::clean_text(title),
                    url: Self::resolve_duckduckgo_url(href),
                    snippet: Self::clean_text(snippet),
                    source: "duckduckgo".into(),
                });
            }

            pos = abs_start + 1;
        }

        results
    }

    /// DuckDuckGo's HTML results wrap every real URL in a redirect link of the
    /// form `https://duckduckgo.com/l/?uddg=<urlencoded-real-url>&rut=...`.
    /// Unwrap the `uddg` query parameter so callers receive the actual target.
    fn resolve_duckduckgo_url(href: &str) -> String {
        url::Url::parse(href)
            .ok()
            .and_then(|u| {
                u.query_pairs()
                    .find(|(k, _)| k == "uddg")
                    .map(|(_, v)| v.into_owned())
            })
            .filter(|resolved| resolved.starts_with("http://") || resolved.starts_with("https://"))
            .unwrap_or_else(|| href.to_string())
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
    pub async fn search(
        &self,
        query: &str,
        config: &SearchConfig,
    ) -> Result<Vec<SearchResult>, SearchError> {
        let url = config
            .api_url
            .as_deref()
            .unwrap_or(SearchBackend::SearXNG.default_url());
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
    pub async fn search(
        &self,
        query: &str,
        config: &SearchConfig,
    ) -> Result<Vec<SearchResult>, SearchError> {
        let api_key = config
            .api_key
            .as_deref()
            .ok_or_else(|| SearchError::BackendUnavailable("Brave requires API key".into()))?;
        let url = config
            .api_url
            .as_deref()
            .unwrap_or(SearchBackend::Brave.default_url());
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
                    let description = item
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("");

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_duckduckgo_uddg() {
        let href = "https://duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage%3Fa%3D1%26b%3D2&rut=abc";
        assert_eq!(
            DuckDuckGoBackend::resolve_duckduckgo_url(href),
            "https://example.com/page?a=1&b=2"
        );
    }

    #[test]
    fn test_resolve_duckduckgo_non_redirect() {
        assert_eq!(
            DuckDuckGoBackend::resolve_duckduckgo_url("https://example.com/plain"),
            "https://example.com/plain"
        );
    }

    #[test]
    fn test_resolve_duckduckgo_invalid_uddg() {
        let href = "https://duckduckgo.com/l/?uddg=javascript%3Aalert(1)&rut=abc";
        assert_eq!(
            DuckDuckGoBackend::resolve_duckduckgo_url(href),
            href.to_string()
        );
    }

    #[test]
    fn test_parse_html_response_unwraps_uddg() {
        let body = r##"
<html>
  <a class="result__a" href="https://duckduckgo.com/l/?uddg=https%3A%2F%2Freal.example.com%2Fx&rut=1">Real</a>
</html>
"##;
        let results = DuckDuckGoBackend::parse_html_response(body);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://real.example.com/x");
    }
}
