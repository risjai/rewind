//! Runner registry HTTP endpoints (Phase 3, commit 4/13).
//!
//! Wires REST CRUD on top of `rewind-store`'s runner CRUD methods,
//! using the [`crypto::CryptoBox`](crate::crypto::CryptoBox) to
//! encrypt the auth token at rest under `REWIND_RUNNER_SECRET_KEY`.
//!
//! ## Routes (mounted at `/api`)
//!
//! | method | path                                  | purpose                                   |
//! |--------|---------------------------------------|-------------------------------------------|
//! | GET    | `/api/runners`                        | list runners (no tokens)                  |
//! | POST   | `/api/runners`                        | register; returns raw token ONCE          |
//! | GET    | `/api/runners/{id}`                   | get one runner (no tokens)                |
//! | DELETE | `/api/runners/{id}`                   | remove runner (cascades runner_id→NULL)   |
//! | POST   | `/api/runners/{id}/regenerate-token`  | rotate token; returns new raw token ONCE  |
//!
//! ## Bootstrap behavior
//!
//! All write endpoints (POST/DELETE/regenerate) return `503 Service
//! Unavailable` if `REWIND_RUNNER_SECRET_KEY` is unset, with a clear
//! `error: "REWIND_RUNNER_SECRET_KEY env var is not set..."` body.
//! Read endpoints (GET) work without crypto since they don't touch
//! ciphertext.
//!
//! ## Token surface
//!
//! - **Raw token** is generated server-side at register / regenerate
//!   and returned in the JSON body **exactly once**. Never persisted
//!   in plaintext, never logged, never re-readable from the store.
//! - **`auth_token_hash`** (SHA-256 hex) is stored for the fast
//!   inbound-auth lookup path (commit 6's event ingestion).
//! - **`auth_token_preview`** (`<first 8 chars>***`) is shown in the
//!   dashboard so operators can identify which token they hold.
//! - **`encrypted_token` + `token_nonce`** are stored for the dispatch
//!   path (commit 5's webhook signing) — decrypted on demand, never
//!   in-memory cached, never re-serialized through the API.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use chrono::Utc;
use rewind_replay::ReplayEngine;
use rewind_store::{
    ReplayJob, ReplayJobEvent, ReplayJobEventType, ReplayJobState, Runner, RunnerMode,
    RunnerStatus,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::crypto::{self, CryptoBox};
use crate::{AppState, StoreEvent};

/// Build the runners + replay-job-create sub-router (bearer-protected).
/// Caller should `.nest("/api", ...)` it alongside the existing
/// `api::routes`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/runners", get(list_runners).post(register_runner))
        .route(
            "/runners/{id}",
            get(get_runner).delete(remove_runner),
        )
        .route(
            "/runners/{id}/regenerate-token",
            post(regenerate_token),
        )
        // Phase 3 commit 6: dashboard initiates a replay job for a
        // session. Bearer-protected (dashboard action).
        .route(
            "/sessions/{sid}/replay-jobs",
            post(create_replay_job).get(list_replay_jobs_for_session),
        )
        .route(
            "/replay-jobs/{id}",
            get(get_replay_job).delete(cancel_replay_job_handler),
        )
}

/// Runner-callback routes mount OUTSIDE the bearer-auth middleware
/// because runners authenticate with `X-Rewind-Runner-Auth` and may
/// not even know the operator's bearer token. The handler enforces
/// its own auth + ownership check.
///
/// Mounted at the top-level router in [`crate::WebServer::build_router`].
pub fn runner_callback_routes() -> Router<AppState> {
    Router::new().route(
        "/api/replay-jobs/{id}/events",
        post(post_replay_job_event),
    )
}

// ──────────────────────────────────────────────────────────────────
// Public response shapes (no token leakage)
// ──────────────────────────────────────────────────────────────────

