use crate::auth::{AuthHandle, AuthStore};
use crate::dto::*;
use crate::error::{BridgeError, Result};
use crate::intent_engine::{BlockAction, IntentParser, MetricType, ProcessAction, UserIntent};

use aios_block_mgr::registry::BlockRegistry;
use aios_context::telemetry::TelemetryStore;
use aios_debug::crash_reporter::CrashKind;
use aios_debug::{CrashReporter, PanicHandler};
use aios_hal::audio::AudioDevice;
use aios_ipc::bus::IpcBus;
use aios_live_update::engine::LiveUpdateEngine;
use aios_llm::{default_config, LlmEngine};
use aios_process_mgr::scheduler::Scheduler;
use aios_process_mgr::task::ProcessId;
use aios_security::access_control::AccessControlLayer;
use aios_store::installer::BlockInstaller;
use aios_store::manifest::ManifestInfo;
use aios_store::StoreRegistry;
use aios_telemetry::{FlightRecorder, MetricCollector, TraceContext};
use aios_watchdog::watchdog::Watchdog;
use sha2::{Digest, Sha256};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tower_http::services::ServeDir;

pub struct BridgeContext {
    pub intent_parser: IntentParser,
    pub scheduler: Arc<Mutex<Scheduler>>,
    /// Shared so the text console can observe the same blocks as the bridge.
    pub registry: Arc<Mutex<BlockRegistry>>,
    pub access_control: Mutex<AccessControlLayer>,
    pub telemetry: Mutex<TelemetryStore>,
    /// Shared watchdog state so host-side console controls observe the same
    /// instance as the REST status endpoint.
    pub watchdog: Arc<Mutex<Watchdog>>,
    pub llm: tokio::sync::Mutex<LlmEngine>,
    pub start_time: SystemTime,
    pub request_counter: AtomicU64,
    pub bridge_block_id: u32,
    pub store_registry: Mutex<StoreRegistry>,
    pub metric_collector: Mutex<MetricCollector>,
    pub flight_recorder: Mutex<FlightRecorder>,
    pub trace_context: Mutex<TraceContext>,
    pub crash_reporter: Mutex<CrashReporter>,
    pub _panic_handler: Mutex<PanicHandler>,
    /// System-control hub: Wi-Fi / layout / power endpoints (`/api/v1/sys/*`).
    pub sys_control: tokio::sync::Mutex<aios_sys_control::SysControlHub>,
    /// Directory holding installed block binaries (`<name>_<version>.wasm`).
    pub blocks_dir: String,
    /// Local user authentication store (register / login / sessions).
    pub auth: AuthHandle,
    /// Live-update engine backing the hot-swap intent (rollback bookkeeping).
    pub live_update: Mutex<LiveUpdateEngine>,
    /// Kernel IPC bus shared by the bridge, telemetry feed and text console.
    pub ipc_bus: Arc<Mutex<IpcBus>>,
    /// Voice capture/STT pipeline and pending confirmation state.
    pub voice: Arc<crate::voice::VoiceRuntime>,
    /// Lazily detected audio inventory; `None` means "not scanned yet".
    pub audio_devices: Mutex<Option<Vec<AudioDevice>>>,
    /// Whether public self-registration is enabled (`AIOS_BRIDGE_ALLOW_SIGNUP`).
    pub allow_signup: bool,
}

impl BridgeContext {
    pub fn new(
        scheduler: Arc<Mutex<Scheduler>>,
        registry: BlockRegistry,
        access_control: AccessControlLayer,
        watchdog: Watchdog,
        bridge_block_id: u32,
    ) -> Self {
        let data_dir = std::env::var("AIOS_DATA_DIR").unwrap_or_else(|_| "aios_data".to_string());
        let allow_signup = std::env::var("AIOS_BRIDGE_ALLOW_SIGNUP")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(false);
        let auth = crate::auth::open_auth(std::path::Path::new(&data_dir)).unwrap_or_else(|e| {
            log::warn!("AUTH: failed to open auth store: {e}; bridge cannot authorize users");
            std::sync::Arc::new(Mutex::new(AuthStore::empty()))
        });
        Self {
            intent_parser: IntentParser::new(),
            scheduler,
            registry: Arc::new(Mutex::new(registry)),
            access_control: Mutex::new(access_control),
            telemetry: Mutex::new(TelemetryStore::new()),
            watchdog: Arc::new(Mutex::new(watchdog)),
            llm: tokio::sync::Mutex::new(LlmEngine::from_config(default_config())),
            start_time: SystemTime::now(),
            request_counter: AtomicU64::new(0),
            bridge_block_id,
            store_registry: Mutex::new(StoreRegistry::new()),
            metric_collector: Mutex::new(MetricCollector::new("aios")),
            flight_recorder: Mutex::new(FlightRecorder::new(1024, 3600)),
            trace_context: Mutex::new(TraceContext::new()),
            crash_reporter: Mutex::new(CrashReporter::new("aios-bridge", "1.0.0")),
            _panic_handler: Mutex::new(PanicHandler::new("aios-bridge", "1.0.0")),
            sys_control: tokio::sync::Mutex::new(aios_sys_control::SysControlHub::defaults()),
            blocks_dir: std::env::var("AIOS_BLOCKS_DIR").unwrap_or_else(|_| "./blocks".to_string()),
            auth,
            live_update: Mutex::new(LiveUpdateEngine::new(10_000)),
            ipc_bus: Arc::new(Mutex::new(IpcBus::new(256))),
            voice: Arc::new(crate::voice::VoiceRuntime::new(
                aios_voice::pipeline::VoicePipeline::with_defaults(),
            )),
            audio_devices: Mutex::new(None),
            allow_signup,
        }
    }

    /// Shared handle to the watchdog instance used by the REST status endpoint.
    pub fn watchdog_handle(&self) -> Arc<Mutex<Watchdog>> {
        self.watchdog.clone()
    }

    /// Override the directory used by the update-service endpoints.
    pub fn with_blocks_dir(mut self, dir: impl Into<String>) -> Self {
        self.blocks_dir = dir.into();
        self
    }

    /// Enable or disable public self-registration for tests and embedding.
    pub fn with_signup(mut self, allowed: bool) -> Self {
        self.allow_signup = allowed;
        self
    }

    /// Override the authentication store for tests and embedding.
    pub fn with_auth(mut self, auth: AuthHandle) -> Self {
        self.auth = auth;
        self
    }

    pub fn uptime_secs(&self) -> u64 {
        self.start_time.elapsed().map(|d| d.as_secs()).unwrap_or(0)
    }
}

type SharedState = Arc<BridgeContext>;

/// Lock a shared component, recovering a poisoned mutex instead of panicking.
///
/// Poisoning means another task panicked while holding the lock; recovering the
/// guard keeps the bridge available and records the event in the log.
fn lock_or_recover<'a, T>(mutex: &'a Mutex<T>, label: &str) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        log::warn!("Recovering poisoned {label} mutex");
        poisoned.into_inner()
    })
}

