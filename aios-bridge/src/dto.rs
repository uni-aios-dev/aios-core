use aios_store::manifest::SignatureInfo;
use serde::{Deserialize, Serialize};

/// Maximum accepted user prompt length in characters.
pub const MAX_PROMPT_CHARS: usize = 8_192;
/// Maximum accepted system prompt length in characters.
pub const MAX_SYSTEM_PROMPT_CHARS: usize = 4_096;
/// Maximum number of prompts in one workflow request.
pub const MAX_WORKFLOW_PROMPTS: usize = 64;
/// Maximum accepted `max_tokens` value.
pub const MAX_TOKENS: u32 = 4_096;
/// Maximum encoded length of an uploaded WASM block.
pub const MAX_WASM_BASE64_CHARS: usize = 16 * 1024 * 1024;
/// Maximum decoded size of an uploaded WASM block in bytes.
pub const MAX_WASM_BYTES: usize = 12 * 1024 * 1024;
/// Maximum accepted voice audio upload (WAV or s16le PCM) in bytes.
pub const MAX_VOICE_AUDIO_BYTES: usize = 10 * 1024 * 1024;
/// Maximum accepted voice transcript length in characters.
pub const MAX_VOICE_TEXT_CHARS: usize = 8_192;

/// Capture mode requested by the Studio microphone control.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceCaptureMode {
    PushToTalk,
    Toggle,
    Vad,
}

/// Start-recording request; an omitted mode keeps the pipeline default.
#[derive(Debug, Deserialize)]
pub struct VoiceStartRequest {
    pub mode: Option<VoiceCaptureMode>,
}

/// Text-only voice request, also used to confirm a privileged script.
#[derive(Debug, Deserialize)]
pub struct VoiceTextRequest {
    pub text: String,
    #[serde(default)]
    pub confirm: bool,
}

/// Acknowledgement carrying the current pipeline phase and optional details.
#[derive(Debug, Serialize)]
pub struct VoiceAckResponse {
    pub success: bool,
    pub phase: String,
    pub transcript: Option<String>,
    pub script: Option<String>,
    pub permissions: Vec<String>,
    pub output: Option<String>,
    pub error: Option<String>,
}

/// Snapshot of the latest voice pipeline state for UI polling.
#[derive(Debug, Serialize)]
pub struct VoiceStatusResponse {
    pub phase: String,
    pub transcript: String,
    pub script: String,
    pub permissions: Vec<String>,
    pub output: String,
    pub error: String,
}

#[derive(Debug, Deserialize)]
pub struct IntentRequest {
    pub prompt: String,
}

impl IntentRequest {
    /// Reject empty and oversized prompts before any parsing or LLM call.
    pub fn validate(&self) -> std::result::Result<(), String> {
        validate_prompt(&self.prompt, MAX_PROMPT_CHARS, "prompt")
    }
}

