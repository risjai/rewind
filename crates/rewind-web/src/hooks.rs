use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
    routing::post,
    Router,
};
use chrono::Utc;
use dashmap::DashMap;
use rewind_store::{
    Session, SessionSource, SessionStatus, Step, StepStatus, StepType, Timeline,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Instant;
use uuid::Uuid;

use crate::{AppState, StoreEvent};

// ── Payload types ──────────────────────────────────────────

/// Outer envelope POSTed by the hook script.
#[derive(Debug, Deserialize)]
pub struct HookEventEnvelope {
    pub source: String,
    pub event_type: String,
    pub timestamp: String,
    pub payload: serde_json::Value,
}

/// Inner payload specific to Claude Code hooks.
#[derive(Debug, Deserialize)]
pub struct ClaudeCodeHookPayload {
    pub session_id: String,
    pub hook_event_name: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input: Option<serde_json::Value>,
    pub tool_response: Option<serde_json::Value>,
    pub tool_use_id: Option<String>,
    pub transcript_path: Option<String>,
    pub cwd: Option<String>,
    pub permission_mode: Option<String>,
}

// ── In-memory session tracking ─────────────────────────────

pub struct HookSessionState {
    pub session_id: String,
    pub timeline_id: String,
    pub root_span_id: String,
    pub step_counter: AtomicU32,
    /// Maps tool_use_id -> (step_id, created_at_instant) for Pre/Post matching
    pub pending_steps: Mutex<HashMap<String, (String, Instant)>>,
}

pub struct HookIngestionState {
    /// Maps claude session_id -> HookSessionState
    pub sessions: DashMap<String, HookSessionState>,
    /// Deduplication: hash -> last seen time
    dedup_cache: Mutex<HashMap<u64, Instant>>,
}

impl Default for HookIngestionState {
    fn default() -> Self {
        Self {
            sessions: DashMap::new(),
            dedup_cache: Mutex::new(HashMap::new()),
        }
    }
}

impl HookIngestionState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rehydrate in-memory state from the database on server startup.
    /// This prevents duplicate sessions from being created after a restart.
    pub fn rehydrate_from_store(&self, store: &rewind_store::Store) {
        let sessions = match store.list_sessions() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to list sessions for rehydration: {e}");
                return;
            }
        };

        let mut count = 0;
        for session in sessions {
            if session.source != rewind_store::SessionSource::Hooks {
                continue;
            }

            let claude_session_id = match session.metadata.get("claude_session_id").and_then(|v| v.as_str()) {
                Some(id) => id.to_string(),
                None => continue,
            };

            // Already rehydrated (shouldn't happen but be safe)
            if self.sessions.contains_key(&claude_session_id) {
                continue;
            }

            // Find the root timeline
            let timeline_id = match store.get_root_timeline(&session.id) {
                Ok(Some(t)) => t.id,
                _ => continue,
            };

            // Find the root span
            let root_span_id = match store.get_spans_by_session(&session.id) {
                Ok(spans) => spans.into_iter()
                    .find(|s| s.parent_span_id.is_none())
                    .map(|s| s.id)
                    .unwrap_or_default(),
                Err(_) => String::new(),
            };

            self.sessions.insert(
                claude_session_id,
                HookSessionState {
                    session_id: session.id.clone(),
                    timeline_id,
                    root_span_id,
                    step_counter: AtomicU32::new(session.total_steps),
                    pending_steps: Mutex::new(HashMap::new()),
                },
            );
            count += 1;
        }

        if count > 0 {
            tracing::info!("Rehydrated {count} hook sessions from database");
        }
    }

    /// Returns true if this envelope was already seen within the last 60 seconds.
    fn is_duplicate(&self, envelope: &HookEventEnvelope) -> bool {
        let mut hasher = Sha256::new();
        hasher.update(envelope.source.as_bytes());
        hasher.update(envelope.event_type.as_bytes());
        hasher.update(envelope.timestamp.as_bytes());
        if let Ok(payload_bytes) = serde_json::to_vec(&envelope.payload) {
            hasher.update(&payload_bytes);
        }
        let hash_bytes = hasher.finalize();
        // Use first 8 bytes as u64
        let hash_key = u64::from_le_bytes(hash_bytes[..8].try_into().unwrap());

        let now = Instant::now();
        let mut cache = self.dedup_cache.lock().unwrap();

        // Periodic cleanup: remove entries older than 60 seconds
        if cache.len() > 1000 {
            cache.retain(|_, ts| now.duration_since(*ts).as_secs() < 60);
        }

        if let Some(ts) = cache.get(&hash_key)
            && now.duration_since(*ts).as_secs() < 60
        {
            return true;
        }
        cache.insert(hash_key, now);
        false
    }
}

