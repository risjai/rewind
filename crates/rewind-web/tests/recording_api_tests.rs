use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use rewind_store::*;
use rewind_web::{AppState, HookIngestionState, StoreEvent};
use serde_json::json;
use std::sync::{Arc, Mutex, OnceLock};
use tempfile::TempDir;
use tower::ServiceExt;

/// Serializes any test that mutates the global `REWIND_ALLOW_MAIN_EDITS`
/// env var. Without this, two tests that both flip the var run in
/// parallel and one ends up reading the other's state — leading to
/// intermittent CI flakes (review #164: the parallel race surfaced
/// after switching `test_patch_promote_main_protection_follows_target`
/// from `.last()` to a fixed step_number, which made the test do a
/// real PATCH against main with the env bypass and so collide with
/// `test_patch_step_edit_on_main_blocked_and_env_bypass`).
fn main_edits_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap_or_else(|e| e.into_inner())
}

fn setup() -> (Router, Arc<Mutex<Store>>, TempDir) {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let store = Arc::new(Mutex::new(store));
    let (event_tx, _) = tokio::sync::broadcast::channel::<StoreEvent>(64);
    let state = AppState {
        store: store.clone(),
        event_tx,
        hooks: Arc::new(HookIngestionState::new()),
        otel_config: None,
        auth_token: None, crypto: None, dispatcher: None, base_url: "http://127.0.0.1:4800".to_string(),
    };
    let app = Router::new().nest("/api", rewind_web::api_routes(state));
    (app, store, tmp)
}

async fn post_json(app: &Router, path: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let resp = app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or(json!({"raw": String::from_utf8_lossy(&body).to_string()}));
    (status, json)
}

async fn get_json(app: &Router, path: &str) -> (StatusCode, serde_json::Value) {
    let resp = app.clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or(json!(null));
    (status, json)
}

async fn delete_json(app: &Router, path: &str) -> (StatusCode, serde_json::Value) {
    let resp = app.clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or(json!(null));
    (status, json)
}

/// Like `delete_json`, but returns the raw body as a UTF-8 string. Handlers
/// that map errors via `(StatusCode, String)` send plain text; `delete_json`
/// can't parse that as JSON, so use this when you need to inspect the
/// error message body.
async fn delete_raw(app: &Router, path: &str) -> (StatusCode, String) {
    let resp = app.clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

// ── Session Lifecycle ──────────────────────────────────────

#[tokio::test]
async fn test_start_session() {
    let (app, store, _tmp) = setup();

    let (status, body) = post_json(&app, "/api/sessions/start", json!({
        "name": "my-agent-session",
        "thread_id": "thread-123"
    })).await;

    assert_eq!(status, StatusCode::CREATED);
    assert!(body["session_id"].is_string());
    assert!(body["root_timeline_id"].is_string());

    let session_id = body["session_id"].as_str().unwrap();
    let s = store.lock().unwrap();
    let session = s.get_session(session_id).unwrap().unwrap();
    assert_eq!(session.name, "my-agent-session");
    assert_eq!(session.source, SessionSource::Api);
    assert_eq!(session.thread_id.as_deref(), Some("thread-123"));
}

#[tokio::test]
async fn test_start_session_idempotent_returns_existing() {
    // Multi-replica runners need /sessions/start to be idempotent on
    // a stable client_session_key so two POSTs from different replicas
    // for the same conversation_id resolve to ONE rewind session.
    let (app, store, _tmp) = setup();

    let (status1, body1) = post_json(&app, "/api/sessions/start", json!({
        "name": "ray-agent-conv-1",
        "client_session_key": "conv-1",
        "metadata": {"question": "What clusters are running?"}
    })).await;
    assert_eq!(status1, StatusCode::CREATED, "first call creates");
    let sid1 = body1["session_id"].as_str().unwrap();
    let tid1 = body1["root_timeline_id"].as_str().unwrap();

    // Second call with the same key — different name/metadata to prove
    // the server returns the EXISTING row rather than a freshly-built one.
    let (status2, body2) = post_json(&app, "/api/sessions/start", json!({
        "name": "would-be-different",
        "client_session_key": "conv-1",
        "metadata": {"question": "ignored"}
    })).await;
    assert_eq!(status2, StatusCode::OK, "duplicate returns 200, not 201");
    assert_eq!(body2["session_id"].as_str().unwrap(), sid1, "same session id");
    assert_eq!(body2["root_timeline_id"].as_str().unwrap(), tid1, "same root timeline id");

    // No duplicate row was inserted.
    let s = store.lock().unwrap();
    let all = s.list_sessions().unwrap();
    let matching: Vec<_> = all
        .iter()
        .filter(|x| x.client_session_key.as_deref() == Some("conv-1"))
        .collect();
    assert_eq!(matching.len(), 1, "exactly one row keyed to conv-1");
    assert_eq!(matching[0].name, "ray-agent-conv-1", "first writer wins on name");
    assert_eq!(
        matching[0].metadata.get("question").and_then(|v| v.as_str()),
        Some("What clusters are running?"),
        "first writer wins on metadata too",
    );
}

#[tokio::test]
async fn test_start_session_empty_key_treated_as_none() {
    // Review #155 R1: empty / whitespace-only client_session_key
    // must NOT collapse callers into one shared session. Two POSTs
    // with empty keys should produce two distinct sessions, same
    // backward-compat behavior as omitting the field entirely.
    let (app, store, _tmp) = setup();

    let (s1, b1) = post_json(&app, "/api/sessions/start", json!({
        "name": "first",
        "client_session_key": ""
    })).await;
    let (s2, b2) = post_json(&app, "/api/sessions/start", json!({
        "name": "second",
        "client_session_key": "   "
    })).await;

    assert_eq!(s1, StatusCode::CREATED, "empty key should create, not dedup");
    assert_eq!(s2, StatusCode::CREATED, "whitespace key should create too");
    assert_ne!(
        b1["session_id"], b2["session_id"],
        "empty/whitespace keys must NOT route to the same session",
    );

    // The persisted client_session_key column should be NULL (None)
    // for both — no UNIQUE-index claim happened.
    let s = store.lock().unwrap();
    let all = s.list_sessions().unwrap();
    let with_key = all
        .iter()
        .filter(|x| x.client_session_key.is_some())
        .count();
    assert_eq!(with_key, 0, "no session should hold an empty/ws key in DB");
}

#[tokio::test]
async fn test_start_session_distinct_keys_create_distinct_sessions() {
    let (app, _store, _tmp) = setup();

    let (s1, b1) = post_json(&app, "/api/sessions/start", json!({
        "name": "a",
        "client_session_key": "key-A"
    })).await;
    let (s2, b2) = post_json(&app, "/api/sessions/start", json!({
        "name": "b",
        "client_session_key": "key-B"
    })).await;

    assert_eq!(s1, StatusCode::CREATED);
    assert_eq!(s2, StatusCode::CREATED);
    assert_ne!(b1["session_id"], b2["session_id"]);
}

#[tokio::test]
async fn test_start_session_without_key_creates_new_each_time() {
    // Backward compat: callers that don't pass client_session_key get
    // the original create-every-time behavior. Two POSTs with the
    // same name but no key produce two distinct sessions.
    let (app, _store, _tmp) = setup();

    let (s1, b1) = post_json(&app, "/api/sessions/start", json!({
        "name": "no-key-session"
    })).await;
    let (s2, b2) = post_json(&app, "/api/sessions/start", json!({
        "name": "no-key-session"
    })).await;

    assert_eq!(s1, StatusCode::CREATED);
    assert_eq!(s2, StatusCode::CREATED);
    assert_ne!(
        b1["session_id"], b2["session_id"],
        "no key supplied -> server creates fresh session each call",
    );
}

#[tokio::test]
async fn test_end_session() {
    let (app, _store, _tmp) = setup();

    let (_, start_body) = post_json(&app, "/api/sessions/start", json!({
        "name": "test"
    })).await;
    let sid = start_body["session_id"].as_str().unwrap();

    let (status, body) = post_json(&app, &format!("/api/sessions/{sid}/end"), json!({
        "status": "completed"
    })).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["session_id"].as_str().unwrap(), sid);
}

#[tokio::test]
async fn test_end_session_not_found() {
    let (app, _store, _tmp) = setup();

    let (status, _body) = post_json(&app, "/api/sessions/nonexistent/end", json!({
        "status": "completed"
    })).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ── LLM Call Recording ─────────────────────────────────────

#[tokio::test]
async fn test_record_llm_call() {
    let (app, store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    let (status, body) = post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "request_body": {"messages": [{"role": "user", "content": "hello"}]},
        "response_body": {"content": "Hi there!"},
        "model": "gpt-4o-mini",
        "duration_ms": 500,
        "tokens_in": 10,
        "tokens_out": 5
    })).await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["step_number"].as_u64().unwrap(), 1);

    let s = store.lock().unwrap();
    let session = s.get_session(sid).unwrap().unwrap();
    assert_eq!(session.total_steps, 1);
    assert_eq!(session.total_tokens, 15);
}

