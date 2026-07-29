use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct IntentRequest {
    pub prompt: String,
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
pub struct SearchResponse {
    pub success: bool,
    pub query: String,
    pub results: Vec<serde_json::Value>,
    pub total_results: usize,
    pub summary: Option<String>,
    pub duration_ms: u64,
    pub error: Option<String>,
}
