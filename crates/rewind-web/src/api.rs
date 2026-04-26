use axum::{
    extract::{DefaultBodyLimit, Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{delete, get, post},
    Router,
};
use rewind_replay::ReplayEngine;
use rewind_store::{Session, SessionSource, Step, StepStatus, StepType, Timeline};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AppState, StoreEvent};

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/sessions", get(list_sessions))
        .route("/sessions/{id}", get(get_session))
        .route("/sessions/{id}/steps", get(get_session_steps))
        .route("/sessions/{id}/timelines", get(get_session_timelines))
        .route("/sessions/{id}/diff", get(diff_timelines))
        .route("/sessions/{id}/export/otel", post(export_otel))
        .route("/sessions/{id}/savings", get(get_session_savings))
        .route("/steps/{id}", get(get_step_detail))
        .route("/baselines", get(list_baselines))
        .route("/baselines/{name}", get(get_baseline))
        .route("/cache/stats", get(cache_stats))
        .route("/snapshots", get(list_snapshots))
        .route("/sessions/{id}/spans", get(get_session_spans))
        .route("/threads", get(list_threads))
        .route("/threads/{id}", get(get_thread))
        // Explicit Recording API (wire-format-agnostic)
        .route("/sessions/start", post(start_session))
        .route("/sessions/{id}/end", post(end_session))
        .route("/sessions/{id}/llm-calls", post(record_llm_call))
        .route("/sessions/{id}/llm-calls/replay-lookup", post(replay_lookup_llm))
        .route("/sessions/{id}/tool-calls", post(record_tool_call))
        .route("/sessions/{id}/tool-calls/replay-lookup", post(replay_lookup_tool))
        .route("/sessions/{id}/fork", post(fork_session))
        .route("/sessions/{session_id}/timelines/{timeline_id}", delete(delete_timeline_handler))
        .route("/replay-contexts", post(create_replay_context))
        .route("/replay-contexts/{id}", delete(delete_replay_context_handler))
        .layer(DefaultBodyLimit::max(10 * 1024 * 1024)) // 10MB
        .with_state(state)
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn list_sessions(
    State(state): State<AppState>,
) -> Result<Json<Vec<rewind_store::Session>>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;
    let sessions = store.list_sessions().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;
    Ok(Json(sessions))
}

async fn get_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<SessionDetailResponse>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;

    let session = resolve_session(&store, &id)
        .map_err(|e| (StatusCode::NOT_FOUND, format!("{e}")))?;

    let timelines = store.get_timelines(&session.id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;

    Ok(Json(SessionDetailResponse { session, timelines }))
}

#[derive(Serialize)]
struct SessionDetailResponse {
    session: rewind_store::Session,
    timelines: Vec<rewind_store::Timeline>,
}

#[derive(Deserialize)]
struct StepsQuery {
    timeline: Option<String>,
    include_blobs: Option<u8>,
}