// ── Response type ──────────────────────────────────────────

#[derive(Serialize)]
struct HookResponse {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

// ── Routes ─────────────────────────────────────────────────

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/event", post(handle_hook_event))
        .route("/events", post(handle_hook_events_batch))
        .with_state(state)
}

// ── Handlers ───────────────────────────────────────────────

async fn handle_hook_event(
    State(state): State<AppState>,
    Json(envelope): Json<HookEventEnvelope>,
) -> (StatusCode, Json<HookResponse>) {
    match process_envelope(&state, envelope) {
        Ok(msg) => (
            StatusCode::OK,
            Json(HookResponse {
                status: "ok",
                message: msg,
            }),
        ),
        Err(e) => {
            tracing::error!("Hook ingestion error (returning 200 anyway): {e}");
            (
                StatusCode::OK,
                Json(HookResponse {
                    status: "ok",
                    message: Some(format!("error logged: {e}")),
                }),
            )
        }
    }
}

async fn handle_hook_events_batch(
    State(state): State<AppState>,
    Json(envelopes): Json<Vec<HookEventEnvelope>>,
) -> (StatusCode, Json<HookResponse>) {
    let count = envelopes.len();
    let mut errors = 0u32;
    for envelope in envelopes {
        if let Err(e) = process_envelope(&state, envelope) {
            tracing::error!("Hook batch ingestion error: {e}");
            errors += 1;
        }
    }
    (
        StatusCode::OK,
        Json(HookResponse {
            status: "ok",
            message: Some(format!("processed {count} events, {errors} errors")),
        }),
    )
}

// ── Public API for buffer drain ─────────────────────────────

/// Process a single hook event. Called from buffer drain on server startup.
pub async fn process_hook_event(state: &AppState, envelope: HookEventEnvelope) {
    if let Err(e) = process_envelope(state, envelope) {
        tracing::error!("Hook event processing error during drain: {e}");
    }
}

// ── Core processing logic ──────────────────────────────────

fn process_envelope(state: &AppState, envelope: HookEventEnvelope) -> anyhow::Result<Option<String>> {
    // Dedup check
    if state.hooks.is_duplicate(&envelope) {
        tracing::debug!("Duplicate hook event skipped");
        return Ok(Some("duplicate".to_string()));
    }

    let payload: ClaudeCodeHookPayload = serde_json::from_value(envelope.payload.clone())
        .map_err(|e| anyhow::anyhow!("Failed to parse hook payload: {e}"))?;

    // Normalize event_type to lowercase for case-insensitive matching.
    // Claude Code CLI uses PascalCase (PreToolUse) but Cursor extension
    // uses camelCase (preToolUse). Also handle legacy names (beforeSubmitPrompt).
    let event_type_lower = envelope.event_type.to_lowercase();

    let source = &envelope.source;

    match event_type_lower.as_str() {
        "sessionstart" => handle_session_start(state, &payload, source),
        "pretooluse" => handle_pre_tool_use(state, &payload, source),
        "posttooluse" => handle_post_tool_use(state, &payload, StepStatus::Success, source),
        "posttoolusefailure" => handle_post_tool_use(state, &payload, StepStatus::Error, source),
        "userpromptsubmit" | "beforesubmitprompt" | "beforepromptsubmit" => handle_user_prompt(state, &payload, &envelope.payload, source),
        "sessionend" | "stop" => handle_session_end(state, &payload),
        _ => handle_generic_event(state, &payload, &envelope.event_type, &envelope.payload, source),
    }
}