/// Public view of a runner. Excludes ciphertext, nonce, and any
/// derivative of the raw token beyond the operator-visible preview.
#[derive(Debug, Serialize, Clone)]
pub struct RunnerView {
    pub id: String,
    pub name: String,
    pub mode: String,
    pub webhook_url: Option<String>,
    pub auth_token_preview: String,
    pub status: String,
    pub created_at: String,
    pub last_seen_at: Option<String>,
}

impl From<Runner> for RunnerView {
    fn from(r: Runner) -> Self {
        Self {
            id: r.id,
            name: r.name,
            mode: r.mode.as_str().to_string(),
            webhook_url: r.webhook_url,
            auth_token_preview: r.auth_token_preview,
            status: r.status.as_str().to_string(),
            created_at: r.created_at.to_rfc3339(),
            last_seen_at: r.last_seen_at.map(|t| t.to_rfc3339()),
        }
    }
}

/// Returned ONLY at register / regenerate. The raw token is never
/// retrievable after this response.
#[derive(Debug, Serialize)]
pub struct RegisterRunnerResponse {
    pub runner: RunnerView,
    /// The raw auth token. Save this NOW — it cannot be retrieved
    /// after this response. Used by the runner to verify HMAC-signed
    /// dispatch webhooks (`X-Rewind-Signature` header).
    pub raw_token: String,
    pub raw_token_warning: &'static str,
}

// ──────────────────────────────────────────────────────────────────
// Request shapes
// ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RegisterRunnerRequest {
    pub name: String,
    pub mode: RunnerMode,
    /// Required if `mode == "webhook"`; must be `http(s)://` URL.
    /// Forbidden if `mode == "polling"`.
    pub webhook_url: Option<String>,
}

// ──────────────────────────────────────────────────────────────────
// Validation
// ──────────────────────────────────────────────────────────────────

const NAME_MIN: usize = 1;
const NAME_MAX: usize = 100;

fn validate_name(name: &str) -> Result<(), String> {
    let n = name.trim();
    if n.len() < NAME_MIN {
        return Err("name must not be empty".into());
    }
    if n.chars().count() > NAME_MAX {
        return Err(format!("name must be at most {NAME_MAX} characters"));
    }
    if n.chars().any(|c| c.is_control()) {
        return Err("name must not contain control characters".into());
    }
    Ok(())
}

/// Validate the (mode, webhook_url) tuple at registration time.
///
/// **Review #153 HIGH 1:** `mode = polling` is rejected with a 400.
/// The Phase 3 plan defers pull-based runners to v3.1 — there is no
/// polling worker in the current dispatcher path, so accepting a
/// polling registration would silently create a runner that can
/// never receive jobs. The schema column accepts `polling` for
/// future-compat, but the API surface refuses it until commit-N
/// (v3.1) ships the polling worker.
///
/// **Review #153 HIGH 2:** SSRF validation on `webhook_url` is
/// performed by the caller via [`url_guard::validate_export_endpoint`]
/// after this function returns the trimmed URL. We additionally
/// reject userinfo (`http://user:pass@host/...`) here because
/// `url_guard` doesn't enforce that.
fn validate_webhook_url(mode: RunnerMode, url: Option<&str>) -> Result<String, String> {
    if matches!(mode, RunnerMode::Polling) {
        return Err(
            "polling mode is not implemented in v1; only mode=webhook is accepted. \
             Pull-based runners are tracked for v3.1."
                .into(),
        );
    }
    let u = url
        .ok_or_else(|| "webhook_url is required for mode=webhook".to_string())?
        .trim();
    if !u.starts_with("http://") && !u.starts_with("https://") {
        return Err("webhook_url must be http:// or https://".into());
    }
    if u == "http://" || u == "https://" {
        return Err("webhook_url is missing a host".into());
    }
    let parsed = url::Url::parse(u).map_err(|e| format!("webhook_url is malformed: {e}"))?;
    // Reject embedded credentials — no legitimate webhook target uses
    // basic-auth-style URLs, and they're a known SSRF/credential-leak
    // surface.
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("webhook_url must not contain userinfo (user:pass@...)".into());
    }
    if parsed.host_str().is_none() {
        return Err("webhook_url is missing a host".into());
    }
    Ok(u.to_string())
}