async fn get_session_steps(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<StepsQuery>,
) -> Result<Json<Vec<StepResponse>>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;

    let session = resolve_session(&store, &id)
        .map_err(|e| (StatusCode::NOT_FOUND, format!("{e}")))?;

    let engine = ReplayEngine::new(&store);
    let timelines = store.get_timelines(&session.id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;

    let timeline_id = if let Some(ref tref) = query.timeline {
        resolve_timeline_ref(&timelines, tref)
            .map_err(|e| (StatusCode::NOT_FOUND, format!("{e}")))?
    } else {
        timelines.iter()
            .find(|t| t.parent_timeline_id.is_none())
            .map(|t| t.id.clone())
            .ok_or_else(|| (StatusCode::NOT_FOUND, "No root timeline".to_string()))?
    };

    let steps = engine.get_full_timeline_steps(&timeline_id, &session.id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;

    let with_blobs = query.include_blobs.unwrap_or(0) == 1;

    let responses: Vec<StepResponse> = steps.iter().map(|s| {
        let response_preview = extract_preview(&store, &s.response_blob);
        let (req_body, resp_body) = if with_blobs {
            (
                blob_to_json(&store, &s.request_blob),
                blob_to_json(&store, &s.response_blob),
            )
        } else {
            (None, None)
        };
        StepResponse {
            id: s.id.clone(),
            timeline_id: s.timeline_id.clone(),
            session_id: s.session_id.clone(),
            step_number: s.step_number,
            step_type: s.step_type.as_str().to_string(),
            step_type_label: s.step_type.label().to_string(),
            step_type_icon: s.step_type.icon().to_string(),
            status: s.status.as_str().to_string(),
            created_at: s.created_at.to_rfc3339(),
            duration_ms: s.duration_ms,
            tokens_in: s.tokens_in,
            tokens_out: s.tokens_out,
            model: s.model.clone(),
            error: s.error.clone(),
            tool_name: s.tool_name.clone(),
            response_preview,
            request_body: req_body,
            response_body: resp_body,
        }
    }).collect();

    Ok(Json(responses))
}

#[derive(Serialize)]
struct StepResponse {
    id: String,
    timeline_id: String,
    session_id: String,
    step_number: u32,
    step_type: String,
    step_type_label: String,
    step_type_icon: String,
    status: String,
    created_at: String,
    duration_ms: u64,
    tokens_in: u64,
    tokens_out: u64,
    model: String,
    error: Option<String>,
    tool_name: Option<String>,
    response_preview: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_body: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_body: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct SpanResponse {
    id: String,
    session_id: String,
    timeline_id: String,
    parent_span_id: Option<String>,
    span_type: String,
    span_type_icon: String,
    name: String,
    status: String,
    started_at: String,
    ended_at: Option<String>,
    duration_ms: u64,
    metadata: serde_json::Value,
    error: Option<String>,
    child_spans: Vec<SpanResponse>,
    steps: Vec<StepResponse>,
}

async fn get_step_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<StepDetailResponse>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;

    let step = store.get_step(&id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?.ok_or_else(|| (StatusCode::NOT_FOUND, format!("Step not found: {id}")))?;

    let request_body = if !step.request_blob.is_empty() {
        store.blobs.get(&step.request_blob).ok()
            .and_then(|data| String::from_utf8(data).ok())
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
    } else {
        None
    };

    let response_body = if !step.response_blob.is_empty() {
        store.blobs.get(&step.response_blob).ok()
            .and_then(|data| String::from_utf8(data).ok())
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
    } else {
        None
    };

    let messages = request_body.as_ref().and_then(extract_messages);

    Ok(Json(StepDetailResponse {
        id: step.id,
        timeline_id: step.timeline_id,
        session_id: step.session_id,
        step_number: step.step_number,
        step_type: step.step_type.as_str().to_string(),
        status: step.status.as_str().to_string(),
        created_at: step.created_at.to_rfc3339(),
        duration_ms: step.duration_ms,
        tokens_in: step.tokens_in,
        tokens_out: step.tokens_out,
        model: step.model,
        error: step.error,
        tool_name: step.tool_name,
        request_body,
        response_body,
        messages,
    }))
}

#[derive(Serialize)]
struct StepDetailResponse {
    id: String,
    timeline_id: String,
    session_id: String,
    step_number: u32,
    step_type: String,
    status: String,
    created_at: String,
    duration_ms: u64,
    tokens_in: u64,
    tokens_out: u64,
    model: String,
    error: Option<String>,
    tool_name: Option<String>,
    request_body: Option<serde_json::Value>,
    response_body: Option<serde_json::Value>,
    messages: Option<Vec<MessageView>>,
}

#[derive(Serialize)]
struct MessageView {
    role: String,
    content: String,
}

fn extract_messages(request: &serde_json::Value) -> Option<Vec<MessageView>> {
    let messages = request.get("messages")?.as_array()?;
    let views: Vec<MessageView> = messages.iter().filter_map(|m| {
        let role = m.get("role")?.as_str()?.to_string();
        let content = if let Some(s) = m.get("content").and_then(|c| c.as_str()) {
            s.to_string()
        } else if let Some(arr) = m.get("content").and_then(|c| c.as_array()) {
            arr.iter().filter_map(|block| {
                block.get("text").and_then(|t| t.as_str()).map(String::from)
            }).collect::<Vec<_>>().join("\n")
        } else {
            return None;
        };
        Some(MessageView { role, content })
    }).collect();
    if views.is_empty() { None } else { Some(views) }
}

async fn get_session_timelines(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<rewind_store::Timeline>>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;
    let session = resolve_session(&store, &id)
        .map_err(|e| (StatusCode::NOT_FOUND, format!("{e}")))?;
    let timelines = store.get_timelines(&session.id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;
    Ok(Json(timelines))
}

#[derive(Deserialize)]
struct DiffQuery {
    left: String,
    right: String,
}

async fn diff_timelines(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<DiffQuery>,
) -> Result<Json<rewind_replay::TimelineDiff>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;
    let session = resolve_session(&store, &id)
        .map_err(|e| (StatusCode::NOT_FOUND, format!("{e}")))?;

    let timelines = store.get_timelines(&session.id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;

    let left_id = resolve_timeline_ref(&timelines, &query.left)
        .map_err(|e| (StatusCode::NOT_FOUND, format!("{e}")))?;
    let right_id = resolve_timeline_ref(&timelines, &query.right)
        .map_err(|e| (StatusCode::NOT_FOUND, format!("{e}")))?;

    let engine = ReplayEngine::new(&store);
    let diff = engine.diff_timelines(&session.id, &left_id, &right_id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Diff error: {e}"))
    })?;
    Ok(Json(diff))
}

async fn list_baselines(
    State(state): State<AppState>,
) -> Result<Json<Vec<rewind_store::Baseline>>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;
    let baselines = store.list_baselines().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;
    Ok(Json(baselines))
}

async fn get_baseline(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<BaselineDetailResponse>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;
    let baseline = store.get_baseline_by_name(&name).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?.ok_or_else(|| (StatusCode::NOT_FOUND, format!("Baseline not found: {name}")))?;

    let steps = store.get_baseline_steps(&baseline.id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;

    Ok(Json(BaselineDetailResponse { baseline, steps }))
}

#[derive(Serialize)]
struct BaselineDetailResponse {
    baseline: rewind_store::Baseline,
    steps: Vec<rewind_store::BaselineStep>,
}

async fn cache_stats(
    State(state): State<AppState>,
) -> Result<Json<rewind_store::CacheStats>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;
    let stats = store.cache_stats().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;
    Ok(Json(stats))
}

async fn list_snapshots(
    State(state): State<AppState>,
) -> Result<Json<Vec<rewind_store::Snapshot>>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;
    let snapshots = store.list_snapshots().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;
    Ok(Json(snapshots))
}

