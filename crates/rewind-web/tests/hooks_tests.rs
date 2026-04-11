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

fn setup() -> (Router, Arc<Mutex<Store>>, TempDir, Arc<HookIngestionState>) {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let store = Arc::new(Mutex::new(store));
    let (event_tx, _) = tokio::sync::broadcast::channel::<StoreEvent>(16);
    let hooks = Arc::new(HookIngestionState::new());
    let state = AppState {
        store: store.clone(),
        event_tx,
        hooks: hooks.clone(),
    };
    let app = Router::new().nest("/api/hooks", rewind_web::hooks::routes(state));
    (app, store, tmp, hooks)
}

async fn post_json(app: Router, uri: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(json!(null));
    (status, value)
}

fn make_envelope(event_type: &str, session_id: &str, extra_payload: serde_json::Value) -> serde_json::Value {
    let mut payload = json!({
        "session_id": session_id,
        "hook_event_name": event_type,
        "cwd": "/Users/test/my-project"
    });
    if let Some(obj) = extra_payload.as_object() {
        for (k, v) in obj {
            payload[k] = v.clone();
        }
    }
    json!({
        "source": "claude-code",
        "event_type": event_type,
        "timestamp": "2026-04-11T10:30:00.123Z",
        "payload": payload
    })
}

// ── Test 1: PreToolUse creates session and step ──────────

#[tokio::test]
async fn test_hook_pre_tool_use_creates_step() {
    let (app, store, _tmp, hooks) = setup();

    let envelope = make_envelope("PreToolUse", "test-session-123", json!({
        "tool_name": "Read",
        "tool_input": {"file_path": "/src/main.rs"},
        "tool_use_id": "toolu_123"
    }));

    let (status, body) = post_json(app, "/api/hooks/event", envelope).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");

    // Verify session was auto-created
    assert!(hooks.sessions.contains_key("test-session-123"));

    let sess_state = hooks.sessions.get("test-session-123").unwrap();
    let s = store.lock().unwrap();

    // Session exists in the store
    let session = s.get_session(&sess_state.session_id).unwrap().unwrap();
    assert_eq!(session.source, SessionSource::Hooks);

    // Step exists with correct type and tool_name
    let steps = s.get_steps(&sess_state.timeline_id).unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].step_type, StepType::ToolCall);
    assert_eq!(steps[0].tool_name.as_deref(), Some("Read"));
    assert_eq!(steps[0].status, StepStatus::Pending);
}

// ── Test 2: PostToolUse matches PreToolUse ───────────────

#[tokio::test]
async fn test_hook_post_tool_use_matches_pre() {
    let (app, store, _tmp, hooks) = setup();

    // Send PreToolUse first
    let pre_envelope = make_envelope("PreToolUse", "test-session-match", json!({
        "tool_name": "Read",
        "tool_input": {"file_path": "/src/main.rs"},
        "tool_use_id": "toolu_match_1"
    }));
    let (status, _) = post_json(app.clone(), "/api/hooks/event", pre_envelope).await;
    assert_eq!(status, StatusCode::OK);

    // Send PostToolUse with the same tool_use_id
    let post_envelope = make_envelope("PostToolUse", "test-session-match", json!({
        "tool_name": "Read",
        "tool_use_id": "toolu_match_1",
        "tool_response": {"content": "file contents here"}
    }));
    // Need a unique timestamp to avoid dedup
    let mut post_env = post_envelope;
    post_env["timestamp"] = json!("2026-04-11T10:30:01.123Z");

    let (status, _) = post_json(app, "/api/hooks/event", post_env).await;
    assert_eq!(status, StatusCode::OK);

    // Verify only 1 step exists (updated, not duplicated)
    let sess_state = hooks.sessions.get("test-session-match").unwrap();
    let s = store.lock().unwrap();
    let steps = s.get_steps(&sess_state.timeline_id).unwrap();
    assert_eq!(steps.len(), 1, "PostToolUse should update existing step, not create a new one");

    // The step should now have a response_blob (non-empty)
    assert!(
        !steps[0].response_blob.is_empty(),
        "Step should have a response_blob after PostToolUse"
    );
    assert_eq!(steps[0].status, StepStatus::Success);
}

// ── Test 3: Session deduplication ────────────────────────

#[tokio::test]
async fn test_hook_session_deduplication() {
    let (app, store, _tmp, hooks) = setup();

    // Send 3 events with the same session_id but different event types / timestamps
    for (i, event_type) in ["PreToolUse", "PostToolUse", "UserPromptSubmit"].iter().enumerate() {
        let envelope = json!({
            "source": "claude-code",
            "event_type": event_type,
            "timestamp": format!("2026-04-11T10:30:0{}.123Z", i),
            "payload": {
                "session_id": "test-session-dedup",
                "hook_event_name": event_type,
                "tool_name": "Read",
                "tool_use_id": format!("toolu_{}", i),
                "cwd": "/Users/test/my-project"
            }
        });
        let (status, _) = post_json(app.clone(), "/api/hooks/event", envelope).await;
        assert_eq!(status, StatusCode::OK);
    }

    // Only 1 session should exist in hooks state
    let session_count = hooks.sessions.iter().count();
    assert_eq!(session_count, 1, "Should have exactly 1 session despite 3 events");

    // Only 1 session in the store
    let s = store.lock().unwrap();
    let sessions = s.list_sessions().unwrap();
    assert_eq!(sessions.len(), 1, "Store should have exactly 1 session");
}