#[tokio::test]
async fn test_record_multiple_steps_sequential_numbering() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    let (_, b1) = post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "request_body": {},
        "response_body": {"content": "step 1"},
        "model": "gpt-4o",
        "duration_ms": 100
    })).await;
    assert_eq!(b1["step_number"].as_u64().unwrap(), 1);

    let (_, b2) = post_json(&app, &format!("/api/sessions/{sid}/tool-calls"), json!({
        "tool_name": "get_pods",
        "request_body": {"cluster": "mulesoft"},
        "response_body": {"pods": []},
        "duration_ms": 200
    })).await;
    assert_eq!(b2["step_number"].as_u64().unwrap(), 2);

    let (_, b3) = post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "request_body": {},
        "response_body": {"content": "step 3"},
        "model": "gpt-4o",
        "duration_ms": 150
    })).await;
    assert_eq!(b3["step_number"].as_u64().unwrap(), 3);
}

// ── Tool Call Recording ────────────────────────────────────

#[tokio::test]
async fn test_record_tool_call() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    let (status, body) = post_json(&app, &format!("/api/sessions/{sid}/tool-calls"), json!({
        "tool_name": "get_cluster_pods",
        "request_body": {"cluster_name": "mulesoft"},
        "response_body": {"pods": [{"name": "head-0"}]},
        "duration_ms": 234
    })).await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["step_number"].as_u64().unwrap(), 1);
}

#[tokio::test]
async fn test_record_tool_call_with_error() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    let (status, _body) = post_json(&app, &format!("/api/sessions/{sid}/tool-calls"), json!({
        "tool_name": "query_splunk",
        "request_body": {"query": "index=ray"},
        "response_body": {},
        "duration_ms": 5000,
        "error": "Splunk timeout after 5s"
    })).await;

    assert_eq!(status, StatusCode::CREATED);
}

// ── Idempotency ────────────────────────────────────────────

#[tokio::test]
async fn test_idempotent_record_with_client_step_id() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    let step_id = uuid::Uuid::new_v4().to_string();

    let (s1, b1) = post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "client_step_id": step_id,
        "request_body": {},
        "response_body": {"content": "hello"},
        "model": "gpt-4o",
        "duration_ms": 100
    })).await;
    assert_eq!(s1, StatusCode::CREATED);
    assert_eq!(b1["step_number"].as_u64().unwrap(), 1);

    let (s2, b2) = post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "client_step_id": step_id,
        "request_body": {},
        "response_body": {"content": "hello"},
        "model": "gpt-4o",
        "duration_ms": 100
    })).await;
    assert_eq!(s2, StatusCode::OK, "duplicate should return 200, not 201");
    assert!(b2["step_number"].is_number(), "duplicate should return JSON with step_number, got: {b2}");
}

// ── Fork ───────────────────────────────────────────────────

#[tokio::test]
async fn test_fork_session() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    for i in 0..3 {
        post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
            "request_body": {},
            "response_body": {"content": format!("step {}", i+1)},
            "model": "gpt-4o",
            "duration_ms": 100
        })).await;
    }

    let (status, body) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 2,
        "label": "try-different-prompt"
    })).await;

    assert_eq!(status, StatusCode::CREATED);
    assert!(body["fork_timeline_id"].is_string());
}

#[tokio::test]
async fn test_fork_invalid_step() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "request_body": {},
        "response_body": {},
        "model": "gpt-4o",
        "duration_ms": 100
    })).await;

    let (status, _) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 99,
        "label": "bad-fork"
    })).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_fork_of_fork() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    for _ in 0..3 {
        post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
            "request_body": {},
            "response_body": {"content": "step"},
            "model": "gpt-4o",
            "duration_ms": 100
        })).await;
    }

    let (_, fork1) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 2,
        "label": "fork-1"
    })).await;
    let fork1_id = fork1["fork_timeline_id"].as_str().unwrap();

    let (status, fork2) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 1,
        "label": "fork-of-fork",
        "timeline_id": fork1_id
    })).await;

    assert_eq!(status, StatusCode::CREATED);
    assert!(fork2["fork_timeline_id"].is_string());
    assert_ne!(fork2["fork_timeline_id"].as_str().unwrap(), fork1_id);
}

// ── Delete Timeline (#143) ──────────────────────────────────

#[tokio::test]
async fn test_delete_fork_happy_path() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    for _ in 0..3 {
        post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
            "request_body": {},
            "response_body": {},
            "model": "gpt-4o",
            "duration_ms": 100
        })).await;
    }

    let (_, fork) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 2,
        "label": "throwaway"
    })).await;
    let fork_id = fork["fork_timeline_id"].as_str().unwrap();

    let (status, body) = delete_json(&app, &format!("/api/sessions/{sid}/timelines/{fork_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["deleted"], true);

    // Verify the fork is gone from the timelines listing.
    let (_, timelines) = get_json(&app, &format!("/api/sessions/{sid}/timelines")).await;
    let remaining: Vec<&str> = timelines.as_array().unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert!(!remaining.contains(&fork_id));
}

#[tokio::test]
async fn test_delete_root_timeline_returns_409() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();
    let root_id = start["root_timeline_id"].as_str().unwrap();

    let (status, msg) = delete_raw(&app, &format!("/api/sessions/{sid}/timelines/{root_id}")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(msg.contains("root"), "got: {msg}");
}

#[tokio::test]
async fn test_delete_fork_with_children_returns_409() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    for _ in 0..3 {
        post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
            "request_body": {}, "response_body": {}, "model": "gpt-4o", "duration_ms": 100,
        })).await;
    }

    let (_, parent) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 2, "label": "parent-fork"
    })).await;
    let parent_id = parent["fork_timeline_id"].as_str().unwrap();

    // Seed the parent fork with a step so the child fork is valid.
    post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "request_body": {}, "response_body": {}, "model": "gpt-4o", "duration_ms": 100,
    })).await;
    post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 1, "label": "child-fork", "timeline_id": parent_id
    })).await;

    let (status, msg) = delete_raw(&app, &format!("/api/sessions/{sid}/timelines/{parent_id}")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(msg.contains("child fork"), "got: {msg}");
}

#[tokio::test]
async fn test_delete_unknown_timeline_returns_404() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    let (status, _) = delete_json(&app, &format!("/api/sessions/{sid}/timelines/does-not-exist")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_delete_rejects_session_prefix_and_latest() {
    // Destructive endpoint must require the full session ID — prefix-match
    // and the "latest" shortcut are accepted by other endpoints but are
    // footguns here. See santa review Important-5 on PR #146.
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();
    let prefix = &sid[..8];

    let (s_prefix, _) = delete_raw(&app, &format!("/api/sessions/{prefix}/timelines/some-tid")).await;
    assert_eq!(s_prefix, StatusCode::BAD_REQUEST);

    let (s_latest, _) = delete_raw(&app, "/api/sessions/latest/timelines/some-tid").await;
    assert_eq!(s_latest, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_delete_fork_with_active_replay_context_returns_409() {
    // santa review Important-6 on PR #146.
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();
    for _ in 0..3 {
        post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
            "request_body": {}, "response_body": {}, "model": "gpt-4o", "duration_ms": 100,
        })).await;
    }

    let (_, fork) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 2, "label": "in-use"
    })).await;
    let fork_id = fork["fork_timeline_id"].as_str().unwrap();

    post_json(&app, "/api/replay-contexts", json!({
        "session_id": sid,
        "from_step": 2,
        "fork_timeline_id": fork_id,
    })).await;

    let (status, msg) = delete_raw(&app, &format!("/api/sessions/{sid}/timelines/{fork_id}")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(msg.contains("active replay context"), "got: {msg}");
}

// ── Replay Context ─────────────────────────────────────────

#[tokio::test]
async fn test_create_and_delete_replay_context() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();
    let tid = start["root_timeline_id"].as_str().unwrap();

    for _ in 0..3 {
        post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
            "request_body": {},
            "response_body": {},
            "model": "gpt-4o",
            "duration_ms": 100
        })).await;
    }

    let (status, body) = post_json(&app, "/api/replay-contexts", json!({
        "session_id": sid,
        "from_step": 1,
        "fork_timeline_id": tid
    })).await;

    assert_eq!(status, StatusCode::CREATED);
    assert!(body["replay_context_id"].is_string());
    assert_eq!(body["parent_steps_count"].as_u64().unwrap(), 3);
    assert_eq!(body["fork_at_step"].as_u64().unwrap(), 1);

    let ctx_id = body["replay_context_id"].as_str().unwrap();
    let (del_status, del_body) = delete_json(&app, &format!("/api/replay-contexts/{ctx_id}")).await;
    assert_eq!(del_status, StatusCode::OK);
    assert_eq!(del_body["released"].as_bool().unwrap(), true);
}

// ── Replay Lookup ──────────────────────────────────────────