async fn get_session_spans(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<StepsQuery>,
) -> Result<Json<Vec<SpanResponse>>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;

    let session = resolve_session(&store, &id)
        .map_err(|e| (StatusCode::NOT_FOUND, format!("{e}")))?;

    let engine = ReplayEngine::new(&store);
    let timelines = store.get_timelines(&session.id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;

    let timeline_id = if let Some(ref tref) = query.timeline {
        resolve_timeline_ref(&timelines, tref)
            .map_err(|e| (StatusCode::NOT_FOUND, format!("{e}")))?
    } else {
        timelines.iter()
            .find(|t| t.parent_timeline_id.is_none())
            .map(|t| t.id.clone())
            .ok_or_else(|| (StatusCode::NOT_FOUND, "No root timeline".to_string()))?
    };

    let spans = engine.get_full_timeline_spans(&timeline_id, &session.id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;

    let steps = engine.get_full_timeline_steps(&timeline_id, &session.id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;

    let tree = build_span_tree(&spans, &steps, &store);
    Ok(Json(tree))
}

fn build_span_tree(spans: &[rewind_store::Span], steps: &[rewind_store::Step], store: &rewind_store::Store) -> Vec<SpanResponse> {
    let root_spans: Vec<&rewind_store::Span> = spans.iter()
        .filter(|s| s.parent_span_id.is_none())
        .collect();

    root_spans.iter().map(|s| build_span_response(s, spans, steps, store)).collect()
}

fn build_span_response(span: &rewind_store::Span, all_spans: &[rewind_store::Span], all_steps: &[rewind_store::Step], store: &rewind_store::Store) -> SpanResponse {
    let child_spans: Vec<SpanResponse> = all_spans.iter()
        .filter(|s| s.parent_span_id.as_deref() == Some(&span.id))
        .map(|s| build_span_response(s, all_spans, all_steps, store))
        .collect();

    let span_steps: Vec<StepResponse> = all_steps.iter()
        .filter(|s| s.span_id.as_deref() == Some(&span.id))
        .map(|s| {
            let response_preview = extract_preview(store, &s.response_blob);
            StepResponse {
                id: s.id.clone(),
                timeline_id: s.timeline_id.clone(),
                session_id: s.session_id.clone(),
                step_number: s.step_number,
                step_type: s.step_type.as_str().to_string(),
                step_type_label: s.step_type.label().to_string(),
                step_type_icon: s.step_type.icon().to_string(),
                status: s.status.as_str().to_string(),
                created_at: s.created_at.to_rfc3339(),
                duration_ms: s.duration_ms,
                tokens_in: s.tokens_in,
                tokens_out: s.tokens_out,
                model: s.model.clone(),
                error: s.error.clone(),
                tool_name: s.tool_name.clone(),
                response_preview,
                request_body: None,
                response_body: None,
            }
        }).collect();

    SpanResponse {
        id: span.id.clone(),
        session_id: span.session_id.clone(),
        timeline_id: span.timeline_id.clone(),
        parent_span_id: span.parent_span_id.clone(),
        span_type: span.span_type.as_str().to_string(),
        span_type_icon: span.span_type.icon().to_string(),
        name: span.name.clone(),
        status: span.status.clone(),
        started_at: span.started_at.to_rfc3339(),
        ended_at: span.ended_at.map(|dt| dt.to_rfc3339()),
        duration_ms: span.duration_ms,
        metadata: span.metadata.clone(),
        error: span.error.clone(),
        child_spans,
        steps: span_steps,
    }
}

#[derive(Serialize)]
struct ThreadSummary {
    thread_id: String,
    session_count: usize,
    total_steps: u32,
    total_tokens: u64,
}

async fn list_threads(
    State(state): State<AppState>,
) -> Result<Json<Vec<ThreadSummary>>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;

    let thread_ids = store.list_thread_ids().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;

    let mut threads = Vec::new();
    for tid in &thread_ids {
        let sessions = store.get_sessions_by_thread(tid).map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
        })?;
        threads.push(ThreadSummary {
            thread_id: tid.clone(),
            session_count: sessions.len(),
            total_steps: sessions.iter().map(|s| s.total_steps).sum(),
            total_tokens: sessions.iter().map(|s| s.total_tokens).sum(),
        });
    }

    Ok(Json(threads))
}

#[derive(Serialize)]
struct ThreadDetailResponse {
    thread_id: String,
    sessions: Vec<rewind_store::Session>,
}

async fn get_thread(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ThreadDetailResponse>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;

    let sessions = store.get_sessions_by_thread(&id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;

    Ok(Json(ThreadDetailResponse {
        thread_id: id,
        sessions,
    }))
}

fn resolve_session(store: &rewind_store::Store, session_ref: &str) -> anyhow::Result<rewind_store::Session> {
    if session_ref == "latest" {
        store.get_latest_session()?.ok_or_else(|| anyhow::anyhow!("No sessions found"))
    } else {
        if let Some(session) = store.get_session(session_ref)? {
            return Ok(session);
        }
        let sessions = store.list_sessions()?;
        sessions.into_iter()
            .find(|s| s.id.starts_with(session_ref))
            .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_ref))
    }
}

fn resolve_timeline_ref(timelines: &[rewind_store::Timeline], reference: &str) -> anyhow::Result<String> {
    if let Some(t) = timelines.iter().find(|t| t.id == reference) {
        return Ok(t.id.clone());
    }
    if let Some(t) = timelines.iter().find(|t| t.id.starts_with(reference)) {
        return Ok(t.id.clone());
    }
    if let Some(t) = timelines.iter().find(|t| t.label == reference) {
        return Ok(t.id.clone());
    }
    anyhow::bail!("Timeline not found: {}", reference)
}

pub fn extract_preview_from_store(store: &rewind_store::Store, blob_hash: &str) -> String {
    extract_preview(store, blob_hash)
}