// ──────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────

/// Map a missing crypto key to a 503 with operator-actionable body.
fn require_crypto(state: &AppState) -> Result<&CryptoBox, (StatusCode, Json<ErrorBody>)> {
    state.crypto.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: format!(
                    "{} env var is not set; runner registry endpoints unavailable. Generate one with `openssl rand -base64 32` and restart the server.",
                    crypto::KEY_ENV_VAR
                ),
            }),
        )
    })
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: String,
}

fn bad_request<E: ToString>(e: E) -> (StatusCode, Json<ErrorBody>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorBody {
            error: e.to_string(),
        }),
    )
}

fn internal<E: ToString>(e: E) -> (StatusCode, Json<ErrorBody>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorBody {
            error: e.to_string(),
        }),
    )
}

fn not_found(what: &str) -> (StatusCode, Json<ErrorBody>) {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorBody {
            error: format!("{what} not found"),
        }),
    )
}

fn conflict(msg: String) -> (StatusCode, Json<ErrorBody>) {
    (StatusCode::CONFLICT, Json(ErrorBody { error: msg }))
}

// ──────────────────────────────────────────────────────────────────
// Handlers
// ──────────────────────────────────────────────────────────────────

/// `GET /api/runners` — list all runners (no tokens).
async fn list_runners(
    State(state): State<AppState>,
) -> Result<Json<Vec<RunnerView>>, (StatusCode, Json<ErrorBody>)> {
    let store = state
        .store
        .lock()
        .map_err(|e| internal(format!("store lock: {e}")))?;
    let runners = store.list_runners().map_err(internal)?;
    Ok(Json(runners.into_iter().map(RunnerView::from).collect()))
}

/// `POST /api/runners` — register a new runner. Returns the raw
/// auth token in the body **once**.
async fn register_runner(
    State(state): State<AppState>,
    Json(req): Json<RegisterRunnerRequest>,
) -> Result<(StatusCode, Json<RegisterRunnerResponse>), (StatusCode, Json<ErrorBody>)> {
    let cb = require_crypto(&state)?;
    validate_name(&req.name).map_err(bad_request)?;
    let mode = req.mode.clone();
    let webhook_url = validate_webhook_url(mode, req.webhook_url.as_deref())
        .map_err(bad_request)?;
    // Review #153 HIGH 2: SSRF guard — refuse webhook_url targets
    // that resolve to loopback / private / link-local / metadata IPs.
    // Reuses the same policy that gates `export_otel`.
    crate::url_guard::validate_export_endpoint(&webhook_url)
        .await
        .map_err(bad_request)?;

    let raw_token = crypto::generate_runner_token();
    let nonce = CryptoBox::fresh_nonce();
    let encrypted = cb
        .encrypt(raw_token.expose().as_bytes(), &nonce)
        .map_err(internal)?;

    let runner = Runner {
        id: Uuid::new_v4().to_string(),
        name: req.name.trim().to_string(),
        mode: req.mode,
        // Polling mode is rejected above (HIGH 1), so webhook_url is
        // always Some(url). The schema column stays Option<String>
        // for forward-compat when v3.1 ships polling.
        webhook_url: Some(webhook_url),
        encrypted_token: encrypted,
        token_nonce: nonce.to_vec(),
        auth_token_hash: crypto::hash_runner_token(raw_token.expose()),
        auth_token_preview: crypto::token_preview(raw_token.expose()),
        created_at: chrono::Utc::now(),
        last_seen_at: None,
        status: RunnerStatus::Active,
    };

    {
        let store = state
            .store
            .lock()
            .map_err(|e| internal(format!("store lock: {e}")))?;
        store.create_runner(&runner).map_err(internal)?;
    }

    let response = RegisterRunnerResponse {
        runner: runner.into(),
        raw_token: raw_token.expose().to_string(),
        raw_token_warning: "Save this token now. It cannot be retrieved after this response.",
    };
    Ok((StatusCode::CREATED, Json(response)))
}