#[tokio::test]
async fn test_replay_lookup_hit_and_miss() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();
    let tid = start["root_timeline_id"].as_str().unwrap();

    for i in 0..3 {
        post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
            "request_body": {"step": i},
            "response_body": {"content": format!("response {}", i+1)},
            "model": "gpt-4o",
            "duration_ms": 100
        })).await;
    }

    let (_, ctx) = post_json(&app, "/api/replay-contexts", json!({
        "session_id": sid,
        "from_step": 0,
        "fork_timeline_id": tid
    })).await;
    let ctx_id = ctx["replay_context_id"].as_str().unwrap();

    let (_, hit1) = post_json(&app, &format!("/api/sessions/{sid}/llm-calls/replay-lookup"), json!({
        "replay_context_id": ctx_id
    })).await;
    assert_eq!(hit1["hit"].as_bool().unwrap(), true);
    assert!(hit1["response_body"].is_object());
    assert_eq!(hit1["model"].as_str().unwrap(), "gpt-4o");

    let (_, hit2) = post_json(&app, &format!("/api/sessions/{sid}/llm-calls/replay-lookup"), json!({
        "replay_context_id": ctx_id
    })).await;
    assert_eq!(hit2["hit"].as_bool().unwrap(), true);

    let (_, hit3) = post_json(&app, &format!("/api/sessions/{sid}/llm-calls/replay-lookup"), json!({
        "replay_context_id": ctx_id
    })).await;
    assert_eq!(hit3["hit"].as_bool().unwrap(), true);

    let (_, miss) = post_json(&app, &format!("/api/sessions/{sid}/llm-calls/replay-lookup"), json!({
        "replay_context_id": ctx_id
    })).await;
    assert_eq!(miss["hit"].as_bool().unwrap(), false, "step 4 doesn't exist, should miss");
}

// ── Steps with Blobs ───────────────────────────────────────

#[tokio::test]
async fn test_get_steps_with_blobs() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "request_body": {"messages": [{"role": "user", "content": "hello"}]},
        "response_body": {"content": "world"},
        "model": "gpt-4o",
        "duration_ms": 100
    })).await;

    let (status_no_blobs, body_no_blobs) = get_json(&app, &format!("/api/sessions/{sid}/steps")).await;
    assert_eq!(status_no_blobs, StatusCode::OK);
    let steps = body_no_blobs.as_array().unwrap();
    assert_eq!(steps.len(), 1);
    assert!(steps[0].get("request_body").is_none(), "without include_blobs, should not have request_body");

    let (status_with_blobs, body_with_blobs) = get_json(&app, &format!("/api/sessions/{sid}/steps?include_blobs=1")).await;
    assert_eq!(status_with_blobs, StatusCode::OK);
    let steps = body_with_blobs.as_array().unwrap();
    assert_eq!(steps.len(), 1);
    assert!(steps[0]["request_body"].is_object(), "with include_blobs=1, should have request_body");
    assert!(steps[0]["response_body"].is_object(), "with include_blobs=1, should have response_body");
}

// ── Full Recording Workflow ────────────────────────────────

#[tokio::test]
async fn test_full_react_recording_workflow() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({
        "name": "ray-agent-test",
        "metadata": {"question": "how is mulesoft?"}
    })).await;
    let sid = start["session_id"].as_str().unwrap();

    let (_, s1) = post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "request_body": {"messages": [{"role": "user", "content": "how is mulesoft?"}]},
        "response_body": {"content": "Let me check", "tool_invocations": [{"name": "get_pods"}]},
        "model": "llmgateway__OpenAIGPT4OmniMini",
        "duration_ms": 800,
        "tokens_in": 100,
        "tokens_out": 20
    })).await;
    assert_eq!(s1["step_number"].as_u64().unwrap(), 1);

    let (_, s2) = post_json(&app, &format!("/api/sessions/{sid}/tool-calls"), json!({
        "tool_name": "get_cluster_pods",
        "request_body": {"cluster": "mulesoft"},
        "response_body": {"pods": [{"name": "head-0", "status": "Running"}]},
        "duration_ms": 234
    })).await;
    assert_eq!(s2["step_number"].as_u64().unwrap(), 2);

    let (_, s3) = post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "request_body": {"messages": [
            {"role": "user", "content": "how is mulesoft?"},
            {"role": "assistant", "content": "Let me check"},
            {"role": "tool", "content": "{\"pods\": [{\"name\": \"head-0\"}]}"}
        ]},
        "response_body": {"content": "Mulesoft cluster is healthy with 1 head pod running."},
        "model": "llmgateway__OpenAIGPT4OmniMini",
        "duration_ms": 600,
        "tokens_in": 200,
        "tokens_out": 30
    })).await;
    assert_eq!(s3["step_number"].as_u64().unwrap(), 3);

    let (end_status, _) = post_json(&app, &format!("/api/sessions/{sid}/end"), json!({
        "status": "completed"
    })).await;
    assert_eq!(end_status, StatusCode::OK);

    let (_, fork) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 2,
        "label": "try-gpt4o"
    })).await;
    assert!(fork["fork_timeline_id"].is_string());

    let (_, steps) = get_json(&app, &format!("/api/sessions/{sid}/steps?include_blobs=1")).await;
    let steps_arr = steps.as_array().unwrap();
    assert_eq!(steps_arr.len(), 3);
    assert_eq!(steps_arr[0]["step_type"].as_str().unwrap(), "llm_call");
    assert_eq!(steps_arr[1]["step_type"].as_str().unwrap(), "tool_call");
    assert_eq!(steps_arr[1]["tool_name"].as_str().unwrap(), "get_cluster_pods");
    assert_eq!(steps_arr[2]["step_type"].as_str().unwrap(), "llm_call");
}

// ── Preview Fallback ───────────────────────────────────────

#[tokio::test]
async fn test_llm_gateway_format_preview() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "request_body": {},
        "response_body": {
            "generations": [{"content": "Mulesoft cluster is healthy", "tool_invocations": []}],
            "generation_safety_score": 0.99
        },
        "model": "llmgateway__OpenAIGPT4OmniMini",
        "duration_ms": 500
    })).await;

    let (_, steps) = get_json(&app, &format!("/api/sessions/{sid}/steps")).await;
    let steps_arr = steps.as_array().unwrap();
    assert_eq!(steps_arr.len(), 1);
    let preview = steps_arr[0]["response_preview"].as_str().unwrap();
    assert!(preview.contains("Mulesoft cluster is healthy"), "LLM Gateway format should produce a useful preview, got: {preview}");
}

// ── Status Guard (auto-reopen Completed, reject Failed) ───

#[tokio::test]
async fn test_auto_reopen_completed_session_for_llm() {
    let (app, store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "request_body": {}, "response_body": {"content": "step1"},
        "model": "gpt-4o", "duration_ms": 100
    })).await;

    post_json(&app, &format!("/api/sessions/{sid}/end"), json!({"status": "completed"})).await;

    // Recording on completed session should auto-reopen and succeed
    let (status, body) = post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "request_body": {}, "response_body": {"content": "step2"},
        "model": "gpt-4o", "duration_ms": 100
    })).await;

    assert_eq!(status, StatusCode::CREATED, "completed session should auto-reopen");
    assert_eq!(body["step_number"].as_u64().unwrap(), 2, "step numbering should continue");

    let s = store.lock().unwrap();
    let session = s.get_session(sid).unwrap().unwrap();
    assert_eq!(session.status, SessionStatus::Recording, "session should be back to Recording");
}

#[tokio::test]
async fn test_auto_reopen_completed_session_for_tool() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    post_json(&app, &format!("/api/sessions/{sid}/end"), json!({"status": "completed"})).await;

    let (status, _) = post_json(&app, &format!("/api/sessions/{sid}/tool-calls"), json!({
        "tool_name": "test", "request_body": {}, "response_body": {},
        "duration_ms": 100
    })).await;

    assert_eq!(status, StatusCode::CREATED, "completed session should auto-reopen for tools too");
}

#[tokio::test]
async fn test_record_on_failed_session_rejected() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    post_json(&app, &format!("/api/sessions/{sid}/end"), json!({"status": "errored"})).await;

    let (status, _) = post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "request_body": {}, "response_body": {},
        "model": "gpt-4o", "duration_ms": 100
    })).await;

    assert_eq!(status, StatusCode::CONFLICT, "recording on failed session should be rejected");
}

#[tokio::test]
async fn test_record_tool_on_failed_session_rejected() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    post_json(&app, &format!("/api/sessions/{sid}/end"), json!({"status": "failed"})).await;

    let (status, _) = post_json(&app, &format!("/api/sessions/{sid}/tool-calls"), json!({
        "tool_name": "test", "request_body": {}, "response_body": {},
        "duration_ms": 100
    })).await;

    assert_eq!(status, StatusCode::CONFLICT, "recording on failed session should be rejected");
}

// ── Idempotency (corrected) ────────────────────────────────