fn validate_prompt(value: &str, limit: usize, label: &str) -> std::result::Result<(), String> {
    let chars = value.chars().count();
    if chars == 0 {
        return Err(format!("{label} must not be empty"));
    }
    if chars > limit {
        return Err(format!("{label} exceeds {limit} characters"));
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct IntentResponse {
    pub success: bool,
    pub intent_type: String,
    pub description: String,
    pub result: serde_json::Value,
    pub required_capabilities: Vec<String>,
    pub execution_plan: Vec<ExecutionStep>,
}

#[derive(Debug, Serialize)]
pub struct ExecutionStep {
    pub step: String,
    pub action: String,
    pub target: String,
}

#[derive(Debug, Serialize)]
pub struct SystemStatus {
    pub status: String,
    pub watchdog: WatchdogStatus,
    pub processes: ProcessList,
    pub blocks: BlockList,
    pub resources: ResourceMetrics,
}

#[derive(Debug, Serialize)]
pub struct WatchdogStatus {
    pub state: String,
    pub uptime_secs: u64,
}

#[derive(Debug, Serialize)]
pub struct ProcessList {
    pub total: usize,
    pub running: usize,
    pub suspended: usize,
    pub entries: Vec<ProcessEntry>,
}

#[derive(Debug, Serialize)]
pub struct ProcessEntry {
    pub pid: u64,
    pub name: String,
    pub priority: String,
    pub state: String,
    pub ram_mb: u64,
    pub cpu_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct BlockList {
    pub total: usize,
    pub active: usize,
    pub entries: Vec<BlockEntry>,
}

#[derive(Debug, Serialize)]
pub struct BlockEntry {
    pub id: u32,
    pub name: String,
    pub version: String,
    pub state: String,
}

#[derive(Debug, Serialize)]
pub struct ResourceMetrics {
    pub ram_used_mb: u64,
    pub ram_total_mb: u64,
    pub ram_percent: f64,
    pub process_count: usize,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub uptime_secs: u64,
    pub bridge_version: String,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub code: u16,
    pub details: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WorkflowRequest {
    pub prompts: Vec<String>,
}

impl WorkflowRequest {
    /// Reject oversized batches and oversized individual prompts.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.prompts.is_empty() {
            return Err("prompts must not be empty".to_string());
        }
        if self.prompts.len() > MAX_WORKFLOW_PROMPTS {
            return Err(format!("prompts exceeds {MAX_WORKFLOW_PROMPTS} entries"));
        }
        for prompt in &self.prompts {
            validate_prompt(prompt, MAX_PROMPT_CHARS, "prompt")?;
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct WorkflowStepResult {
    pub step: usize,
    pub prompt: String,
    pub success: bool,
    pub intent_type: String,
    pub description: String,
    pub result: serde_json::Value,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WorkflowResponse {
    pub total_steps: usize,
    pub successful: usize,
    pub failed: usize,
    pub results: Vec<WorkflowStepResult>,
}

#[derive(Debug, Deserialize)]
pub struct LlmQueryRequest {
    pub prompt: String,
    pub system_prompt: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

impl LlmQueryRequest {
    /// Validate prompt sizes and the token budget before querying the LLM.
    pub fn validate(&self) -> std::result::Result<(), String> {
        validate_prompt(&self.prompt, MAX_PROMPT_CHARS, "prompt")?;
        if let Some(system) = &self.system_prompt {
            validate_prompt(system, MAX_SYSTEM_PROMPT_CHARS, "system_prompt")?;
        }
        if let Some(tokens) = self.max_tokens {
            if tokens == 0 || tokens > MAX_TOKENS {
                return Err(format!("max_tokens must be 1..={MAX_TOKENS}"));
            }
        }
        if let Some(temperature) = self.temperature {
            if !temperature.is_finite() || !(0.0..=2.0).contains(&temperature) {
                return Err("temperature must be between 0 and 2".to_string());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct LlmQueryResponse {
    pub success: bool,
    pub text: Option<String>,
    pub duration_ms: u64,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct BrowseRequest {
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct BrowseResponse {
    pub success: bool,
    pub title: String,
    pub text_content: String,
    pub links: Vec<serde_json::Value>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub backend: Option<String>,
    pub max_results: Option<usize>,
    pub enable_summary: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct StoreIndexResponse {
    pub success: bool,
    pub count: usize,
    pub manifests: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct StoreRegisterRequest {
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub checksum_sha256: String,
}

#[derive(Debug, Serialize)]
pub struct StoreRegisterResponse {
    pub success: bool,
    pub name: String,
    pub version: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StorePublishRequest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub checksum_sha256: String,
    pub wasm_base64: String,
    /// Optional Ed25519 signature over the canonical manifest bytes (see
    /// `aios-store::manifest::sign_manifest`). When present, the bridge
    /// verifies it before installing; when absent the install follows the
    /// installer's trust policy (`AIOS_TRUSTED_PUBLIC_KEYS`).
    #[serde(default)]
    pub signature: Option<SignatureInfo>,
}

impl StorePublishRequest {
    /// Reject oversized uploads before base64 decoding or installation.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.wasm_base64.len() > MAX_WASM_BASE64_CHARS {
            return Err(format!(
                "wasm_base64 exceeds {MAX_WASM_BASE64_CHARS} characters"
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StorePublishResponse {
    pub success: bool,
    pub name: String,
    pub version: String,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MetricsResponse {
    pub success: bool,
    pub prometheus: String,
}

#[derive(Debug, Serialize)]
pub struct TracesResponse {
    pub success: bool,
    pub traces: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct CrashReportRequest {
    pub kind: String,
    pub message: String,
    pub stack_trace: Option<String>,
    pub zero_knowledge: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct CrashReportResponse {
    pub success: bool,
    pub report: Option<serde_json::Value>,
    pub total_reports: usize,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub success: bool,
    pub query: String,
    pub results: Vec<serde_json::Value>,
    pub total_results: usize,
    pub summary: Option<String>,
    pub duration_ms: u64,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub success: bool,
    pub token: Option<String>,
    pub username: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MeResponse {
    pub username: String,
    pub user_count: usize,
    pub data_dir: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_prompt_limits_are_enforced() {
        assert!(IntentRequest {
            prompt: String::new()
        }
        .validate()
        .is_err());
        assert!(IntentRequest {
            prompt: "x".repeat(MAX_PROMPT_CHARS)
        }
        .validate()
        .is_ok());
        assert!(IntentRequest {
            prompt: "x".repeat(MAX_PROMPT_CHARS + 1)
        }
        .validate()
        .is_err());
    }

    #[test]
    fn workflow_batch_limits_are_enforced() {
        assert!(WorkflowRequest {
            prompts: Vec::new()
        }
        .validate()
        .is_err());
        assert!(WorkflowRequest {
            prompts: vec!["ok".into(); MAX_WORKFLOW_PROMPTS]
        }
        .validate()
        .is_ok());
        assert!(WorkflowRequest {
            prompts: vec!["ok".into(); MAX_WORKFLOW_PROMPTS + 1]
        }
        .validate()
        .is_err());
    }

    #[test]
    fn llm_limits_are_enforced() {
        let request = |max_tokens, temperature| LlmQueryRequest {
            prompt: "hello".into(),
            system_prompt: None,
            max_tokens,
            temperature,
        };
        assert!(request(Some(MAX_TOKENS), Some(0.5)).validate().is_ok());
        assert!(request(Some(MAX_TOKENS + 1), None).validate().is_err());
        assert!(request(Some(0), None).validate().is_err());
        assert!(request(None, Some(f32::NAN)).validate().is_err());
        assert!(request(None, Some(3.0)).validate().is_err());
    }

    #[test]
    fn publish_upload_limits_are_enforced() {
        let request = |size| StorePublishRequest {
            name: "test".into(),
            version: "1.0.0".into(),
            description: String::new(),
            author: String::new(),
            capabilities: Vec::new(),
            checksum_sha256: String::new(),
            wasm_base64: "A".repeat(size),
            signature: None,
        };
        assert!(request(MAX_WASM_BASE64_CHARS).validate().is_ok());
        assert!(request(MAX_WASM_BASE64_CHARS + 1).validate().is_err());
    }
}