/// `GET /api/runners/{id}` — fetch a single runner (no tokens).
async fn get_runner(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<RunnerView>, (StatusCode, Json<ErrorBody>)> {
    let store = state
        .store
        .lock()
        .map_err(|e| internal(format!("store lock: {e}")))?;
    let runner = store
        .get_runner(&id)
        .map_err(internal)?
        .ok_or_else(|| not_found("runner"))?;
    Ok(Json(runner.into()))
}

/// `DELETE /api/runners/{id}` — hard-delete the runner.
///
/// **Review #153 MEDIUM 4 (active-jobs guard):** refuses deletion
/// when the runner has non-terminal jobs (state ∈ {pending,
/// dispatched, in_progress}). Returns `409 Conflict` with the count
/// in the body; operators must drain in-flight jobs first (mark
/// runner disabled + wait for them to settle, OR cancel them
/// explicitly). Otherwise `ON DELETE SET NULL` would orphan the
/// in-flight jobs (no dispatcher to send to, no auth surface for
/// inbound events).
///
/// Historical / terminal jobs (`completed`, `errored`) survive the
/// cascade with `runner_id` nulled out and render as "Runner
/// deleted" in the dashboard. (Review #152 FK rules.)
async fn remove_runner(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    let store = state
        .store
        .lock()
        .map_err(|e| internal(format!("store lock: {e}")))?;
    let active = store.count_active_jobs_for_runner(&id).map_err(internal)?;
    if active > 0 {
        return Err(conflict(format!(
            "runner has {active} non-terminal job(s) (pending/dispatched/in_progress); \
             drain or cancel them before deletion. Alternatively, mark the runner \
             disabled to stop new dispatches and wait for active jobs to settle."
        )));
    }
    let removed = store.delete_runner(&id).map_err(internal)?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(not_found("runner"))
    }
}

/// `POST /api/runners/{id}/regenerate-token` — rotate the runner's
/// auth token. Returns the new raw token once.
///
/// **Review #153 HIGH 3 (active-jobs guard):** refuses rotation
/// while in-flight jobs reference this runner. Returns `409 Conflict`
/// with the count. The old `auth_token_hash` is invalidated
/// immediately on rotation, which would break any in-flight
/// runner→server callback signed with the old token (the runner
/// already accepted the dispatch under it). Operator workflow:
/// drain or cancel in-flight jobs, then rotate. Future v3.1 may
/// add a token-grace list to allow lossless rotation.
async fn regenerate_token(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<RegisterRunnerResponse>, (StatusCode, Json<ErrorBody>)> {
    let cb = require_crypto(&state)?;

    let raw_token = crypto::generate_runner_token();
    let nonce = CryptoBox::fresh_nonce();
    let encrypted = cb
        .encrypt(raw_token.expose().as_bytes(), &nonce)
        .map_err(internal)?;
    let auth_token_hash = crypto::hash_runner_token(raw_token.expose());
    let auth_token_preview = crypto::token_preview(raw_token.expose());

    let updated_runner = {
        let store = state
            .store
            .lock()
            .map_err(|e| internal(format!("store lock: {e}")))?;
        // Verify the runner exists first; rotation on a deleted runner
        // is a 404, not a silent no-op that returns a token bound to
        // nothing.
        let existing = store
            .get_runner(&id)
            .map_err(internal)?
            .ok_or_else(|| not_found("runner"))?;
        // Active-jobs guard: see docstring above.
        let active = store.count_active_jobs_for_runner(&id).map_err(internal)?;
        if active > 0 {
            return Err(conflict(format!(
                "runner has {active} non-terminal job(s) (pending/dispatched/in_progress); \
                 rotating now would invalidate the token bound to in-flight callbacks. \
                 Drain or cancel them before rotating, or mark the runner disabled to \
                 stop new dispatches and wait for active jobs to settle."
            )));
        }
        store
            .rotate_runner_token(
                &id,
                &encrypted,
                &nonce,
                &auth_token_hash,
                &auth_token_preview,
            )
            .map_err(internal)?;
        // Re-fetch so the response reflects the post-rotation state.
        store
            .get_runner(&id)
            .map_err(internal)?
            .unwrap_or(existing)
    };

    Ok(Json(RegisterRunnerResponse {
        runner: updated_runner.into(),
        raw_token: raw_token.expose().to_string(),
        raw_token_warning: "Save this token now. It cannot be retrieved after this response.",
    }))
}

// ──────────────────────────────────────────────────────────────────
// Phase 3 commit 6: Replay job dispatch + event ingestion
// ──────────────────────────────────────────────────────────────────

/// Dispatch endpoint shape A: server creates fork + replay context
/// + replay job atomically.
///
/// Used by the dashboard "Run replay" button when the operator
/// clicks on a session step.
#[derive(Debug, Deserialize)]
pub struct CreateReplayJobShapeA {
    pub runner_id: String,
    pub source_timeline_id: String,
    pub at_step: u32,
    #[serde(default)]
    pub strict_match: bool,
}

/// Dispatch endpoint shape B: caller already has a replay context
/// (e.g. CLI or programmatic clients). Server validates ownership
/// and that the cursor isn't already in use.
#[derive(Debug, Deserialize)]
pub struct CreateReplayJobShapeB {
    pub runner_id: String,
    pub replay_context_id: String,
}

/// Dual-shape request body. `serde(untagged)` picks based on which
/// fields are present.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum CreateReplayJobRequest {
    CreateAndDispatch(CreateReplayJobShapeA),
    ReuseContext(CreateReplayJobShapeB),
}

