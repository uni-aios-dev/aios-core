use crate::cloud::CloudEngine;
use crate::local::{LocalEngine, LocalModelKind};
use crate::types::*;

pub enum LlmEngine {
    Cloud(CloudEngine),
    MicroLocal(LocalEngine),
    FullLocal(LocalEngine),
}

impl LlmEngine {
    pub fn from_config(config: LlmConfig) -> Self {
        match config.backend {
            BackendKind::Cloud(_) => LlmEngine::Cloud(CloudEngine::new(config)),
            BackendKind::MicroLocal => {
                LlmEngine::MicroLocal(LocalEngine::new(config, LocalModelKind::Micro))
            }
            BackendKind::FullLocal => {
                LlmEngine::FullLocal(LocalEngine::new(config, LocalModelKind::Full))
            }
        }
    }

    pub async fn query(&self, request: &LlmRequest) -> LlmResult<LlmResponse> {
        match self {
            LlmEngine::Cloud(e) => e.query(request).await,
            LlmEngine::MicroLocal(e) => e.query(request).await,
            LlmEngine::FullLocal(e) => e.query(request).await,
        }
    }
}

pub fn default_config() -> LlmConfig {
    LlmConfig {
        backend: BackendKind::Cloud(CloudProvider::Groq),
        model: CloudProvider::Groq.default_model().to_string(),
        api_key: std::env::var("AIOS_LLM_API_KEY").ok(),
        api_url: None,
        max_tokens: 512,
        temperature: 0.7,
    }
}