#[tokio::test]
async fn test_idempotent_returns_original_step_number() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    let step_id = uuid::Uuid::new_v4().to_string();

    let (s1, b1) = post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "client_step_id": step_id,
        "request_body": {},
        "response_body": {"content": "hello"},
        "model": "gpt-4o",
        "duration_ms": 100
    })).await;
    assert_eq!(s1, StatusCode::CREATED);
    assert_eq!(b1["step_number"].as_u64().unwrap(), 1);

    // Record another step to advance the counter
    post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "request_body": {},
        "response_body": {},
        "model": "gpt-4o",
        "duration_ms": 50
    })).await;

    // Retry the original -- should return step_number=1 (original), not 3
    let (s2, b2) = post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "client_step_id": step_id,
        "request_body": {},
        "response_body": {"content": "hello"},
        "model": "gpt-4o",
        "duration_ms": 100
    })).await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b2["step_number"].as_u64().unwrap(), 1,
        "idempotent retry must return the ORIGINAL step_number, not a new one");

    // Verify no counter gap: next step should be 3, not 4
    let (_, b3) = post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "request_body": {},
        "response_body": {},
        "model": "gpt-4o",
        "duration_ms": 50
    })).await;
    assert_eq!(b3["step_number"].as_u64().unwrap(), 3, "no counter gap from idempotent retry");
}

// ── Interleaved Replay ─────────────────────────────────────

#[tokio::test]
async fn test_interleaved_llm_and_tool_replay() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();
    let tid = start["root_timeline_id"].as_str().unwrap();

    // Record: LlmCall, ToolCall, LlmCall (typical ReAct pattern)
    post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "request_body": {"messages": [{"role": "user", "content": "hi"}]},
        "response_body": {"content": "Let me check", "tool_calls": ["get_pods"]},
        "model": "gpt-4o",
        "duration_ms": 500
    })).await;

    post_json(&app, &format!("/api/sessions/{sid}/tool-calls"), json!({
        "tool_name": "get_pods",
        "request_body": {"cluster": "mulesoft"},
        "response_body": {"pods": [{"name": "head-0"}]},
        "duration_ms": 200
    })).await;

    post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "request_body": {"messages": []},
        "response_body": {"content": "Cluster is healthy"},
        "model": "gpt-4o",
        "duration_ms": 400
    })).await;

    // Create replay context from step 0
    let (_, ctx) = post_json(&app, "/api/replay-contexts", json!({
        "session_id": sid,
        "from_step": 0,
        "fork_timeline_id": tid
    })).await;
    let ctx_id = ctx["replay_context_id"].as_str().unwrap();

    // Interleaved replay: llm, tool, llm -- must match in order
    let (_, r1) = post_json(&app, &format!("/api/sessions/{sid}/llm-calls/replay-lookup"), json!({
        "replay_context_id": ctx_id
    })).await;
    assert_eq!(r1["hit"].as_bool().unwrap(), true, "step 1 should be LlmCall hit");
    assert!(r1["response_body"]["content"].as_str().unwrap().contains("Let me check"));

    let (_, r2) = post_json(&app, &format!("/api/sessions/{sid}/tool-calls/replay-lookup"), json!({
        "replay_context_id": ctx_id
    })).await;
    assert_eq!(r2["hit"].as_bool().unwrap(), true, "step 2 should be ToolCall hit");

    let (_, r3) = post_json(&app, &format!("/api/sessions/{sid}/llm-calls/replay-lookup"), json!({
        "replay_context_id": ctx_id
    })).await;
    assert_eq!(r3["hit"].as_bool().unwrap(), true, "step 3 should be LlmCall hit");
    assert!(r3["response_body"]["content"].as_str().unwrap().contains("Cluster is healthy"));

    // Step 4 doesn't exist
    let (_, r4) = post_json(&app, &format!("/api/sessions/{sid}/llm-calls/replay-lookup"), json!({
        "replay_context_id": ctx_id
    })).await;
    assert_eq!(r4["hit"].as_bool().unwrap(), false, "step 4 should miss");
}

// ── Source Label ────────────────────────────────────────────

#[tokio::test]
async fn test_source_label_stored_in_metadata() {
    let (app, store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({
        "name": "ray-agent-test",
        "source": "ray-agent"
    })).await;
    let sid = start["session_id"].as_str().unwrap();

    let s = store.lock().unwrap();
    let session = s.get_session(sid).unwrap().unwrap();
    assert_eq!(session.source, rewind_store::SessionSource::Api);
    assert_eq!(session.metadata["source_label"].as_str().unwrap(), "ray-agent");
}

// ── Edge Cases ─────────────────────────────────────────────

#[tokio::test]
async fn test_record_llm_on_nonexistent_session() {
    let (app, _store, _tmp) = setup();

    let (status, _) = post_json(&app, "/api/sessions/nonexistent-id/llm-calls", json!({
        "request_body": {},
        "response_body": {},
        "model": "gpt-4o",
        "duration_ms": 100
    })).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_record_tool_on_nonexistent_session() {
    let (app, _store, _tmp) = setup();

    let (status, _) = post_json(&app, "/api/sessions/nonexistent-id/tool-calls", json!({
        "tool_name": "test",
        "request_body": {},
        "response_body": {},
        "duration_ms": 100
    })).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_record_with_explicit_timeline_id() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();
    let tid = start["root_timeline_id"].as_str().unwrap();

    let (status, body) = post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "timeline_id": tid,
        "request_body": {},
        "response_body": {"content": "explicit timeline"},
        "model": "gpt-4o",
        "duration_ms": 100
    })).await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["step_number"].as_u64().unwrap(), 1);
}

#[tokio::test]
async fn test_fork_at_step_zero_rejected() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "request_body": {},
        "response_body": {},
        "model": "gpt-4o",
        "duration_ms": 100
    })).await;

    let (status, _) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 0,
        "label": "bad"
    })).await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "fork at step 0 should be rejected");
}

#[tokio::test]
async fn test_replay_context_nonexistent_session() {
    let (app, _store, _tmp) = setup();

    let (status, _) = post_json(&app, "/api/replay-contexts", json!({
        "session_id": "nonexistent",
        "from_step": 0,
        "fork_timeline_id": "nonexistent-tl"
    })).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_replay_lookup_wrong_session() {
    let (app, _store, _tmp) = setup();

    let (_, s1) = post_json(&app, "/api/sessions/start", json!({"name": "session-1"})).await;
    let sid1 = s1["session_id"].as_str().unwrap();
    let tid1 = s1["root_timeline_id"].as_str().unwrap();

    let (_, s2) = post_json(&app, "/api/sessions/start", json!({"name": "session-2"})).await;
    let sid2 = s2["session_id"].as_str().unwrap();

    post_json(&app, &format!("/api/sessions/{sid1}/llm-calls"), json!({
        "request_body": {}, "response_body": {}, "model": "gpt-4o", "duration_ms": 100
    })).await;

    let (_, ctx) = post_json(&app, "/api/replay-contexts", json!({
        "session_id": sid1,
        "from_step": 0,
        "fork_timeline_id": tid1
    })).await;
    let ctx_id = ctx["replay_context_id"].as_str().unwrap();

    let (status, _) = post_json(&app, &format!("/api/sessions/{sid2}/llm-calls/replay-lookup"), json!({
        "replay_context_id": ctx_id
    })).await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "context belongs to session-1, not session-2");
}

#[tokio::test]
async fn test_replay_lookup_nonexistent_context() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    let (status, _) = post_json(&app, &format!("/api/sessions/{sid}/llm-calls/replay-lookup"), json!({
        "replay_context_id": "nonexistent-ctx"
    })).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_delete_nonexistent_replay_context() {
    let (app, _store, _tmp) = setup();

    let (status, body) = delete_json(&app, "/api/replay-contexts/nonexistent-id").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["released"].as_bool().unwrap(), false, "should report not found");
}