pub async fn start_server(state: SharedState, addr: &str) -> Result<()> {
    start_server_with_shutdown(state, addr, std::future::pending()).await
}

/// Starts the bridge listener and stops it gracefully when `shutdown` resolves.
pub async fn start_server_with_shutdown<F>(
    state: SharedState,
    addr: &str,
    shutdown: F,
) -> Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    // CORS: restrict to a single origin when configured, otherwise disable
    // CORS headers entirely (the UI is served from the same origin as the
    // API, so cross-origin access is not needed). This replaces the previous
    // permissive policy that allowed any website to call the bridge.
    let allowed_origin = std::env::var("AIOS_CORS_ORIGIN").ok();
    let cors = match allowed_origin {
        Some(origin) => {
            let value = axum::http::HeaderValue::from_str(&origin).map_err(|e| {
                BridgeError::ServerError(format!("Invalid AIOS_CORS_ORIGIN '{origin}': {e}"))
            })?;
            Some(
                CorsLayer::new()
                    .allow_origin(AllowOrigin::exact(value))
                    .allow_headers(Any)
                    .allow_methods([
                        axum::http::Method::GET,
                        axum::http::Method::POST,
                        axum::http::Method::OPTIONS,
                    ]),
            )
        }
        None => None,
    };

    tokio::spawn(crate::voice::run_voice_worker(state.clone()));

    let app = Router::new()
        // Public auth endpoints.
        .route("/api/v1/auth/register", post(auth_register_handler))
        .route("/api/v1/auth/login", post(auth_login_handler))
        // Protected API surface.
        .route("/api/v1/system/status", get(status_handler))
        .route("/api/v1/intent", post(intent_handler))
        .route("/api/v1/workflow", post(workflow_handler))
        .route("/api/v1/llm/query", post(llm_query_handler))
        .route("/api/v1/browse", post(browse_handler))
        .route("/api/v1/search", post(search_handler))
        .route("/api/v1/store/index", get(store_index_handler))
        .route("/api/v1/store/register", post(store_register_handler))
        .route("/api/v1/store/publish", post(store_publish_handler))
        .route("/api/v1/metrics", get(metrics_handler))
        .route("/api/v1/traces", get(traces_handler))
        .route("/api/v1/crash-report", post(crash_report_handler))
        .route("/api/v1/sys/status", get(sys_status_handler))
        .route(
            "/api/v1/audio/devices",
            get(crate::audio::audio_devices_handler),
        )
        .route(
            "/api/v1/audio/rescan",
            post(crate::audio::audio_rescan_handler),
        )
        .route("/api/v1/sys/wifi/scan", get(sys_wifi_scan_handler))
        .route("/api/v1/sys/wifi/connect", post(sys_wifi_connect_handler))
        .route("/api/v1/sys/layout", post(sys_layout_handler))
        .route("/api/v1/auth/me", get(auth_me_handler))
        .route(
            "/api/v1/voice/start",
            post(crate::voice::voice_start_handler),
        )
        .route("/api/v1/voice/stop", post(crate::voice::voice_stop_handler))
        .route(
            "/api/v1/voice/transcribe",
            post(crate::voice::voice_transcribe_handler),
        )
        .route(
            "/api/v1/voice/execute-text",
            post(crate::voice::voice_execute_text_handler),
        )
        .route(
            "/api/v1/voice/status",
            get(crate::voice::voice_status_handler),
        )
        .route(
            "/api/v1/voice/audit",
            get(crate::voice::voice_audit_handler),
        )
        .route("/ws/telemetry", get(ws_handler))
        .route("/ws/voice", get(crate::voice::voice_ws_handler))
        // Public, unauthenticated fallbacks and happy-path plumbing.
        .route("/api/v1/health", get(health_handler))
        .route("/store/index.json", get(store_catalog_handler))
        .route("/store/blocks/{name}.wasm", get(store_block_handler))
        .route("/index.json", get(store_catalog_handler))
        .route("/blocks/{name}.wasm", get(store_block_handler))
        .route_layer(middleware::from_fn_with_state::<
            _,
            _,
            (State<SharedState>, Request),
        >(state.clone(), require_auth))
        .route_layer(middleware::from_fn_with_state::<
            _,
            _,
            (State<SharedState>, Request),
        >(state.clone(), record_metrics))
        .with_state(state.clone())
        .fallback_service(ServeDir::new("aios-studio"));

    let app = match cors {
        Some(layer) => app.layer(layer),
        None => app,
    };

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| BridgeError::ServerError(format!("Bind failed: {e}")))?;

    log::info!("AIOS Bridge listening on {addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(|e| BridgeError::ServerError(format!("Server error: {e}")))?;

    Ok(())
}

/// Reject calls to protected routes unless a valid bearer token is presented.
///
/// Authorization is default-deny for every `/api/` and `/ws/` route. The small
/// public set is explicit: health, login, registration (only when enabled) and
/// the unauthenticated update-service catalog. Static SPA assets served by the
/// fallback are not API routes and remain public.
async fn require_auth(State(state): State<SharedState>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    if path.starts_with("/ws/") {
        // Telemetry and voice WebSocket endpoints validate their own tokens.
        return next.run(req).await;
    }
    let public_api = path == "/api/v1/health"
        || path == "/api/v1/auth/login"
        || (state.allow_signup && path == "/api/v1/auth/register")
        || path == "/index.json"
        || path == "/store/index.json"
        || path.starts_with("/blocks/")
        || path.starts_with("/store/blocks/");
    if !path.starts_with("/api/") || public_api {
        return next.run(req).await;
    }

    let Some(header) = req.headers().get(axum::http::header::AUTHORIZATION) else {
        return (
            StatusCode::UNAUTHORIZED,
            [(axum::http::header::WWW_AUTHENTICATE, "Bearer")],
            Json(serde_json::json!({
                "success": false,
                "error": "Authentication required"
            })),
        )
            .into_response();
    };
    let Ok(header_str) = header.to_str() else {
        return (
            StatusCode::UNAUTHORIZED,
            [(axum::http::header::WWW_AUTHENTICATE, "Bearer")],
            Json(serde_json::json!({
                "success": false,
                "error": "Malformed authorization header"
            })),
        )
            .into_response();
    };
    let user = {
        let store = lock_or_recover(&state.auth, "auth");
        store.bearer_user(header_str).ok()
    };
    match user {
        Some(user) => {
            let mut req = req;
            req.extensions_mut().insert(user);
            next.run(req).await
        }
        None => (
            StatusCode::UNAUTHORIZED,
            [(axum::http::header::WWW_AUTHENTICATE, "Bearer")],
            Json(serde_json::json!({
                "success": false,
                "error": "Invalid or expired token"
            })),
        )
            .into_response(),
    }
}