fn blob_to_json(store: &rewind_store::Store, blob_hash: &str) -> Option<serde_json::Value> {
    if blob_hash.is_empty() {
        return None;
    }
    store.blobs.get(blob_hash).ok()
        .and_then(|data| String::from_utf8(data).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
}

fn extract_preview(store: &rewind_store::Store, blob_hash: &str) -> String {
    if blob_hash.is_empty() {
        return String::new();
    }
    store.blobs.get(blob_hash).ok()
        .and_then(|data| String::from_utf8(data).ok())
        .and_then(|json_str| {
            let val: serde_json::Value = serde_json::from_str(&json_str).ok()?;
            // OpenAI format
            if let Some(content) = val.pointer("/choices/0/message/content").and_then(|c| c.as_str()) {
                return Some(content.replace('\n', " ").chars().take(200).collect());
            }
            if let Some(calls) = val.pointer("/choices/0/message/tool_calls").and_then(|c| c.as_array()) {
                let names: Vec<&str> = calls.iter()
                    .filter_map(|c| c.pointer("/function/name").and_then(|n| n.as_str()))
                    .collect();
                return Some(format!("tool_calls: [{}]", names.join(", ")));
            }
            // Anthropic format
            if let Some(content) = val.get("content").and_then(|c| c.as_array())
                && let Some(text) = content.first().and_then(|b| b.get("text")).and_then(|t| t.as_str()) {
                    return Some(text.replace('\n', " ").chars().take(200).collect());
                }
            // LLM Gateway format (generations[0].content)
            if let Some(content) = val.pointer("/generations/0/content").and_then(|c| c.as_str()) {
                return Some(content.replace('\n', " ").chars().take(200).collect());
            }
            if let Some(invocations) = val.pointer("/generations/0/tool_invocations").and_then(|c| c.as_array()) {
                let names: Vec<&str> = invocations.iter()
                    .filter_map(|c| c.get("name").and_then(|n| n.as_str()))
                    .collect();
                return Some(format!("tool_invocations: [{}]", names.join(", ")));
            }
            // Generic: any top-level "content" string
            if let Some(content) = val.get("content").and_then(|c| c.as_str()) {
                return Some(content.replace('\n', " ").chars().take(200).collect());
            }
            // Last resort: raw JSON truncated
            Some(json_str.chars().take(200).collect())
        })
        .unwrap_or_default()
}

// ── OTel Export ──────────────────────────────────────────────

#[derive(Deserialize)]
struct OtelExportRequest {
    endpoint: Option<String>,
    #[serde(default)]
    include_content: bool,
    timeline_id: Option<String>,
    #[serde(default)]
    all_timelines: bool,
}

#[derive(Serialize)]
struct OtelExportResponse {
    spans_exported: usize,
    trace_id: String,
}

async fn export_otel(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<OtelExportRequest>,
) -> Result<Json<OtelExportResponse>, (StatusCode, String)> {
    // Destructure body to take ownership (avoids unnecessary clones)
    let OtelExportRequest { endpoint: body_endpoint, include_content, timeline_id, all_timelines } = body;

    // Resolve endpoint: request body takes priority, then server-side env var
    let export_endpoint = body_endpoint
        .or_else(|| state.otel_config.as_ref().map(|c| c.endpoint.clone()))
        .unwrap_or_default();
    if export_endpoint.is_empty() {
        return Err((
            StatusCode::NOT_IMPLEMENTED,
            "No endpoint provided. Pass 'endpoint' in request body or set REWIND_OTEL_ENDPOINT.".to_string(),
        ));
    }

    // SSRF guard: reject endpoints that resolve to private/reserved IP ranges
    // (RFC 1918, link-local, loopback, cloud metadata, etc.) before we open any
    // outbound connection. See docs/security-audit.md §CRITICAL-01.
    crate::url_guard::validate_export_endpoint(&export_endpoint)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    // Extract session data synchronously (Store is not Send)
    let data = {
        let store = state
            .store
            .lock()
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock: {e}")))?;

        let session = resolve_session(&store, &id)
            .map_err(|e| (StatusCode::NOT_FOUND, format!("{e}")))?;

        let opts = rewind_otel::extract::ExtractOptions {
            timeline_id,
            all_timelines,
        };

        rewind_otel::extract::extract_session_data(&store, &session.id, &opts)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Extract error: {e}")))?
    };

    // Build export config: use request endpoint, fall back to server-side protocol/headers
    let server_config = state.otel_config.as_ref();
    let config = rewind_otel::export::ExportConfig {
        endpoint: export_endpoint,
        protocol: server_config.map(|c| c.protocol).unwrap_or(rewind_otel::export::Protocol::Http),
        headers: server_config.map(|c| c.headers.clone()).unwrap_or_default(),
        include_content,
    };

    let trace_id = rewind_otel::export::trace_id_from_session(&data.session.id);

    // Export (sync — no Store needed)
    let spans_exported = rewind_otel::export::export_to_otlp(&data, &config)
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Export error: {e}")))?;

    Ok(Json(OtelExportResponse {
        spans_exported,
        trace_id: trace_id.to_string(),
    }))
}

async fn get_session_savings(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<rewind_store::pricing::ReplaySavings>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;

    let session = resolve_session(&store, &id).map_err(|e| {
        (StatusCode::NOT_FOUND, e.to_string())
    })?;

    let timelines = store.get_timelines(&session.id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;

    let mut cumulative = rewind_store::pricing::ReplaySavings {
        steps_total: 0,
        steps_cached: 0,
        steps_live: 0,
        tokens_saved: 0,
        cost_saved_usd: 0.0,
        time_saved_ms: 0,
    };

    for fork in &timelines {
        let parent_id = match &fork.parent_timeline_id {
            Some(id) => id,
            None => continue,
        };
        let fork_at = match fork.fork_at_step {
            Some(n) => n,
            None => continue,
        };
        let parent_steps = store.get_steps(parent_id).unwrap_or_default();
        let own_steps = store.get_steps(&fork.id).unwrap_or_default();

        let cached: Vec<_> = parent_steps.into_iter()
            .filter(|s| s.step_number <= fork_at)
            .collect();
        let savings = rewind_store::pricing::compute_savings(&cached, &own_steps);

        cumulative.steps_total += savings.steps_total;
        cumulative.steps_cached += savings.steps_cached;
        cumulative.steps_live += savings.steps_live;
        cumulative.tokens_saved += savings.tokens_saved;
        cumulative.cost_saved_usd += savings.cost_saved_usd;
        cumulative.time_saved_ms += savings.time_saved_ms;
    }

    // Round cumulative cost
    cumulative.cost_saved_usd = (cumulative.cost_saved_usd * 100.0).round() / 100.0;

    Ok(Json(cumulative))
}

// ══════════════════════════════════════════════════════════
// Explicit Recording API (wire-format-agnostic)
// ══════════════════════════════════════════════════════════

#[derive(Deserialize)]
struct StartSessionRequest {
    name: String,
    source: Option<String>,
    thread_id: Option<String>,
    metadata: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct StartSessionResponse {
    session_id: String,
    root_timeline_id: String,
}

async fn start_session(
    State(state): State<AppState>,
    Json(body): Json<StartSessionRequest>,
) -> Result<(StatusCode, Json<StartSessionResponse>), (StatusCode, String)> {
    let mut session = Session::new(&body.name);
    session.source = SessionSource::Api;
    session.thread_id = body.thread_id;
    if let Some(meta) = body.metadata {
        session.metadata = meta;
    }
    if let Some(ref src) = body.source {
        session.metadata["source_label"] = serde_json::json!(src);
    }

    let timeline = Timeline::new_root(&session.id);
    let session_id = session.id.clone();
    let timeline_id = timeline.id.clone();

    {
        let store = state.store.lock().map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
        })?;
        store.create_session(&session).map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
        })?;
        store.create_timeline(&timeline).map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
        })?;
    }

    let _ = state.event_tx.send(StoreEvent::SessionUpdated {
        session_id: session_id.clone(),
        status: "recording".to_string(),
        total_steps: 0,
        total_tokens: 0,
    });

    Ok((StatusCode::CREATED, Json(StartSessionResponse {
        session_id,
        root_timeline_id: timeline_id,
    })))
}

