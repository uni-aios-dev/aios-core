use crate::backends::SearchBackendImpl;
use crate::summarizer::SearchSummarizer;
use crate::types::{SearchConfig, SearchError, SearchSummary};
use aios_llm::LlmEngine;
use std::time::Instant;

pub struct SearchEngine {
    config: SearchConfig,
    summarizer: Option<SearchSummarizer>,
}

impl SearchEngine {
    pub fn new(config: SearchConfig) -> Self {
        Self {
            config,
            summarizer: None,
        }
    }

    pub fn with_llm(config: SearchConfig, llm: LlmEngine) -> Self {
        Self {
            summarizer: Some(SearchSummarizer::new(Some(llm))),
            config,
        }
    }

    pub async fn search(&self, query: &str) -> Result<SearchSummary, SearchError> {
        let start = Instant::now();
        let backend = SearchBackendImpl::from_config(&self.config);
        let mut results = backend.search(query, &self.config).await?;

        if results.len() > self.config.max_results {
            results.truncate(self.config.max_results);
        }

        let duration_ms = start.elapsed().as_millis() as u64;
        let total = results.len();

        let summary = if self.config.enable_summary {
            if let Some(summarizer) = &self.summarizer {
                summarizer.summarize(query, &results).await.unwrap_or(None)
            } else {
                None
            }
        } else {
            None
        };

        Ok(SearchSummary {
            query: query.to_string(),
            results,
            total_results: total,
            summary,
            duration_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SearchBackend;

    #[test]
    fn test_search_config_defaults() {
        let config = SearchConfig::default();
        assert!(matches!(config.backend, SearchBackend::DuckDuckGo));
        assert_eq!(config.max_results, 10);
        assert!(config.enable_summary);
    }

    #[test]
    fn test_search_engine_creation() {
        let config = SearchConfig::default();
        let engine = SearchEngine::new(config);
        assert!(engine.summarizer.is_none());
    }

    #[test]
    fn test_search_backend_urls() {
        assert!(SearchBackend::DuckDuckGo.default_url().contains("duckduckgo"));
        assert!(SearchBackend::SearXNG.default_url().contains("localhost"));
        assert!(SearchBackend::Brave.default_url().contains("brave.com"));
    }
}
