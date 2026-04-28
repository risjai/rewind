//! Integration tests for replay-job dispatch + event-ingestion HTTP
//! endpoints (Phase 3, commit 6/13).
//!
//! Uses the same direct-mount pattern as `runners_tests.rs`. The
//! dispatcher's outbound webhook is exercised against a tokio
//! TcpListener stub spun up per test.

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
    Router,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::Utc;
use http_body_util::BodyExt;
use rewind_store::{ReplayJob, ReplayJobState, Session, Store, Timeline};
use rewind_web::{
    crypto::CryptoBox, dispatcher::Dispatcher, runners::runner_callback_routes, AppState,
    HookIngestionState, StoreEvent,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

fn enable_loopback_webhook_dev_escape_once() {
    use std::sync::Once;
    static SET: Once = Once::new();
    SET.call_once(|| {
        // Review #154 F6: dispatch-time SSRF check would reject the
        // localhost stub URLs these tests POST to. Set the dev
        // escape env var once for the entire test process.
        // SAFETY: env var swap is unsafe in the 2024 edition; the
        // Once guard ensures it happens before any dispatch runs.
        unsafe {
            std::env::set_var("REWIND_ALLOW_LOOPBACK_WEBHOOKS", "1");
        }
    });
}

fn setup() -> (Router, Router, Arc<Mutex<Store>>, TempDir) {
    enable_loopback_webhook_dev_escape_once();
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let store = Arc::new(Mutex::new(store));
    let (event_tx, _) = tokio::sync::broadcast::channel::<StoreEvent>(64);
    let crypto = Some(CryptoBox::from_base64_key(&STANDARD.encode([0x42u8; 32])).unwrap());
    let dispatcher = crypto.clone().and_then(|c| {
        Dispatcher::new(c, "http://127.0.0.1:4800".to_string()).ok()
    });
    let state = AppState {
        store: store.clone(),
        event_tx,
        hooks: Arc::new(HookIngestionState::new()),
        otel_config: None,
        auth_token: None,
        crypto,
        dispatcher,
        base_url: "http://127.0.0.1:4800".to_string(),
    };
    let api = Router::new().nest("/api", rewind_web::api_routes(state.clone()));
    let callbacks = runner_callback_routes().with_state(state);
    (api, callbacks, store, tmp)
}

async fn json_post(app: Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, body)
}

async fn json_post_with_header(
    app: Router,
    uri: &str,
    body: Value,
    header_name: &str,
    header_value: &str,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header_name, header_value)
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, body)
}