#[derive(Deserialize)]
struct EndSessionRequest {
    status: String,
    #[allow(dead_code)]
    error: Option<String>,
}

#[derive(Serialize)]
struct EndSessionResponse {
    session_id: String,
}

async fn end_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<EndSessionRequest>,
) -> Result<Json<EndSessionResponse>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;
    let session = resolve_session(&store, &id)
        .map_err(|e| (StatusCode::NOT_FOUND, format!("{e}")))?;

    let status = match body.status.as_str() {
        "errored" | "failed" => rewind_store::SessionStatus::Failed,
        _ => rewind_store::SessionStatus::Completed,
    };
    store.update_session_status(&session.id, status).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;

    let _ = state.event_tx.send(StoreEvent::SessionUpdated {
        session_id: session.id.clone(),
        status: body.status,
        total_steps: session.total_steps,
        total_tokens: session.total_tokens,
    });

    Ok(Json(EndSessionResponse { session_id: session.id }))
}

#[derive(Deserialize)]
struct RecordLlmCallRequest {
    timeline_id: Option<String>,
    client_step_id: Option<String>,
    request_body: serde_json::Value,
    response_body: serde_json::Value,
    model: String,
    duration_ms: u64,
    tokens_in: Option<u64>,
    tokens_out: Option<u64>,
}

#[derive(Serialize)]
struct RecordStepResponse {
    step_number: u32,
}

async fn record_llm_call(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<RecordLlmCallRequest>,
) -> Result<(StatusCode, Json<RecordStepResponse>), (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;

    let session = resolve_session(&store, &id)
        .map_err(|e| (StatusCode::NOT_FOUND, format!("{e}")))?;

    match session.status {
        rewind_store::SessionStatus::Recording => {}
        rewind_store::SessionStatus::Completed => {
            store.update_session_status(&session.id, rewind_store::SessionStatus::Recording)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}")))?;
        }
        status => return Err((StatusCode::CONFLICT, format!(
            "Session is '{}', cannot record", status.as_str()
        ))),
    }

    let timeline_id = match body.timeline_id {
        Some(ref tid) => tid.clone(),
        None => store.get_root_timeline(&session.id)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}")))?
            .map(|t| t.id)
            .ok_or_else(|| (StatusCode::NOT_FOUND, "No root timeline".to_string()))?,
    };

    if let Some(ref cid) = body.client_step_id
        && let Ok(Some(existing)) = store.get_step(cid)
    {
        return Ok((StatusCode::OK, Json(RecordStepResponse { step_number: existing.step_number })));
    }

    let step_number = store.next_step_number(&session.id, &timeline_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Counter error: {e}")))?;

    let request_bytes = serde_json::to_vec(&body.request_body).unwrap_or_default();
    let request_blob = store.blobs.put(&request_bytes)
        .map_err(|e| (StatusCode::INSUFFICIENT_STORAGE, format!("Blob error: {e}")))?;
    let response_blob = store.blobs.put(&serde_json::to_vec(&body.response_body).unwrap_or_default())
        .map_err(|e| (StatusCode::INSUFFICIENT_STORAGE, format!("Blob error: {e}")))?;
    // Step 0.1: canonical post-redaction hash for replay-cache validation.
    // Distinct from request_blob (content-addressed). Same redaction
    // pipeline as the proxy record path so cache lookups match across
    // record paths even though the stored blob may differ in redaction.
    let request_canonical_hash = rewind_store::normalize_and_hash(&request_bytes);

    let mut step = Step::new_llm_call(&timeline_id, &session.id, step_number, &body.model);
    step.status = StepStatus::Success;
    step.duration_ms = body.duration_ms;
    step.tokens_in = body.tokens_in.unwrap_or(0);
    step.tokens_out = body.tokens_out.unwrap_or(0);
    step.request_blob = request_blob;
    step.response_blob = response_blob;
    step.request_hash = Some(request_canonical_hash);
    if let Some(ref cid) = body.client_step_id {
        step.id = cid.clone();
    }

    store.create_step(&step).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;

    let total_tokens = step.tokens_in + step.tokens_out;
    let _ = store.update_session_stats(&session.id, step_number, total_tokens);

    let _ = state.event_tx.send(StoreEvent::StepCreated {
        session_id: session.id,
        step: Box::new(step),
    });

    Ok((StatusCode::CREATED, Json(RecordStepResponse { step_number })))
}