#[tokio::test]
async fn test_end_session_errored_status() {
    let (app, store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    post_json(&app, &format!("/api/sessions/{sid}/end"), json!({"status": "errored"})).await;

    let s = store.lock().unwrap();
    let session = s.get_session(sid).unwrap().unwrap();
    assert_eq!(session.status, SessionStatus::Failed);
}

#[tokio::test]
async fn test_end_session_failed_status() {
    let (app, store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    post_json(&app, &format!("/api/sessions/{sid}/end"), json!({"status": "failed"})).await;

    let s = store.lock().unwrap();
    let session = s.get_session(sid).unwrap().unwrap();
    assert_eq!(session.status, SessionStatus::Failed);
}

#[tokio::test]
async fn test_end_session_unknown_status_maps_to_completed() {
    let (app, store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    let (status, _) = post_json(&app, &format!("/api/sessions/{sid}/end"), json!({
        "status": "banana"
    })).await;
    assert_eq!(status, StatusCode::OK);

    let s = store.lock().unwrap();
    let session = s.get_session(sid).unwrap().unwrap();
    assert_eq!(session.status, SessionStatus::Completed, "unknown status should map to Completed");
}

#[tokio::test]
async fn test_tool_idempotency_with_client_step_id() {
    let (app, _store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap();

    let step_id = uuid::Uuid::new_v4().to_string();

    let (s1, b1) = post_json(&app, &format!("/api/sessions/{sid}/tool-calls"), json!({
        "client_step_id": step_id,
        "tool_name": "get_pods",
        "request_body": {"cluster": "x"},
        "response_body": {"pods": []},
        "duration_ms": 100
    })).await;
    assert_eq!(s1, StatusCode::CREATED);
    assert_eq!(b1["step_number"].as_u64().unwrap(), 1);

    let (s2, b2) = post_json(&app, &format!("/api/sessions/{sid}/tool-calls"), json!({
        "client_step_id": step_id,
        "tool_name": "get_pods",
        "request_body": {"cluster": "x"},
        "response_body": {"pods": []},
        "duration_ms": 100
    })).await;
    assert_eq!(s2, StatusCode::OK, "duplicate tool call should return 200");
    assert_eq!(b2["step_number"].as_u64().unwrap(), 1, "should return original step_number");
}

#[tokio::test]
async fn test_start_session_with_metadata_preserved() {
    let (app, store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({
        "name": "test",
        "metadata": {"question": "how is mulesoft?", "cluster": "dev1"}
    })).await;
    let sid = start["session_id"].as_str().unwrap();

    let s = store.lock().unwrap();
    let session = s.get_session(sid).unwrap().unwrap();
    assert_eq!(session.metadata["question"].as_str().unwrap(), "how is mulesoft?");
    assert_eq!(session.metadata["cluster"].as_str().unwrap(), "dev1");
}

#[tokio::test]
async fn test_source_label_with_metadata_both_preserved() {
    let (app, store, _tmp) = setup();

    let (_, start) = post_json(&app, "/api/sessions/start", json!({
        "name": "test",
        "source": "ray-agent",
        "metadata": {"env": "dev1"}
    })).await;
    let sid = start["session_id"].as_str().unwrap();

    let s = store.lock().unwrap();
    let session = s.get_session(sid).unwrap().unwrap();
    assert_eq!(session.metadata["env"].as_str().unwrap(), "dev1");
    assert_eq!(session.metadata["source_label"].as_str().unwrap(), "ray-agent",
        "source_label should be added on top of provided metadata");
}

// ── Helpers ────────────────────────────────────────────────

async fn patch_json(app: &Router, path: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let resp = app.clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or(json!({"raw": String::from_utf8_lossy(&body).to_string()}));
    (status, json)
}

/// Create a session, record `n` LLM-call steps on the root timeline, and
/// return `(session_id, root_timeline_id)`.
async fn seed_session(app: &Router, n: u32) -> (String, String) {
    let (_, start) = post_json(app, "/api/sessions/start", json!({"name": "test"})).await;
    let sid = start["session_id"].as_str().unwrap().to_string();
    let tid = start["root_timeline_id"].as_str().unwrap().to_string();

    for i in 0..n {
        post_json(app, &format!("/api/sessions/{sid}/llm-calls"), json!({
            "request_body": {"step": i},
            "response_body": {"content": format!("response-{}", i + 1)},
            "model": "gpt-4o",
            "duration_ms": 100
        })).await;
    }
    (sid, tid)
}

/// Record `n` LLM-call steps on a specific timeline and return the step
/// IDs by querying the steps endpoint.
async fn seed_steps_on_timeline(app: &Router, sid: &str, tid: &str, n: u32) -> Vec<String> {
    for i in 0..n {
        post_json(app, &format!("/api/sessions/{sid}/llm-calls"), json!({
            "timeline_id": tid,
            "request_body": {"fork_step": i},
            "response_body": {"content": format!("fork-response-{}", i + 1)},
            "model": "gpt-4o",
            "duration_ms": 100
        })).await;
    }
    let (_, steps_json) = get_json(app, &format!("/api/sessions/{sid}/steps?timeline={tid}")).await;
    steps_json.as_array().unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect()
}

// ── Step Edit on Fork (T3: PATCH + cascade-count) ──────────

#[tokio::test]
async fn test_patch_step_edit_response_on_fork() {
    let (app, store, _tmp) = setup();
    let (sid, _root_tid) = seed_session(&app, 3).await;

    let (_, fork) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 2, "label": "edit-test"
    })).await;
    let fork_tid = fork["fork_timeline_id"].as_str().unwrap();

    let step_ids = seed_steps_on_timeline(&app, &sid, fork_tid, 2).await;
    let fork_own_ids: Vec<_> = step_ids.iter()
        .filter(|id| {
            let s = store.lock().unwrap();
            let step = s.get_step(id).unwrap().unwrap();
            step.timeline_id == fork_tid
        })
        .cloned()
        .collect();
    let target_id = &fork_own_ids[0];

    let original_hash = {
        let s = store.lock().unwrap();
        s.get_step(target_id).unwrap().unwrap().request_hash.clone()
    };

    let (status, body) = patch_json(&app, &format!("/api/steps/{target_id}/edit"), json!({
        "response_body": {"content": "edited response"}
    })).await;

    assert_eq!(status, StatusCode::OK, "editing response on fork should succeed, got: {body}");
    assert_eq!(body["step_id"].as_str().unwrap(), target_id.as_str());

    let updated_hash = {
        let s = store.lock().unwrap();
        s.get_step(target_id).unwrap().unwrap().request_hash.clone()
    };
    assert_eq!(original_hash, updated_hash, "request_hash should be unchanged when only response is edited");
}

#[tokio::test]
async fn test_patch_step_edit_request_changes_hash() {
    let (app, store, _tmp) = setup();
    let (sid, _root_tid) = seed_session(&app, 2).await;

    let (_, fork) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 1, "label": "hash-test"
    })).await;
    let fork_tid = fork["fork_timeline_id"].as_str().unwrap();

    let step_ids = seed_steps_on_timeline(&app, &sid, fork_tid, 1).await;
    let fork_own_ids: Vec<_> = step_ids.iter()
        .filter(|id| {
            let s = store.lock().unwrap();
            s.get_step(id).unwrap().unwrap().timeline_id == fork_tid
        })
        .cloned()
        .collect();
    let target_id = &fork_own_ids[0];

    let original_hash = {
        let s = store.lock().unwrap();
        s.get_step(target_id).unwrap().unwrap().request_hash.clone()
    };

    let (status, _) = patch_json(&app, &format!("/api/steps/{target_id}/edit"), json!({
        "request_body": {"messages": [{"role": "user", "content": "different prompt"}]}
    })).await;
    assert_eq!(status, StatusCode::OK);

    let updated_hash = {
        let s = store.lock().unwrap();
        s.get_step(target_id).unwrap().unwrap().request_hash.clone()
    };
    assert_ne!(original_hash, updated_hash, "request_hash must change when request_body is edited");
}

#[tokio::test]
async fn test_patch_step_edit_on_main_blocked_and_env_bypass() {
    let _env_guard = main_edits_env_lock();
    let (app, _store, _tmp) = setup();

    // Phase 1: env var unset → editing main blocked
    unsafe { std::env::remove_var("REWIND_ALLOW_MAIN_EDITS"); }

    let (sid, _root_tid) = seed_session(&app, 2).await;

    let (_, steps_json) = get_json(&app, &format!("/api/sessions/{sid}/steps")).await;
    let step_id = steps_json.as_array().unwrap()[0]["id"].as_str().unwrap();

    let (status, body) = patch_json(&app, &format!("/api/steps/{step_id}/edit"), json!({
        "response_body": {"content": "nope"}
    })).await;

    assert_eq!(status, StatusCode::CONFLICT, "editing main timeline should be blocked");
    let msg = body.get("raw").and_then(|v| v.as_str()).unwrap_or("");
    assert!(msg.contains("REWIND_ALLOW_MAIN_EDITS"), "error should mention the env var, got: {msg}");

    // Phase 2: set env var → same edit succeeds
    unsafe { std::env::set_var("REWIND_ALLOW_MAIN_EDITS", "true"); }

    let (status2, body2) = patch_json(&app, &format!("/api/steps/{step_id}/edit"), json!({
        "response_body": {"content": "allowed via env"}
    })).await;

    assert_eq!(status2, StatusCode::OK, "main edit should be allowed with env var set, got: {body2}");

    unsafe { std::env::remove_var("REWIND_ALLOW_MAIN_EDITS"); }
}

#[tokio::test]
async fn test_patch_step_cascade_delete() {
    let (app, _store, _tmp) = setup();
    let (sid, _root_tid) = seed_session(&app, 3).await;

    let (_, fork) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 2, "label": "cascade-test"
    })).await;
    let fork_tid = fork["fork_timeline_id"].as_str().unwrap();

    let step_ids = seed_steps_on_timeline(&app, &sid, fork_tid, 4).await;
    let fork_own_ids: Vec<_> = step_ids.iter()
        .filter(|id| {
            let s = _store.lock().unwrap();
            s.get_step(id).unwrap().unwrap().timeline_id == fork_tid
        })
        .cloned()
        .collect();
    assert!(fork_own_ids.len() >= 4, "should have 4 own steps on the fork");

    let target_id = &fork_own_ids[0];

    let (status, body) = patch_json(&app, &format!("/api/steps/{target_id}/edit"), json!({
        "response_body": {"content": "edited"}
    })).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["deleted_downstream_count"].as_u64().unwrap(), 3,
        "editing step 1 of 4 should cascade-delete 3 downstream steps");
}

