use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
    Router,
};
use rewind_replay::ReplayEngine;
use serde::{Deserialize, Serialize};

use crate::AppState;

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/sessions", get(list_sessions))
        .route("/sessions/{id}", get(get_session))
        .route("/sessions/{id}/steps", get(get_session_steps))
        .route("/sessions/{id}/timelines", get(get_session_timelines))
        .route("/sessions/{id}/diff", get(diff_timelines))
        .route("/steps/{id}", get(get_step_detail))
        .route("/baselines", get(list_baselines))
        .route("/baselines/{name}", get(get_baseline))
        .route("/cache/stats", get(cache_stats))
        .route("/snapshots", get(list_snapshots))
        .route("/sessions/{id}/spans", get(get_session_spans))
        .route("/threads", get(list_threads))
        .route("/threads/{id}", get(get_thread))
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

    let responses: Vec<StepResponse> = steps.iter().map(|s| {
        let response_preview = extract_preview(&store, &s.response_blob);
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
            response_preview,
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
    response_preview: String,
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
                response_preview,
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

fn extract_preview(store: &rewind_store::Store, blob_hash: &str) -> String {
    if blob_hash.is_empty() {
        return String::new();
    }
    store.blobs.get(blob_hash).ok()
        .and_then(|data| String::from_utf8(data).ok())
        .and_then(|json_str| {
            let val: serde_json::Value = serde_json::from_str(&json_str).ok()?;
            if let Some(content) = val.pointer("/choices/0/message/content").and_then(|c| c.as_str()) {
                return Some(content.replace('\n', " ").chars().take(200).collect());
            }
            if let Some(calls) = val.pointer("/choices/0/message/tool_calls").and_then(|c| c.as_array()) {
                let names: Vec<&str> = calls.iter()
                    .filter_map(|c| c.pointer("/function/name").and_then(|n| n.as_str()))
                    .collect();
                return Some(format!("tool_calls: [{}]", names.join(", ")));
            }
            if let Some(content) = val.get("content").and_then(|c| c.as_array())
                && let Some(text) = content.first().and_then(|b| b.get("text")).and_then(|t| t.as_str()) {
                    return Some(text.replace('\n', " ").chars().take(200).collect());
                }
            Some(json_str.chars().take(200).collect())
        })
        .unwrap_or_default()
}