#[derive(Deserialize)]
struct RecordToolCallRequest {
    timeline_id: Option<String>,
    client_step_id: Option<String>,
    tool_name: String,
    request_body: serde_json::Value,
    response_body: serde_json::Value,
    duration_ms: u64,
    error: Option<String>,
}

async fn record_tool_call(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<RecordToolCallRequest>,
) -> Result<(StatusCode, Json<RecordStepResponse>), (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;

    let session = resolve_session(&store, &id)
        .map_err(|e| (StatusCode::NOT_FOUND, format!("{e}")))?;

    match session.status {
        rewind_store::SessionStatus::Recording => {}
        rewind_store::SessionStatus::Completed => {
            store.update_session_status(&session.id, rewind_store::SessionStatus::Recording)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}")))?;
        }
        status => return Err((StatusCode::CONFLICT, format!(
            "Session is '{}', cannot record", status.as_str()
        ))),
    }

    let timeline_id = match body.timeline_id {
        Some(ref tid) => tid.clone(),
        None => store.get_root_timeline(&session.id)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}")))?
            .map(|t| t.id)
            .ok_or_else(|| (StatusCode::NOT_FOUND, "No root timeline".to_string()))?,
    };

    if let Some(ref cid) = body.client_step_id
        && let Ok(Some(existing)) = store.get_step(cid)
    {
        return Ok((StatusCode::OK, Json(RecordStepResponse { step_number: existing.step_number })));
    }

    let step_number = store.next_step_number(&session.id, &timeline_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Counter error: {e}")))?;

    let request_bytes = serde_json::to_vec(&body.request_body).unwrap_or_default();
    let request_blob = store.blobs.put(&request_bytes)
        .map_err(|e| (StatusCode::INSUFFICIENT_STORAGE, format!("Blob error: {e}")))?;
    let response_blob = store.blobs.put(&serde_json::to_vec(&body.response_body).unwrap_or_default())
        .map_err(|e| (StatusCode::INSUFFICIENT_STORAGE, format!("Blob error: {e}")))?;
    // Step 0.1 — canonical hash for tool-call replay validation. Tool
    // request bodies are usually short JSON arg dicts; redaction may be a
    // no-op but normalize_and_hash applies it consistently with LLM calls.
    let request_canonical_hash = rewind_store::normalize_and_hash(&request_bytes);

    let mut step = Step::new_llm_call(&timeline_id, &session.id, step_number, "");
    step.step_type = StepType::ToolCall;
    step.status = if body.error.is_some() { StepStatus::Error } else { StepStatus::Success };
    step.duration_ms = body.duration_ms;
    step.request_blob = request_blob;
    step.response_blob = response_blob;
    step.tool_name = Some(body.tool_name);
    step.error = body.error;
    step.request_hash = Some(request_canonical_hash);
    if let Some(ref cid) = body.client_step_id {
        step.id = cid.clone();
    }

    store.create_step(&step).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;

    let _ = store.update_session_stats(&session.id, step_number, 0);

    let _ = state.event_tx.send(StoreEvent::StepCreated {
        session_id: session.id,
        step: Box::new(step),
    });

    Ok((StatusCode::CREATED, Json(RecordStepResponse { step_number })))
}

#[derive(Deserialize)]
struct ReplayLookupRequest {
    replay_context_id: String,
    /// Step 0.1: when supplied, the server hashes this body via
    /// `rewind_store::normalize_and_hash` and compares against the cached
    /// step's stored `request_hash`. On mismatch the server logs and either
    /// (default) returns the cached step with `divergent: true` flag set,
    /// or (if the replay_context was created with strict_match=1) returns
    /// HTTP 409. Pre-Tier-1 SDKs that omit this field bypass validation —
    /// behavior matches v0.12 ordinal-only semantics.
    request_body: Option<serde_json::Value>,
    #[allow(dead_code)]
    tool_name: Option<String>,
}