/// Spawn a stub HTTP server on 127.0.0.1 that always replies 202.
/// Returns the URL the dispatcher will POST to.
async fn spawn_runner_stub_accepting() -> (String, tokio::sync::mpsc::Receiver<axum::http::HeaderMap>) {
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let app = axum::Router::new().route(
        "/wh",
        axum::routing::post(move |headers: axum::http::HeaderMap| {
            let tx = tx.clone();
            async move {
                let _ = tx.send(headers).await;
                StatusCode::ACCEPTED
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/wh"), rx)
}

async fn spawn_runner_stub_returning_500() -> String {
    let app = axum::Router::new().route(
        "/wh",
        axum::routing::post(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/wh")
}

/// Register a runner directly via the Store (bypasses the HTTP
/// SSRF guard so tests can target a localhost stub server).
/// Returns (id, raw_token).
fn register_runner(store: &Arc<Mutex<Store>>, webhook_url: &str) -> (String, String) {
    use rewind_store::{Runner, RunnerMode, RunnerStatus};
    let cb = CryptoBox::from_base64_key(&STANDARD.encode([0x42u8; 32])).unwrap();
    let raw_token = rewind_web::crypto::generate_runner_token();
    let nonce = CryptoBox::fresh_nonce();
    let encrypted = cb.encrypt(raw_token.expose().as_bytes(), &nonce).unwrap();
    let runner = Runner {
        id: Uuid::new_v4().to_string(),
        name: format!("test-runner-{}", Uuid::new_v4()),
        mode: RunnerMode::Webhook,
        webhook_url: Some(webhook_url.to_string()),
        encrypted_token: encrypted,
        token_nonce: nonce.to_vec(),
        auth_token_hash: rewind_web::crypto::hash_runner_token(raw_token.expose()),
        auth_token_preview: rewind_web::crypto::token_preview(raw_token.expose()),
        created_at: Utc::now(),
        last_seen_at: None,
        status: RunnerStatus::Active,
    };
    let id = runner.id.clone();
    let s = store.lock().unwrap();
    s.create_runner(&runner).unwrap();
    (id, raw_token.expose().to_string())
}

/// Seed session + timeline + replay context.
fn seed_session_and_context(store: &Arc<Mutex<Store>>) -> (String, String, String) {
    let s = store.lock().unwrap();
    let session = Session::new("dispatch-test-session");
    let session_id = session.id.clone();
    let timeline = Timeline::new_root(&session_id);
    s.create_session(&session).unwrap();
    s.create_timeline(&timeline).unwrap();
    let ctx_id = Uuid::new_v4().to_string();
    s.create_replay_context(&ctx_id, &session_id, &timeline.id, 0)
        .unwrap();
    (session_id, timeline.id, ctx_id)
}

// ──────────────────────────────────────────────────────────────────
// POST /api/sessions/{id}/replay-jobs (shape B: reuse-context)
// ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn create_replay_job_shape_b_dispatches_to_runner_and_transitions_to_dispatched() {
    let (api, _callbacks, store, _tmp) = setup();
    let (webhook_url, mut rx) = spawn_runner_stub_accepting().await;
    let (runner_id, _raw) = register_runner(&store, &webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    let (status, body) = json_post(
        api.clone(),
        &format!("/api/sessions/{session_id}/replay-jobs"),
        json!({"runner_id": runner_id, "replay_context_id": ctx_id}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body:?}");
    let job_id = body["job_id"].as_str().unwrap().to_string();
    // Review #154 round 2 BLOCKER fix: dispatch endpoint now
    // transitions pending → dispatched synchronously before the
    // tokio::spawn so runner callbacks always find the job in
    // `dispatched` state. Pre-fix this asserted "pending".
    assert_eq!(body["state"], "dispatched");
    assert_eq!(body["replay_context_id"], ctx_id);
    // Review #154 F2: shape B also returns the context's timeline id
    // (so the dashboard / SDK can resolve `_timeline_id` correctly).
    // Pre-#154 this was null for shape B.
    assert!(body["fork_timeline_id"].is_string());

    // Wait for the runner stub to receive the dispatch call.
    let headers =
        tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .expect("dispatcher must call the runner within 3s")
            .expect("stub channel closed");
    assert!(headers.contains_key("x-rewind-job-id"));
    let sig = headers
        .get("x-rewind-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap();
    assert!(sig.starts_with("sha256="), "signature header malformed: {sig}");

    // Review #154 round 2: state is now flipped to Dispatched
    // SYNCHRONOUSLY before tokio::spawn, so the snapshot already
    // shows Dispatched. The deadline + lease are set asynchronously
    // by apply_outcome AFTER the dispatcher's HTTP call settles —
    // we wait for those instead of for the state transition itself.
    // Increased the poll budget to absorb parallel-test contention.
    for _ in 0..120 {
        let snapshot = {
            let s = store.lock().unwrap();
            s.get_replay_job(&job_id).unwrap().unwrap()
        };
        if matches!(snapshot.state, ReplayJobState::Dispatched)
            && snapshot.dispatch_deadline_at.is_some()
            && snapshot.lease_expires_at.is_some()
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("dispatcher's apply_outcome never set deadline/lease");
}

#[tokio::test]
async fn create_replay_job_with_runner_500_response_transitions_to_errored() {
    let (api, _callbacks, store, _tmp) = setup();
    let webhook_url = spawn_runner_stub_returning_500().await;
    let (runner_id, _raw) = register_runner(&store, &webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    let (status, body) = json_post(
        api.clone(),
        &format!("/api/sessions/{session_id}/replay-jobs"),
        json!({"runner_id": runner_id, "replay_context_id": ctx_id}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let job_id = body["job_id"].as_str().unwrap().to_string();

    for _ in 0..30 {
        let snapshot = {
            let s = store.lock().unwrap();
            s.get_replay_job(&job_id).unwrap().unwrap()
        };
        if matches!(snapshot.state, ReplayJobState::Errored) {
            assert_eq!(snapshot.error_stage.as_deref(), Some("dispatch"));
            assert!(snapshot
                .error_message
                .unwrap_or_default()
                .contains("HTTP 500"));
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("job never reached Errored state");
}

#[tokio::test]
async fn create_replay_job_rejects_context_in_use() {
    let (api, _callbacks, store, _tmp) = setup();
    let (webhook_url, _rx) = spawn_runner_stub_accepting().await;
    let (runner_id, _raw) = register_runner(&store, &webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    // Manually insert an in-flight job referencing the context.
    {
        let s = store.lock().unwrap();
        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: Some(runner_id.clone()),
            session_id: session_id.clone(),
            replay_context_id: Some(ctx_id.clone()),
            state: ReplayJobState::Dispatched,
            error_message: None,
            error_stage: None,
            created_at: Utc::now(),
            dispatched_at: Some(Utc::now()),
            started_at: None,
            completed_at: None,
            dispatch_deadline_at: Some(Utc::now() + chrono::Duration::seconds(60)),
            lease_expires_at: Some(Utc::now() + chrono::Duration::seconds(300)),
            progress_step: 0,
            progress_total: None,
        };
        s.create_replay_job(&job).unwrap();
    }

    let (status, body) = json_post(
        api,
        &format!("/api/sessions/{session_id}/replay-jobs"),
        json!({"runner_id": runner_id, "replay_context_id": ctx_id}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("in-flight"));
}

// ── Review #154 round 2: dispatch race ─────────────────────────

/// Pre-fix: the dispatcher's apply_outcome flipped state pending→
/// dispatched AFTER the HTTP POST returned. Fast runners that
/// emitted `started` immediately on receiving the webhook would
/// hit the state-machine guard from #152 round 2
/// (Pending → Started is illegal) and the runner would see a 409.
/// Post-fix: state is advanced to `dispatched` SYNCHRONOUSLY
/// before the tokio::spawn, so the runner can call back at any
/// time after receiving the webhook with the correct state visible.
#[tokio::test]
async fn create_replay_job_response_state_is_dispatched_not_pending() {
    let (api, _callbacks, store, _tmp) = setup();
    let (webhook_url, _rx) = spawn_runner_stub_accepting().await;
    let (runner_id, _raw) = register_runner(&store, &webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    let (status, body) = json_post(
        api,
        &format!("/api/sessions/{session_id}/replay-jobs"),
        json!({"runner_id": runner_id, "replay_context_id": ctx_id}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["state"], "dispatched", "pre-spawn transition not applied");

    // The job in the store must be in dispatched state IMMEDIATELY
    // after the response; no async wait required.
    let s = store.lock().unwrap();
    let job = s.get_replay_job(body["job_id"].as_str().unwrap()).unwrap().unwrap();
    assert_eq!(
        job.state,
        rewind_store::ReplayJobState::Dispatched,
        "store row must already be dispatched (race window closed)"
    );
}

/// Defensive regression for the round-2 follow-up: the failure-path
/// `dispatched → errored` transition must NOT clobber an
/// `in_progress` job whose runner already raced through `started`
/// before the dispatcher's HTTP call returned. Uses the strict
/// `mark_dispatched_job_as_errored` helper so the UPDATE is a no-op
/// on non-`dispatched` rows.
#[tokio::test]
async fn failed_dispatch_does_not_clobber_already_in_progress_job() {
    use rewind_store::ReplayJob;
    let (_api, _callbacks, store, _tmp) = setup();
    let (webhook_url, _rx) = spawn_runner_stub_accepting().await;
    let (runner_id, _raw) = register_runner(&store, &webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    // Manually insert an in_progress job (simulates: pre-spawn
    // advanced to dispatched, runner immediately emitted started,
    // SQL guard accepted the started → in_progress transition,
    // then the dispatch HTTP call failed and apply_outcome ran).
    let job_id = {
        let s = store.lock().unwrap();
        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: Some(runner_id.clone()),
            session_id: session_id.clone(),
            replay_context_id: Some(ctx_id.clone()),
            state: ReplayJobState::InProgress, // ← already started!
            error_message: None,
            error_stage: None,
            created_at: Utc::now(),
            dispatched_at: Some(Utc::now()),
            started_at: Some(Utc::now()),
            completed_at: None,
            dispatch_deadline_at: Some(Utc::now() + chrono::Duration::seconds(60)),
            lease_expires_at: Some(Utc::now() + chrono::Duration::seconds(300)),
            progress_step: 1,
            progress_total: Some(5),
        };
        let id = job.id.clone();
        s.create_replay_job(&job).unwrap();
        id
    };

    // Simulate the failure-path apply_outcome firing.
    let updated = {
        let s = store.lock().unwrap();
        s.mark_dispatched_job_as_errored(
            &job_id,
            "fake dispatch HTTP error after runner started",
            "dispatch",
        )
        .unwrap()
    };
    assert!(
        !updated,
        "mark_dispatched_job_as_errored must be a no-op on in_progress rows"
    );

    // Confirm the job is still in_progress with started_at intact.
    let s = store.lock().unwrap();
    let after = s.get_replay_job(&job_id).unwrap().unwrap();
    assert_eq!(after.state, ReplayJobState::InProgress);
    assert!(after.started_at.is_some());
    assert!(after.error_message.is_none());
}

/// Pre-fix race scenario simulation: a runner that posts `started`
/// the moment it receives the dispatch must NOT be rejected by the
/// state machine. The pre-spawn transition guarantees the job is
/// dispatched before the runner can possibly call back.
#[tokio::test]
async fn started_event_accepted_immediately_after_dispatch_response() {
    let (api, callbacks, store, _tmp) = setup();
    let (webhook_url, _rx) = spawn_runner_stub_accepting().await;
    let (runner_id, raw) = register_runner(&store, &webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    let (status, body) = json_post(
        api,
        &format!("/api/sessions/{session_id}/replay-jobs"),
        json!({"runner_id": runner_id, "replay_context_id": ctx_id}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let job_id = body["job_id"].as_str().unwrap().to_string();

    // Race simulation: post `started` event IMMEDIATELY without
    // waiting for the dispatcher's apply_outcome. Pre-fix this
    // returned 409 (state was still pending). Post-fix this
    // returns 202 (state is dispatched).
    let (event_status, event_body) = json_post_with_header(
        callbacks,
        &format!("/api/replay-jobs/{job_id}/events"),
        json!({"event_type": "started"}),
        "X-Rewind-Runner-Auth",
        &raw,
    )
    .await;
    assert_eq!(
        event_status,
        StatusCode::ACCEPTED,
        "started event must be accepted immediately (race fixed): body={event_body:?}"
    );
    assert_eq!(event_body["accepted"], true);
    assert_eq!(event_body["state"], "in_progress");
}

// ── Review #154 F1: strict_match must propagate to the context ──

#[tokio::test]
async fn shape_a_strict_match_true_sets_replay_context_strict_flag() {
    let (api, _callbacks, store, _tmp) = setup();
    let (webhook_url, _rx) = spawn_runner_stub_accepting().await;
    let (runner_id, _raw) = register_runner(&store, &webhook_url);
    // Need a session with at least one step to fork from.
    let (session_id, root_timeline_id) = seed_session_with_step(&store);

    let (status, body) = json_post(
        api.clone(),
        &format!("/api/sessions/{session_id}/replay-jobs"),
        json!({
            "runner_id": runner_id,
            "source_timeline_id": root_timeline_id,
            "at_step": 1,
            "strict_match": true,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body:?}");

    let ctx_id = body["replay_context_id"].as_str().unwrap();
    let s = store.lock().unwrap();
    let ctx = s.get_replay_context(ctx_id).unwrap().unwrap();
    assert!(
        ctx.strict_match,
        "F1: strict_match=true on the request must set the context's strict_match flag"
    );
}

#[tokio::test]
async fn shape_a_strict_match_omitted_defaults_to_warn_only() {
    let (api, _callbacks, store, _tmp) = setup();
    let (webhook_url, _rx) = spawn_runner_stub_accepting().await;
    let (runner_id, _raw) = register_runner(&store, &webhook_url);
    let (session_id, root_timeline_id) = seed_session_with_step(&store);

    let (status, body) = json_post(
        api,
        &format!("/api/sessions/{session_id}/replay-jobs"),
        json!({
            "runner_id": runner_id,
            "source_timeline_id": root_timeline_id,
            "at_step": 1,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let ctx_id = body["replay_context_id"].as_str().unwrap();
    let s = store.lock().unwrap();
    let ctx = s.get_replay_context(ctx_id).unwrap().unwrap();
    assert!(!ctx.strict_match);
}

// ── Review #154 F3: Shape B cursor + TTL validation ──

#[tokio::test]
async fn shape_b_rejects_context_whose_cursor_already_advanced() {
    let (api, _callbacks, store, _tmp) = setup();
    let (webhook_url, _rx) = spawn_runner_stub_accepting().await;
    let (runner_id, _raw) = register_runner(&store, &webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    // Pre-advance the cursor (simulates a previous job that ran
    // partway through the recording).
    {
        let s = store.lock().unwrap();
        let _step = s.advance_replay_context(&ctx_id).unwrap();
    }

    let (status, body) = json_post(
        api,
        &format!("/api/sessions/{session_id}/replay-jobs"),
        json!({"runner_id": runner_id, "replay_context_id": ctx_id}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let err = body["error"].as_str().unwrap();
    assert!(err.contains("cursor already advanced"), "got: {err}");
}

#[tokio::test]
async fn shape_b_rejects_expired_context() {
    let (api, _callbacks, store, _tmp) = setup();
    let (webhook_url, _rx) = spawn_runner_stub_accepting().await;
    let (runner_id, _raw) = register_runner(&store, &webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    // Backdate last_accessed_at past the TTL window via the test-only
    // helper (review #154 F3).
    {
        let s = store.lock().unwrap();
        let very_old = chrono::Utc::now() - chrono::Duration::seconds(7200);
        s._test_set_replay_context_last_accessed(&ctx_id, very_old)
            .unwrap();
    }

    let (status, body) = json_post(
        api,
        &format!("/api/sessions/{session_id}/replay-jobs"),
        json!({"runner_id": runner_id, "replay_context_id": ctx_id}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let err = body["error"].as_str().unwrap();
    assert!(err.contains("TTL"), "got: {err}");
}

// Helper: session with one recorded step so Shape A's fork(at_step=1)
// has something to fork from.
fn seed_session_with_step(store: &Arc<Mutex<Store>>) -> (String, String) {
    use rewind_store::{Session, SessionSource, SessionStatus, Step, StepStatus, Timeline};
    let s = store.lock().unwrap();
    let session = Session {
        id: Uuid::new_v4().to_string(),
        name: "f1-f3-test".into(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        status: SessionStatus::Recording,
        source: SessionSource::Hooks,
        total_steps: 0,
        total_tokens: 0,
        metadata: serde_json::json!({}),
        thread_id: None,
        thread_ordinal: None,
    };
    let timeline = Timeline::new_root(&session.id);
    s.create_session(&session).unwrap();
    s.create_timeline(&timeline).unwrap();
    let mut step = Step::new_llm_call(&timeline.id, &session.id, 1, "stub-model");
    step.status = StepStatus::Success;
    step.duration_ms = 10;
    s.create_step(&step).unwrap();
    s.update_session_stats(&session.id, 1, 0).unwrap();
    (session.id, timeline.id)
}

#[tokio::test]
async fn create_replay_job_rejects_unknown_session() {
    let (api, _callbacks, _store, _tmp) = setup();
    let (status, body) = json_post(
        api,
        "/api/sessions/00000000-0000-0000-0000-000000000000/replay-jobs",
        json!({"runner_id": "x", "replay_context_id": "y"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].as_str().unwrap().contains("session"));
}

#[tokio::test]
async fn create_replay_job_rejects_inactive_runner() {
    let (api, _callbacks, store, _tmp) = setup();
    let (webhook_url, _rx) = spawn_runner_stub_accepting().await;
    let (runner_id, _raw) = register_runner(&store, &webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    {
        let s = store.lock().unwrap();
        s.set_runner_status(&runner_id, rewind_store::RunnerStatus::Disabled)
            .unwrap();
    }

    let (status, body) = json_post(
        api,
        &format!("/api/sessions/{session_id}/replay-jobs"),
        json!({"runner_id": runner_id, "replay_context_id": ctx_id}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("Disabled") || body["error"].as_str().unwrap().contains("disabled"));
}

// ──────────────────────────────────────────────────────────────────
// POST /api/replay-jobs/{id}/events  (runner callbacks)
// ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn event_endpoint_rejects_missing_runner_auth_header() {
    let (_api, callbacks, _store, _tmp) = setup();
    let (status, body) = json_post(
        callbacks,
        "/api/replay-jobs/x/events",
        json!({"event_type": "started"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body["error"].as_str().unwrap().contains("X-Rewind-Runner-Auth"));
}

#[tokio::test]
async fn event_endpoint_rejects_unknown_runner_token() {
    let (_api, callbacks, _store, _tmp) = setup();
    let (status, body) = json_post_with_header(
        callbacks,
        "/api/replay-jobs/x/events",
        json!({"event_type": "started"}),
        "X-Rewind-Runner-Auth",
        "totally-not-a-valid-token",
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body["error"].as_str().unwrap().contains("invalid"));
}

#[tokio::test]
async fn event_endpoint_rejects_runner_that_does_not_own_job() {
    let (_api, callbacks, store, _tmp) = setup();
    let (webhook_url, _rx) = spawn_runner_stub_accepting().await;
    // Register two runners; the job belongs to runner_a, runner_b
    // tries to post events.
    let (runner_a, _) = register_runner(&store, &webhook_url);
    let (_runner_b, raw_b) = register_runner(&store, &webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    let job_id = {
        let s = store.lock().unwrap();
        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: Some(runner_a),
            session_id,
            replay_context_id: Some(ctx_id),
            state: ReplayJobState::Dispatched,
            error_message: None,
            error_stage: None,
            created_at: Utc::now(),
            dispatched_at: Some(Utc::now()),
            started_at: None,
            completed_at: None,
            dispatch_deadline_at: Some(Utc::now() + chrono::Duration::seconds(60)),
            lease_expires_at: Some(Utc::now() + chrono::Duration::seconds(300)),
            progress_step: 0,
            progress_total: None,
        };
        let id = job.id.clone();
        s.create_replay_job(&job).unwrap();
        id
    };

    let (status, body) = json_post_with_header(
        callbacks,
        &format!("/api/replay-jobs/{job_id}/events"),
        json!({"event_type": "started"}),
        "X-Rewind-Runner-Auth",
        &raw_b, // runner_b's token
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body["error"].as_str().unwrap().contains("does not own"));
}

#[tokio::test]
async fn event_endpoint_started_transitions_dispatched_to_in_progress() {
    let (_api, callbacks, store, _tmp) = setup();
    let (webhook_url, _rx) = spawn_runner_stub_accepting().await;
    let (runner_id, raw) = register_runner(&store, &webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    let job_id = {
        let s = store.lock().unwrap();
        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: Some(runner_id),
            session_id,
            replay_context_id: Some(ctx_id),
            state: ReplayJobState::Dispatched,
            error_message: None,
            error_stage: None,
            created_at: Utc::now(),
            dispatched_at: Some(Utc::now()),
            started_at: None,
            completed_at: None,
            dispatch_deadline_at: Some(Utc::now() + chrono::Duration::seconds(60)),
            lease_expires_at: Some(Utc::now() + chrono::Duration::seconds(300)),
            progress_step: 0,
            progress_total: None,
        };
        let id = job.id.clone();
        s.create_replay_job(&job).unwrap();
        id
    };

    let (status, body) = json_post_with_header(
        callbacks,
        &format!("/api/replay-jobs/{job_id}/events"),
        json!({"event_type": "started"}),
        "X-Rewind-Runner-Auth",
        &raw,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["accepted"], true);
    assert_eq!(body["state"], "in_progress");

    let s = store.lock().unwrap();
    let after = s.get_replay_job(&job_id).unwrap().unwrap();
    assert_eq!(after.state, ReplayJobState::InProgress);
    assert!(after.started_at.is_some());
}

#[tokio::test]
async fn event_endpoint_progress_updates_step_no_state_change() {
    let (_api, callbacks, store, _tmp) = setup();
    let (webhook_url, _rx) = spawn_runner_stub_accepting().await;
    let (runner_id, raw) = register_runner(&store, &webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    let job_id = {
        let s = store.lock().unwrap();
        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: Some(runner_id),
            session_id,
            replay_context_id: Some(ctx_id),
            state: ReplayJobState::InProgress,
            error_message: None,
            error_stage: None,
            created_at: Utc::now(),
            dispatched_at: Some(Utc::now()),
            started_at: Some(Utc::now()),
            completed_at: None,
            dispatch_deadline_at: None,
            lease_expires_at: Some(Utc::now() + chrono::Duration::seconds(300)),
            progress_step: 0,
            progress_total: None,
        };
        let id = job.id.clone();
        s.create_replay_job(&job).unwrap();
        id
    };

    let (status, _body) = json_post_with_header(
        callbacks,
        &format!("/api/replay-jobs/{job_id}/events"),
        json!({"event_type": "progress", "step_number": 7, "progress_total": 20}),
        "X-Rewind-Runner-Auth",
        &raw,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let s = store.lock().unwrap();
    let after = s.get_replay_job(&job_id).unwrap().unwrap();
    assert_eq!(after.state, ReplayJobState::InProgress);
    assert_eq!(after.progress_step, 7);
    assert_eq!(after.progress_total, Some(20));
}

#[tokio::test]
async fn event_endpoint_completed_transitions_to_completed() {
    let (_api, callbacks, store, _tmp) = setup();
    let (webhook_url, _rx) = spawn_runner_stub_accepting().await;
    let (runner_id, raw) = register_runner(&store, &webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    let job_id = {
        let s = store.lock().unwrap();
        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: Some(runner_id),
            session_id,
            replay_context_id: Some(ctx_id),
            state: ReplayJobState::InProgress,
            error_message: None,
            error_stage: None,
            created_at: Utc::now(),
            dispatched_at: Some(Utc::now()),
            started_at: Some(Utc::now()),
            completed_at: None,
            dispatch_deadline_at: None,
            lease_expires_at: Some(Utc::now() + chrono::Duration::seconds(300)),
            progress_step: 5,
            progress_total: Some(5),
        };
        let id = job.id.clone();
        s.create_replay_job(&job).unwrap();
        id
    };

    let (status, _body) = json_post_with_header(
        callbacks,
        &format!("/api/replay-jobs/{job_id}/events"),
        json!({"event_type": "completed"}),
        "X-Rewind-Runner-Auth",
        &raw,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let s = store.lock().unwrap();
    let after = s.get_replay_job(&job_id).unwrap().unwrap();
    assert_eq!(after.state, ReplayJobState::Completed);
}

#[tokio::test]
async fn event_endpoint_returns_409_on_illegal_transition() {
    // Progress on Pending is illegal (review #152 round 2 state
    // machine). The atomic helper rejects, the endpoint returns 409.
    let (_api, callbacks, store, _tmp) = setup();
    let (webhook_url, _rx) = spawn_runner_stub_accepting().await;
    let (runner_id, raw) = register_runner(&store, &webhook_url);
    let (session_id, _, ctx_id) = seed_session_and_context(&store);

    let job_id = {
        let s = store.lock().unwrap();
        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: Some(runner_id),
            session_id,
            replay_context_id: Some(ctx_id),
            state: ReplayJobState::Pending, // ← never dispatched
            error_message: None,
            error_stage: None,
            created_at: Utc::now(),
            dispatched_at: None,
            started_at: None,
            completed_at: None,
            dispatch_deadline_at: None,
            lease_expires_at: None,
            progress_step: 0,
            progress_total: None,
        };
        let id = job.id.clone();
        s.create_replay_job(&job).unwrap();
        id
    };

    let (status, body) = json_post_with_header(
        callbacks,
        &format!("/api/replay-jobs/{job_id}/events"),
        json!({"event_type": "progress", "step_number": 1}),
        "X-Rewind-Runner-Auth",
        &raw,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["accepted"], false);
    assert!(body["reason"]
        .as_str()
        .unwrap()
        .contains("state machine"));
}