#[tokio::test]
async fn test_cascade_count_matches_actual_delete() {
    let (app, _store, _tmp) = setup();
    let (sid, _root_tid) = seed_session(&app, 2).await;

    let (_, fork) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 1, "label": "count-test"
    })).await;
    let fork_tid = fork["fork_timeline_id"].as_str().unwrap();

    let step_ids = seed_steps_on_timeline(&app, &sid, fork_tid, 3).await;
    let fork_own_ids: Vec<_> = step_ids.iter()
        .filter(|id| {
            let s = _store.lock().unwrap();
            s.get_step(id).unwrap().unwrap().timeline_id == fork_tid
        })
        .cloned()
        .collect();
    let target_id = &fork_own_ids[0];

    let (cc_status, cc_body) = get_json(&app, &format!("/api/steps/{target_id}/cascade-count")).await;
    assert_eq!(cc_status, StatusCode::OK);
    let predicted = cc_body["deleted_downstream_count"].as_u64().unwrap();
    assert_eq!(cc_body["on_main"].as_bool().unwrap(), false, "fork step should not be on main");

    let (patch_status, patch_body) = patch_json(&app, &format!("/api/steps/{target_id}/edit"), json!({
        "response_body": {"content": "edited"}
    })).await;
    assert_eq!(patch_status, StatusCode::OK);
    let actual = patch_body["deleted_downstream_count"].as_u64().unwrap();

    assert_eq!(predicted, actual, "cascade-count dry-run should match actual delete count");
}

#[tokio::test]
async fn test_patch_step_body_too_large() {
    let (app, _store, _tmp) = setup();
    let (sid, _root_tid) = seed_session(&app, 2).await;

    let (_, fork) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 1, "label": "large-body"
    })).await;
    let fork_tid = fork["fork_timeline_id"].as_str().unwrap();

    let step_ids = seed_steps_on_timeline(&app, &sid, fork_tid, 1).await;
    let fork_own_ids: Vec<_> = step_ids.iter()
        .filter(|id| {
            let s = _store.lock().unwrap();
            s.get_step(id).unwrap().unwrap().timeline_id == fork_tid
        })
        .cloned()
        .collect();
    let target_id = &fork_own_ids[0];

    let big = "x".repeat(5 * 1024 * 1024 + 1);
    let (status, _) = patch_json(&app, &format!("/api/steps/{target_id}/edit"), json!({
        "request_body": big
    })).await;

    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "body > 5MB should be rejected with 413");
}

#[tokio::test]
async fn test_patch_step_both_bodies_missing() {
    let (app, _store, _tmp) = setup();
    let (sid, _root_tid) = seed_session(&app, 2).await;

    let (_, fork) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 1, "label": "empty-body"
    })).await;
    let fork_tid = fork["fork_timeline_id"].as_str().unwrap();

    let step_ids = seed_steps_on_timeline(&app, &sid, fork_tid, 1).await;
    let fork_own_ids: Vec<_> = step_ids.iter()
        .filter(|id| {
            let s = _store.lock().unwrap();
            s.get_step(id).unwrap().unwrap().timeline_id == fork_tid
        })
        .cloned()
        .collect();
    let target_id = &fork_own_ids[0];

    let (status, _) = patch_json(&app, &format!("/api/steps/{target_id}/edit"), json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "empty body should return 400");
}

// ── Fork-and-Edit-Step (T5) ───────────────────────────────

#[tokio::test]
async fn test_fork_and_edit_step_happy_path() {
    let (app, store, _tmp) = setup();
    let (sid, root_tid) = seed_session(&app, 3).await;

    let (status, body) = post_json(&app, &format!("/api/sessions/{sid}/fork-and-edit-step"), json!({
        "source_timeline_id": root_tid,
        "at_step": 2,
        "response_body": {"content": "edited at step 2"},
        "label": "edit-fork"
    })).await;

    assert_eq!(status, StatusCode::CREATED, "fork-and-edit should return 201, got: {body}");
    let fork_tid = body["fork_timeline_id"].as_str().unwrap();
    let edited_step_id = body["step_id"].as_str().unwrap();
    assert!(!fork_tid.is_empty());
    assert!(!edited_step_id.is_empty());

    let (_, fork_steps) = get_json(&app, &format!("/api/sessions/{sid}/steps?timeline={fork_tid}")).await;
    let steps = fork_steps.as_array().unwrap();

    assert_eq!(steps.len(), 2, "fork should have step 1 (inherited) + step 2 (edited)");
    assert_eq!(steps[0]["step_number"].as_u64().unwrap(), 1, "first step should be inherited step 1");
    assert_eq!(steps[1]["step_number"].as_u64().unwrap(), 2, "second step should be the edited step 2");

    let s = store.lock().unwrap();
    let edited = s.get_step(edited_step_id).unwrap().unwrap();
    assert_eq!(edited.step_number, 2);
    assert_eq!(edited.timeline_id, fork_tid);
}

#[tokio::test]
async fn test_fork_and_edit_step_bad_timeline() {
    let (app, _store, _tmp) = setup();
    let (sid, _root_tid) = seed_session(&app, 2).await;

    let (status, _) = post_json(&app, &format!("/api/sessions/{sid}/fork-and-edit-step"), json!({
        "source_timeline_id": "nonexistent-timeline-id",
        "at_step": 1,
        "response_body": {"content": "won't happen"}
    })).await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "nonexistent source_timeline_id should return 400");
}

#[tokio::test]
async fn test_fork_and_edit_step_at_zero() {
    let (app, _store, _tmp) = setup();
    let (sid, root_tid) = seed_session(&app, 2).await;

    let (status, _) = post_json(&app, &format!("/api/sessions/{sid}/fork-and-edit-step"), json!({
        "source_timeline_id": root_tid,
        "at_step": 0,
        "response_body": {"content": "won't happen"}
    })).await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "at_step=0 should return 400");
}

#[tokio::test]
async fn test_fork_and_edit_step_inherited_step() {
    // Regression: fork-and-edit on a step that lives on the parent
    // timeline (inherited via a previous fork) used to 400 because
    // `store.get_step_by_number(timeline_id, n)` only matches steps
    // OWNED by that timeline. The dashboard, however, happily shows
    // inherited steps in the picker, so the user-facing workflow
    // "open fork → edit any visible step" was silently broken for
    // every step the fork didn't own. The fix routes the lookup
    // through `engine.get_full_timeline_steps` (the union view).
    //
    // Repro recipe used in dev1 smoke (2026-04-29):
    //   1. seed main with 5 steps
    //   2. fork at step 2 → fork owns nothing, inherits steps 1-2
    //   3. fork-and-edit on FORK at_step=2 → 400 before fix, 201 after
    let (app, _store, _tmp) = setup();
    let (sid, _root_tid) = seed_session(&app, 5).await;

    let (_, fork) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 2, "label": "first-fork"
    })).await;
    let fork_tid = fork["fork_timeline_id"].as_str().unwrap();

    // Now ask fork-and-edit to edit step #2 of THIS fork. Step #2 is
    // inherited from main; the fork itself owns no steps yet.
    let (status, body) = post_json(&app, &format!("/api/sessions/{sid}/fork-and-edit-step"), json!({
        "source_timeline_id": fork_tid,
        "at_step": 2,
        "response_body": {"edit": "of-an-inherited-step"},
        "label": "second-fork-via-inherited-edit"
    })).await;

    assert_eq!(
        status, StatusCode::CREATED,
        "fork-and-edit on an inherited step should succeed (got: {body})"
    );
    let new_fork_tid = body["fork_timeline_id"].as_str().unwrap();
    let new_step_id = body["step_id"].as_str().unwrap();
    assert!(!new_fork_tid.is_empty());
    assert!(!new_step_id.is_empty());

    // The newly-created step lives on the new fork at step_number = at_step.
    // Note: nested-fork inheritance via get_full_timeline_steps is still
    // a known follow-up (only walks one parent level up), so this test
    // asserts the direct invariants of the edit itself rather than the
    // resulting fork's full step view.
    let s = _store.lock().unwrap();
    let new_step = s.get_step(new_step_id).unwrap().unwrap();
    assert_eq!(new_step.step_number, 2);
    assert_eq!(new_step.timeline_id, new_fork_tid);

    // Roundtrip the response_body through the blob store to prove the
    // edited bytes actually persisted (a future refactor that reused
    // the original step's response_blob would otherwise pass the
    // step_number / timeline_id checks above).
    let resp_bytes = s.blobs.get(&new_step.response_blob).unwrap();
    let body_back: serde_json::Value = serde_json::from_slice(&resp_bytes).unwrap();
    assert_eq!(body_back, json!({"edit": "of-an-inherited-step"}));
}