/// Ensure a session exists in both the store and in-memory state.
/// If the session doesn't exist yet (e.g. a non-SessionStart event arrived first),
/// create it with partial=true metadata.
fn ensure_session(state: &AppState, claude_session_id: &str, cwd: Option<&str>, transcript_path: Option<&str>, hook_source: Option<&str>, partial: bool) -> anyhow::Result<()> {
    // Fast path: session already exists in memory
    if state.hooks.sessions.contains_key(claude_session_id) {
        // Backfill transcript_path and hook_source if missing.
        // Only acquire the store lock when there's something that could be backfilled.
        if (transcript_path.is_some() || hook_source.is_some())
            && let Some(sess_state) = state.hooks.sessions.get(claude_session_id)
            && let Ok(store) = state.store.lock()
            && let Ok(Some(session)) = store.get_session(&sess_state.session_id)
        {
            let need_tp = transcript_path.is_some() && session.metadata.get("transcript_path").is_none();
            let need_src = hook_source.is_some() && session.metadata.get("hook_source").is_none();
            if need_tp || need_src {
                let mut meta = session.metadata.clone();
                if let Some(tp) = transcript_path {
                    meta["transcript_path"] = serde_json::json!(tp);
                }
                if let Some(src) = hook_source {
                    meta["hook_source"] = serde_json::json!(src);
                }
                let _ = store.update_session_metadata(&sess_state.session_id, &meta);
                tracing::info!("Backfilled metadata for session {}", &claude_session_id[..8.min(claude_session_id.len())]);
            }
        }
        return Ok(());
    }

    tracing::debug!("Hook session {} not found, creating...", &claude_session_id[..8.min(claude_session_id.len())]);

    // Slow path: need to create session.
    // Use a dedicated per-process mutex to serialize session creation and prevent
    // concurrent requests from both creating sessions for the same claude_session_id.
    use std::sync::OnceLock;
    static SESSION_CREATE_LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
    let _guard = SESSION_CREATE_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .map_err(|e| anyhow::anyhow!("Session creation lock poisoned: {e}"))?;

    // Double-check after acquiring lock (another thread may have created it)
    if state.hooks.sessions.contains_key(claude_session_id) {
        tracing::debug!("Hook session {} created by another thread", &claude_session_id[..8.min(claude_session_id.len())]);
        return Ok(());
    }

    tracing::info!("Creating hook session for {} (partial={})", &claude_session_id[..8.min(claude_session_id.len())], partial);

    let display_name = if let Some(src) = hook_source.filter(|s| *s != "claude-code") {
        // Find hook_source as a path segment anywhere in cwd, use everything after it
        let suffix = cwd.and_then(|p| {
            let parts: Vec<&str> = p.split('/').collect();
            parts.iter().position(|seg| *seg == src).and_then(|i| {
                let rest = parts[i + 1..].join("/");
                if rest.is_empty() { None } else { Some(rest) }
            })
        });
        match suffix {
            Some(s) => format!("{}/{}", src, s),
            None => src.to_string(),
        }
    } else {
        cwd.and_then(|p| std::path::Path::new(p).file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("session")
            .to_string()
    };
    let short_id = &claude_session_id[..std::cmp::min(8, claude_session_id.len())];
    let session_name = format!("{} ({})", display_name, short_id);

    let mut session = Session::new(&session_name);
    session.source = SessionSource::Hooks;
    let mut meta = serde_json::json!({"claude_session_id": claude_session_id});
    if partial {
        meta["partial"] = serde_json::json!(true);
    }
    if let Some(tp) = transcript_path {
        meta["transcript_path"] = serde_json::json!(tp);
    }
    if let Some(src) = hook_source {
        meta["hook_source"] = serde_json::json!(src);
    }
    session.metadata = meta;

    let timeline = Timeline::new_root(&session.id);
    // No root span — SubagentStart/Stop hooks don't fire, so the span tree
    // adds no value and triggers a less detailed nested UI. Without spans,
    // the flat step timeline is used, which shows tool names, tokens, and previews.

    let rewind_session_id = session.id.clone();
    let timeline_id = timeline.id.clone();

    {
        let store = state.store.lock().map_err(|e| anyhow::anyhow!("Lock: {e}"))?;
        store.create_session(&session)?;
        store.create_timeline(&timeline)?;
    }

    // Insert into DashMap AFTER successful store operations
    state.hooks.sessions.insert(
        claude_session_id.to_string(),
        HookSessionState {
            session_id: rewind_session_id,
            timeline_id,
            root_span_id: String::new(),
            step_counter: AtomicU32::new(0),
            pending_steps: Mutex::new(HashMap::new()),
        },
    );

    Ok(())
}

fn handle_session_start(state: &AppState, payload: &ClaudeCodeHookPayload, source: &str) -> anyhow::Result<Option<String>> {
    ensure_session(state, &payload.session_id, payload.cwd.as_deref(), payload.transcript_path.as_deref(), Some(source), false)?;

    let mut revived = false;
    if let Some(sess_state) = state.hooks.sessions.get(&payload.session_id) {
        let revive_event = {
            let store = state.store.lock().map_err(|e| anyhow::anyhow!("Lock: {e}"))?;
            if let Some(session) = store.get_session(&sess_state.session_id)? {
                // Revive session: Claude Code fires SessionEnd between each user turn,
                // so a new SessionStart means the session is active again.
                let event = if session.status == rewind_store::SessionStatus::Completed {
                    store.update_session_status(&sess_state.session_id, rewind_store::SessionStatus::Recording)?;
                    revived = true;
                    Some(crate::StoreEvent::SessionUpdated {
                        session_id: sess_state.session_id.clone(),
                        status: "recording".to_string(),
                        total_steps: session.total_steps,
                        total_tokens: session.total_tokens,
                    })
                } else {
                    None
                };

                let needs_update = session.metadata.get("partial").is_some()
                    || (payload.transcript_path.is_some() && session.metadata.get("transcript_path").is_none());
                if needs_update {
                    let mut meta = session.metadata.clone();
                    if let Some(m) = meta.as_object_mut() { m.remove("partial"); }
                    if let Some(tp) = payload.transcript_path.as_deref() {
                        meta["transcript_path"] = serde_json::json!(tp);
                    }
                    store.update_session_metadata(&sess_state.session_id, &meta)?;
                }

                event
            } else {
                None
            }
        };

        if let Some(evt) = revive_event {
            let _ = state.event_tx.send(evt);
        }
    }

    Ok(Some(if revived { "session_revived" } else { "session_created" }.to_string()))
}

fn handle_pre_tool_use(state: &AppState, payload: &ClaudeCodeHookPayload, source: &str) -> anyhow::Result<Option<String>> {
    ensure_session(state, &payload.session_id, payload.cwd.as_deref(), payload.transcript_path.as_deref(), Some(source), true)?;

    let sess_state = state.hooks.sessions.get(&payload.session_id)
        .ok_or_else(|| anyhow::anyhow!("Session state missing after ensure"))?;

    let step_num = sess_state.step_counter.fetch_add(1, Ordering::Relaxed) + 1;

    let mut step = Step::new_llm_call(&sess_state.timeline_id, &sess_state.session_id, step_num, "");
    step.id = Uuid::new_v4().to_string();
    step.step_type = StepType::ToolCall;
    step.status = StepStatus::Pending;
    step.tool_name = payload.tool_name.clone();
    if !sess_state.root_span_id.is_empty() {
        step.span_id = Some(sess_state.root_span_id.clone());
    }
    step.created_at = Utc::now();

    // Store tool_input as request blob
    if let Some(ref tool_input) = payload.tool_input {
        let store = state.store.lock().map_err(|e| anyhow::anyhow!("Lock: {e}"))?;
        let input_bytes = serde_json::to_vec(tool_input)?;
        step.request_blob = store.blobs.put(&input_bytes)?;
    }

    let step_id = step.id.clone();

    {
        let store = state.store.lock().map_err(|e| anyhow::anyhow!("Lock: {e}"))?;
        store.create_step(&step)?;
        store.update_session_stats(&sess_state.session_id, step_num, 0)?;
    }

    // Store pending step for Post matching
    if let Some(ref tool_use_id) = payload.tool_use_id {
        let mut pending = sess_state.pending_steps.lock().unwrap();
        pending.insert(tool_use_id.clone(), (step_id, Instant::now()));
    }

    // Emit event for WebSocket live updates
    let _ = state.event_tx.send(StoreEvent::StepCreated {
        session_id: sess_state.session_id.clone(),
        step: Box::new(step),
    });

    Ok(None)
}

fn handle_post_tool_use(
    state: &AppState,
    payload: &ClaudeCodeHookPayload,
    status: StepStatus,
    source: &str,
) -> anyhow::Result<Option<String>> {
    ensure_session(state, &payload.session_id, payload.cwd.as_deref(), payload.transcript_path.as_deref(), Some(source), true)?;

    let sess_state = state.hooks.sessions.get(&payload.session_id)
        .ok_or_else(|| anyhow::anyhow!("Session state missing after ensure"))?;

    // Try to find the pending step by tool_use_id
    let pending_info = payload.tool_use_id.as_ref().and_then(|tool_use_id| {
        let mut pending = sess_state.pending_steps.lock().unwrap();
        pending.remove(tool_use_id)
    });

    if let Some((step_id, created_at)) = pending_info {
        // Update the existing step
        let duration_ms = created_at.elapsed().as_millis() as u64;
        let mut response_blob = String::new();

        if let Some(ref tool_response) = payload.tool_response {
            let store = state.store.lock().map_err(|e| anyhow::anyhow!("Lock: {e}"))?;
            let resp_bytes = serde_json::to_vec(tool_response)?;
            response_blob = store.blobs.put(&resp_bytes)?;
        }

        let error_msg = if status == StepStatus::Error {
            payload.tool_response
                .as_ref()
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| Some("Tool call failed".to_string()))
        } else {
            None
        };

        {
            let store = state.store.lock().map_err(|e| anyhow::anyhow!("Lock: {e}"))?;
            store.update_step_completion(
                &step_id,
                status.clone(),
                &response_blob,
                duration_ms,
                error_msg.as_deref(),
            )?;

            // Emit updated step for WebSocket
            if let Some(updated_step) = store.get_step(&step_id)? {
                let _ = state.event_tx.send(StoreEvent::StepCreated {
                    session_id: sess_state.session_id.clone(),
                    step: Box::new(updated_step),
                });
            }
        }
    } else {
        // No matching PreToolUse found — create a standalone step
        let step_num = sess_state.step_counter.fetch_add(1, Ordering::Relaxed) + 1;
        let mut step = Step::new_llm_call(&sess_state.timeline_id, &sess_state.session_id, step_num, "");
        step.id = Uuid::new_v4().to_string();
        step.step_type = StepType::ToolCall;
        step.status = status;
        step.tool_name = payload.tool_name.clone();
        if !sess_state.root_span_id.is_empty() {
        step.span_id = Some(sess_state.root_span_id.clone());
    }
        step.created_at = Utc::now();

        if let Some(ref tool_input) = payload.tool_input {
            let store = state.store.lock().map_err(|e| anyhow::anyhow!("Lock: {e}"))?;
            let input_bytes = serde_json::to_vec(tool_input)?;
            step.request_blob = store.blobs.put(&input_bytes)?;
        }

        if let Some(ref tool_response) = payload.tool_response {
            let store = state.store.lock().map_err(|e| anyhow::anyhow!("Lock: {e}"))?;
            let resp_bytes = serde_json::to_vec(tool_response)?;
            step.response_blob = store.blobs.put(&resp_bytes)?;
        }

        {
            let store = state.store.lock().map_err(|e| anyhow::anyhow!("Lock: {e}"))?;
            store.create_step(&step)?;
            store.update_session_stats(&sess_state.session_id, step_num, 0)?;
        }

        let _ = state.event_tx.send(StoreEvent::StepCreated {
            session_id: sess_state.session_id.clone(),
            step: Box::new(step),
        });
    }

    Ok(None)
}

