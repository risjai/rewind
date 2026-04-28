//! Integration tests for `/api/runners` CRUD endpoints (Phase 3,
//! commit 4/13).
//!
//! Covers:
//! - GET /api/runners — empty + non-empty.
//! - POST /api/runners — register success (raw_token returned ONCE),
//!   503 when crypto key not configured, 400 on bad input,
//!   webhook_url validation, polling-mode requires no url.
//! - GET /api/runners/{id} — success + 404.
//! - DELETE /api/runners/{id} — success + 404 + cascade-on-jobs.
//! - POST /api/runners/{id}/regenerate-token — old hash invalidated,
//!   new hash works, returns new raw token, 404 on missing runner.

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
    Router,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use http_body_util::BodyExt;
use chrono::Utc;
use rewind_store::{ReplayJob, ReplayJobState, Store};
use rewind_web::{crypto::CryptoBox, AppState, HookIngestionState, StoreEvent, WebServer};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

fn setup_with_crypto() -> (Router, Arc<Mutex<Store>>, TempDir) {
    setup_inner(true)
}

fn setup_without_crypto() -> (Router, Arc<Mutex<Store>>, TempDir) {
    setup_inner(false)
}

fn setup_inner(with_crypto: bool) -> (Router, Arc<Mutex<Store>>, TempDir) {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let store = Arc::new(Mutex::new(store));
    let (event_tx, _) = tokio::sync::broadcast::channel::<StoreEvent>(16);
    let crypto = if with_crypto {
        Some(CryptoBox::from_base64_key(&STANDARD.encode([0x42u8; 32])).unwrap())
    } else {
        None
    };
    let state = AppState {
        store: store.clone(),
        event_tx,
        hooks: Arc::new(HookIngestionState::new()),
        otel_config: None,
        auth_token: None,
        crypto,
    };
    let app = Router::new().nest("/api", rewind_web::api_routes(state));
    (app, store, tmp)
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

async fn http_get(app: Router, uri: &str) -> (StatusCode, Value) {
    let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
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

async fn http_delete(app: Router, uri: &str) -> StatusCode {
    let req = Request::builder()
        .method("DELETE")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    app.oneshot(req).await.unwrap().status()
}

// ──────────────────────────────────────────────────────────────────
// LIST
// ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_returns_empty_array_when_no_runners() {
    let (app, _store, _tmp) = setup_with_crypto();
    let (status, body) = http_get(app, "/api/runners").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_array());
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn list_returns_registered_runners_without_token_fields() {
    let (app, _store, _tmp) = setup_with_crypto();

    // Register one runner.
    let (status, _) = json_post(
        app.clone(),
        "/api/runners",
        json!({
            "name": "my-runner",
            "mode": "webhook",
            "webhook_url": "http://1.1.1.1:9999/webhook"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // List should show it without raw_token / encrypted_token / nonce.
    let (status, body) = http_get(app, "/api/runners").await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    let r = &arr[0];
    assert_eq!(r["name"], "my-runner");
    assert_eq!(r["mode"], "webhook");
    assert_eq!(r["webhook_url"], "http://1.1.1.1:9999/webhook");
    assert_eq!(r["status"], "active");
    assert!(r["auth_token_preview"].as_str().unwrap().ends_with("***"));
    // No raw / encrypted token surface in list response.
    assert!(r.get("raw_token").is_none());
    assert!(r.get("encrypted_token").is_none());
    assert!(r.get("token_nonce").is_none());
    assert!(r.get("auth_token_hash").is_none());
}

// ──────────────────────────────────────────────────────────────────
// REGISTER
// ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn register_returns_raw_token_once_and_persists_runner() {
    let (app, store, _tmp) = setup_with_crypto();

    let (status, body) = json_post(
        app.clone(),
        "/api/runners",
        json!({
            "name": "ray-agent",
            "mode": "webhook",
            "webhook_url": "https://1.1.1.1/webhook"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(body["raw_token"].is_string());
    assert!(body["raw_token"].as_str().unwrap().len() >= 32);
    assert!(body["raw_token_warning"]
        .as_str()
        .unwrap()
        .contains("cannot be retrieved"));

    let id = body["runner"]["id"].as_str().unwrap();
    assert!(!id.is_empty());

    // The stored row should have a non-empty encrypted_token + nonce.
    let s = store.lock().unwrap();
    let runner = s.get_runner(id).unwrap().unwrap();
    assert!(!runner.encrypted_token.is_empty());
    assert_eq!(runner.token_nonce.len(), 12);
    // auth_token_hash matches sha256(raw_token).
    let raw = body["raw_token"].as_str().unwrap();
    assert_eq!(runner.auth_token_hash, rewind_web::crypto::hash_runner_token(raw));
    // Preview is "<first 8>***".
    assert!(runner.auth_token_preview.ends_with("***"));
    assert_eq!(runner.auth_token_preview.len(), 8 + 3);
    // get_runner_by_auth_hash succeeds with the raw token.
    let by_hash = s
        .get_runner_by_auth_hash(&rewind_web::crypto::hash_runner_token(raw))
        .unwrap();
    assert!(by_hash.is_some());
    assert_eq!(by_hash.unwrap().id, runner.id);
}

#[tokio::test]
async fn register_returns_503_when_crypto_key_unset() {
    let (app, _store, _tmp) = setup_without_crypto();
    let (status, body) = json_post(
        app,
        "/api/runners",
        json!({
            "name": "doomed",
            "mode": "webhook",
            "webhook_url": "http://1.1.1.1/webhook"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let err = body["error"].as_str().unwrap();
    assert!(err.contains("REWIND_RUNNER_SECRET_KEY"));
    assert!(err.contains("openssl rand"));
}

#[tokio::test]
async fn register_validates_empty_name() {
    let (app, _store, _tmp) = setup_with_crypto();
    let (status, body) = json_post(
        app,
        "/api/runners",
        json!({"name": "   ", "mode": "webhook", "webhook_url": "http://1.1.1.1"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("name"));
}

#[tokio::test]
async fn register_validates_long_name() {
    let (app, _store, _tmp) = setup_with_crypto();
    let too_long: String = "a".repeat(101);
    let (status, body) = json_post(
        app,
        "/api/runners",
        json!({"name": too_long, "mode": "webhook", "webhook_url": "http://1.1.1.1"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("100"));
}

#[tokio::test]
async fn register_webhook_mode_requires_url() {
    let (app, _store, _tmp) = setup_with_crypto();
    let (status, body) = json_post(
        app,
        "/api/runners",
        json!({"name": "r", "mode": "webhook"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("webhook_url is required"));
}

#[tokio::test]
async fn register_webhook_mode_rejects_non_http_url() {
    let (app, _store, _tmp) = setup_with_crypto();
    let (status, body) = json_post(
        app,
        "/api/runners",
        json!({"name": "r", "mode": "webhook", "webhook_url": "ftp://x.com"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("http:// or https://"));
}

/// Review #153 HIGH 1: polling mode is deferred to v3.1; the API
/// must reject it at registration. (Previously this asserted the
/// opposite — that polling registered successfully — which created
/// dispatch-orphan runners.)
#[tokio::test]
async fn register_polling_mode_is_rejected_until_v3_1() {
    let (app, _store, _tmp) = setup_with_crypto();
    let (status, body) = json_post(
        app.clone(),
        "/api/runners",
        json!({"name": "polling-runner", "mode": "polling"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let err = body["error"].as_str().unwrap();
    assert!(err.contains("polling"));
    assert!(err.contains("v3.1"));

    // Same rejection regardless of webhook_url presence — polling is
    // not implemented period.
    let (status, _) = json_post(
        app,
        "/api/runners",
        json!({"name": "polling2", "mode": "polling", "webhook_url": "http://1.1.1.1"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ──────────────────────────────────────────────────────────────────
// GET / DELETE
// ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn get_runner_returns_404_for_unknown_id() {
    let (app, _store, _tmp) = setup_with_crypto();
    let (status, body) = http_get(app, "/api/runners/00000000-0000-0000-0000-000000000000").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].as_str().unwrap().contains("not found"));
}

#[tokio::test]
async fn get_runner_returns_runner_view_for_existing_id() {
    let (app, _store, _tmp) = setup_with_crypto();
    let (_, body) = json_post(
        app.clone(),
        "/api/runners",
        json!({"name": "r1", "mode": "webhook", "webhook_url": "http://1.1.1.1/wh"}),
    )
    .await;
    let id = body["runner"]["id"].as_str().unwrap().to_string();

    let (status, body) = http_get(app, &format!("/api/runners/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], id);
    assert_eq!(body["name"], "r1");
    assert!(body.get("raw_token").is_none(), "GET must not leak raw token");
}

#[tokio::test]
async fn delete_runner_returns_204_then_404() {
    let (app, _store, _tmp) = setup_with_crypto();
    let (_, body) = json_post(
        app.clone(),
        "/api/runners",
        json!({"name": "doomed", "mode": "webhook", "webhook_url": "http://1.1.1.1/wh"}),
    )
    .await;
    let id = body["runner"]["id"].as_str().unwrap().to_string();

    let status = http_delete(app.clone(), &format!("/api/runners/{id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Subsequent GET → 404.
    let (status, _) = http_get(app.clone(), &format!("/api/runners/{id}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Subsequent DELETE → 404 (idempotent on absence).
    let status = http_delete(app, &format!("/api/runners/{id}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ──────────────────────────────────────────────────────────────────
// REGENERATE TOKEN
// ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn regenerate_returns_new_raw_token_and_invalidates_old_hash() {
    let (app, store, _tmp) = setup_with_crypto();
    let (_, register_body) = json_post(
        app.clone(),
        "/api/runners",
        json!({"name": "rotator", "mode": "webhook", "webhook_url": "http://1.1.1.1/wh"}),
    )
    .await;
    let id = register_body["runner"]["id"].as_str().unwrap().to_string();
    let original_token = register_body["raw_token"].as_str().unwrap().to_string();
    let original_hash = rewind_web::crypto::hash_runner_token(&original_token);

    // Old hash is currently retrievable.
    {
        let s = store.lock().unwrap();
        assert!(s.get_runner_by_auth_hash(&original_hash).unwrap().is_some());
    }

    // Rotate.
    let (status, body) = json_post(
        app.clone(),
        &format!("/api/runners/{id}/regenerate-token"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let new_token = body["raw_token"].as_str().unwrap().to_string();
    assert_ne!(new_token, original_token, "rotation must produce a new token");
    let new_hash = rewind_web::crypto::hash_runner_token(&new_token);

    // Old hash → no longer matches anyone.
    {
        let s = store.lock().unwrap();
        assert!(
            s.get_runner_by_auth_hash(&original_hash).unwrap().is_none(),
            "old hash should be invalidated by rotation"
        );
        // New hash → matches our runner.
        let by_hash = s.get_runner_by_auth_hash(&new_hash).unwrap();
        assert!(by_hash.is_some());
        assert_eq!(by_hash.unwrap().id, id);
    }
    // RunnerView in response reflects the new preview.
    let new_preview = body["runner"]["auth_token_preview"].as_str().unwrap();
    let expected_preview = rewind_web::crypto::token_preview(&new_token);
    assert_eq!(new_preview, expected_preview);
}

#[tokio::test]
async fn regenerate_returns_404_for_unknown_runner() {
    let (app, _store, _tmp) = setup_with_crypto();
    let (status, body) = json_post(
        app,
        "/api/runners/00000000-0000-0000-0000-000000000000/regenerate-token",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].as_str().unwrap().contains("not found"));
}

#[tokio::test]
async fn regenerate_returns_503_when_crypto_key_unset() {
    let (app, _store, _tmp) = setup_without_crypto();
    let (status, _) = json_post(
        app,
        "/api/runners/any-id/regenerate-token",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

// ──────────────────────────────────────────────────────────────────
// Integration with crypto: round-trip the encrypted token
// ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn encrypted_token_decrypts_to_the_returned_raw_token() {
    // Phase 3 commit 5 will need this property: the dispatcher reads
    // (encrypted_token, nonce) from the runners row, decrypts under
    // the app key, and uses the plaintext to HMAC-sign outbound
    // webhooks. If decrypt didn't recover the raw token, dispatch
    // would silently sign with garbage.
    let (app, store, _tmp) = setup_with_crypto();
    let (_, body) = json_post(
        app,
        "/api/runners",
        json!({"name": "decryptor", "mode": "webhook", "webhook_url": "http://1.1.1.1/wh"}),
    )
    .await;
    let id = body["runner"]["id"].as_str().unwrap();
    let raw_token = body["raw_token"].as_str().unwrap();

    let s = store.lock().unwrap();
    let runner = s.get_runner(id).unwrap().unwrap();
    drop(s);

    let cb = CryptoBox::from_base64_key(&STANDARD.encode([0x42u8; 32])).unwrap();
    let recovered = cb
        .decrypt(&runner.encrypted_token, &runner.token_nonce)
        .unwrap();
    assert_eq!(recovered.expose(), raw_token);
}

// ──────────────────────────────────────────────────────────────────
// Review #153 round 2 regression coverage
// ──────────────────────────────────────────────────────────────────

// ── HIGH 2: SSRF — webhook_url validation ─────────────────────

/// Loopback IP literal must be rejected to prevent runners pointing
/// the dispatcher at the Rewind server itself.
#[tokio::test]
async fn register_rejects_loopback_webhook_url() {
    let (app, _store, _tmp) = setup_with_crypto();
    for url in [
        "http://127.0.0.1/webhook",
        "http://127.0.0.1:8080/webhook",
        "http://[::1]/webhook",
        "http://localhost/webhook",
    ] {
        let (status, body) = json_post(
            app.clone(),
            "/api/runners",
            json!({"name": "doomed", "mode": "webhook", "webhook_url": url}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "loopback URL {url} should be rejected"
        );
        let err = body["error"].as_str().unwrap_or("");
        assert!(
            err.contains("blocked")
                || err.contains("loopback")
                || err.contains("resolve")
                || err.contains("private"),
            "expected SSRF rejection for {url}, got: {err}"
        );
    }
}

/// RFC 1918 private ranges must be rejected.
#[tokio::test]
async fn register_rejects_private_ip_webhook_url() {
    let (app, _store, _tmp) = setup_with_crypto();
    for url in [
        "http://10.0.0.1/webhook",
        "http://172.16.0.1/webhook",
        "http://192.168.0.1/webhook",
    ] {
        let (status, _) = json_post(
            app.clone(),
            "/api/runners",
            json!({"name": "doomed", "mode": "webhook", "webhook_url": url}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{url} should be rejected");
    }
}

/// 169.254.0.0/16 = link-local + cloud metadata services
/// (AWS/GCP/Azure 169.254.169.254). Critical SSRF target.
#[tokio::test]
async fn register_rejects_cloud_metadata_webhook_url() {
    let (app, _store, _tmp) = setup_with_crypto();
    let (status, body) = json_post(
        app,
        "/api/runners",
        json!({
            "name": "doomed",
            "mode": "webhook",
            "webhook_url": "http://169.254.169.254/latest/meta-data/iam/security-credentials/"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let err = body["error"].as_str().unwrap_or("");
    assert!(err.contains("169.254") || err.contains("link-local") || err.contains("blocked"));
}

/// userinfo (`http://user:pass@host/`) is rejected for credential-
/// leak / SSRF safety. No legitimate webhook target uses it.
#[tokio::test]
async fn register_rejects_webhook_url_with_userinfo() {
    let (app, _store, _tmp) = setup_with_crypto();
    let (status, body) = json_post(
        app,
        "/api/runners",
        json!({
            "name": "doomed",
            "mode": "webhook",
            "webhook_url": "http://admin:secret@1.1.1.1/webhook"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("userinfo"));
}

// ── HIGH 3: rotation blocked while active jobs exist ──────────

/// Helper: insert a session + timeline + replay_context + active
/// job referencing the runner. Uses only public Store API so the
/// test stays at the same boundary the production code uses.
fn seed_active_job(store: &Arc<Mutex<Store>>, runner_id: &str) -> String {
    use rewind_store::{Session, Timeline};
    let s = store.lock().unwrap();

    let session = Session::new("test-active-job");
    let session_id = session.id.clone();
    let timeline = Timeline::new_root(&session_id);
    s.create_session(&session).unwrap();
    s.create_timeline(&timeline).unwrap();

    let ctx_id = Uuid::new_v4().to_string();
    s.create_replay_context(&ctx_id, &session_id, &timeline.id, 0)
        .unwrap();

    let job = ReplayJob {
        id: Uuid::new_v4().to_string(),
        runner_id: Some(runner_id.to_string()),
        session_id: session_id.clone(),
        replay_context_id: Some(ctx_id),
        state: ReplayJobState::Dispatched,
        error_message: None,
        error_stage: None,
        created_at: Utc::now(),
        dispatched_at: Some(Utc::now()),
        started_at: None,
        completed_at: None,
        dispatch_deadline_at: None,
        lease_expires_at: None,
        progress_step: 0,
        progress_total: None,
    };
    let job_id = job.id.clone();
    s.create_replay_job(&job).unwrap();
    job_id
}

#[tokio::test]
async fn regenerate_token_blocked_when_active_jobs_exist() {
    let (app, store, _tmp) = setup_with_crypto();
    let (_, body) = json_post(
        app.clone(),
        "/api/runners",
        json!({"name": "rotator", "mode": "webhook", "webhook_url": "http://1.1.1.1/wh"}),
    )
    .await;
    let runner_id = body["runner"]["id"].as_str().unwrap().to_string();
    let original_hash = rewind_web::crypto::hash_runner_token(body["raw_token"].as_str().unwrap());

    // Seed an active job referencing the runner.
    let _job_id = seed_active_job(&store, &runner_id);

    // Rotation must be refused with 409.
    let (status, error_body) = json_post(
        app,
        &format!("/api/runners/{runner_id}/regenerate-token"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(error_body["error"]
        .as_str()
        .unwrap()
        .contains("non-terminal"));

    // Old hash is STILL valid (rotation didn't happen).
    let s = store.lock().unwrap();
    assert!(
        s.get_runner_by_auth_hash(&original_hash).unwrap().is_some(),
        "rotation must not have invalidated the old token"
    );
}

// ── MEDIUM 4: deletion blocked while active jobs exist ────────

#[tokio::test]
async fn delete_runner_blocked_when_active_jobs_exist() {
    let (app, store, _tmp) = setup_with_crypto();
    let (_, body) = json_post(
        app.clone(),
        "/api/runners",
        json!({"name": "doomed", "mode": "webhook", "webhook_url": "http://1.1.1.1/wh"}),
    )
    .await;
    let runner_id = body["runner"]["id"].as_str().unwrap().to_string();
    let _job_id = seed_active_job(&store, &runner_id);

    let resp = http_delete_with_body(app.clone(), &format!("/api/runners/{runner_id}")).await;
    assert_eq!(resp.0, StatusCode::CONFLICT);
    assert!(resp.1["error"].as_str().unwrap().contains("non-terminal"));

    // Runner still exists.
    let (status, _) = http_get(app, &format!("/api/runners/{runner_id}")).await;
    assert_eq!(status, StatusCode::OK);
}

// `http_delete` returned only StatusCode; for the 409 case we want
// the error body too. New helper.
async fn http_delete_with_body(app: Router, uri: &str) -> (StatusCode, Value) {
    let req = axum::http::Request::builder()
        .method("DELETE")
        .uri(uri)
        .body(axum::body::Body::empty())
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

// ── MEDIUM 5: malformed REWIND_RUNNER_SECRET_KEY ──────────────

/// Malformed key (set but bad base64 / wrong length) must produce
/// an Err from `CryptoBox::from_env`. The server's bootstrap helper
/// then panics (tested via the `WebServer::new_standalone` path
/// that the `bootstrap_crypto` helper uses).
#[tokio::test]
async fn malformed_secret_key_yields_err_from_from_env() {
    let prev = std::env::var(rewind_web::crypto::KEY_ENV_VAR).ok();
    // SAFETY: env var swap is unsafe in 2024 edition; we restore.
    unsafe {
        std::env::set_var(rewind_web::crypto::KEY_ENV_VAR, "not-valid-base64!!!");
    }
    let result = rewind_web::crypto::CryptoBox::from_env();
    // restore before assertions to avoid leaking on panic
    unsafe {
        match prev {
            Some(v) => std::env::set_var(rewind_web::crypto::KEY_ENV_VAR, v),
            None => std::env::remove_var(rewind_web::crypto::KEY_ENV_VAR),
        }
    }
    let err = result.expect_err("malformed key must Err, not silently log");
    let msg = err.to_string();
    assert!(
        msg.contains("not valid base64") || msg.contains("expected"),
        "expected explicit malformed-key error, got: {msg}"
    );
}

// ── LOW 6: full-router auth middleware coverage ───────────────

/// Mirror the `auth_tests::spawn_server_with_token` helper so we
/// can prove `/api/runners` is gated by the production
/// `auth_middleware`. The `setup_with_crypto` helper above mounts
/// `api_routes` directly without the middleware (sufficient for
/// behavior tests but doesn't exercise the protected stack).
async fn spawn_server_with_token_and_crypto(
    token: Option<String>,
) -> (std::net::SocketAddr, TempDir) {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    // Bind a port and drop the listener; let WebServer rebind.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    // The crypto key is set globally per process; another test may
    // rely on its absence. Set it for this spawn and rely on the
    // `bootstrap_crypto` helper picking it up. NOTE: this is racy
    // across tests but we accept it because failing-startup tests
    // explicitly clear/restore.
    unsafe {
        std::env::set_var(
            rewind_web::crypto::KEY_ENV_VAR,
            STANDARD.encode([0x42u8; 32]),
        );
    }

    let server = WebServer::new_standalone(store).with_auth_token(token);
    tokio::spawn(async move {
        let _ = server.run(addr).await;
    });

    let probe = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(200))
        .build()
        .unwrap();
    for _ in 0..60 {
        if let Ok(resp) = probe
            .get(format!("http://{addr}/_rewind/health"))
            .send()
            .await
            && resp.status().is_success()
        {
            return (addr, tmp);
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("server did not start on {addr}");
}

#[tokio::test]
async fn api_runners_is_gated_by_auth_middleware_in_production_routing() {
    let token = "review-153-bearer-token-secret".to_string();
    let (addr, _tmp) = spawn_server_with_token_and_crypto(Some(token.clone())).await;

    let client = reqwest::Client::new();

    // Without the bearer token → 401 (auth middleware rejects).
    let resp = client
        .get(format!("http://{addr}/api/runners"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "/api/runners must be protected by auth_middleware in production routing"
    );

    // With the right bearer token → 200 (protected route is reachable).
    let resp = client
        .get(format!("http://{addr}/api/runners"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // POST /api/runners is also gated.
    let resp = client
        .post(format!("http://{addr}/api/runners"))
        .json(&json!({"name": "x", "mode": "webhook", "webhook_url": "http://1.1.1.1/wh"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "POST /api/runners must be protected"
    );

    // DELETE too.
    let resp = client
        .delete(format!("http://{addr}/api/runners/some-id"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}