async fn auth_register_handler(
    State(state): State<SharedState>,
    Json(req): Json<RegisterRequest>,
) -> std::result::Result<Json<AuthResponse>, IntentApiError> {
    if !state.allow_signup {
        return Err(IntentApiError {
            status: StatusCode::FORBIDDEN,
            error: BridgeError::CapabilityDenied("Self-registration is disabled".into()),
        });
    }
    let mut store = lock_or_recover(&state.auth, "auth");
    let token = store.register(&req.username, &req.password)?;
    Ok(Json(AuthResponse {
        success: true,
        token: Some(token),
        username: Some(req.username.trim().to_lowercase()),
        error: None,
    }))
}

async fn auth_login_handler(
    State(state): State<SharedState>,
    Json(req): Json<LoginRequest>,
) -> std::result::Result<Json<AuthResponse>, IntentApiError> {
    let mut store = lock_or_recover(&state.auth, "auth");
    let token = store.login(&req.username, &req.password)?;
    Ok(Json(AuthResponse {
        success: true,
        token: Some(token),
        username: Some(req.username.trim().to_lowercase()),
        error: None,
    }))
}

async fn auth_me_handler(State(state): State<SharedState>, req: Request) -> Json<MeResponse> {
    let username = req
        .extensions()
        .get::<String>()
        .cloned()
        .unwrap_or_else(|| "anonymous".into());
    let store = lock_or_recover(&state.auth, "auth");
    let user_count = store.user_count();
    let data_dir = store
        .users_path()
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "aios_data".to_string());
    Json(MeResponse {
        username,
        user_count,
        data_dir,
    })
}

async fn record_metrics(State(state): State<SharedState>, req: Request, next: Next) -> Response {
    let started = Instant::now();
    let response = next.run(req).await;
    let latency_ms = started.elapsed().as_secs_f64() * 1000.0;
    if let Ok(mut collector) = state.metric_collector.lock() {
        collector.increment_counter("http_requests_total", 1);
        collector.set_gauge("http_last_latency_ms", latency_ms);
        collector.observe_histogram("http_request_latency_ms", latency_ms);
    }
    response
}

async fn health_handler(State(state): State<SharedState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        uptime_secs: state.uptime_secs(),
        bridge_version: "1.0.0".into(),
    })
}

async fn status_handler(State(state): State<SharedState>) -> Json<SystemStatus> {
    let scheduler = lock_or_recover(&state.scheduler, "scheduler");
    let registry = lock_or_recover(&state.registry, "registry");
    let watchdog = lock_or_recover(&state.watchdog, "watchdog");

    let process_count = scheduler.process_count();
    let (ram_used, ram_total) = scheduler.ram_usage();

    let processes: Vec<ProcessEntry> = scheduler
        .all_processes()
        .iter()
        .map(|p| ProcessEntry {
            pid: p.pid.0,
            name: p.name.clone(),
            priority: format!("{:?}", p.priority),
            state: format!("{:?}", p.state),
            ram_mb: p.ram_quota_mb,
            cpu_ms: p.cpu_time_ms,
        })
        .collect();

    let running = processes.iter().filter(|p| p.state == "Running").count();
    let suspended = processes.iter().filter(|p| p.state == "Suspended").count();

    let block_data: Vec<BlockEntry> = registry
        .all_ids()
        .iter()
        .filter_map(|id| {
            registry.get(*id).ok().map(|b| BlockEntry {
                id: b.manifest.id.0,
                name: b.manifest.name.clone(),
                version: b.manifest.version.clone(),
                state: format!("{:?}", b.state),
            })
        })
        .collect();

    let ws = watchdog.state();

    Json(SystemStatus {
        status: "running".into(),
        watchdog: WatchdogStatus {
            state: format!("{ws:?}"),
            uptime_secs: state.uptime_secs(),
        },
        processes: ProcessList {
            total: process_count,
            running,
            suspended,
            entries: processes,
        },
        blocks: BlockList {
            total: block_data.len(),
            active: block_data.iter().filter(|b| b.state == "Active").count(),
            entries: block_data,
        },
        resources: ResourceMetrics {
            ram_used_mb: ram_used,
            ram_total_mb: ram_total,
            ram_percent: if ram_total > 0 {
                ram_used as f64 / ram_total as f64 * 100.0
            } else {
                0.0
            },
            process_count,
        },
    })
}

async fn intent_handler(
    State(state): State<SharedState>,
    Json(req): Json<IntentRequest>,
) -> std::result::Result<Json<IntentResponse>, IntentApiError> {
    req.validate().map_err(invalid_request)?;
    let bridge_id = state.bridge_block_id;
    let prompt = req.prompt;

    let llm = state.llm.lock().await;
    let intent = state
        .intent_parser
        .parse_with_llm_fallback(&prompt, &llm)
        .await;
    drop(llm);

    let plan = state.intent_parser.create_execution_plan(&intent);

    {
        let acl = lock_or_recover(&state.access_control, "access_control");
        for cap in &plan.required_capabilities {
            if let Err(e) = acl.check_permission(bridge_id, cap) {
                return Err(IntentApiError {
                    status: StatusCode::FORBIDDEN,
                    error: BridgeError::CapabilityDenied(format!(
                        "Missing capability {}: {e}",
                        cap.name()
                    )),
                });
            }
        }
    }

    let result = execute_intent(&state, &intent).map_err(|e| IntentApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        error: BridgeError::SystemCallFailed(format!("Execution failed: {e}")),
    })?;

    let caps: Vec<String> = plan
        .required_capabilities
        .iter()
        .map(|c| c.name().to_string())
        .collect();
    let steps: Vec<ExecutionStep> = plan
        .steps
        .iter()
        .map(|s| ExecutionStep {
            step: "execute".into(),
            action: s.clone(),
            target: match &intent {
                UserIntent::ProcessControl { target, .. } => target.clone(),
                UserIntent::BlockManagement { block_name, .. } => {
                    block_name.clone().unwrap_or_default()
                }
                _ => "system".into(),
            },
        })
        .collect();

    Ok(Json(IntentResponse {
        success: true,
        intent_type: format!("{intent:?}")
            .split(' ')
            .next()
            .unwrap_or("Unknown")
            .into(),
        description: format!("Executed: {prompt}"),
        result,
        required_capabilities: caps,
        execution_plan: steps,
    }))
}

