//! Voice bridge: HTTP control, WebSocket phases and capability-checked execution.

use crate::dto::{
    ErrorResponse, VoiceAckResponse, VoiceAuditResponse, VoiceCaptureMode, VoiceStartRequest,
    VoiceStatusResponse, VoiceTextRequest, MAX_VOICE_AUDIO_BYTES, MAX_VOICE_TEXT_CHARS,
};
use crate::server::BridgeContext;

use aios_builder::diff::script_diff;
use aios_builder::{BorrowedLlmScriptLlm, GeneratedScript, OfflineScriptLlm, ScriptGenerator};
use aios_context::telemetry::TelemetryEntry;
use aios_security::capability::Capability;
use aios_telemetry::flight_recorder::EventKind;
use aios_voice::capture::{decode_audio, CaptureMode};
use aios_voice::pipeline::{VoiceEvent, VoicePipeline};
use aios_wasm::isolation::IsolationConfig;
use aios_wasm::sandbox::{HostPolicy, SandboxConfig, WasmBlock};
use axum::body::Bytes;
use axum::extract::{Query, State, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Write as _;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

/// Synthetic block id used for voice-session capability tokens.
pub const VOICE_BLOCK_ID: u32 = u32::MAX - 16;
/// Maximum guarded host-call records retained from one execution.
pub const MAX_AUDIT_RECORDS: usize = 128;
/// Maximum characters retained from one audit record.
pub const MAX_AUDIT_CHARS: usize = 512;
/// Number of audit entries returned by the inspection endpoint.
pub const AUDIT_TAIL_LIMIT: usize = 200;
const AUDIT_FILE_NAME: &str = "voice_audit.jsonl";

type SharedState = Arc<BridgeContext>;
type VoiceResult = std::result::Result<Json<VoiceAckResponse>, (StatusCode, Json<ErrorResponse>)>;

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn lock_or_recover<'a, T>(mutex: &'a Mutex<T>, label: &str) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        log::warn!("Recovering poisoned {label} mutex");
        poisoned.into_inner()
    })
}

fn voice_error(
    status: StatusCode,
    code: &str,
    reason: impl Into<String>,
) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            error: reason.into(),
            code: status.as_u16(),
            details: Some(code.to_string()),
        }),
    )
}

fn permissions(capabilities: &[Capability]) -> Vec<String> {
    capabilities.iter().map(|c| c.name().to_string()).collect()
}

fn phase_response(
    pipeline: &VoicePipeline,
    transcript: Option<String>,
    script: Option<String>,
    permissions: Vec<String>,
    output: Option<String>,
    error: Option<String>,
) -> VoiceAckResponse {
    VoiceAckResponse {
        success: error.is_none(),
        phase: pipeline.current().phase().to_string(),
        transcript,
        script,
        permissions,
        output,
        error,
    }
}

struct PendingVoice {
    transcript: String,
    generated: GeneratedScript,
    diff: String,
}

/// Runtime state shared by voice handlers and the transcript worker.
pub struct VoiceRuntime {
    pipeline: VoicePipeline,
    pending: Mutex<Option<PendingVoice>>,
    last_result: Mutex<Option<LastVoiceResult>>,
    last_script: Mutex<String>,
}

#[derive(Clone)]
struct LastVoiceResult {
    transcript: String,
    script: String,
    script_diff: String,
    host_calls: Vec<String>,
    permissions: Vec<String>,
    output: String,
    error: String,
}

#[derive(Default)]
struct WasmExecution {
    output: String,
    audit: Vec<String>,
    error: Option<String>,
}

impl VoiceRuntime {
    /// Attach a pipeline to the bridge runtime.
    pub fn new(pipeline: VoicePipeline) -> Self {
        Self {
            pipeline,
            pending: Mutex::new(None),
            last_result: Mutex::new(None),
            last_script: Mutex::new(String::new()),
        }
    }

    /// Pipeline used by capture and transcription handlers.
    pub fn pipeline(&self) -> &VoicePipeline {
        &self.pipeline
    }