fn handle_user_prompt(state: &AppState, payload: &ClaudeCodeHookPayload, raw_payload: &serde_json::Value, source: &str) -> anyhow::Result<Option<String>> {
    ensure_session(state, &payload.session_id, payload.cwd.as_deref(), payload.transcript_path.as_deref(), Some(source), true)?;

    let sess_state = state.hooks.sessions.get(&payload.session_id)
        .ok_or_else(|| anyhow::anyhow!("Session state missing after ensure"))?;

    let step_num = sess_state.step_counter.fetch_add(1, Ordering::Relaxed) + 1;

    let mut step = Step::new_llm_call(&sess_state.timeline_id, &sess_state.session_id, step_num, "");
    step.id = Uuid::new_v4().to_string();
    step.step_type = StepType::UserPrompt;
    step.status = StepStatus::Success;
    if !sess_state.root_span_id.is_empty() {
        step.span_id = Some(sess_state.root_span_id.clone());
    }
    step.created_at = Utc::now();

    // Store the full raw payload — user prompts may have the text in various fields
    // (content, prompt, message) depending on the Claude Code version/extension
    {
        let store = state.store.lock().map_err(|e| anyhow::anyhow!("Lock: {e}"))?;
        let payload_bytes = serde_json::to_vec(raw_payload)?;
        step.request_blob = store.blobs.put(&payload_bytes)?;
    }

    {
        let store = state.store.lock().map_err(|e| anyhow::anyhow!("Lock: {e}"))?;
        store.create_step(&step)?;
        store.update_session_stats(&sess_state.session_id, step_num, 0)?;
    }

    let _ = state.event_tx.send(StoreEvent::StepCreated {
        session_id: sess_state.session_id.clone(),
        step: Box::new(step),
    });

    Ok(None)
}

