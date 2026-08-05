use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    pub backend: BackendKind,
    pub model: String,
    pub api_key: Option<String>,
    pub api_url: Option<String>,
    pub max_tokens: u32,
    pub temperature: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BackendKind {
    Cloud(CloudProvider),
    MicroLocal,
    FullLocal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CloudProvider {
    Groq,
    OpenRouter,
    GoogleAiStudio,
}

impl CloudProvider {
    pub fn default_url(&self) -> &str {
        match self {
            CloudProvider::Groq => "https://api.groq.com/openai/v1/chat/completions",
            CloudProvider::OpenRouter => "https://openrouter.ai/api/v1/chat/completions",
            CloudProvider::GoogleAiStudio => "https://generativelanguage.googleapis.com/v1/models",
        }
    }

    pub fn default_model(&self) -> &str {
        match self {
            CloudProvider::Groq => "llama-3.3-70b-versatile",
            CloudProvider::OpenRouter => "meta-llama/llama-3.3-70b-instruct",
            CloudProvider::GoogleAiStudio => "gemini-2.0-flash",
        }
    }
}

/// Short lower-case name of a cloud provider (`groq`, `openrouter`, `google`).
pub fn provider_name(p: &CloudProvider) -> &'static str {
    match p {
        CloudProvider::Groq => "groq",
        CloudProvider::OpenRouter => "openrouter",
        CloudProvider::GoogleAiStudio => "google",
    }
}

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub system_prompt: String,
    pub user_prompt: String,
    pub max_tokens: u32,
    pub temperature: f32,
}

impl Default for LlmRequest {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            user_prompt: String::new(),
            max_tokens: 512,
            temperature: 0.7,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub text: String,
    pub tokens_used: u32,
    pub duration_ms: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("Backend not available: {0}")]
    NotAvailable(String),
    #[error("API error: {0}")]
    ApiError(String),
    #[error("HTTP error: {0}")]
    HttpError(#[from] reqwest::Error),
    #[error("Serialization error: {0}")]
    SerdeError(#[from] serde_json::Error),
}

pub type LlmResult<T> = Result<T, LlmError>;

/// Channel used to push incremental text deltas from a streaming query.
pub type LlmStreamSink = tokio::sync::mpsc::UnboundedSender<Result<String, LlmError>>;

/// Extracts the incremental text delta from one SSE `data:` payload.
/// Supports both the OpenAI `choices[0].delta.content` shape and the
/// Google AI Studio `candidates[0].content.parts[0].text` shape.
pub fn extract_stream_delta(payload: &str, google_shape: bool) -> LlmResult<String> {
    let json: serde_json::Value = serde_json::from_str(payload)?;
    let text = if google_shape {
        json["candidates"][0]["content"]["parts"][0]["text"].as_str()
    } else {
        json["choices"][0]["delta"]["content"]
            .as_str()
            .or_else(|| json["choices"][0]["text"].as_str())
    };
    Ok(text.unwrap_or_default().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_openai_delta() {
        let payload = r#"{"choices":[{"delta":{"content":"hello"},"index":0}]}"#;
        assert_eq!(extract_stream_delta(payload, false).unwrap(), "hello");
    }

    #[test]
    fn extract_openai_legacy_text() {
        let payload = r#"{"choices":[{"text":"legacy"}]}"#;
        assert_eq!(extract_stream_delta(payload, false).unwrap(), "legacy");
    }

    #[test]
    fn extract_google_part() {
        let payload = r#"{"candidates":[{"content":{"parts":[{"text":"gemini"}]}}]}"#;
        assert_eq!(extract_stream_delta(payload, true).unwrap(), "gemini");
    }

    #[test]
    fn extract_empty_delta() {
        assert_eq!(extract_stream_delta("{}", false).unwrap(), "");
        assert_eq!(extract_stream_delta("{}", true).unwrap(), "");
    }
}