async fn workflow_handler(
    State(state): State<SharedState>,
    Json(req): Json<WorkflowRequest>,
) -> std::result::Result<Json<WorkflowResponse>, IntentApiError> {
    req.validate().map_err(invalid_request)?;
    let mut results = Vec::new();
    let bridge_id = state.bridge_block_id;

    let llm = state.llm.lock().await;
    for (i, prompt) in req.prompts.iter().enumerate() {
        let intent = state
            .intent_parser
            .parse_with_llm_fallback(prompt, &llm)
            .await;
        let plan = state.intent_parser.create_execution_plan(&intent);

        {
            let acl = lock_or_recover(&state.access_control, "access_control");
            let mut denied = false;
            for cap in &plan.required_capabilities {
                if acl.check_permission(bridge_id, cap).is_err() {
                    results.push(WorkflowStepResult {
                        step: i + 1,
                        prompt: prompt.clone(),
                        success: false,
                        intent_type: "Unknown".into(),
                        description: format!("Missing capability {}", cap.name()),
                        result: serde_json::json!(null),
                        error: Some(format!("Missing capability: {}", cap.name())),
                    });
                    denied = true;
                    break;
                }
            }
            if denied {
                continue;
            }
        }

        match execute_intent(&state, &intent) {
            Ok(result) => {
                let intent_type = format!("{intent:?}")
                    .split(' ')
                    .next()
                    .unwrap_or("Unknown")
                    .into();
                results.push(WorkflowStepResult {
                    step: i + 1,
                    prompt: prompt.clone(),
                    success: true,
                    intent_type,
                    description: format!("Executed: {prompt}"),
                    result,
                    error: None,
                });
            }
            Err(e) => {
                results.push(WorkflowStepResult {
                    step: i + 1,
                    prompt: prompt.clone(),
                    success: false,
                    intent_type: "Unknown".into(),
                    description: format!("Failed: {e}"),
                    result: serde_json::json!(null),
                    error: Some(e),
                });
            }
        }
    }
    drop(llm);

    let successful = results.iter().filter(|r| r.success).count();
    let failed = results.iter().filter(|r| !r.success).count();

    Ok(Json(WorkflowResponse {
        total_steps: results.len(),
        successful,
        failed,
        results,
    }))
}

fn execute_intent(
    state: &SharedState,
    intent: &UserIntent,
) -> std::result::Result<serde_json::Value, String> {
    match intent {
        UserIntent::ProcessControl { action, target } => {
            let mut scheduler = state.scheduler.lock().map_err(|e| e.to_string())?;
            match action {
                ProcessAction::List => {
                    let count = scheduler.process_count();
                    Ok(serde_json::json!({ "process_count": count }))
                }
                ProcessAction::Kill => {
                    let pid: u64 = target
                        .parse()
                        .map_err(|_| format!("Invalid PID: {target}"))?;
                    scheduler
                        .kill_process(ProcessId(pid))
                        .map(|p| serde_json::json!({ "killed": p.name, "pid": pid }))
                        .map_err(|e| e.to_string())
                }
                ProcessAction::Spawn => scheduler
                    .spawn_process(target, aios_process_mgr::task::Priority::Normal, 128)
                    .map(|pid| serde_json::json!({ "spawned": target, "pid": pid.0 }))
                    .map_err(|e| e.to_string()),
                ProcessAction::AdjustPriority => {
                    let (pid, priority) = parse_adjust_target(target)?;
                    scheduler
                        .set_priority(ProcessId(pid), priority)
                        .map(
                            |_| serde_json::json!({ "pid": pid, "priority": priority.to_string() }),
                        )
                        .map_err(|e| e.to_string())
                }
            }
        }
        UserIntent::BlockManagement {
            action,
            wasm_path,
            block_name,
        } => {
            let mut registry = state.registry.lock().map_err(|e| e.to_string())?;
            match action {
                BlockAction::List => {
                    let count = registry.count();
                    Ok(serde_json::json!({ "block_count": count }))
                }
                BlockAction::Load => {
                    let name = block_name.as_deref().unwrap_or("unknown");
                    let requested = wasm_path
                        .clone()
                        .unwrap_or_else(|| std::path::PathBuf::from(name));
                    let path = block_binary_path(&state.blocks_dir, &requested)?;
                    let data = std::fs::read(&path).map_err(|e| format!("Read failed: {e}"))?;
                    aios_block_mgr::loader::BlockLoader::load_from_binary(
                        &mut registry,
                        name,
                        "1.0.0",
                        data,
                    )
                    .map(|m| serde_json::json!({ "loaded": m.name, "id": m.id.to_string() }))
                    .map_err(|e| e.to_string())
                }
                BlockAction::Unload => {
                    let name = block_name.as_deref().unwrap_or("unknown");
                    let id = registry
                        .find_by_name(name)
                        .ok_or_else(|| format!("Block not found: {name}"))
                        .map(|b| b.manifest.id)?;
                    registry
                        .unload_block(id)
                        .map(|_| serde_json::json!({ "unloaded": name }))
                        .map_err(|e| e.to_string())
                }
                BlockAction::HotSwap => {
                    let name = block_name.as_deref().unwrap_or("unknown");
                    let requested = wasm_path
                        .as_ref()
                        .ok_or_else(|| format!("Hot-swap of '{name}' missing wasm_path"))?;
                    let wasm_path = block_binary_path(&state.blocks_dir, requested)?;
                    let new_binary = std::fs::read(&wasm_path)
                        .map_err(|e| format!("Read failed for {:?}: {e}", wasm_path))?;
                    let binary_len = new_binary.len();
                    let new_version =
                        version_from_path(&wasm_path, name).unwrap_or_else(|| "1.0.0".into());
                    let new_sha256 = aios_core::crypto::compute_sha256_bytes(&new_binary);

                    let old = registry
                        .find_by_name(name)
                        .ok_or_else(|| format!("Block not found: {name}"))
                        .map(|e| {
                            (
                                e.manifest.id.0,
                                e.manifest.version.clone(),
                                e.binary.clone(),
                            )
                        })?;
                    drop(registry);

                    let (block_id, old_version, old_binary) = old;
                    let mut queue = state.ipc_bus.lock().map_err(|e| e.to_string())?;
                    let mut engine = state.live_update.lock().map_err(|e| e.to_string())?;
                    engine
                        .perform_swap(
                            block_id,
                            old_binary,
                            old_version.clone(),
                            Vec::new(),
                            new_binary.clone(),
                            new_version.clone(),
                            new_sha256,
                            &mut queue,
                            None,
                        )
                        .map_err(|e| e.to_string())?;

                    let mut registry = state.registry.lock().map_err(|e| e.to_string())?;
                    let swapped_name = registry
                        .swap_binary(
                            aios_core::block::BlockId::new(block_id),
                            new_binary,
                            new_version.clone(),
                        )
                        .map_err(|e| e.to_string())?
                        .name;
                    drop(registry);

                    Ok(serde_json::json!({
                        "block": swapped_name,
                        "id": block_id,
                        "old_version": old_version,
                        "new_version": new_version,
                        "swapped": true,
                        "binary_size": binary_len,
                    }))
                }
            }
        }
        UserIntent::SystemQuery { metric } => {
            let scheduler = state.scheduler.lock().map_err(|e| e.to_string())?;
            let (ram_used, ram_total) = scheduler.ram_usage();
            match metric {
                MetricType::Cpu => {
                    let running = scheduler.running_count();
                    let cores = aios_process_mgr::scheduler::Scheduler::available_cpu_cores();
                    let capacity = (cores * 2).max(1);
                    let cpu_percent = ((running as f64 / capacity as f64) * 100.0).min(100.0);
                    Ok(serde_json::json!({
                        "cpu_percent": cpu_percent,
                        "running": running,
                        "cores": cores,
                    }))
                }
                MetricType::Memory => {
                    Ok(serde_json::json!({ "ram_used_mb": ram_used, "ram_total_mb": ram_total }))
                }
                MetricType::Processes => {
                    Ok(serde_json::json!({ "process_count": scheduler.process_count() }))
                }
                MetricType::Blocks => {
                    drop(scheduler);
                    let registry = state.registry.lock().map_err(|e| e.to_string())?;
                    Ok(serde_json::json!({ "block_count": registry.count() }))
                }
                MetricType::All => {
                    let process_count = scheduler.process_count();
                    drop(scheduler);
                    let registry = state.registry.lock().map_err(|e| e.to_string())?;
                    Ok(serde_json::json!({
                        "process_count": process_count,
                        "block_count": registry.count(),
                        "ram_used_mb": ram_used,
                        "ram_total_mb": ram_total,
                    }))
                }
            }
        }
        UserIntent::MemoryCompaction => Ok(serde_json::json!({ "compaction": "triggered" })),
        UserIntent::WorkflowExecution { steps } => {
            let mut results = Vec::new();
            for step in steps {
                let outcome = execute_intent(state, step);
                results.push(serde_json::json!({
                    "success": outcome.is_ok(),
                    "result": outcome.unwrap_or_else(|e| serde_json::json!({ "error": e })),
                }));
            }
            let successful = results.iter().filter(|r| r["success"] == true).count();
            Ok(serde_json::json!({
                "total_steps": results.len(),
                "successful": successful,
                "results": results,
            }))
        }
        UserIntent::Unknown { raw_prompt } => Ok(
            serde_json::json!({ "unknown_intent": raw_prompt, "hint": "Try: 'show processes', 'status', 'kill 2', 'Р·Р°РїСѓСЃС‚Рё Р±Р»РѕРє'" }),
        ),
    }
}