fn handle_session_end(state: &AppState, payload: &ClaudeCodeHookPayload) -> anyhow::Result<Option<String>> {
    if !state.hooks.sessions.contains_key(&payload.session_id) {
        // Session never started — nothing to close
        return Ok(Some("unknown_session".to_string()));
    }

    let sess_state = state.hooks.sessions.get(&payload.session_id)
        .ok_or_else(|| anyhow::anyhow!("Session state missing"))?;

    let now = Utc::now();

    {
        let store = state.store.lock().map_err(|e| anyhow::anyhow!("Lock: {e}"))?;
        store.update_session_status(&sess_state.session_id, SessionStatus::Completed)?;
        if !sess_state.root_span_id.is_empty() {
            store.update_span_status(&sess_state.root_span_id, "completed", Some(now), 0, None)?;
        }
    }

    // Emit session update
    let _ = state.event_tx.send(StoreEvent::SessionUpdated {
        session_id: sess_state.session_id.clone(),
        status: "completed".to_string(),
        total_steps: sess_state.step_counter.load(Ordering::Relaxed),
        total_tokens: 0,
    });

    // Clean up in-memory state
    drop(sess_state);
    // NOTE: Do NOT remove from DashMap here. Claude Code fires Stop/SessionEnd
    // between each user message turn, not just when the window closes. Removing
    // would cause the next message's hooks to create a duplicate session.
    // The session stays in memory for the lifetime of the server process.

    Ok(Some("session_completed".to_string()))
}