    fn set_pending(&self, transcript: String, generated: GeneratedScript, diff: String) {
        *lock_or_recover(&self.pending, "voice pending") = Some(PendingVoice {
            transcript,
            generated,
            diff,
        });
    }

    fn take_pending(&self) -> Option<PendingVoice> {
        lock_or_recover(&self.pending, "voice pending").take()
    }

    fn record(&self, result: LastVoiceResult) {
        *lock_or_recover(&self.last_result, "voice result") = Some(result);
    }

    fn last_script(&self) -> String {
        lock_or_recover(&self.last_script, "voice last script").clone()
    }

    fn remember_script(&self, script: &str) {
        *lock_or_recover(&self.last_script, "voice last script") = script.to_string();
    }

    /// Current voice phase, transcript, script and execution result.
    pub fn status(&self) -> VoiceStatusResponse {
        self.snapshot()
    }

    fn snapshot(&self) -> VoiceStatusResponse {
        let phase = self.pipeline.current().phase().to_string();
        match lock_or_recover(&self.last_result, "voice result").as_ref() {
            Some(result) => VoiceStatusResponse {
                phase,
                transcript: result.transcript.clone(),
                script: result.script.clone(),
                script_diff: result.script_diff.clone(),
                host_calls: result.host_calls.clone(),
                permissions: result.permissions.clone(),
                output: result.output.clone(),
                error: result.error.clone(),
            },
            None => VoiceStatusResponse {
                phase,
                transcript: String::new(),
                script: String::new(),
                script_diff: String::new(),
                host_calls: Vec::new(),
                permissions: Vec::new(),
                output: String::new(),
                error: String::new(),
            },
        }
    }
}

/// Start microphone capture.
pub async fn voice_start_handler(
    State(state): State<SharedState>,
    Json(req): Json<VoiceStartRequest>,
) -> VoiceResult {
    let mode = match req.mode {
        Some(VoiceCaptureMode::PushToTalk) => Some(CaptureMode::PushToTalk),
        Some(VoiceCaptureMode::Toggle) => Some(CaptureMode::Toggle),
        Some(VoiceCaptureMode::Vad) | None => Some(CaptureMode::Toggle),
    };
    state
        .voice
        .pipeline()
        .start(mode)
        .map_err(|e| voice_error(StatusCode::CONFLICT, "VOICE_CAPTURE", e))?;
    Ok(Json(phase_response(
        state.voice.pipeline(),
        None,
        None,
        Vec::new(),
        None,
        None,
    )))
}

/// Stop capture and hand the audio frame to STT.
pub async fn voice_stop_handler(State(state): State<SharedState>) -> VoiceResult {
    state
        .voice
        .pipeline()
        .stop()
        .map_err(|e| voice_error(StatusCode::BAD_REQUEST, "VOICE_CAPTURE", e))?;
    Ok(Json(phase_response(
        state.voice.pipeline(),
        None,
        None,
        Vec::new(),
        None,
        None,
    )))
}

/// Transcribe a WAV or s16le PCM upload (at most 10 MiB).
pub async fn voice_transcribe_handler(
    State(state): State<SharedState>,
    body: Bytes,
) -> VoiceResult {
    if body.is_empty() {
        return Err(voice_error(
            StatusCode::BAD_REQUEST,
            "VOICE_AUDIO_EMPTY",
            "audio body must not be empty",
        ));
    }
    if body.len() > MAX_VOICE_AUDIO_BYTES {
        return Err(voice_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "VOICE_AUDIO_TOO_LARGE",
            format!("audio exceeds {MAX_VOICE_AUDIO_BYTES} bytes"),
        ));
    }
    let frame = decode_audio(&body, 16_000)
        .map_err(|e| voice_error(StatusCode::BAD_REQUEST, "VOICE_AUDIO_INVALID", e))?;
    let transcript = state
        .voice
        .pipeline()
        .transcribe_frame(frame)
        .await
        .map_err(|e| voice_error(StatusCode::INTERNAL_SERVER_ERROR, "VOICE_STT", e))?;
    Ok(Json(phase_response(
        state.voice.pipeline(),
        Some(transcript),
        None,
        Vec::new(),
        None,
        None,
    )))
}

