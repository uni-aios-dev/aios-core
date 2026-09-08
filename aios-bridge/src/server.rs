use crate::auth::{AuthHandle, AuthStore};
use crate::dto::*;
use crate::error::{BridgeError, Result};
use crate::intent_engine::{BlockAction, IntentParser, MetricType, ProcessAction, UserIntent};

use aios_block_mgr::registry::BlockRegistry;
use aios_context::telemetry::TelemetryStore;
use aios_debug::crash_reporter::CrashKind;
use aios_debug::{CrashReporter, PanicHandler};
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
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tower_http::services::ServeDir;

pub struct BridgeContext {
    pub intent_parser: IntentParser,
    pub scheduler: Arc<Mutex<Scheduler>>,
    pub registry: Mutex<BlockRegistry>,
    pub access_control: Mutex<AccessControlLayer>,
    pub telemetry: Mutex<TelemetryStore>,
    pub watchdog: Mutex<Watchdog>,
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
        let auth = crate::auth::open_auth(std::path::Path::new(&data_dir))
            .unwrap_or_else(|e| {
                log::warn!("AUTH: failed to open auth store, auth disabled: {e}");
                std::sync::Arc::new(Mutex::new(
                    AuthStore::new(std::path::Path::new(&data_dir))
                        .expect("in-memory auth store fallback"),
                ))
            });
        Self {
            intent_parser: IntentParser::new(),
            scheduler,
            registry: Mutex::new(registry),
            access_control: Mutex::new(access_control),
            telemetry: Mutex::new(TelemetryStore::new()),
            watchdog: Mutex::new(watchdog),
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
        }
    }

    /// Override the directory used by the update-service endpoints.
    pub fn with_blocks_dir(mut self, dir: impl Into<String>) -> Self {
        self.blocks_dir = dir.into();
        self
    }

    pub fn uptime_secs(&self) -> u64 {
        self.start_time.elapsed().map(|d| d.as_secs()).unwrap_or(0)
    }
}

type SharedState = Arc<BridgeContext>;

pub async fn start_server(state: SharedState, addr: &str) -> Result<()> {
    // CORS: restrict to a single origin when configured, otherwise disable
    // CORS headers entirely (the UI is served from the same origin as the
    // API, so cross-origin access is not needed). This replaces the previous
    // permissive policy that allowed any website to call the bridge.
    let allowed_origin = std::env::var("AIOS_CORS_ORIGIN").ok();
    let cors = allowed_origin.as_ref().map(|origin| {
        CorsLayer::new()
            .allow_origin(AllowOrigin::exact(
                axum::http::HeaderValue::from_str(origin)
                    .unwrap_or(axum::http::HeaderValue::from_static("*")),
            ))
            .allow_headers(Any)
            .allow_methods([
                axum::http::Method::GET,
                axum::http::Method::POST,
                axum::http::Method::OPTIONS,
            ])
    });

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
        .route("/api/v1/sys/wifi/scan", get(sys_wifi_scan_handler))
        .route("/api/v1/sys/wifi/connect", post(sys_wifi_connect_handler))
        .route("/api/v1/sys/layout", post(sys_layout_handler))
        .route("/api/v1/auth/me", get(auth_me_handler))
        .route("/ws/telemetry", get(ws_handler))
        // Public, unauthenticated fallbacks and happy-path plumbing.
        .route("/api/v1/health", get(health_handler))
        .route("/store/index.json", get(store_catalog_handler))
        .route("/store/blocks/{name}.wasm", get(store_block_handler))
        .route("/index.json", get(store_catalog_handler))
        .route("/blocks/{name}.wasm", get(store_block_handler))
        .route_layer(
            middleware::from_fn_with_state::<_, _, (State<SharedState>, Request)>(
                state.clone(),
                require_auth,
            ),
        )
        .route_layer(
            middleware::from_fn_with_state::<_, _, (State<SharedState>, Request)>(
                state.clone(),
                record_metrics,
            ),
        )
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
        .await
        .map_err(|e| BridgeError::ServerError(format!("Server error: {e}")))?;

    Ok(())
}