/// Parse an `AdjustPriority` target of the form `"<pid> <priority>"`, e.g.
/// `"3 Critical"` (the LLM classifier produces `"pid priority"`).
fn parse_adjust_target(
    target: &str,
) -> std::result::Result<(u64, aios_process_mgr::task::Priority), String> {
    let mut parts = target.split_whitespace();
    let pid_str = parts
        .next()
        .ok_or_else(|| format!("Missing PID in target: '{target}'"))?;
    let pid: u64 = pid_str
        .parse()
        .map_err(|_| format!("Invalid PID: '{target}'"))?;
    let prio_name = parts
        .next()
        .ok_or_else(|| format!("Missing priority in target: '{target}'"))?;
    let priority = aios_process_mgr::task::Priority::from_name(prio_name)
        .ok_or_else(|| format!("Unknown priority '{prio_name}'"))?;
    Ok((pid, priority))
}

/// Resolve a block binary path inside `AIOS_BLOCKS_DIR`.
///
/// Relative paths are joined to the configured blocks directory; absolute
/// paths are accepted only when they resolve beneath it. Symlinks and `..`
/// traversal are rejected after canonicalization.
fn block_binary_path(
    blocks_dir: &str,
    requested: &std::path::Path,
) -> std::result::Result<std::path::PathBuf, String> {
    let base = std::fs::canonicalize(blocks_dir)
        .map_err(|e| format!("Blocks directory '{blocks_dir}' unavailable: {e}"))?;
    let joined = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        std::path::Path::new(blocks_dir).join(requested)
    };
    let resolved = std::fs::canonicalize(&joined)
        .map_err(|e| format!("Block path {:?} unavailable: {e}", joined))?;
    if !resolved.starts_with(&base) {
        return Err(format!(
            "Block path {:?} is outside AIOS_BLOCKS_DIR",
            requested
        ));
    }
    Ok(resolved)
}

/// Guess a block version from a `<name>_<version>.wasm` path (falls back to
/// the last `_`-delimited segment when it looks like a version).
fn version_from_path(path: &std::path::Path, name: &str) -> Option<String> {
    let stem = path.file_stem().and_then(|s| s.to_str())?;
    let prefix = format!("{name}_");
    if let Some(tail) = stem.strip_prefix(&prefix) {
        if !tail.is_empty() {
            return Some(tail.to_string());
        }
    }
    stem.rsplit('_')
        .next()
        .filter(|v| v.contains('.'))
        .map(|v| v.to_string())
}

async fn ws_handler(
    Query(params): Query<WsAuthParams>,
    headers: axum::http::HeaderMap,
    State(state): State<SharedState>,
    ws: WebSocketUpgrade,
) -> Response {
    // Browsers cannot set an Authorization header on WebSocket upgrades, so a
    // token may also be supplied as `?token=`. A valid token is always required
    // — there is no "no users yet, feed is open" bypass.
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
        return (
            StatusCode::UNAUTHORIZED,
            [(axum::http::header::WWW_AUTHENTICATE, "Bearer")],
            Json(serde_json::json!({
                "success": false,
                "error": "Invalid or missing telemetry token",
            })),
        )
            .into_response();
    }
    ws.on_upgrade(move |socket| handle_ws_socket(socket, state))
        .into_response()
}

/// Query parameters accepted by the telemetry WebSocket endpoint.
#[derive(serde::Deserialize)]
pub struct WsAuthParams {
    /// Bearer token obtained from `/api/v1/auth/login` or `/register`.
    pub token: Option<String>,
}

async fn handle_ws_socket(mut socket: WebSocket, state: SharedState) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(100));

    loop {
        interval.tick().await;
        let (ram_used, ram_total, process_count) = {
            let scheduler = match state.scheduler.lock() {
                Ok(s) => s,
                Err(_) => break,
            };
            let usage = scheduler.ram_usage();
            (usage.0, usage.1, scheduler.process_count())
        };

        let telemetry = serde_json::json!({
            "timestamp_ms": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),
            "ram_used_mb": ram_used,
            "ram_total_mb": ram_total,
            "ram_percent": if ram_total > 0 { ram_used as f64 / ram_total as f64 * 100.0 } else { 0.0 },
            "process_count": process_count,
            "voice_phase": state.voice.pipeline().current().phase(),
        });

        if socket
            .send(Message::Text(telemetry.to_string()))
            .await
            .is_err()
        {
            break;
        }
    }
}

pub struct IntentApiError {
    pub status: StatusCode,
    pub error: BridgeError,
}

fn invalid_request(message: impl Into<String>) -> IntentApiError {
    IntentApiError {
        status: StatusCode::BAD_REQUEST,
        error: BridgeError::InvalidRequest(message.into()),
    }
}

impl IntoResponse for IntentApiError {
    fn into_response(self) -> axum::response::Response {
        let body = Json(ErrorResponse {
            error: self.error.to_string(),
            code: self.status.as_u16(),
            details: None,
        });
        (self.status, body).into_response()
    }
}