#[derive(Serialize)]
struct ReplayLookupResponse {
    hit: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_body: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    step_number: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_timeline_id: Option<String>,
    /// Step 0.1: true when the incoming request_body's canonical hash did
    /// not match the cached step's request_hash. The cached response is
    /// still returned (warn-on-divergence default); SDKs may surface this
    /// as a warning to the operator. Set only when a real comparison was
    /// performed — pre-migration steps with NULL request_hash and lookups
    /// without request_body skip validation entirely and leave this false.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    divergent: bool,
}

async fn replay_lookup_llm(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ReplayLookupRequest>,
) -> Result<(StatusCode, [(axum::http::HeaderName, axum::http::HeaderValue); 1], Json<ReplayLookupResponse>), (StatusCode, String)> {
    do_replay_lookup(&state, &id, &body, StepType::LlmCall).await
}

async fn replay_lookup_tool(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ReplayLookupRequest>,
) -> Result<(StatusCode, [(axum::http::HeaderName, axum::http::HeaderValue); 1], Json<ReplayLookupResponse>), (StatusCode, String)> {
    do_replay_lookup(&state, &id, &body, StepType::ToolCall).await
}

/// Header emitted on cache hits where the incoming request_body diverged
/// from the originally-recorded step's canonical hash. Operators / SDKs
/// can surface this in tracing without parsing the JSON response body.
const HEADER_CACHE_DIVERGENT: &str = "x-rewind-cache-divergent";

async fn do_replay_lookup(
    state: &AppState,
    session_ref: &str,
    body: &ReplayLookupRequest,
    expected_type: StepType,
) -> Result<
    (StatusCode, [(axum::http::HeaderName, axum::http::HeaderValue); 1], Json<ReplayLookupResponse>),
    (StatusCode, String),
> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;

    let ctx = store.get_replay_context(&body.replay_context_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}")))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "Replay context not found".to_string()))?;

    let (ctx_session_id, ctx_timeline_id, from_step, _current_step, strict_match) = ctx;

    let session = resolve_session(&store, session_ref)
        .map_err(|e| (StatusCode::NOT_FOUND, format!("{e}")))?;

    if session.id != ctx_session_id {
        return Err((StatusCode::BAD_REQUEST, "Replay context belongs to a different session".to_string()));
    }

    let ordinal = store.advance_replay_context(&body.replay_context_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}")))?;

    let target_step = from_step + ordinal;

    // O(1) lookup via idx_steps_timeline index instead of fetching all steps.
    // For forked timelines, check the fork timeline first, then fall back to parent.
    let cached_step = store.get_step_by_number(&ctx_timeline_id, target_step)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}")))?
        .or_else(|| {
            // Check parent timeline for inherited steps (fork scenario)
            store.get_timelines(&session.id).ok()
                .and_then(|timelines| timelines.iter()
                    .find(|t| t.id == ctx_timeline_id)
                    .and_then(|t| t.parent_timeline_id.clone()))
                .and_then(|parent_id| store.get_step_by_number(&parent_id, target_step).ok().flatten())
        })
        .filter(|s| s.step_type == expected_type);

    match cached_step {
        Some(ref step) if !step.response_blob.is_empty() => {
            // Step 0.1: cache content validation. The lookup is ordinal-based
            // (see advance_replay_context above), so mismatches here mean
            // the agent's call sequence diverged from the recording — either
            // the predicate (`is_llm_call`) misclassified an auxiliary call
            // and shifted the ordinal, or the agent's reasoning path took a
            // different branch this run.
            //
            // Validation runs only when:
            //   1. Caller supplied request_body (pre-Tier-1 SDKs may omit)
            //   2. Cached step has a request_hash (pre-migration rows are NULL
            //      and treated as "match anything" for backwards compat)
            let divergent = match (&body.request_body, &step.request_hash) {
                (Some(req), Some(stored_hash)) => {
                    let body_bytes = serde_json::to_vec(req).unwrap_or_default();
                    let incoming_hash = rewind_store::normalize_and_hash(&body_bytes);
                    if &incoming_hash != stored_hash {
                        if strict_match {
                            tracing::warn!(
                                replay_context_id = %body.replay_context_id,
                                target_step,
                                stored_hash = %stored_hash,
                                incoming_hash = %incoming_hash,
                                "cache_divergence (strict_match) — returning 409"
                            );
                            return Err((
                                StatusCode::CONFLICT,
                                format!(
                                    "Cache divergence at step {} (strict_match=true): incoming request hash {} does not match recorded {}",
                                    target_step,
                                    &incoming_hash[..16],
                                    &stored_hash[..16],
                                ),
                            ));
                        }
                        tracing::warn!(
                            replay_context_id = %body.replay_context_id,
                            target_step,
                            stored_hash = %stored_hash,
                            incoming_hash = %incoming_hash,
                            "cache_divergence — returning cached step with X-Rewind-Cache-Divergent header"
                        );
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            };

            let resp_data = store.blobs.get(&step.response_blob).unwrap_or_default();
            let resp_json = serde_json::from_slice(&resp_data).unwrap_or(serde_json::Value::Null);

            let header_value = if divergent { "true" } else { "false" };
            Ok((
                StatusCode::OK,
                [(
                    axum::http::HeaderName::from_static(HEADER_CACHE_DIVERGENT),
                    axum::http::HeaderValue::from_static(if header_value == "true" { "true" } else { "false" }),
                )],
                Json(ReplayLookupResponse {
                    hit: true,
                    response_body: Some(resp_json),
                    model: Some(step.model.clone()),
                    step_number: Some(step.step_number),
                    active_timeline_id: Some(ctx_timeline_id),
                    divergent,
                }),
            ))
        }
        _ => Ok((
            StatusCode::OK,
            [(
                axum::http::HeaderName::from_static(HEADER_CACHE_DIVERGENT),
                axum::http::HeaderValue::from_static("false"),
            )],
            Json(ReplayLookupResponse {
                hit: false,
                response_body: None,
                model: None,
                step_number: None,
                active_timeline_id: Some(ctx_timeline_id),
                divergent: false,
            }),
        )),
    }
}

#[derive(Deserialize)]
struct ForkRequest {
    at_step: u32,
    label: String,
    timeline_id: Option<String>,
}

#[derive(Serialize)]
struct ForkResponse {
    fork_timeline_id: String,
}

async fn fork_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ForkRequest>,
) -> Result<(StatusCode, Json<ForkResponse>), (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;

    let session = resolve_session(&store, &id)
        .map_err(|e| (StatusCode::NOT_FOUND, format!("{e}")))?;

    let timelines = store.get_timelines(&session.id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;

    let source_timeline_id = match body.timeline_id {
        Some(ref tid) => resolve_timeline_ref(&timelines, tid)
            .map_err(|e| (StatusCode::NOT_FOUND, format!("{e}")))?,
        None => timelines.iter()
            .find(|t| t.parent_timeline_id.is_none())
            .map(|t| t.id.clone())
            .ok_or_else(|| (StatusCode::NOT_FOUND, "No root timeline".to_string()))?,
    };

    let engine = ReplayEngine::new(&store);
    let fork = engine.fork(&session.id, &source_timeline_id, body.at_step, &body.label)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e}")))?;

    Ok((StatusCode::CREATED, Json(ForkResponse {
        fork_timeline_id: fork.id,
    })))
}

