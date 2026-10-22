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
                LlmEngine::MicroLocal(Self::local_engine(config, LocalModelKind::Micro))
            }
            BackendKind::FullLocal => {
                LlmEngine::FullLocal(Self::local_engine(config, LocalModelKind::Full))
            }
        }
    }

    /// Builds a local engine and preloads the GGUF model pointed at by
    /// `AIOS_MODEL_PATH`, if that variable is set and the file is readable.
    fn local_engine(config: LlmConfig, kind: LocalModelKind) -> LocalEngine {
        let mut engine = LocalEngine::new(config, kind);
        if let Ok(path) = std::env::var("AIOS_MODEL_PATH") {
            let path = std::path::PathBuf::from(path);
            if path.is_file() {
                if let Err(e) = engine.load_model_from_path(&path) {
                    log::warn!("AIOS: failed to load AIOS_MODEL_PATH {path:?}: {e}");
                }
            } else {
                log::warn!("AIOS: AIOS_MODEL_PATH {path:?} does not exist");
            }
        }
        engine
    }

    pub async fn query(&self, request: &LlmRequest) -> LlmResult<LlmResponse> {
        match self {
            LlmEngine::Cloud(e) => e.query(request).await,
            LlmEngine::MicroLocal(e) => e.query(request).await,
            LlmEngine::FullLocal(e) => e.query(request).await,
        }
    }

    /// Streams a completion over `tx`, sending one message per token/delta.
    /// Errors are sent as `Err` messages on the channel; the channel is not
    /// closed by the caller.
    pub async fn query_stream(&self, request: &LlmRequest, tx: LlmStreamSink) {
        match self {
            LlmEngine::Cloud(e) => e.query_stream(request, tx).await,
            LlmEngine::MicroLocal(e) | LlmEngine::FullLocal(e) => e.query_stream(request, tx).await,
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

/// Backend selected by `AIOS_LLM_BACKEND`: `cloud` (default), `micro-local`
/// or `full-local`. The local backends are the ones that honour
/// `AIOS_MODEL_PATH`.
pub fn config_from_env() -> LlmConfig {
    let mut config = default_config();
    match std::env::var("AIOS_LLM_BACKEND")
        .unwrap_or_default()
        .as_str()
    {
        "micro-local" | "micro" => {
            config.backend = BackendKind::MicroLocal;
            config.model = "qwen2-local".into();
        }
        "full-local" | "local" => {
            config.backend = BackendKind::FullLocal;
            config.model = "qwen2-local".into();
        }
        _ => {}
    }
    config
}
