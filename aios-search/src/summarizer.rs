use crate::types::{SearchError, SearchResult};
use aios_llm::{LlmEngine, LlmRequest};

pub struct SearchSummarizer {
    llm: Option<LlmEngine>,
}

impl SearchSummarizer {
    pub fn new(llm: Option<LlmEngine>) -> Self {
        Self { llm }
    }

    pub async fn summarize(
        &self,
        query: &str,
        results: &[SearchResult],
    ) -> Result<Option<String>, SearchError> {
        let llm = self.llm.as_ref().ok_or_else(|| {
            SearchError::LlmError("LLM engine not configured for summarization".into())
        })?;

        let results_text: String = results
            .iter()
            .take(5)
            .enumerate()
            .map(|(i, r)| {
                format!(
                    "{}. {} - {}\n   URL: {}\n",
                    i + 1,
                    r.title,
                    r.snippet,
                    r.url
                )
            })
            .collect();

        let system_prompt = "You are a search result summarizer. Summarize the following search results concisely in 2-3 sentences. Highlight the most relevant information for the user's query. Write in the same language as the query.";

        let user_prompt = format!(
            "Search query: {query}\n\nSearch results:\n{results_text}\n\nProvide a concise summary:"
        );

        let request = LlmRequest {
            system_prompt: system_prompt.into(),
            user_prompt,
            max_tokens: 300,
            temperature: 0.3,
        };

        match llm.query(&request).await {
            Ok(response) => Ok(Some(response.text)),
            Err(e) => Err(SearchError::LlmError(e.to_string())),
        }
    }
}