async fn llm_query_handler(
    State(state): State<SharedState>,
    Json(req): Json<LlmQueryRequest>,
) -> std::result::Result<Json<LlmQueryResponse>, IntentApiError> {
    req.validate().map_err(invalid_request)?;
    let llm_request = aios_llm::LlmRequest {
        system_prompt: req
            .system_prompt
            .unwrap_or_else(|| "You are a helpful AI assistant.".into()),
        user_prompt: req.prompt,
        max_tokens: req.max_tokens.unwrap_or(512),
        temperature: req.temperature.unwrap_or(0.7),
    };

    let llm = state.llm.lock().await;
    match llm.query(&llm_request).await {
        Ok(response) => Ok(Json(LlmQueryResponse {
            success: true,
            text: Some(response.text),
            duration_ms: response.duration_ms,
            error: None,
        })),
        Err(e) => Ok(Json(LlmQueryResponse {
            success: false,
            text: None,
            duration_ms: 0,
            error: Some(e.to_string()),
        })),
    }
}

impl From<BridgeError> for IntentApiError {
    fn from(e: BridgeError) -> Self {
        let status = match &e {
            BridgeError::CapabilityDenied(_) => StatusCode::FORBIDDEN,
            BridgeError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            BridgeError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            BridgeError::TooManyRequests(_) => StatusCode::TOO_MANY_REQUESTS,
            BridgeError::IntentParseFailed(_) => StatusCode::UNPROCESSABLE_ENTITY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self { status, error: e }
    }
}

fn bridge_browser_config() -> aios_browser::types::BrowserConfig {
    aios_browser::types::BrowserConfig::for_untrusted()
}

async fn browse_handler(Json(req): Json<BrowseRequest>) -> Json<BrowseResponse> {
    let config = bridge_browser_config();
    let engine = aios_browser::BrowserEngine::new(config);

    match engine.navigate(&req.url).await {
        Ok(page) => {
            let links: Vec<serde_json::Value> = page
                .links
                .iter()
                .map(|l| serde_json::json!({ "href": l.href, "text": l.text }))
                .collect();

            Json(BrowseResponse {
                success: true,
                title: page.title,
                text_content: page.text_content,
                links,
                error: None,
            })
        }
        Err(e) => Json(BrowseResponse {
            success: false,
            title: String::new(),
            text_content: String::new(),
            links: Vec::new(),
            error: Some(e.to_string()),
        }),
    }
}

async fn search_handler(Json(req): Json<SearchRequest>) -> Json<SearchResponse> {
    let config = aios_search::SearchConfig {
        backend: match req.backend.as_deref() {
            Some("searxng") => aios_search::SearchBackend::SearXNG,
            Some("brave") => aios_search::SearchBackend::Brave,
            _ => aios_search::SearchBackend::DuckDuckGo,
        },
        max_results: req.max_results.unwrap_or(10),
        enable_summary: req.enable_summary.unwrap_or(true),
        ..Default::default()
    };

    let engine = aios_search::SearchEngine::new(config);

    match engine.search(&req.query).await {
        Ok(summary) => {
            let results: Vec<serde_json::Value> = summary
                .results
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "title": r.title,
                        "url": r.url,
                        "snippet": r.snippet,
                        "source": r.source,
                    })
                })
                .collect();

            Json(SearchResponse {
                success: true,
                query: summary.query,
                results,
                total_results: summary.total_results,
                summary: summary.summary,
                duration_ms: summary.duration_ms,
                error: None,
            })
        }
        Err(e) => Json(SearchResponse {
            success: false,
            query: req.query,
            results: Vec::new(),
            total_results: 0,
            summary: None,
            duration_ms: 0,
            error: Some(e.to_string()),
        }),
    }
}

async fn store_index_handler(State(state): State<SharedState>) -> Json<StoreIndexResponse> {
    let registry = lock_or_recover(&state.store_registry, "store_registry");
    let manifests: Vec<serde_json::Value> = registry
        .list()
        .iter()
        .map(|m| {
            serde_json::json!({
                "name": m.name,
                "version": m.version,
                "author": m.author,
                "description": m.description,
                "wasm_sha256": m.wasm_sha256,
                "store_url": m.store_url,
            })
        })
        .collect();
    let count = manifests.len();
    Json(StoreIndexResponse {
        success: true,
        count,
        manifests,
    })
}

async fn store_register_handler(
    State(state): State<SharedState>,
    Json(req): Json<StoreRegisterRequest>,
) -> Json<StoreRegisterResponse> {
    let mut registry = lock_or_recover(&state.store_registry, "store_registry");
    let manifest = aios_store::ManifestInfo {
        name: req.name.clone(),
        version: req.version.clone(),
        author: req.author,
        description: req.description,
        wasm_sha256: req.checksum_sha256.clone(),
        capabilities: std::collections::HashSet::new(),
        wasm_size_bytes: 0,
        signature: None,
        store_url: None,
    };
    match registry.register(manifest) {
        Ok(()) => Json(StoreRegisterResponse {
            success: true,
            name: req.name,
            version: req.version,
        }),
        Err(_e) => Json(StoreRegisterResponse {
            success: false,
            name: req.name,
            version: req.version,
        }),
    }
}

/// Raw catalog endpoint used by the update service and `store update`
/// clients. Serves the on-disk block index as `application/json`.
async fn store_catalog_handler(State(state): State<SharedState>) -> Response {
    let installer = BlockInstaller::new(&state.blocks_dir);
    let installed = installer.list_installed();
    let catalog: Vec<ManifestInfo> = installed.iter().map(|b| b.manifest.clone()).collect();
    let body = match serde_json::to_vec(&catalog) {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "catalog serialization failed",
            )
                .into_response()
        }
    };
    (StatusCode::OK, [("content-type", "application/json")], body).into_response()
}

/// Binary block download for the update service. `{name}` is the block name.
async fn store_block_handler(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Response {
    let installer = BlockInstaller::new(&state.blocks_dir);
    match installer.find_installed(&name) {
        Some(installed) => match std::fs::read(&installed.path) {
            Ok(bytes) => (
                StatusCode::OK,
                [("content-type", "application/wasm")],
                bytes,
            )
                .into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to read block: {e}"),
            )
                .into_response(),
        },
        None => (
            StatusCode::NOT_FOUND,
            format!("block '{name}' not installed"),
        )
            .into_response(),
    }
}