/// Submit a transcript directly; `confirm` approves a pending privileged script.
pub async fn voice_execute_text_handler(
    State(state): State<SharedState>,
    Json(req): Json<VoiceTextRequest>,
) -> VoiceResult {
    if req.confirm {
        return confirm_pending(&state).await;
    }
    if req.text.trim().is_empty() || req.text.chars().count() > MAX_VOICE_TEXT_CHARS {
        return Err(voice_error(
            StatusCode::BAD_REQUEST,
            "VOICE_TEXT_INVALID",
            "text must be non-empty and within the character limit",
        ));
    }
    state.voice.pipeline().submit_text(req.text.clone());
    Ok(Json(phase_response(
        state.voice.pipeline(),
        Some(req.text),
        None,
        Vec::new(),
        None,
        None,
    )))
}

/// Poll the latest phase, transcript, script and execution output.
pub async fn voice_status_handler(State(state): State<SharedState>) -> Json<VoiceStatusResponse> {
    Json(state.voice.snapshot())
}

/// WebSocket query parameters; a bearer token is always required.
#[derive(Debug, Deserialize)]
pub struct VoiceWsAuthParams {
    /// Bearer token obtained from `/api/v1/auth/login` or `/register`.
    pub token: Option<String>,
}

/// Authenticated WebSocket endpoint streaming voice phase events.
pub async fn voice_ws_handler(
    Query(params): Query<VoiceWsAuthParams>,
    headers: axum::http::HeaderMap,
    State(state): State<SharedState>,
    ws: WebSocketUpgrade,
) -> Response {
    let token = params.token.or_else(|| {
        headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::trim)
            .map(str::to_string)
    });
    let allowed = match token {
        Some(token) if !token.is_empty() => {
            let store = lock_or_recover(&state.auth, "auth");
            store.bearer_user(&format!("Bearer {token}")).is_ok()
        }
        _ => false,
    };
    if !allowed {
        return voice_error(
            StatusCode::UNAUTHORIZED,
            "VOICE_WS_UNAUTHORIZED",
            "Invalid or missing voice token",
        )
        .into_response();
    }
    ws.on_upgrade(move |socket| handle_voice_socket(socket, state))
        .into_response()
}

async fn handle_voice_socket(mut socket: axum::extract::ws::WebSocket, state: SharedState) {
    let mut rx = state.voice.pipeline().subscribe();
    loop {
        let event = match rx.recv().await {
            Ok(event) => event,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => break,
        };
        let payload = serde_json::json!({
            "phase": event.phase(),
            "event": event,
        });
        if socket
            .send(axum::extract::ws::Message::Text(payload.to_string()))
            .await
            .is_err()
        {
            break;
        }
    }
}

