pub mod cloud;
pub mod factory;
pub mod local;
pub mod types;

pub use factory::{default_config, LlmEngine};
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let cfg = default_config();
        assert!(matches!(
            cfg.backend,
            BackendKind::Cloud(CloudProvider::Groq)
        ));
        assert!(!cfg.model.is_empty());
    }

    #[test]
    fn test_config_serde_roundtrip() {
        let cfg = LlmConfig {
            backend: BackendKind::Cloud(CloudProvider::OpenRouter),
            model: "test-model".into(),
            api_key: Some("sk-test".into()),
            api_url: None,
            max_tokens: 1024,
            temperature: 0.3,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let restored: LlmConfig = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            restored.backend,
            BackendKind::Cloud(CloudProvider::OpenRouter)
        ));
        assert_eq!(restored.model, "test-model");
        assert_eq!(restored.api_key, Some("sk-test".into()));
        assert_eq!(restored.max_tokens, 1024);
    }

    #[test]
    fn test_cloud_provider_defaults() {
        assert!(CloudProvider::Groq.default_url().contains("groq.com"));
        assert!(CloudProvider::OpenRouter
            .default_url()
            .contains("openrouter.ai"));
        assert!(CloudProvider::GoogleAiStudio
            .default_url()
            .contains("googleapis.com"));
        assert!(!CloudProvider::Groq.default_model().is_empty());
    }

    #[test]
    fn test_request_default() {
        let req = LlmRequest::default();
        assert_eq!(req.max_tokens, 512);
        assert!((req.temperature - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn test_engine_from_config_cloud() {
        let cfg = LlmConfig {
            backend: BackendKind::Cloud(CloudProvider::Groq),
            model: "llama".into(),
            api_key: Some("test-key".into()),
            api_url: None,
            max_tokens: 100,
            temperature: 0.5,
        };
        let engine = LlmEngine::from_config(cfg);
        assert!(matches!(engine, LlmEngine::Cloud(_)));
    }

    #[test]
    fn test_engine_from_config_micro_local() {
        let cfg = LlmConfig {
            backend: BackendKind::MicroLocal,
            model: "qwen2.5-0.5b".into(),
            api_key: None,
            api_url: None,
            max_tokens: 100,
            temperature: 0.5,
        };
        let engine = LlmEngine::from_config(cfg);
        assert!(matches!(engine, LlmEngine::MicroLocal(_)));
    }

    #[test]
    fn test_engine_from_config_full_local() {
        let cfg = LlmConfig {
            backend: BackendKind::FullLocal,
            model: "qwen2.5-7b".into(),
            api_key: None,
            api_url: None,
            max_tokens: 100,
            temperature: 0.5,
        };
        let engine = LlmEngine::from_config(cfg);
        assert!(matches!(engine, LlmEngine::FullLocal(_)));
    }

    #[test]
    fn test_detect_local_models_empty() {
        let models = local::detect_local_models();
        assert!(models.is_empty());
    }

    #[test]
    fn test_engine_config_accessor() {
        let cfg = default_config();
        let engine = LlmEngine::from_config(cfg.clone());
        let back = engine.config();
        assert_eq!(back.model, cfg.model);
        assert_eq!(back.max_tokens, cfg.max_tokens);
        assert!(!engine.backend_label().is_empty());
    }
}