/// Reject calls to protected routes unless a valid bearer token is presented.
async fn require_auth(State(state): State<SharedState>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    // Always allow the public surfaces.
    if path == "/api/v1/auth/register"
        || path == "/api/v1/auth/login"
        || path == "/api/v1/health"
        || path == "/ws/telemetry"
        || path == "/api/v1/sys/status"
        || !path.starts_with("/api/")
    {
        return next.run(req).await;
    }
    // `/api/v1/auth/me` is protected and reads the token below.

    let Some(header) = req.headers().get(axum::http::header::AUTHORIZATION) else {
        return Json(serde_json::json!({
            "success": false,
            "error": "Authentication required"
        }))
        .into_response();
    };
    let Ok(header_str) = header.to_str() else {
        return Json(serde_json::json!({
            "success": false,
            "error": "Malformed authorization header"
        }))
        .into_response();
    };
    let user = {
        let store = state.auth.lock().unwrap();
        store.bearer_user(header_str).ok()
    };
    match user {
        Some(user) => {
            let mut req = req;
            req.extensions_mut().insert(user);
            next.run(req).await
        }
        None => Json(serde_json::json!({
            "success": false,
            "error": "Invalid or expired token"
        }))
        .into_response(),
    }
}

async fn auth_register_handler(
    State(state): State<SharedState>,
    Json(req): Json<RegisterRequest>,
) -> std::result::Result<Json<AuthResponse>, IntentApiError> {
    let mut store = state.auth.lock().unwrap();
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
    let mut store = state.auth.lock().unwrap();
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
    let store = state.auth.lock().unwrap();
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
    let scheduler = state.scheduler.lock().unwrap();
    let registry = state.registry.lock().unwrap();
    let watchdog = state.watchdog.lock().unwrap();

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
        let acl = state.access_control.lock().unwrap();
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
) -> Json<WorkflowResponse> {
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
            let acl = state.access_control.lock().unwrap();
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

    Json(WorkflowResponse {
        total_steps: results.len(),
        successful,
        failed,
        results,
    })
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
                    Err("Adjust priority not implemented via bridge yet".into())
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
                    let data = std::fs::read(
                        wasm_path
                            .as_ref()
                            .unwrap_or(&std::path::PathBuf::from(name)),
                    )
                    .map_err(|e| format!("Read failed: {e}"))?;
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
                BlockAction::HotSwap => Err("Hot-swap not implemented via bridge yet".into()),
            }
        }
        UserIntent::SystemQuery { metric } => {
            let scheduler = state.scheduler.lock().map_err(|e| e.to_string())?;
            let (ram_used, ram_total) = scheduler.ram_usage();
            match metric {
                MetricType::Cpu => {
                    Ok(serde_json::json!({ "cpu": "metrics not available from bridge" }))
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
        UserIntent::WorkflowExecution { .. } => {
            Err("Workflow execution not implemented via bridge yet".into())
        }
        UserIntent::Unknown { raw_prompt } => Ok(
            serde_json::json!({ "unknown_intent": raw_prompt, "hint": "Try: 'show processes', 'status', 'kill 2', 'запусти блок'" }),
        ),
    }
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<SharedState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_socket(socket, state))
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
) -> impl IntoResponse {
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
        Ok(response) => Json(LlmQueryResponse {
            success: true,
            text: Some(response.text),
            duration_ms: response.duration_ms,
            error: None,
        }),
        Err(e) => Json(LlmQueryResponse {
            success: false,
            text: None,
            duration_ms: 0,
            error: Some(e.to_string()),
        }),
    }
}

impl From<BridgeError> for IntentApiError {
    fn from(e: BridgeError) -> Self {
        let status = match &e {
            BridgeError::CapabilityDenied(_) => StatusCode::FORBIDDEN,
            BridgeError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            BridgeError::IntentParseFailed(_) => StatusCode::UNPROCESSABLE_ENTITY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self { status, error: e }
    }
}

async fn browse_handler(Json(req): Json<BrowseRequest>) -> Json<BrowseResponse> {
    let config = aios_browser::types::BrowserConfig::default();
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
    let registry = state.store_registry.lock().unwrap();
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
    let mut registry = state.store_registry.lock().unwrap();
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
    let collector = state.metric_collector.lock().unwrap();
    let prometheus = collector.to_prometheus();
    Json(MetricsResponse {
        success: true,
        prometheus,
    })
}

async fn traces_handler(State(state): State<SharedState>) -> Json<TracesResponse> {
    let trace = state.trace_context.lock().unwrap();
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
    let mut reporter = state.crash_reporter.lock().unwrap();
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