/// Background worker that turns every `Transcribed` event into a script and execution.
pub async fn run_voice_worker(state: SharedState) {
    let mut rx = state.voice.pipeline().subscribe();
    loop {
        match rx.recv().await {
            Ok(VoiceEvent::Transcribed { text }) => {
                process_transcript(&state, text).await;
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
}

async fn process_transcript(state: &SharedState, text: String) {
    let pipeline = state.voice.pipeline();
    pipeline.emit(VoiceEvent::Thinking);
    let generated = generate_script(state, &text).await;
    let generated = match generated {
        Ok(generated) => generated,
        Err(reason) => {
            pipeline.emit(VoiceEvent::Failed {
                reason: reason.clone(),
            });
            state.voice.record(LastVoiceResult {
                transcript: text,
                script: String::new(),
                script_diff: String::new(),
                host_calls: Vec::new(),
                permissions: Vec::new(),
                output: String::new(),
                error: reason,
            });
            return;
        }
    };

    let diff = script_diff(&state.voice.last_script(), &generated.script);
    let required = permissions(&generated.capabilities);
    if !generated.capabilities.is_empty() {
        state.voice.set_pending(text, generated, diff.clone());
        pipeline.emit(VoiceEvent::AwaitingConfirmation {
            script: state.voice.pending_script().unwrap_or_default(),
            diff: diff.clone(),
            permissions: required.clone(),
        });
        state.voice.record(LastVoiceResult {
            transcript: String::new(),
            script: String::new(),
            script_diff: diff,
            host_calls: Vec::new(),
            permissions: required,
            output: String::new(),
            error: String::new(),
        });
        return;
    }

    pipeline.emit(VoiceEvent::ScriptReady {
        script: generated.script.clone(),
        permissions: required.clone(),
    });
    execute_generated(state, text, generated, diff).await;
}

impl VoiceRuntime {
    fn pending_script(&self) -> Option<String> {
        lock_or_recover(&self.pending, "voice pending")
            .as_ref()
            .map(|pending| pending.generated.script.clone())
    }
}

async fn generate_script(
    state: &SharedState,
    text: &str,
) -> std::result::Result<GeneratedScript, String> {
    {
        let llm = state.llm.lock().await;
        let adapter = BorrowedLlmScriptLlm::new(&llm);
        let generator = ScriptGenerator::new(&adapter, 2);
        if let Ok(generated) = generator.generate(text, "voice").await {
            return Ok(generated);
        }
    }
    log::warn!("VOICE: LLM generation unavailable, using offline script fallback");
    const OFFLINE: OfflineScriptLlm = OfflineScriptLlm;
    ScriptGenerator::new(&OFFLINE, 1)
        .generate(text, "voice")
        .await
}

async fn confirm_pending(state: &SharedState) -> VoiceResult {
    let pending = state.voice.take_pending().ok_or_else(|| {
        voice_error(
            StatusCode::CONFLICT,
            "VOICE_NO_PENDING",
            "no pending script",
        )
    })?;
    {
        let mut acl = lock_or_recover(&state.access_control, "access control");
        acl.issue_token(VOICE_BLOCK_ID, pending.generated.capabilities.clone())
            .map_err(|e| voice_error(StatusCode::FORBIDDEN, "VOICE_ACL", e.to_string()))?;
    }
    state.voice.pipeline().emit(VoiceEvent::ScriptReady {
        script: pending.generated.script.clone(),
        permissions: permissions(&pending.generated.capabilities),
    });
    execute_generated(state, pending.transcript, pending.generated, pending.diff).await;
    Ok(Json(phase_response(
        state.voice.pipeline(),
        None,
        None,
        Vec::new(),
        None,
        None,
    )))
}

async fn execute_generated(
    state: &SharedState,
    transcript: String,
    generated: GeneratedScript,
    diff: String,
) {
    let pipeline = state.voice.pipeline();
    let caps = generated.capabilities.clone();
    {
        let mut acl = lock_or_recover(&state.access_control, "access control");
        if !acl.has_token(VOICE_BLOCK_ID) {
            if let Err(e) = acl.issue_token(VOICE_BLOCK_ID, Vec::new()) {
                deny(
                    pipeline,
                    &state.voice,
                    transcript,
                    &generated,
                    e.to_string(),
                    Vec::new(),
                    diff,
                );
                return;
            }
        }
        for capability in &caps {
            if let Err(e) = acl.check_permission(VOICE_BLOCK_ID, capability) {
                deny(
                    pipeline,
                    &state.voice,
                    transcript,
                    &generated,
                    e.to_string(),
                    Vec::new(),
                    diff,
                );
                return;
            }
        }
    }

    pipeline.emit(VoiceEvent::Executing);
    let execution = execute_wasm(&generated, &caps);
    record_audit(state, &execution.audit);
    if let Some(reason) = execution.error {
        deny(
            pipeline,
            &state.voice,
            transcript,
            &generated,
            reason,
            execution.audit,
            diff,
        );
        return;
    }
    if let Err(e) = save_workflow(&transcript, &generated) {
        log::warn!("VOICE: failed to save workflow: {e}");
    }
    pipeline.emit(VoiceEvent::Completed {
        output: execution.output.clone(),
    });
    state.voice.remember_script(&generated.script);
    state.voice.record(LastVoiceResult {
        transcript,
        script: generated.script.clone(),
        script_diff: diff,
        host_calls: execution.audit,
        permissions: permissions(&caps),
        output: execution.output,
        error: String::new(),
    });
}

fn deny(
    pipeline: &VoicePipeline,
    runtime: &VoiceRuntime,
    transcript: String,
    generated: &GeneratedScript,
    reason: String,
    host_calls: Vec<String>,
    diff: String,
) {
    pipeline.emit(VoiceEvent::Failed {
        reason: reason.clone(),
    });
    runtime.record(LastVoiceResult {
        transcript,
        script: generated.script.clone(),
        script_diff: diff,
        host_calls,
        permissions: permissions(&generated.capabilities),
        output: String::new(),
        error: reason,
    });
}

fn execute_wasm(generated: &GeneratedScript, capabilities: &[Capability]) -> WasmExecution {
    let mut outcome = WasmExecution::default();
    let mut block = match WasmBlock::new(
        generated.workflow.name.clone(),
        "1.0.0".to_string(),
        &generated.wasm,
        SandboxConfig::default(),
        IsolationConfig::default(),
    ) {
        Ok(block) => block,
        Err(e) => {
            outcome.error = Some(e.to_string());
            return outcome;
        }
    };
    let mut store = match block.create_store() {
        Ok(store) => store,
        Err(e) => {
            outcome.error = Some(e.to_string());
            return outcome;
        }
    };
    if let Err(e) =
        block.instantiate_with_policy(&mut store, HostPolicy::new(capabilities.to_vec()))
    {
        outcome.audit = store.data().audit_log.clone();
        outcome.error = Some(e.to_string());
        return outcome;
    }
    let values = match block.call_func(&mut store, "step_count", &[]) {
        Ok(values) => values,
        Err(e) => {
            outcome.audit = store.data().audit_log.clone();
            outcome.error = Some(e.to_string());
            return outcome;
        }
    };
    let count = values
        .first()
        .and_then(|value| value.i32())
        .unwrap_or(0)
        .max(0) as usize;
    for index in 0..count {
        if let Err(e) = block.call_func(&mut store, &format!("step_{index}"), &[]) {
            outcome.audit = store.data().audit_log.clone();
            outcome.error = Some(e.to_string());
            return outcome;
        }
    }
    outcome.audit = store.data().audit_log.clone();
    outcome.output = format!("executed {count} EasyLang steps");
    outcome
}

/// Audit path under the configured AIOS data directory.
fn audit_path() -> std::path::PathBuf {
    let data_dir = std::env::var("AIOS_DATA_DIR").unwrap_or_else(|_| "aios_data".to_string());
    std::path::Path::new(&data_dir).join(AUDIT_FILE_NAME)
}

fn bounded_audit(audit: &[String]) -> Vec<String> {
    audit
        .iter()
        .take(MAX_AUDIT_RECORDS)
        .map(|line| line.chars().take(MAX_AUDIT_CHARS).collect())
        .collect()
}

/// Append one JSON line with the guarded host calls of a voice execution.
fn write_audit(
    path: &std::path::Path,
    calls: &[String],
    timestamp_ms: u128,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let entry = serde_json::json!({
        "timestamp_ms": timestamp_ms,
        "block_id": VOICE_BLOCK_ID,
        "calls": calls,
        "denied": calls.iter().filter(|line| line.contains("denied") || line.contains("rejected")).count(),
    });
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{entry}")
}

fn record_audit(state: &SharedState, audit: &[String]) {
    if audit.is_empty() {
        return;
    }
    let calls = bounded_audit(audit);
    lock_or_recover(&state.flight_recorder, "flight recorder").record_with_data(
        EventKind::Syscall,
        "voice host calls",
        &calls.join("\n"),
    );
    lock_or_recover(&state.telemetry, "telemetry").record(
        TelemetryEntry::new("voice.host_calls", calls.len() as f64, 0)
            .with_block(VOICE_BLOCK_ID)
            .with_process("voice-wasm"),
    );
    if let Err(e) = write_audit(&audit_path(), &calls, now_ms()) {
        log::warn!("VOICE: failed to persist host-call audit: {e}");
    }
}

/// Read the most recent persisted host-call audit entries, oldest first.
pub fn read_audit(path: &std::path::Path) -> Vec<serde_json::Value> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .rev()
        .take(AUDIT_TAIL_LIMIT)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// `GET /api/v1/voice/audit` — authenticated host-call audit inspection.
pub async fn voice_audit_handler(State(_state): State<SharedState>) -> Json<VoiceAuditResponse> {
    Json(VoiceAuditResponse {
        entries: read_audit(&audit_path()),
    })
}

fn save_workflow(transcript: &str, generated: &GeneratedScript) -> std::result::Result<(), String> {
    let dir = std::env::var("AIOS_WORKFLOWS_DIR").unwrap_or_else(|_| "workflows".to_string());
    std::fs::create_dir_all(&dir).map_err(|e| format!("workflow dir: {e}"))?;
    let mut hasher = DefaultHasher::new();
    transcript.hash(&mut hasher);
    let stem = format!("voice_{:x}_{:x}", now_ms(), hasher.finish());
    std::fs::write(
        std::path::Path::new(&dir).join(format!("{stem}.easy")),
        &generated.script,
    )
    .map_err(|e| format!("save .easy: {e}"))?;
    std::fs::write(
        std::path::Path::new(&dir).join(format!("{stem}.wasm")),
        &generated.wasm,
    )
    .map_err(|e| format!("save .wasm: {e}"))?;
    let metadata = serde_json::json!({
        "phrase": transcript,
        "script": generated.script,
        "capabilities": permissions(&generated.capabilities),
        "timestamp_ms": now_ms(),
    });
    std::fs::write(
        std::path::Path::new(&dir).join(format!("{stem}.meta.json")),
        serde_json::to_string_pretty(&metadata).map_err(|e| format!("metadata: {e}"))?,
    )
    .map_err(|e| format!("save metadata: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_voice_block_id_is_stable() {
        assert_eq!(VOICE_BLOCK_ID, u32::MAX - 16);
    }

    #[test]
    fn audit_records_are_bounded() {
        let audit: Vec<String> = (0..MAX_AUDIT_RECORDS + 5)
            .map(|_| "x".repeat(MAX_AUDIT_CHARS + 20))
            .collect();
        let bounded = bounded_audit(&audit);
        assert_eq!(bounded.len(), MAX_AUDIT_RECORDS);
        assert_eq!(bounded[0].chars().count(), MAX_AUDIT_CHARS);
    }

    #[test]
    fn audit_is_appended_and_read_as_a_tail() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(AUDIT_FILE_NAME);
        write_audit(&path, &["log: first".into()], 1).expect("first entry");
        write_audit(&path, &["browse: denied FsRead".into()], 2).expect("second entry");
        let entries = read_audit(&path);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["timestamp_ms"], 1);
        assert_eq!(entries[1]["calls"][0], "browse: denied FsRead");
        assert_eq!(entries[1]["denied"], 1);
    }

    #[test]
    fn status_exposes_pending_diff_and_host_calls() {
        let runtime = VoiceRuntime::new(VoicePipeline::with_defaults());
        runtime.record(LastVoiceResult {
            transcript: "открой сайт".into(),
            script: String::new(),
            script_diff: "+ browse \"https://example.com\"\n".into(),
            host_calls: vec!["browse: https://example.com".into()],
            permissions: vec!["NetConnect".into()],
            output: String::new(),
            error: String::new(),
        });
        let status = runtime.status();
        assert_eq!(status.script_diff, "+ browse \"https://example.com\"\n");
        assert_eq!(status.host_calls.len(), 1);
    }
}