#[tokio::test]
async fn test_fork_and_edit_step_inherited_step_below_fork_boundary() {
    // Reviewer S2: the original dev1 repro was editing step 3 of a
    // fork created at step 5 — i.e. a step *well below* the fork
    // boundary. The first inherited-step test exercised the boundary
    // step (step 2 of a fork at step 2); this variant proves the
    // union-view lookup also resolves steps from deeper in the
    // inherited chain (step 2 of a fork at step 4).
    let (app, _store, _tmp) = setup();
    let (sid, _root_tid) = seed_session(&app, 5).await;

    let (_, fork) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 4, "label": "deep-inherit-fork"
    })).await;
    let fork_tid = fork["fork_timeline_id"].as_str().unwrap();

    let (status, body) = post_json(&app, &format!("/api/sessions/{sid}/fork-and-edit-step"), json!({
        "source_timeline_id": fork_tid,
        "at_step": 2,
        "response_body": {"edit": "deep-inherited"},
        "label": "edit-two-below-boundary"
    })).await;

    assert_eq!(
        status, StatusCode::CREATED,
        "fork-and-edit on a step well below the fork boundary should succeed (got: {body})"
    );
    let new_step_id = body["step_id"].as_str().unwrap();
    let s = _store.lock().unwrap();
    let new_step = s.get_step(new_step_id).unwrap().unwrap();
    assert_eq!(new_step.step_number, 2);
    let resp_bytes = s.blobs.get(&new_step.response_blob).unwrap();
    let body_back: serde_json::Value = serde_json::from_slice(&resp_bytes).unwrap();
    assert_eq!(body_back, json!({"edit": "deep-inherited"}));
}

// ── Edit-on-selected-timeline (promote-and-mutate) ─────────

#[tokio::test]
async fn test_patch_promote_inherited_step() {
    let (app, store, _tmp) = setup();
    let (sid, _root_tid) = seed_session(&app, 5).await;

    let (_, fork) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 3, "label": "promote-test"
    })).await;
    let fork_tid = fork["fork_timeline_id"].as_str().unwrap();

    let (_, steps) = get_json(&app, &format!("/api/sessions/{sid}/steps")).await;
    let main_step2_id = steps.as_array().unwrap()[1]["id"].as_str().unwrap();

    let (status, body) = patch_json(&app, &format!("/api/steps/{main_step2_id}/edit"), json!({
        "response_body": {"promoted": true},
        "target_timeline_id": fork_tid
    })).await;

    assert_eq!(status, StatusCode::OK, "promote should succeed: {body}");
    let resolved_id = body["resolved_step_id"].as_str().unwrap();
    assert_ne!(resolved_id, main_step2_id, "promote creates a new owned step");

    let s = store.lock().unwrap();
    let promoted_step = s.get_step(resolved_id).unwrap().unwrap();
    assert_eq!(promoted_step.timeline_id, fork_tid);
    assert_eq!(promoted_step.step_number, 2);

    let main_step = s.get_step(main_step2_id).unwrap().unwrap();
    let main_resp = s.blobs.get(&main_step.response_blob).unwrap();
    let main_body: serde_json::Value = serde_json::from_slice(&main_resp).unwrap();
    assert!(!main_body.get("promoted").is_some(), "main step should be untouched");
}

#[tokio::test]
async fn test_patch_promote_idempotent_re_edit() {
    // After the first PATCH promote, the fork OWNS a step at step_number=N.
    // The dedup in `get_full_timeline_steps` (added 2026-04-29) makes the
    // inherited row at #N invisible from fork's view — owned wins. So the
    // dashboard's realistic re-edit flow is: first PATCH targets the
    // INHERITED id with target_timeline_id (promote), and the SECOND edit
    // targets the resolved OWNED id WITHOUT target (in-place edit). Both
    // calls must return the same step_id so the panel knows it's editing
    // the same row.
    let (app, _store, _tmp) = setup();
    let (sid, _root_tid) = seed_session(&app, 3).await;

    let (_, fork) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 2, "label": "idempotent-test"
    })).await;
    let fork_tid = fork["fork_timeline_id"].as_str().unwrap();

    let (_, steps) = get_json(&app, &format!("/api/sessions/{sid}/steps")).await;
    let main_step1_id = steps.as_array().unwrap()[0]["id"].as_str().unwrap();

    // Promote: PATCH inherited id with target=fork → creates a new owned row.
    let (s1, b1) = patch_json(&app, &format!("/api/steps/{main_step1_id}/edit"), json!({
        "response_body": {"edit": 1}, "target_timeline_id": fork_tid
    })).await;
    assert_eq!(s1, StatusCode::OK);
    let first_resolved = b1["resolved_step_id"].as_str().unwrap().to_string();
    assert_ne!(first_resolved, main_step1_id, "first promote inserts a new owned step");

    // Re-edit: PATCH the resolved owned id (no target) → in-place edit.
    let (s2, b2) = patch_json(&app, &format!("/api/steps/{first_resolved}/edit"), json!({
        "response_body": {"edit": 2}
    })).await;
    assert_eq!(s2, StatusCode::OK, "in-place edit on the resolved owned step should succeed");
    let second_resolved = b2["resolved_step_id"].as_str().unwrap().to_string();
    assert_eq!(first_resolved, second_resolved, "in-place edit returns the same step id");
}

#[tokio::test]
async fn test_patch_promote_visibility_400() {
    let (app, _store, _tmp) = setup();
    let (sid, _root_tid) = seed_session(&app, 5).await;

    let (_, fork_a) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 2, "label": "fork-a"
    })).await;
    let (_, fork_b) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 4, "label": "fork-b"
    })).await;
    let fork_b_tid = fork_b["fork_timeline_id"].as_str().unwrap();

    seed_steps_on_timeline(&app, &sid, fork_b_tid, 1).await;

    let (_, fork_b_steps) = get_json(&app, &format!("/api/sessions/{sid}/steps?timeline={fork_b_tid}")).await;
    let fork_b_step_id = fork_b_steps.as_array().unwrap().last().unwrap()["id"].as_str().unwrap();

    let fork_a_tid = fork_a["fork_timeline_id"].as_str().unwrap();
    let (status, body) = patch_json(&app, &format!("/api/steps/{fork_b_step_id}/edit"), json!({
        "response_body": {"cross": true},
        "target_timeline_id": fork_a_tid
    })).await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "cross-fork should fail: {body}");
    let msg = body.get("raw").and_then(|v| v.as_str()).unwrap_or("");
    assert!(msg.contains("not visible"), "error should mention visibility: {msg}");
}

#[tokio::test]
async fn test_patch_promote_main_protection_follows_target() {
    let _env_guard = main_edits_env_lock();
    let (app, _store, _tmp) = setup();
    unsafe { std::env::remove_var("REWIND_ALLOW_MAIN_EDITS"); }

    let (sid, root_tid) = seed_session(&app, 3).await;

    let (_, fork) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 2, "label": "target-main-test"
    })).await;
    let fork_tid = fork["fork_timeline_id"].as_str().unwrap();

    // Add a fork-owned step so the fork has both inherited (1, 2) and
    // owned (3) rows in its view. We then promote-and-mutate the
    // INHERITED step #2 with target=main — the visibility check needs
    // an id that exists on both timelines, and inherited rows trivially
    // satisfy that. Picking an explicit step_number here (rather than
    // .last()) makes the test independent of the step_counters seeding
    // behavior on forks: before v0.14.9, fork owned-steps started at 1
    // and (.last() returned the inherited step at step #2 by accident);
    // after v0.14.9, fork owned-steps start at fork_at_step+1=3 and
    // .last() returns the fork-owned step (which isn't visible on main).
    seed_steps_on_timeline(&app, &sid, fork_tid, 1).await;
    let (_, fork_steps) = get_json(&app, &format!("/api/sessions/{sid}/steps?timeline={fork_tid}")).await;
    let inherited_step_id = fork_steps.as_array().unwrap().iter()
        .find(|s| s["step_number"].as_u64() == Some(2))
        .and_then(|s| s["id"].as_str())
        .expect("step #2 should be visible on the fork (inherited from main)");

    let (status, _) = patch_json(&app, &format!("/api/steps/{inherited_step_id}/edit"), json!({
        "response_body": {"target_main": true},
        "target_timeline_id": root_tid
    })).await;
    assert_eq!(status, StatusCode::CONFLICT, "targeting main should be blocked");

    unsafe { std::env::set_var("REWIND_ALLOW_MAIN_EDITS", "true"); }
    let (status2, _) = patch_json(&app, &format!("/api/steps/{inherited_step_id}/edit"), json!({
        "response_body": {"target_main": true},
        "target_timeline_id": root_tid
    })).await;
    assert_eq!(status2, StatusCode::OK, "targeting main with env bypass should succeed");
    unsafe { std::env::remove_var("REWIND_ALLOW_MAIN_EDITS"); }
}