/// Publish a user-created block (base64 wasm) to the local update service.
async fn store_publish_handler(
    State(state): State<SharedState>,
    Json(req): Json<StorePublishRequest>,
) -> Json<StorePublishResponse> {
    use base64::Engine;
    if let Err(error) = req.validate() {
        return Json(StorePublishResponse {
            success: false,
            name: req.name,
            version: req.version,
            error: Some(error),
        });
    }
    let wasm = match base64::engine::general_purpose::STANDARD.decode(&req.wasm_base64) {
        Ok(bytes) => bytes,
        Err(e) => {
            return Json(StorePublishResponse {
                success: false,
                name: req.name,
                version: req.version,
                error: Some(format!("invalid base64: {e}")),
            })
        }
    };

    if wasm.len() > MAX_WASM_BYTES {
        return Json(StorePublishResponse {
            success: false,
            name: req.name,
            version: req.version,
            error: Some(format!(
                "wasm exceeds {MAX_WASM_BYTES} bytes after decoding"
            )),
        });
    }

    let sha = hex::encode(Sha256::digest(&wasm));
    if !req.checksum_sha256.is_empty() && sha != req.checksum_sha256 {
        return Json(StorePublishResponse {
            success: false,
            name: req.name,
            version: req.version,
            error: Some("sha256 checksum mismatch".to_string()),
        });
    }

    let manifest = aios_store::ManifestInfo {
        name: req.name.clone(),
        version: req.version.clone(),
        author: req.author,
        description: req.description,
        wasm_sha256: sha,
        capabilities: req
            .capabilities
            .into_iter()
            .map(|c| c.to_lowercase())
            .collect(),
        wasm_size_bytes: wasm.len() as u64,
        signature: req.signature.clone(),
        store_url: Some(format!("http://127.0.0.1:4242/blocks/{}.wasm", req.name)),
    };

    if manifest.signature.is_some() {
        match aios_store::ManifestValidator::verify_signature(&manifest) {
            Ok(true) => {}
            Ok(false) => {
                return Json(StorePublishResponse {
                    success: false,
                    name: req.name,
                    version: req.version,
                    error: Some("invalid Ed25519 signature".to_string()),
                });
            }
            Err(e) => {
                return Json(StorePublishResponse {
                    success: false,
                    name: req.name,
                    version: req.version,
                    error: Some(format!("signature verification failed: {e}")),
                });
            }
        }
    }

    // `from_env` honors `AIOS_TRUSTED_PUBLIC_KEYS`, so a signed publish is
    // still gated by the local trust policy; unsigned publishes stay allowed
    // unless trusted keys are configured.
    let mut installer = BlockInstaller::from_env(&state.blocks_dir);
    match installer.install_from_bytes(manifest, &wasm) {
        Ok(installed) => Json(StorePublishResponse {
            success: true,
            name: installed.manifest.name,
            version: installed.manifest.version,
            error: None,
        }),
        Err(e) => Json(StorePublishResponse {
            success: false,
            name: req.name,
            version: req.version,
            error: Some(e),
        }),
    }
}

async fn metrics_handler(State(state): State<SharedState>) -> Json<MetricsResponse> {
    let collector = lock_or_recover(&state.metric_collector, "metric_collector");
    let prometheus = collector.to_prometheus();
    Json(MetricsResponse {
        success: true,
        prometheus,
    })
}

async fn traces_handler(State(state): State<SharedState>) -> Json<TracesResponse> {
    let trace = lock_or_recover(&state.trace_context, "trace_context");
    let json: serde_json::Value =
        serde_json::from_str(&trace.to_json()).unwrap_or(serde_json::json!({}));
    let traces = vec![json];
    Json(TracesResponse {
        success: true,
        traces,
    })
}

async fn crash_report_handler(
    State(state): State<SharedState>,
    Json(req): Json<CrashReportRequest>,
) -> Json<CrashReportResponse> {
    let mut reporter = lock_or_recover(&state.crash_reporter, "crash_reporter");
    let kind = match req.kind.to_lowercase().as_str() {
        "panic" => CrashKind::Panic,
        "watchdog" | "watchdog_timeout" => CrashKind::WatchdogTimeout,
        "oom" => CrashKind::OOM,
        "block" | "block_crash" => CrashKind::BlockCrash,
        _ => CrashKind::Unknown,
    };
    let zero_knowledge = req.zero_knowledge.unwrap_or(false);
    let report = reporter.generate_report(
        kind,
        "bridge-handler",
        &req.message,
        req.stack_trace.as_deref().unwrap_or(""),
        "",
        zero_knowledge,
    );
    let total = reporter.report_count();
    let report_json: serde_json::Value =
        serde_json::to_value(&report).unwrap_or(serde_json::json!({}));
    Json(CrashReportResponse {
        success: true,
        report: Some(report_json),
        total_reports: total,
        error: None,
    })
}

#[derive(serde::Serialize)]
struct SysStatusResponse {
    success: bool,
    snapshot: aios_sys_control::SysStatusSnapshot,
}

async fn sys_status_handler(State(state): State<SharedState>) -> Json<SysStatusResponse> {
    let hub = state.sys_control.lock().await;
    let snapshot = hub.refresh_status().await;
    Json(SysStatusResponse {
        success: true,
        snapshot,
    })
}

#[derive(serde::Serialize)]
struct SysScanResponse {
    success: bool,
    networks: Vec<aios_sys_control::net_manager::WifiNetwork>,
    error: Option<String>,
}

async fn sys_wifi_scan_handler(State(state): State<SharedState>) -> Json<SysScanResponse> {
    let hub = state.sys_control.lock().await;
    match hub.scan_networks().await {
        Ok(networks) => Json(SysScanResponse {
            success: true,
            networks,
            error: None,
        }),
        Err(e) => Json(SysScanResponse {
            success: false,
            networks: Vec::new(),
            error: Some(format!("{e}")),
        }),
    }
}

#[derive(serde::Deserialize)]
pub struct SysWifiConnectRequest {
    pub ssid: String,
    pub password: Option<String>,
}

#[derive(serde::Serialize)]
struct SysConnectResponse {
    success: bool,
    link: Option<aios_sys_control::net_manager::LinkStatus>,
    error: Option<String>,
}

async fn sys_wifi_connect_handler(
    State(state): State<SharedState>,
    Json(req): Json<SysWifiConnectRequest>,
) -> Json<SysConnectResponse> {
    let hub = state.sys_control.lock().await;
    match hub.connect_wifi(&req.ssid, req.password.as_deref()).await {
        Ok(link) => Json(SysConnectResponse {
            success: true,
            link: Some(link),
            error: None,
        }),
        Err(e) => Json(SysConnectResponse {
            success: false,
            link: None,
            error: Some(format!("{e}")),
        }),
    }
}

#[derive(serde::Deserialize)]
pub struct SysLayoutRequest {
    /// Hotkey combo name: `alt_shift`, `ctrl_shift` or `cmd_space`.
    pub hotkey: String,
}

#[derive(serde::Serialize)]
struct SysLayoutResponse {
    success: bool,
    applied: bool,
    layout: String,
    error: Option<String>,
}