// ── Test 4: camelCase event routing ──────────────────────

#[tokio::test]
async fn test_hook_camelcase_event_routing() {
    let (app, store, _tmp, hooks) = setup();

    // Send with camelCase event_type (like Cursor extension does)
    let envelope = json!({
        "source": "cursor",
        "event_type": "preToolUse",
        "timestamp": "2026-04-11T10:31:00.000Z",
        "payload": {
            "session_id": "test-session-camel",
            "hook_event_name": "preToolUse",
            "tool_name": "Write",
            "tool_input": {"file_path": "/src/lib.rs", "content": "hello"},
            "tool_use_id": "toolu_camel_1",
            "cwd": "/Users/test/my-project"
        }
    });

    let (status, _) = post_json(app, "/api/hooks/event", envelope).await;
    assert_eq!(status, StatusCode::OK);

    let sess_state = hooks.sessions.get("test-session-camel").unwrap();
    let s = store.lock().unwrap();
    let steps = s.get_steps(&sess_state.timeline_id).unwrap();
    assert_eq!(steps.len(), 1);
    // Should be routed to handle_pre_tool_use (ToolCall), not handle_generic_event (HookEvent)
    assert_eq!(
        steps[0].step_type, StepType::ToolCall,
        "camelCase 'preToolUse' should be routed as ToolCall, not HookEvent"
    );
    assert_eq!(steps[0].tool_name.as_deref(), Some("Write"));
}

// ── Test 5: UserPromptSubmit creates UserPrompt step ─────

#[tokio::test]
async fn test_hook_user_prompt() {
    let (app, store, _tmp, hooks) = setup();

    let envelope = make_envelope("UserPromptSubmit", "test-session-prompt", json!({
        "tool_input": {"prompt": "Explain this code"}
    }));

    let (status, _) = post_json(app, "/api/hooks/event", envelope).await;
    assert_eq!(status, StatusCode::OK);

    let sess_state = hooks.sessions.get("test-session-prompt").unwrap();
    let s = store.lock().unwrap();
    let steps = s.get_steps(&sess_state.timeline_id).unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].step_type, StepType::UserPrompt);
    assert_eq!(steps[0].status, StepStatus::Success);
}

// ── Test 6: Batch endpoint ───────────────────────────────

#[tokio::test]
async fn test_hook_batch_endpoint() {
    let (app, store, _tmp, hooks) = setup();

    let events: Vec<serde_json::Value> = (0..3)
        .map(|i| {
            json!({
                "source": "claude-code",
                "event_type": "PreToolUse",
                "timestamp": format!("2026-04-11T10:30:0{}.000Z", i),
                "payload": {
                    "session_id": "test-session-batch",
                    "hook_event_name": "PreToolUse",
                    "tool_name": format!("Tool{}", i),
                    "tool_use_id": format!("toolu_batch_{}", i),
                    "cwd": "/Users/test/my-project"
                }
            })
        })
        .collect();

    let (status, body) = post_json(app, "/api/hooks/events", json!(events)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert!(
        body["message"].as_str().unwrap().contains("processed 3 events"),
        "Response should mention 3 events processed, got: {}",
        body["message"]
    );

    // All 3 steps should exist
    let sess_state = hooks.sessions.get("test-session-batch").unwrap();
    let s = store.lock().unwrap();
    let steps = s.get_steps(&sess_state.timeline_id).unwrap();
    assert_eq!(steps.len(), 3, "Batch of 3 events should create 3 steps");
}

// ── Test 7: transcript_path stored in session metadata ───

#[tokio::test]
async fn test_hook_transcript_path_stored() {
    let (app, store, _tmp, hooks) = setup();

    let envelope = make_envelope("PreToolUse", "test-session-transcript", json!({
        "tool_name": "Read",
        "tool_input": {"file_path": "/src/main.rs"},
        "tool_use_id": "toolu_transcript_1",
        "transcript_path": "/tmp/test-transcript.jsonl"
    }));

    let (status, _) = post_json(app, "/api/hooks/event", envelope).await;
    assert_eq!(status, StatusCode::OK);

    let sess_state = hooks.sessions.get("test-session-transcript").unwrap();
    let s = store.lock().unwrap();
    let session = s.get_session(&sess_state.session_id).unwrap().unwrap();
    assert_eq!(
        session.metadata["transcript_path"].as_str(),
        Some("/tmp/test-transcript.jsonl"),
        "transcript_path should be stored in session metadata"
    );
}