fn handle_generic_event(
    state: &AppState,
    payload: &ClaudeCodeHookPayload,
    event_type: &str,
    raw_payload: &serde_json::Value,
    source: &str,
) -> anyhow::Result<Option<String>> {
    ensure_session(state, &payload.session_id, payload.cwd.as_deref(), payload.transcript_path.as_deref(), Some(source), true)?;

    let sess_state = state.hooks.sessions.get(&payload.session_id)
        .ok_or_else(|| anyhow::anyhow!("Session state missing after ensure"))?;

    let step_num = sess_state.step_counter.fetch_add(1, Ordering::Relaxed) + 1;

    let mut step = Step::new_llm_call(&sess_state.timeline_id, &sess_state.session_id, step_num, "");
    step.id = Uuid::new_v4().to_string();
    step.step_type = StepType::HookEvent;
    step.status = StepStatus::Success;
    step.tool_name = Some(event_type.to_string());
    if !sess_state.root_span_id.is_empty() {
        step.span_id = Some(sess_state.root_span_id.clone());
    }
    step.created_at = Utc::now();

    // Store the full raw payload as request blob
    {
        let store = state.store.lock().map_err(|e| anyhow::anyhow!("Lock: {e}"))?;
        let payload_bytes = serde_json::to_vec(raw_payload)?;
        step.request_blob = store.blobs.put(&payload_bytes)?;
        store.create_step(&step)?;
        store.update_session_stats(&sess_state.session_id, step_num, 0)?;
    }

    let _ = state.event_tx.send(StoreEvent::StepCreated {
        session_id: sess_state.session_id.clone(),
        step: Box::new(step),
    });

    Ok(None)
}