#[derive(Debug, Serialize)]
pub struct CreateReplayJobResponse {
    pub job_id: String,
    pub replay_context_id: String,
    /// Present when the server created a fresh fork timeline (shape A).
    /// Null when the caller supplied an existing replay_context (shape B).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork_timeline_id: Option<String>,
    pub state: String,
    /// Echoed back so the dashboard knows when the runner must reply.
    pub dispatch_deadline_at: Option<String>,
}

/// `POST /api/sessions/{sid}/replay-jobs`
async fn create_replay_job(
    State(state): State<AppState>,
    Path(sid): Path<String>,
    Json(req): Json<CreateReplayJobRequest>,
) -> Result<(StatusCode, Json<CreateReplayJobResponse>), (StatusCode, Json<ErrorBody>)> {
    let dispatcher = state
        .dispatcher
        .clone()
        .ok_or_else(|| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorBody {
                    error: format!(
                        "{} env var is not set; replay-job dispatch unavailable.",
                        crypto::KEY_ENV_VAR
                    ),
                }),
            )
        })?;

    // 1. Resolve session, runner, and replay-context (creating fork+
    //    context if shape A). All store work in a single lock scope
    //    so we don't race with deletions between checks.
    let (job, runner, fork_timeline_id) = {
        let store = state
            .store
            .lock()
            .map_err(|e| internal(format!("store lock: {e}")))?;
        let session = store
            .get_session(&sid)
            .map_err(internal)?
            .ok_or_else(|| not_found("session"))?;

        let (runner_id, replay_context_id, fork_timeline_id) = match req {
            CreateReplayJobRequest::CreateAndDispatch(a) => {
                // Validate runner.
                let runner = store
                    .get_runner(&a.runner_id)
                    .map_err(internal)?
                    .ok_or_else(|| not_found("runner"))?;
                if !matches!(runner.status, RunnerStatus::Active) {
                    return Err(conflict(format!(
                        "runner {} is in status {:?}; cannot dispatch",
                        runner.id, runner.status
                    )));
                }
                // Validate timeline + at_step + create fork.
                let timelines = store.get_timelines(&session.id).map_err(internal)?;
                if !timelines.iter().any(|t| t.id == a.source_timeline_id) {
                    return Err(bad_request(format!(
                        "source_timeline_id {} not found in session {}",
                        a.source_timeline_id, session.id
                    )));
                }
                let engine = ReplayEngine::new(&store);
                let fork = engine
                    .fork(
                        &session.id,
                        &a.source_timeline_id,
                        a.at_step,
                        &format!("replay-{}", &Uuid::new_v4().to_string()[..8]),
                    )
                    .map_err(|e| bad_request(format!("fork failed: {e}")))?;
                let ctx_id = Uuid::new_v4().to_string();
                store
                    .create_replay_context(&ctx_id, &session.id, &fork.id, a.at_step)
                    .map_err(internal)?;
                (a.runner_id, ctx_id, Some(fork.id))
            }
            CreateReplayJobRequest::ReuseContext(b) => {
                let runner = store
                    .get_runner(&b.runner_id)
                    .map_err(internal)?
                    .ok_or_else(|| not_found("runner"))?;
                if !matches!(runner.status, RunnerStatus::Active) {
                    return Err(conflict(format!(
                        "runner {} is in status {:?}; cannot dispatch",
                        runner.id, runner.status
                    )));
                }
                // Validate context belongs to this session and isn't
                // already being consumed by another in-flight job.
                let ctx = store
                    .get_replay_context(&b.replay_context_id)
                    .map_err(internal)?
                    .ok_or_else(|| not_found("replay_context"))?;
                if ctx.session_id != session.id {
                    return Err(bad_request(format!(
                        "replay_context {} belongs to session {}, not {}",
                        b.replay_context_id, ctx.session_id, session.id
                    )));
                }
                let in_flight = store
                    .count_in_flight_jobs_for_replay_context(&b.replay_context_id)
                    .map_err(internal)?;
                if in_flight > 0 {
                    return Err(conflict(format!(
                        "replay_context {} already has {in_flight} in-flight job(s); \
                         finish or cancel them before dispatching a new one",
                        b.replay_context_id
                    )));
                }
                (b.runner_id, b.replay_context_id, None)
            }
        };

        // 2. Insert the job (state=pending; FK-validated by commit 3).
        let runner = store
            .get_runner(&runner_id)
            .map_err(internal)?
            .ok_or_else(|| not_found("runner"))?;
        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: Some(runner_id.clone()),
            session_id: session.id.clone(),
            replay_context_id: Some(replay_context_id),
            state: ReplayJobState::Pending,
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
        store.create_replay_job(&job).map_err(internal)?;
        (job, runner, fork_timeline_id)
    };

    // 3. Fire the dispatcher in the background. The handler returns
    //    immediately with the job id; progress flows over the
    //    WebSocket as state changes.
    // Fire dispatcher in the background. The dispatcher is pure
    // (returns a DispatchOutcome) so the async HTTP doesn't hold
    // a MutexGuard across await — we apply the outcome inside a
    // briefly-held lock after the network call settles.
    let job_clone = job.clone();
    let runner_clone = runner.clone();
    let store_arc = state.store.clone();
    let event_tx = state.event_tx.clone();
    tokio::spawn(async move {
        let outcome = dispatcher.dispatch(&runner_clone, &job_clone).await;
        let after = {
            let store = match store_arc.lock() {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("dispatch spawn: store lock poisoned: {e}");
                    return;
                }
            };
            if let Err(e) = crate::dispatcher::Dispatcher::apply_outcome(
                &outcome,
                &job_clone.id,
                &store,
            ) {
                tracing::error!("dispatch spawn: apply_outcome failed: {e}");
                return;
            }
            store.get_replay_job(&job_clone.id).ok().flatten()
        };
        if let Some(after) = after {
            let _ = event_tx.send(StoreEvent::ReplayJobUpdated {
                job_id: after.id.clone(),
                session_id: after.session_id.clone(),
                state: after.state.as_str().to_string(),
                progress_step: Some(after.progress_step),
                progress_total: after.progress_total,
                error_message: after.error_message.clone(),
                error_stage: after.error_stage.clone(),
            });
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(CreateReplayJobResponse {
            job_id: job.id.clone(),
            replay_context_id: job.replay_context_id.clone().unwrap_or_default(),
            fork_timeline_id,
            state: "pending".to_string(),
            dispatch_deadline_at: job.dispatch_deadline_at.map(|t| t.to_rfc3339()),
        }),
    ))
}

