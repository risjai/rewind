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
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use rewind_store::{Runner, RunnerMode, RunnerStatus};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::crypto::{self, CryptoBox};
use crate::AppState;

/// Build the runners sub-router. Caller should `.nest("/api", ...)`
/// it alongside the existing `api::routes`.
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
