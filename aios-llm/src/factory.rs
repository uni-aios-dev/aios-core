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

    /// Returns a clone of the active configuration for introspection.
    pub fn config(&self) -> LlmConfig {
        match self {
            LlmEngine::Cloud(e) => e.config().clone(),
            LlmEngine::MicroLocal(e) | LlmEngine::FullLocal(e) => e.config().clone(),
        }
    }

    /// Short human-readable label of the active backend (e.g. `cloud/groq`).
    pub fn backend_label(&self) -> String {
        match self {
            LlmEngine::Cloud(e) => match e.config().backend {
                BackendKind::Cloud(ref p) => format!("cloud/{}", crate::types::provider_name(p)),
                _ => "cloud".into(),
            },
            LlmEngine::MicroLocal(_) => "local/micro".into(),
            LlmEngine::FullLocal(_) => "local/full".into(),
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