#[derive(Debug, Serialize)]
pub struct ReplayJobView {
    pub id: String,
    pub runner_id: Option<String>,
    pub session_id: String,
    pub replay_context_id: Option<String>,
    pub state: String,
    pub error_message: Option<String>,
    pub error_stage: Option<String>,
    pub progress_step: u32,
    pub progress_total: Option<u32>,
    pub created_at: String,
    pub dispatched_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub dispatch_deadline_at: Option<String>,
    pub lease_expires_at: Option<String>,
}

impl From<ReplayJob> for ReplayJobView {
    fn from(j: ReplayJob) -> Self {
        Self {
            id: j.id,
            runner_id: j.runner_id,
            session_id: j.session_id,
            replay_context_id: j.replay_context_id,
            state: j.state.as_str().to_string(),
            error_message: j.error_message,
            error_stage: j.error_stage,
            progress_step: j.progress_step,
            progress_total: j.progress_total,
            created_at: j.created_at.to_rfc3339(),
            dispatched_at: j.dispatched_at.map(|t| t.to_rfc3339()),
            started_at: j.started_at.map(|t| t.to_rfc3339()),
            completed_at: j.completed_at.map(|t| t.to_rfc3339()),
            dispatch_deadline_at: j.dispatch_deadline_at.map(|t| t.to_rfc3339()),
            lease_expires_at: j.lease_expires_at.map(|t| t.to_rfc3339()),
        }
    }
}

