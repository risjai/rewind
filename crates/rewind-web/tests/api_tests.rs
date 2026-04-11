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
    let (event_tx, _) = tokio::sync::broadcast::channel::<StoreEvent>(16);
    let state = AppState {
        store: store.clone(),
        event_tx,
        hooks: Arc::new(HookIngestionState::new()),
    };
    let app = Router::new().nest("/api", rewind_web::api_routes(state));
    (app, store, tmp)
}

fn seed_session(store: &Arc<Mutex<Store>>) -> (Session, Timeline) {
    let s = store.lock().unwrap();
    let session = Session::new("test-session");
    let timeline = Timeline::new_root(&session.id);
    s.create_session(&session).unwrap();
    s.create_timeline(&timeline).unwrap();
    (session, timeline)
}

fn seed_step(store: &Arc<Mutex<Store>>, session: &Session, timeline: &Timeline, n: u32) -> Step {
    let s = store.lock().unwrap();

    let request = json!({
        "model": "gpt-4o",
        "messages": [
            {"role": "system", "content": "You are a helpful assistant."},
            {"role": "user", "content": "Hello world"}
        ]
    });
    let response = json!({
        "choices": [{"message": {"role": "assistant", "content": "Hi there!"}}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5}
    });

    let req_blob = s.blobs.put_json(&request).unwrap();
    let res_blob = s.blobs.put_json(&response).unwrap();

    let mut step = Step::new_llm_call(&timeline.id, &session.id, n, "gpt-4o");
    step.status = StepStatus::Success;
    step.tokens_in = 10;
    step.tokens_out = 5;
    step.duration_ms = 500;
    step.request_blob = req_blob;
    step.response_blob = res_blob;
    s.create_step(&step).unwrap();
    s.update_session_stats(&session.id, n, 15).unwrap();
    step
}

async fn get_json(app: Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap_or(json!(null));
    (status, value)
}

// ── Health ────────────────────────────────────────────────