async fn sys_layout_handler(
    State(state): State<SharedState>,
    Json(req): Json<SysLayoutRequest>,
) -> Json<SysLayoutResponse> {
    use aios_sys_control::input_i18n::Hotkey;
    let combo = match req.hotkey.to_lowercase().as_str() {
        "alt_shift" | "alt+shift" => Some(Hotkey::AltShift),
        "ctrl_shift" | "ctrl+shift" => Some(Hotkey::CtrlShift),
        "cmd_space" | "cmd+space" => Some(Hotkey::CmdSpace),
        _ => None,
    };
    let Some(combo) = combo else {
        return Json(SysLayoutResponse {
            success: false,
            applied: false,
            layout: String::new(),
            error: Some(format!("unknown hotkey '{}'", req.hotkey)),
        });
    };
    let hub = state.sys_control.lock().await;
    let applied = hub.feed_hotkey(combo).await;
    let layout = hub.layout_indicator().await;
    Json(SysLayoutResponse {
        success: true,
        applied,
        layout,
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aios_block_mgr::loader::BlockLoader;
    use aios_process_mgr::task::Priority;
    use aios_security::access_control::AccessControlLayer;
    use aios_watchdog::watchdog::WatchdogConfig;

    fn test_state() -> SharedState {
        let scheduler = Arc::new(Mutex::new(Scheduler::new(4096)));
        let registry = BlockRegistry::new();
        let acl = AccessControlLayer::new(b"test-secret".to_vec(), 60_000);
        let watchdog = Watchdog::new(WatchdogConfig::default());
        let ctx = BridgeContext::new(scheduler, registry, acl, watchdog, 99);
        Arc::new(ctx)
    }

    fn run(
        state: &SharedState,
        intent: &UserIntent,
    ) -> std::result::Result<serde_json::Value, String> {
        execute_intent(state, intent)
    }

    #[test]
    fn test_parse_adjust_target() {
        let (pid, prio) = parse_adjust_target("3 Critical").unwrap();
        assert_eq!(pid, 3);
        assert_eq!(prio, Priority::Critical);
        assert!(parse_adjust_target("3").is_err());
        assert!(parse_adjust_target("abc High").is_err());
        assert!(parse_adjust_target("3 unknown").is_err());
    }

    #[test]
    fn test_version_from_path() {
        let p = std::path::Path::new("/tmp/blocks/foo_2.1.0.wasm");
        assert_eq!(version_from_path(p, "foo").as_deref(), Some("2.1.0"));
        assert_eq!(version_from_path(p, "bar").as_deref(), Some("2.1.0"));
        let p2 = std::path::Path::new("blocks/wasm_heap.wasm");
        assert_eq!(version_from_path(p2, "wasm_heap"), None);
    }

    #[test]
    fn bridge_browse_config_disables_native_fallback() {
        let config = bridge_browser_config();
        assert!(!config.headless_fallback);
        assert!(!config.execute_scripts);
        assert!(!config.allow_private_targets);
    }

    #[test]
    fn test_adjust_priority_via_bridge() {
        let state = test_state();
        let pid = state
            .scheduler
            .lock()
            .unwrap()
            .spawn_process("worker", Priority::Normal, 256)
            .unwrap();
        let intent = UserIntent::ProcessControl {
            action: ProcessAction::AdjustPriority,
            target: format!("{} Critical", pid.0),
        };
        let json = run(&state, &intent).unwrap();
        assert_eq!(json["priority"], "Critical");
        let process = {
            let scheduler = lock_or_recover(&state.scheduler, "scheduler");
            scheduler.get_process(pid).unwrap().clone()
        };
        assert_eq!(process.priority, Priority::Critical);
    }

    #[test]
    fn test_hot_swap_via_bridge() {
        let mut state = test_state();
        let old_binary = b"old-wasm-binary-v1".to_vec();
        {
            let mut registry = lock_or_recover(&state.registry, "registry");
            BlockLoader::load_from_binary(&mut registry, "wasm_heap", "1.0.0", old_binary).unwrap();
        }

        let dir = tempfile::tempdir().unwrap();
        let blocks_dir = dir.path().join("blocks");
        std::fs::create_dir_all(&blocks_dir).unwrap();
        Arc::get_mut(&mut state).unwrap().blocks_dir = blocks_dir.to_string_lossy().into_owned();
        let new_path = blocks_dir.join("wasm_heap_2.0.0.wasm");
        let new_binary = b"new-wasm-binary-v2-bigger".to_vec();
        std::fs::write(&new_path, &new_binary).unwrap();

        let intent = UserIntent::BlockManagement {
            action: BlockAction::HotSwap,
            wasm_path: Some(new_path),
            block_name: Some("wasm_heap".into()),
        };
        let json = run(&state, &intent).unwrap();
        assert_eq!(json["old_version"], "1.0.0");
        assert_eq!(json["new_version"], "2.0.0");
        assert_eq!(json["swapped"], true);

        let (version_on_disk, binary_on_registry) = {
            let registry = lock_or_recover(&state.registry, "registry");
            let entry = registry.find_by_name("wasm_heap").unwrap();
            (entry.manifest.version.clone(), entry.binary.clone())
        };
        assert_eq!(version_on_disk, "2.0.0");
        assert_eq!(binary_on_registry, new_binary);

        let engine = lock_or_recover(&state.live_update, "live_update");
        let history = engine.swap_history();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].old_version, "1.0.0");
        assert_eq!(history[0].new_version, "2.0.0");
        assert!(history[0].success);
    }

    #[test]
    fn test_block_binary_path_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let blocks_dir = dir.path().join("blocks");
        std::fs::create_dir_all(&blocks_dir).unwrap();
        let outside = dir.path().join("secret.wasm");
        std::fs::write(&outside, b"not-a-block").unwrap();
        std::fs::write(blocks_dir.join("ok_1.0.0.wasm"), b"block").unwrap();

        assert!(block_binary_path(
            &blocks_dir.to_string_lossy(),
            std::path::Path::new("../secret.wasm")
        )
        .is_err());
        assert!(block_binary_path(&blocks_dir.to_string_lossy(), &outside).is_err());
        assert!(block_binary_path(
            &blocks_dir.to_string_lossy(),
            std::path::Path::new("ok_1.0.0.wasm")
        )
        .is_ok());
    }

    #[test]
    fn test_workflow_execution_via_bridge() {
        let state = test_state();
        let intent = UserIntent::WorkflowExecution {
            steps: vec![
                UserIntent::ProcessControl {
                    action: ProcessAction::List,
                    target: String::new(),
                },
                UserIntent::BlockManagement {
                    action: BlockAction::List,
                    wasm_path: None,
                    block_name: None,
                },
            ],
        };
        let json = run(&state, &intent).unwrap();
        assert_eq!(json["total_steps"], 2);
        assert_eq!(json["successful"], 2);
        assert_eq!(json["results"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_cpu_metric_via_bridge() {
        let state = test_state();
        let intent = UserIntent::SystemQuery {
            metric: MetricType::Cpu,
        };
        let json = run(&state, &intent).unwrap();
        assert!(json["cpu_percent"].as_f64().is_some());
        assert!(json["cores"].as_u64().is_some());
    }

    #[test]
    fn test_memory_and_process_metrics_via_bridge() {
        let state = test_state();
        let mem = run(
            &state,
            &UserIntent::SystemQuery {
                metric: MetricType::Memory,
            },
        )
        .unwrap();
        assert!(mem["ram_total_mb"].as_u64().is_some());

        let procs = run(
            &state,
            &UserIntent::SystemQuery {
                metric: MetricType::All,
            },
        )
        .unwrap();
        assert_eq!(procs["process_count"], 0);
        assert_eq!(procs["block_count"], 0);
    }
}