/// `GET /api/sessions/{sid}/replay-jobs` — list jobs for a session.
async fn list_replay_jobs_for_session(
    State(state): State<AppState>,
    Path(sid): Path<String>,
) -> Result<Json<Vec<ReplayJobView>>, (StatusCode, Json<ErrorBody>)> {
    let store = state
        .store
        .lock()
        .map_err(|e| internal(format!("store lock: {e}")))?;
    let _ = 100u32; // limit hint preserved for future pagination
    let jobs = store
        .list_replay_jobs_by_session(&sid)
        .map_err(internal)?;
    Ok(Json(jobs.into_iter().map(ReplayJobView::from).collect()))
}

/// `GET /api/replay-jobs/{id}` — fetch a single replay job.
async fn get_replay_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ReplayJobView>, (StatusCode, Json<ErrorBody>)> {
    let store = state
        .store
        .lock()
        .map_err(|e| internal(format!("store lock: {e}")))?;
    let job = store
        .get_replay_job(&id)
        .map_err(internal)?
        .ok_or_else(|| not_found("replay_job"))?;
    Ok(Json(job.into()))
}

/// `DELETE /api/replay-jobs/{id}` — operator cancel (forces the job
/// into `errored` with stage `"cancelled"`). v1 cancellation is
/// fire-and-forget: the runner is not notified; if it later posts
/// progress events they'll bounce off the terminal-state guard.
async fn cancel_replay_job_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    let store = state
        .store
        .lock()
        .map_err(|e| internal(format!("store lock: {e}")))?;
    let advanced = store
        .advance_replay_job_state(
            &id,
            ReplayJobState::Errored,
            Some("cancelled by operator"),
            Some("cancelled"),
        )
        .map_err(internal)?;
    if !advanced {
        return Err(not_found("replay_job (or already terminal)"));
    }
    if let Ok(Some(job)) = store.get_replay_job(&id) {
        let _ = state.event_tx.send(StoreEvent::ReplayJobUpdated {
            job_id: job.id,
            session_id: job.session_id,
            state: "errored".to_string(),
            progress_step: Some(job.progress_step),
            progress_total: job.progress_total,
            error_message: Some("cancelled by operator".to_string()),
            error_stage: Some("cancelled".to_string()),
        });
    }
    Ok(StatusCode::NO_CONTENT)
}

