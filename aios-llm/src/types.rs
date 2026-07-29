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
