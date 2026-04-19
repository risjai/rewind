use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use rewind_store::*;
use rewind_web::{AppState, HookIngestionState, StoreEvent};
use serde_json::json;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use tower::ServiceExt;

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

    let (s2, _b2) = post_json(&app, &format!("/api/sessions/{sid}/llm-calls"), json!({
        "client_step_id": step_id,
        "request_body": {},
        "response_body": {"content": "hello"},
        "model": "gpt-4o",
        "duration_ms": 100
    })).await;
    assert_eq!(s2, StatusCode::OK, "duplicate should return 200, not 201");
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