#[tokio::test]
async fn test_health_endpoint() {
    let (app, _, _tmp) = setup();
    let (status, body) = get_json(app, "/api/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
}

// ── Sessions ─────────────────────────────────────────────

#[tokio::test]
async fn test_list_sessions_empty() {
    let (app, _, _tmp) = setup();
    let (status, body) = get_json(app, "/api/sessions").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_list_sessions_with_data() {
    let (app, store, _tmp) = setup();
    seed_session(&store);
    seed_session(&store);

    let (status, body) = get_json(app, "/api/sessions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_get_session_by_id() {
    let (app, store, _tmp) = setup();
    let (session, _) = seed_session(&store);

    let (status, body) = get_json(app, &format!("/api/sessions/{}", session.id)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["session"]["name"], "test-session");
    assert!(body["timelines"].as_array().unwrap().len() >= 1);
}

#[tokio::test]
async fn test_get_session_by_prefix() {
    let (app, store, _tmp) = setup();
    let (session, _) = seed_session(&store);
    let prefix = &session.id[..8];

    let (status, body) = get_json(app, &format!("/api/sessions/{}", prefix)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["session"]["id"], session.id);
}

#[tokio::test]
async fn test_get_session_latest() {
    let (app, store, _tmp) = setup();
    let (session, _) = seed_session(&store);

    let (status, body) = get_json(app, "/api/sessions/latest").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["session"]["id"], session.id);
}

#[tokio::test]
async fn test_get_session_not_found() {
    let (app, _, _tmp) = setup();
    let (status, _) = get_json(app, "/api/sessions/nonexistent").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ── Steps ────────────────────────────────────────────────

#[tokio::test]
async fn test_get_session_steps() {
    let (app, store, _tmp) = setup();
    let (session, timeline) = seed_session(&store);
    seed_step(&store, &session, &timeline, 1);
    seed_step(&store, &session, &timeline, 2);

    let (status, body) = get_json(app, &format!("/api/sessions/{}/steps", session.id)).await;
    assert_eq!(status, StatusCode::OK);
    let steps = body.as_array().unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0]["step_number"], 1);
    assert_eq!(steps[1]["step_number"], 2);
    assert_eq!(steps[0]["step_type"], "llm_call");
    assert_eq!(steps[0]["step_type_label"], "LLM Call");
    assert_eq!(steps[0]["model"], "gpt-4o");
    assert_eq!(steps[0]["status"], "success");
    assert_eq!(steps[0]["tokens_in"], 10);
    assert_eq!(steps[0]["tokens_out"], 5);
    assert!(!steps[0]["response_preview"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn test_get_step_detail_with_context_window() {
    let (app, store, _tmp) = setup();
    let (session, timeline) = seed_session(&store);
    let step = seed_step(&store, &session, &timeline, 1);

    let (status, body) = get_json(app, &format!("/api/steps/{}", step.id)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["step_number"], 1);
    assert_eq!(body["model"], "gpt-4o");
    assert_eq!(body["tokens_in"], 10);
    assert_eq!(body["tokens_out"], 5);
    assert_eq!(body["duration_ms"], 500);
    assert!(body["request_body"].is_object());
    assert!(body["response_body"].is_object());

    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[1]["role"], "user");
    assert!(messages[0]["content"].as_str().unwrap().contains("helpful assistant"));
    assert!(messages[1]["content"].as_str().unwrap().contains("Hello world"));
}

#[tokio::test]
async fn test_get_step_not_found() {
    let (app, _, _tmp) = setup();
    let (status, _) = get_json(app, "/api/steps/nonexistent-id").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ── Timelines ────────────────────────────────────────────

#[tokio::test]
async fn test_get_timelines_with_fork() {
    let (app, store, _tmp) = setup();
    let (session, timeline) = seed_session(&store);

    let fork = Timeline::new_fork(&session.id, &timeline.id, 1, "fix-attempt");
    store.lock().unwrap().create_timeline(&fork).unwrap();

    let (status, body) = get_json(app, &format!("/api/sessions/{}/timelines", session.id)).await;
    assert_eq!(status, StatusCode::OK);
    let timelines = body.as_array().unwrap();
    assert_eq!(timelines.len(), 2);
    assert_eq!(timelines[0]["label"], "main");
    assert_eq!(timelines[1]["label"], "fix-attempt");
    assert_eq!(timelines[1]["fork_at_step"], 1);
}

// ── Diff ─────────────────────────────────────────────────

#[tokio::test]
async fn test_diff_timelines() {
    let (app, store, _tmp) = setup();
    let (session, timeline) = seed_session(&store);
    seed_step(&store, &session, &timeline, 1);
    seed_step(&store, &session, &timeline, 2);

    let fork = Timeline::new_fork(&session.id, &timeline.id, 1, "forked");
    store.lock().unwrap().create_timeline(&fork).unwrap();

    let uri = format!(
        "/api/sessions/{}/diff?left={}&right={}",
        session.id, timeline.id, fork.id
    );
    let (status, body) = get_json(app, &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["step_diffs"].is_array());
    assert_eq!(body["left_label"], "main");
    assert_eq!(body["right_label"], "forked");
}

// ── Baselines ────────────────────────────────────────────

#[tokio::test]
async fn test_list_baselines_empty() {
    let (app, _, _tmp) = setup();
    let (status, body) = get_json(app, "/api/baselines").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_get_baseline_by_name() {
    let (app, store, _tmp) = setup();
    let (session, timeline) = seed_session(&store);
    seed_step(&store, &session, &timeline, 1);

    let baseline = Baseline::new("my-baseline", &session.id, &timeline.id, "test desc", 1, 15);
    {
        let s = store.lock().unwrap();
        s.create_baseline(&baseline).unwrap();
        let steps = s.get_steps(&timeline.id).unwrap();
        for step in &steps {
            let bs = BaselineStep::from_step(&baseline.id, step, None);
            s.create_baseline_step(&bs).unwrap();
        }
    }

    let (status, body) = get_json(app, "/api/baselines/my-baseline").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["baseline"]["name"], "my-baseline");
    assert_eq!(body["baseline"]["description"], "test desc");
    assert_eq!(body["steps"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn test_get_baseline_not_found() {
    let (app, _, _tmp) = setup();
    let (status, _) = get_json(app, "/api/baselines/nonexistent").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ── Cache ────────────────────────────────────────────────

#[tokio::test]
async fn test_cache_stats_empty() {
    let (app, _, _tmp) = setup();
    let (status, body) = get_json(app, "/api/cache/stats").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["entries"], 0);
    assert_eq!(body["total_hits"], 0);
    assert_eq!(body["total_tokens_saved"], 0);
}

// ── Snapshots ────────────────────────────────────────────

#[tokio::test]
async fn test_list_snapshots_empty() {
    let (app, _, _tmp) = setup();
    let (status, body) = get_json(app, "/api/snapshots").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.as_array().unwrap().is_empty());
}

// ── Response Preview ─────────────────────────────────────

#[tokio::test]
async fn test_response_preview_extraction() {
    let (app, store, _tmp) = setup();
    let (session, timeline) = seed_session(&store);
    seed_step(&store, &session, &timeline, 1);

    let (status, body) = get_json(app, &format!("/api/sessions/{}/steps", session.id)).await;
    assert_eq!(status, StatusCode::OK);
    let preview = body[0]["response_preview"].as_str().unwrap();
    assert!(preview.contains("Hi there!"));
}

// ── Session ordering ─────────────────────────────────────

#[tokio::test]
async fn test_multiple_sessions_returned() {
    let (app, store, _tmp) = setup();
    for i in 0..5 {
        let s = store.lock().unwrap();
        let session = Session::new(&format!("session-{}", i));
        let timeline = Timeline::new_root(&session.id);
        s.create_session(&session).unwrap();
        s.create_timeline(&timeline).unwrap();
    }

    let (status, body) = get_json(app, "/api/sessions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 5);
}

// ── Steps with timeline query param ──────────────────────

#[tokio::test]
async fn test_get_steps_by_timeline_label() {
    let (app, store, _tmp) = setup();
    let (session, timeline) = seed_session(&store);
    seed_step(&store, &session, &timeline, 1);

    let (status, body) = get_json(
        app,
        &format!("/api/sessions/{}/steps?timeline=main", session.id),
    ).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
}

// ── Context window with Anthropic-style content blocks ───

#[tokio::test]
async fn test_context_window_content_blocks() {
    let (app, store, _tmp) = setup();
    let (session, timeline) = seed_session(&store);

    let request = json!({
        "model": "claude-3",
        "messages": [
            {"role": "user", "content": [
                {"type": "text", "text": "Part one."},
                {"type": "text", "text": "Part two."}
            ]}
        ]
    });
    let response = json!({"content": [{"type": "text", "text": "Reply"}]});

    let s = store.lock().unwrap();
    let req_blob = s.blobs.put_json(&request).unwrap();
    let res_blob = s.blobs.put_json(&response).unwrap();

    let mut step = Step::new_llm_call(&timeline.id, &session.id, 1, "claude-3");
    step.status = StepStatus::Success;
    step.request_blob = req_blob;
    step.response_blob = res_blob;
    s.create_step(&step).unwrap();
    drop(s);

    let (status, body) = get_json(app, &format!("/api/steps/{}", step.id)).await;
    assert_eq!(status, StatusCode::OK);

    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
    assert!(messages[0]["content"].as_str().unwrap().contains("Part one."));
    assert!(messages[0]["content"].as_str().unwrap().contains("Part two."));
}