#[tokio::test]
async fn test_patch_promote_preserves_step_type_and_tool_name() {
    let (app, store, _tmp) = setup();

    let (s, b) = post_json(&app, "/api/sessions/start", json!({"name": "tool-promote"})).await;
    assert_eq!(s, StatusCode::CREATED);
    let sid = b["session_id"].as_str().unwrap().to_string();
    let root_tid = b["root_timeline_id"].as_str().unwrap().to_string();

    let (s2, _) = post_json(&app, &format!("/api/sessions/{sid}/tool-calls"), json!({
        "tool_name": "get_pods",
        "request_body": {"cluster": "dev1"},
        "response_body": {"pods": ["a", "b"]},
        "duration_ms": 50
    })).await;
    assert_eq!(s2, StatusCode::CREATED);

    let (_, fork) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 1, "label": "tool-fork"
    })).await;
    let fork_tid = fork["fork_timeline_id"].as_str().unwrap();

    let (_, steps) = get_json(&app, &format!("/api/sessions/{sid}/steps")).await;
    let tool_step_id = steps.as_array().unwrap()[0]["id"].as_str().unwrap();

    let (status, body) = patch_json(&app, &format!("/api/steps/{tool_step_id}/edit"), json!({
        "response_body": {"pods": ["c"]},
        "target_timeline_id": fork_tid
    })).await;
    assert_eq!(status, StatusCode::OK, "promote tool step: {body}");

    let resolved_id = body["resolved_step_id"].as_str().unwrap();
    let s = store.lock().unwrap();
    let promoted = s.get_step(resolved_id).unwrap().unwrap();
    assert_eq!(promoted.step_type, rewind_store::StepType::ToolCall, "step_type must be preserved");
    assert_eq!(promoted.tool_name.as_deref(), Some("get_pods"), "tool_name must be preserved");
}

#[tokio::test]
async fn test_cascade_count_target_aware() {
    // The point of `?target_timeline_id=` on cascade-count is so the
    // confirm modal shows the count that will actually be deleted on
    // the SELECTED timeline, not on the step's physical owner. Verify
    // by comparing the same step's cascade-count without target (=
    // main's downstream) vs with target=fork (= fork's downstream).
    //
    // Note: we deliberately do NOT seed owned steps on the fork at
    // step_numbers that would collide with inherited ones — the dedup
    // would (correctly) hide the inherited rows once owned twins exist,
    // and the promote-path visibility check would then reject the
    // cascade-count call by id. The dashboard's real flow PATCHes the
    // owned id directly in that case, so this scenario isn't reachable
    // from the UI.
    let (app, _store, _tmp) = setup();
    let (sid, _root_tid) = seed_session(&app, 5).await;

    let (_, fork) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 3, "label": "cascade-target"
    })).await;
    let fork_tid = fork["fork_timeline_id"].as_str().unwrap();

    let (_, steps) = get_json(&app, &format!("/api/sessions/{sid}/steps")).await;
    let step2_id = steps.as_array().unwrap()[1]["id"].as_str().unwrap();

    let (s1, b1) = get_json(&app, &format!("/api/steps/{step2_id}/cascade-count")).await;
    assert_eq!(s1, StatusCode::OK);
    let main_count = b1["deleted_downstream_count"].as_u64().unwrap();

    let (s2, b2) = get_json(&app, &format!("/api/steps/{step2_id}/cascade-count?target_timeline_id={fork_tid}")).await;
    assert_eq!(s2, StatusCode::OK);
    let fork_count = b2["deleted_downstream_count"].as_u64().unwrap();
    let on_main = b2["on_main"].as_bool().unwrap();

    // Main has steps 3, 4, 5 after step #2 → main_count = 3.
    // Fork inherits 1, 2, 3 with no own steps → fork_count = 0
    // (count_steps_after counts only OWNED rows; inherited steps live
    // on main and aren't part of the fork's timeline_id row count).
    assert_eq!(main_count, 3, "main should have 3 downstream after step #2");
    assert_eq!(fork_count, 0, "fork has no owned downstream after step #2");
    assert!(main_count > fork_count, "main_count({main_count}) > fork_count({fork_count})");
    assert!(!on_main, "cascade-count with target=fork should report on_main=false");
}

#[tokio::test]
async fn test_patch_promote_cross_fork_mutation_blocked() {
    let (app, store, _tmp) = setup();
    let (sid, _root_tid) = seed_session(&app, 3).await;

    let (_, fork_a) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 2, "label": "fork-a"
    })).await;
    let (_, fork_b) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 2, "label": "fork-b"
    })).await;
    let fork_a_tid = fork_a["fork_timeline_id"].as_str().unwrap();
    let fork_b_tid = fork_b["fork_timeline_id"].as_str().unwrap();

    seed_steps_on_timeline(&app, &sid, fork_a_tid, 1).await;
    seed_steps_on_timeline(&app, &sid, fork_b_tid, 1).await;

    let (_, fork_a_steps) = get_json(&app, &format!("/api/sessions/{sid}/steps?timeline={fork_a_tid}")).await;
    let fork_a_owned = fork_a_steps.as_array().unwrap().iter()
        .find(|s| s["timeline_id"].as_str().unwrap() == fork_a_tid)
        .unwrap();
    let fork_a_step_id = fork_a_owned["id"].as_str().unwrap();

    let (status, body) = patch_json(&app, &format!("/api/steps/{fork_a_step_id}/edit"), json!({
        "response_body": {"sentinel": "CROSS_FORK_MUTATION"},
        "target_timeline_id": fork_b_tid
    })).await;

    assert_eq!(status, StatusCode::BAD_REQUEST,
        "cross-fork mutation must be blocked (got {status}: {body})");
    let msg = body.get("raw").and_then(|v| v.as_str()).unwrap_or("");
    assert!(msg.contains("not visible"), "error should mention visibility: {msg}");

    let s = store.lock().unwrap();
    let (_, fork_b_steps_after) = {
        drop(s);
        get_json(&app, &format!("/api/sessions/{sid}/steps?timeline={fork_b_tid}")).await
    };
    let fork_b_owned = fork_b_steps_after.as_array().unwrap().iter()
        .find(|s| s["timeline_id"].as_str().unwrap() == fork_b_tid)
        .unwrap();
    assert_ne!(
        fork_b_owned.get("response_preview").and_then(|v| v.as_str()).unwrap_or(""),
        "CROSS_FORK_MUTATION",
        "fork_b's step should NOT have been overwritten"
    );
}

#[tokio::test]
async fn test_steps_endpoint_dedupes_owned_over_inherited_after_promote() {
    // Regression for the dev1 v0.14.6 bug where a user promoted an
    // inherited step (PATCH .../edit?target_timeline_id=fork) but the
    // dashboard kept showing the inherited row because /steps?timeline=fork
    // returned BOTH the inherited and the owned step at the same
    // step_number. After the dedup fix in get_full_timeline_steps the
    // owned row supersedes the inherited one in this view.
    let (app, _store, _tmp) = setup();
    let (sid, _root_tid) = seed_session(&app, 3).await;

    let (_, fork) = post_json(&app, &format!("/api/sessions/{sid}/fork"), json!({
        "at_step": 2, "label": "dedup-test"
    })).await;
    let fork_tid = fork["fork_timeline_id"].as_str().unwrap();

    let (_, main_steps) = get_json(&app, &format!("/api/sessions/{sid}/steps")).await;
    let main_step2_id = main_steps.as_array().unwrap()[1]["id"].as_str().unwrap();

    // Promote-and-mutate: PATCH the inherited step #2 with target = fork.
    let (status, body) = patch_json(&app, &format!("/api/steps/{main_step2_id}/edit"), json!({
        "response_body": {"edit": "promoted-via-PATCH"},
        "target_timeline_id": fork_tid
    })).await;
    assert_eq!(status, StatusCode::OK, "promote should succeed: {body}");
    let resolved = body["resolved_step_id"].as_str().unwrap();

    // Fork's step view: must show step #2 ONCE, owned by the fork.
    let (_, fork_view) = get_json(&app, &format!("/api/sessions/{sid}/steps?timeline={fork_tid}")).await;
    let at_two: Vec<&serde_json::Value> = fork_view.as_array().unwrap()
        .iter()
        .filter(|s| s["step_number"].as_u64() == Some(2))
        .collect();
    assert_eq!(at_two.len(), 1,
        "fork view should have exactly one row at step #2 after promote, got {:?}", at_two);
    assert_eq!(at_two[0]["id"].as_str().unwrap(), resolved,
        "the surviving row at step #2 must be the OWNED one (resolved_step_id), not the inherited one");
    assert_eq!(at_two[0]["timeline_id"].as_str().unwrap(), fork_tid);
}

#[tokio::test]
async fn test_cascade_count_rejects_foreign_session_timeline() {
    let (app, _store, _tmp) = setup();
    let (sid1, _) = seed_session(&app, 3).await;
    let (sid2, _) = seed_session(&app, 3).await;

    let (_, steps1) = get_json(&app, &format!("/api/sessions/{sid1}/steps")).await;
    let step1_id = steps1.as_array().unwrap()[0]["id"].as_str().unwrap();

    let (_, session2) = get_json(&app, &format!("/api/sessions/{sid2}")).await;
    let s2_root_tid = session2["timelines"].as_array().unwrap()[0]["id"].as_str().unwrap();

    let (status, body) = get_json(&app, &format!(
        "/api/steps/{step1_id}/cascade-count?target_timeline_id={s2_root_tid}"
    )).await;

    assert_eq!(status, StatusCode::BAD_REQUEST,
        "cross-session cascade-count should be rejected: {body}");
}