// ──────────────────────────────────────────────────────────────────
// Runner-callback: POST /api/replay-jobs/{id}/events
// (mounted OUTSIDE the bearer-auth middleware; uses
// X-Rewind-Runner-Auth + ownership check.)
// ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct PostReplayJobEventRequest {
    pub event_type: ReplayJobEventType,
    pub step_number: Option<u32>,
    pub progress_total: Option<u32>,
    pub payload: Option<serde_json::Value>,
    pub error_message: Option<String>,
    pub error_stage: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PostReplayJobEventResponse {
    pub accepted: bool,
    /// Present when the event was rejected (terminal state, illegal
    /// transition, ownership mismatch). Helps runners tell "you got
    /// here too late" from "you sent garbage".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub state: String,
}

const RUNNER_AUTH_HEADER: &str = "X-Rewind-Runner-Auth";

async fn post_replay_job_event(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<PostReplayJobEventRequest>,
) -> Result<(StatusCode, Json<PostReplayJobEventResponse>), (StatusCode, Json<ErrorBody>)> {
    // 1. Extract runner-auth header.
    let supplied_token = headers
        .get(RUNNER_AUTH_HEADER)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                Json(ErrorBody {
                    error: format!("missing {RUNNER_AUTH_HEADER} header"),
                }),
            )
        })?;
    let supplied_hash = crypto::hash_runner_token(supplied_token);

    // 2. Look up runner by hash, verify ownership, and apply the event
    //    atomically.
    let mut store = state
        .store
        .lock()
        .map_err(|e| internal(format!("store lock: {e}")))?;

    let runner = store
        .get_runner_by_auth_hash(&supplied_hash)
        .map_err(internal)?
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                Json(ErrorBody {
                    error: format!("invalid {RUNNER_AUTH_HEADER} token"),
                }),
            )
        })?;

    let job = store
        .get_replay_job(&job_id)
        .map_err(internal)?
        .ok_or_else(|| not_found("replay_job"))?;

    if job.runner_id.as_deref() != Some(runner.id.as_str()) {
        // Runner can authenticate but doesn't own this job. Return
        // 403 (not 404) so legitimate operators can debug runner
        // misconfiguration.
        return Err((
            StatusCode::FORBIDDEN,
            Json(ErrorBody {
                error: "runner does not own this job".to_string(),
            }),
        ));
    }

    // Touch last_seen_at for runner liveness tracking.
    let _ = store.touch_runner_last_seen(&runner.id);

    // 3. Build event row + apply via atomic state-machine helper.
    let event = ReplayJobEvent {
        id: Uuid::new_v4().to_string(),
        job_id: job.id.clone(),
        event_type: req.event_type,
        step_number: req.step_number,
        payload: req
            .payload
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_default()),
        created_at: Utc::now(),
    };
    let accepted = store
        .record_replay_job_event_atomic(
            &event,
            req.progress_total,
            req.error_message.as_deref(),
            req.error_stage.as_deref(),
            crate::dispatcher::INITIAL_LEASE_SECS,
        )
        .map_err(internal)?;

    // 4. Re-read state for response + WebSocket broadcast.
    let after = store
        .get_replay_job(&job.id)
        .map_err(internal)?
        .unwrap_or(job);

    let state_str = after.state.as_str().to_string();
    let reason = if accepted {
        None
    } else {
        Some("event rejected by state machine (terminal or illegal transition)".to_string())
    };

    if accepted {
        let _ = state.event_tx.send(StoreEvent::ReplayJobUpdated {
            job_id: after.id.clone(),
            session_id: after.session_id.clone(),
            state: state_str.clone(),
            progress_step: Some(after.progress_step),
            progress_total: after.progress_total,
            error_message: after.error_message.clone(),
            error_stage: after.error_stage.clone(),
        });
    }

    let status = if accepted {
        StatusCode::ACCEPTED
    } else {
        StatusCode::CONFLICT
    };
    Ok((
        status,
        Json(PostReplayJobEventResponse {
            accepted,
            reason,
            state: state_str,
        }),
    ))
}