const MAX_REPLAY_CONTEXTS: u64 = 100;

#[derive(Deserialize)]
struct CreateReplayContextRequest {
    session_id: String,
    from_step: u32,
    fork_timeline_id: String,
}

#[derive(Serialize)]
struct CreateReplayContextResponse {
    replay_context_id: String,
    parent_steps_count: u32,
    fork_at_step: u32,
}

async fn create_replay_context(
    State(state): State<AppState>,
    Json(body): Json<CreateReplayContextRequest>,
) -> Result<(StatusCode, Json<CreateReplayContextResponse>), (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;

    let count = store.count_replay_contexts().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;
    if count >= MAX_REPLAY_CONTEXTS {
        return Err((StatusCode::TOO_MANY_REQUESTS, format!(
            "Too many active replay contexts ({count}/{MAX_REPLAY_CONTEXTS}). Delete unused ones first."
        )));
    }

    let session = resolve_session(&store, &body.session_id)
        .map_err(|e| (StatusCode::NOT_FOUND, format!("{e}")))?;

    let engine = ReplayEngine::new(&store);
    let steps = engine.get_full_timeline_steps(&body.fork_timeline_id, &session.id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}")))?;

    let parent_steps_count = steps.len() as u32;

    let ctx_id = Uuid::new_v4().to_string();
    store.create_replay_context(&ctx_id, &session.id, &body.fork_timeline_id, body.from_step)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}")))?;

    Ok((StatusCode::CREATED, Json(CreateReplayContextResponse {
        replay_context_id: ctx_id,
        parent_steps_count,
        fork_at_step: body.from_step,
    })))
}

#[derive(Serialize)]
struct DeleteReplayContextResponse {
    released: bool,
}

async fn delete_replay_context_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<DeleteReplayContextResponse>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;

    let released = store.delete_replay_context(&id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;

    Ok(Json(DeleteReplayContextResponse { released }))
}

#[derive(Serialize)]
struct DeleteTimelineResponse {
    deleted: bool,
}

/// DELETE /api/sessions/{session_id}/timelines/{timeline_id}
///
/// Hard-deletes a fork timeline plus its steps, spans, replay contexts,
/// scores, and step counters. See issue #143.
///
/// Unlike most mutation endpoints, this one **requires a full session ID**
/// — prefix-match and `"latest"` shortcuts are rejected with 400. Losing a
/// fork to the wrong session via a 2-char prefix would be a bad day.
///
/// Status mapping is variant-driven (no string matching on error text):
///   * `NotFound` → 404
///   * `IsRoot` / `HasChildren` / `HasBaselines` / `HasActiveReplayContext` → 409
///   * `Internal` (DB / lock) → 500
async fn delete_timeline_handler(
    State(state): State<AppState>,
    Path((session_id, timeline_id)): Path<(String, String)>,
) -> Result<Json<DeleteTimelineResponse>, (StatusCode, String)> {
    // Require a full UUID — 36 chars, "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx".
    // A short prefix or the "latest" alias would make a destructive call too
    // easy to misfire.
    if session_id.len() != 36 || session_id == "latest" {
        return Err((
            StatusCode::BAD_REQUEST,
            "DELETE requires the full session ID; prefix and 'latest' are rejected".to_string(),
        ));
    }

    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;

    // We deliberately do NOT call `resolve_session` here — the caller must
    // provide the canonical id. Existence is established by `delete_fork`'s
    // own lookup against `timelines WHERE session_id = ?`.
    let engine = ReplayEngine::new(&store);
    engine.delete_fork(&session_id, &timeline_id).map_err(|err| {
        use rewind_replay::DeleteForkError::*;
        let status = match &err {
            NotFound(_, _) => StatusCode::NOT_FOUND,
            IsRoot | HasChildren { .. } | HasBaselines { .. } | HasActiveReplayContext { .. } => {
                StatusCode::CONFLICT
            }
            Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, format!("{err}"))
    })?;

    Ok(Json(DeleteTimelineResponse { deleted: true }))
}
