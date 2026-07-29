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
use aios_store::StoreRegistry;
use aios_telemetry::{FlightRecorder, MetricCollector, TraceContext};
use aios_watchdog::watchdog::Watchdog;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tower_http::services::ServeDir;

pub struct BridgeContext {
    pub intent_parser: IntentParser,
    pub scheduler: Mutex<Scheduler>,
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
}

impl BridgeContext {
    pub fn new(
        scheduler: Scheduler,
        registry: BlockRegistry,
        access_control: AccessControlLayer,
        watchdog: Watchdog,
        bridge_block_id: u32,
    ) -> Self {
        Self {
            intent_parser: IntentParser::new(),
            scheduler: Mutex::new(scheduler),
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
        }
    }

    pub fn uptime_secs(&self) -> u64 {
        self.start_time.elapsed().map(|d| d.as_secs()).unwrap_or(0)
    }
}

type SharedState = Arc<BridgeContext>;

pub async fn start_server(state: SharedState, addr: &str) -> Result<()> {
    let app = Router::new()
        .route("/api/v1/health", get(health_handler))
        .route("/api/v1/system/status", get(status_handler))
        .route("/api/v1/intent", post(intent_handler))
        .route("/api/v1/workflow", post(workflow_handler))
        .route("/api/v1/llm/query", post(llm_query_handler))
        .route("/api/v1/browse", post(browse_handler))
        .route("/api/v1/search", post(search_handler))
        .route("/api/v1/store/index", get(store_index_handler))
        .route("/api/v1/store/register", post(store_register_handler))
        .route("/api/v1/metrics", get(metrics_handler))
        .route("/api/v1/traces", get(traces_handler))
        .route("/api/v1/crash-report", post(crash_report_handler))
        .route("/ws/telemetry", get(ws_handler))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(state)
        .fallback_service(ServeDir::new("aios-studio"));

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| BridgeError::ServerError(format!("Bind failed: {e}")))?;

    log::info!("AIOS Bridge listening on {addr}");
    axum::serve(listener, app)
        .await
        .map_err(|e| BridgeError::ServerError(format!("Server error: {e}")))?;

    Ok(())
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

    let processes: Vec<ProcessEntry> = (0..process_count)
        .filter_map(|i| {
            let pid = ProcessId(i as u64);
            scheduler.get_process(pid).map(|p| ProcessEntry {
                pid: p.pid.0,
                name: p.name.clone(),
                priority: format!("{:?}", p.priority),
                state: format!("{:?}", p.state),
                ram_mb: p.ram_quota_mb,
                cpu_ms: p.cpu_time_ms,
            })
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
    let intent = state.intent_parser.parse_with_llm_fallback(&prompt, &llm).await;
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
        let intent = state.intent_parser.parse_with_llm_fallback(prompt, &llm).await;
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
                    drop(scheduler);
                    let registry = state.registry.lock().map_err(|e| e.to_string())?;
                    Ok(serde_json::json!({
                        "process_count": 0,
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

async fn browse_handler(
    Json(req): Json<BrowseRequest>,
) -> Json<BrowseResponse> {
    let config = aios_browser::types::BrowserConfig::default();
    let engine = aios_browser::BrowserEngine::new(config);

    match engine.navigate(&req.url).await {
        Ok(page) => {
            let links: Vec<serde_json::Value> = page
                .links
                .iter()
                .map(|l| {
                    serde_json::json!({ "href": l.href, "text": l.text })
                })
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

async fn search_handler(
    Json(req): Json<SearchRequest>,
) -> Json<SearchResponse> {
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
