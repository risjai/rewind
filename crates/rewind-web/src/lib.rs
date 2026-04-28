pub mod api;
pub mod auth;
pub mod crypto;
pub mod eval_api;
pub mod hooks;
pub mod otlp_ingest;
mod polling;
pub mod runners;
mod spa;
pub mod transcript;
pub mod url_guard;
mod ws;

use anyhow::Result;
use axum::Router;
use rewind_store::Store;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

pub use api::routes as api_routes;
pub use hooks::HookIngestionState;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum StoreEvent {
    StepCreated {
        session_id: String,
        step: Box<rewind_store::Step>,
    },
    SessionUpdated {
        session_id: String,
        status: String,
        total_steps: u32,
        total_tokens: u64,
    },
}

/// Server-configured OTel export settings (from env vars).
/// None = OTel export not configured (endpoint returns 501).
#[derive(Clone, Debug)]
pub struct OtelConfig {
    pub endpoint: String,
    pub protocol: rewind_otel::export::Protocol,
    pub headers: Vec<(String, String)>,
}

impl OtelConfig {
    /// Read OTel config from environment variables.
    /// Returns None if REWIND_OTEL_ENDPOINT is not set.
    pub fn from_env() -> Option<Self> {
        let endpoint = std::env::var("REWIND_OTEL_ENDPOINT").ok()?;

        let protocol = match std::env::var("REWIND_OTEL_PROTOCOL")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "grpc" => rewind_otel::export::Protocol::Grpc,
            _ => rewind_otel::export::Protocol::Http,
        };

        let headers = std::env::var("REWIND_OTEL_HEADERS")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .filter_map(|h| {
                let (k, v) = h.split_once('=')?;
                Some((k.to_string(), v.to_string()))
            })
            .collect();

        Some(Self {
            endpoint,
            protocol,
            headers,
        })
    }
}

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Mutex<Store>>,
    pub event_tx: broadcast::Sender<StoreEvent>,
    pub hooks: Arc<HookIngestionState>,
    pub otel_config: Option<OtelConfig>,
    /// Bearer token required for protected routes. `None` disables auth (loopback default).
    /// See `auth::resolve_or_generate_token` and `docs/security-audit.md` §CRITICAL-02.
    pub auth_token: Option<String>,
    /// AES-256-GCM cipher used to encrypt/decrypt runner auth tokens at rest.
    /// `None` means `REWIND_RUNNER_SECRET_KEY` was unset at startup; the
    /// `/api/runners` endpoints return `503` in that case so operators
    /// see a clear bootstrap error. Phase 3 commit 4. See `crypto.rs`.
    pub crypto: Option<crypto::CryptoBox>,
}

pub struct WebServer {
    state: AppState,
    dev_mode: bool,
    /// Operator explicitly disabled auth via `--no-auth`. Skips the fail-closed
    /// check on non-loopback bind. See `docs/security-audit.md` §CRITICAL-02.
    auth_disabled: bool,
}

/// Read `REWIND_RUNNER_SECRET_KEY` and build the [`crypto::CryptoBox`].
///
/// **Review #153 MEDIUM 5 — fail-fast on misconfig:** if the env var
/// is *unset*, returns `None` (runner endpoints will 503 with a clear
/// bootstrap error). If the env var is *set but malformed* (bad
/// base64, wrong key length), this **panics at startup** with an
/// operator-actionable message rather than silently booting a server
/// whose runner registry can never dispatch.
///
/// This matches the docstring on `crypto::CryptoBox::from_env`
/// ("operator misconfig — fail loud at startup"). Previously the
/// caller logged-and-swallowed, which made the failure invisible
/// until the first `/api/runners` 503.
fn bootstrap_crypto() -> Option<crypto::CryptoBox> {
    match crypto::CryptoBox::from_env() {
        Ok(maybe) => maybe,
        Err(e) => panic!(
            "FATAL: {} is set but malformed: {e}\n\
             Generate a fresh key with `openssl rand -base64 32` and set it via the env var.",
            crypto::KEY_ENV_VAR
        ),
    }
}

impl WebServer {
    pub fn new(store: Arc<Mutex<Store>>, event_tx: broadcast::Sender<StoreEvent>) -> Self {
        let otel_config = OtelConfig::from_env();
        let crypto = bootstrap_crypto();
        WebServer {
            state: AppState {
                store,
                event_tx,
                hooks: Arc::new(HookIngestionState::new()),
                otel_config,
                auth_token: None,
                crypto,
            },
            dev_mode: false,
            auth_disabled: false,
        }
    }

    pub fn new_standalone(store: Store) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        let otel_config = OtelConfig::from_env();
        let crypto = bootstrap_crypto();
        WebServer {
            state: AppState {
                store: Arc::new(Mutex::new(store)),
                event_tx,
                hooks: Arc::new(HookIngestionState::new()),
                otel_config,
                auth_token: None,
                crypto,
            },
            dev_mode: false,
            auth_disabled: false,
        }
    }

    pub fn dev_mode(mut self, dev: bool) -> Self {
        self.dev_mode = dev;
        self
    }

    /// Set the bearer token. `None` leaves auth unconfigured (loopback default).
    pub fn with_auth_token(mut self, token: Option<String>) -> Self {
        self.state.auth_token = token;
        self
    }

    /// Explicitly disable auth. Overrides the fail-closed check on non-loopback
    /// bind. Use only when auth is provided by an upstream layer (e.g. a sidecar).
    pub fn with_auth_disabled(mut self, disabled: bool) -> Self {
        self.auth_disabled = disabled;
        self
    }

    pub fn state(&self) -> AppState {
        self.state.clone()
    }

    pub async fn run(self, addr: SocketAddr) -> Result<()> {
        // Fail-closed: refuse to start on a non-loopback bind without an auth token.
        // `with_auth_disabled(true)` is the explicit opt-out. See docs/security-audit.md §CRITICAL-02.
        if !addr.ip().is_loopback() && self.state.auth_token.is_none() && !self.auth_disabled {
            anyhow::bail!(
                "Refusing to bind to non-loopback address {} without an auth token.\n\
                 Provide one via --auth-token, REWIND_AUTH_TOKEN, or accept the \
                 auto-generated token at ~/.rewind/auth_token (delete the file to regenerate).\n\
                 Use --no-auth to explicitly disable authentication (dangerous).",
                addr
            );
        }

        let poll_store = self.state.store.clone();
        let poll_tx = self.state.event_tx.clone();

        // Rehydrate hook session state from database (prevents duplicate sessions after restart)
        {
            let store = self.state.store.lock().map_err(|e| anyhow::anyhow!("Lock: {e}"))?;
            self.state.hooks.rehydrate_from_store(&store);
        }

        // Drain buffered hook events before building router (self is consumed by build_router)
        let drain_state = self.state.clone();
        let drain_count = Self::drain_hook_buffer(&drain_state).await;

        // Auto-complete stale recording sessions (must run AFTER buffer drain so
        // buffered SessionEnd events have a chance to update `updated_at` first)
        let stale_count = Self::cleanup_stale_sessions(&drain_state);

        let app = self.build_router();

        // Bind first so port-in-use errors appear before the success banner
        let listener = tokio::net::TcpListener::bind(addr).await
            .map_err(|e| anyhow::anyhow!(
                "Cannot start on port {}: {}. Try a different port with --port.",
                addr.port(), e
            ))?;

        tracing::info!("Rewind Web UI listening on http://{}", addr);
        println!();
        println!("  \x1b[36;1m⏪ Rewind Web UI\x1b[0m");
        println!("  \x1b[2mDashboard:\x1b[0m \x1b[33mhttp://{}\x1b[0m", addr);

        if drain_count > 0 {
            println!("  \x1b[2mDrained:\x1b[0m  \x1b[32m{} buffered hook events\x1b[0m", drain_count);
        }
        if stale_count > 0 {
            println!("  \x1b[2mCleaned:\x1b[0m  \x1b[33m{} stale sessions auto-completed\x1b[0m", stale_count);
        }

        println!();

        // Start background SQLite polling for live updates
        tokio::spawn(polling::start_polling(
            poll_store,
            poll_tx,
            std::time::Duration::from_millis(300),
        ));

        // Start background transcript sync: creates LlmCall steps + aggregates tokens
        let transcript_state = drain_state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                if let Err(e) = transcript::sync_transcript_steps(&transcript_state) {
                    tracing::error!("Transcript sync error: {e}");
                }
            }
        });

        // Periodic stale session cleanup (every 5 minutes)
        let cleanup_state = drain_state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                Self::cleanup_stale_sessions(&cleanup_state);
            }
        });

        // Periodic replay context TTL cleanup (every 10 minutes, 1h TTL)
        let ctx_cleanup_state = drain_state;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(600)).await;
                if let Ok(store) = ctx_cleanup_state.store.lock() {
                    match store.cleanup_expired_replay_contexts(3600) {
                        Ok(n) if n > 0 => tracing::info!("Cleaned up {n} expired replay contexts"),
                        Err(e) => tracing::error!("Replay context cleanup failed: {e}"),
                        _ => {}
                    }
                }
            }
        });

        axum::serve(listener, app).await?;
        Ok(())
    }

    /// Complete stale recording sessions and emit WebSocket updates.
    /// Returns the number of sessions auto-completed.
    fn cleanup_stale_sessions(state: &AppState) -> usize {
        let stale_threshold = chrono::Duration::minutes(30);

        let ids = match state.store.lock() {
            Ok(store) => match store.complete_stale_sessions(stale_threshold) {
                Ok(ids) => ids,
                Err(e) => {
                    tracing::error!("Stale session cleanup failed: {e}");
                    return 0;
                }
            },
            Err(e) => {
                tracing::error!("Lock error during stale session cleanup: {e}");
                return 0;
            }
        };

        if !ids.is_empty() {
            tracing::info!("Auto-completed {} stale sessions: {:?}", ids.len(), ids);

            // Best-effort token backfill from transcript files before notifying frontend
            transcript::backfill_tokens(state, &ids);

            // Re-read sessions from DB so WebSocket events carry real values
            let store = state.store.lock().ok();
            for id in &ids {
                let (steps, tokens) = store.as_ref()
                    .and_then(|s| s.get_session(id).ok().flatten())
                    .map(|s| (s.total_steps, s.total_tokens))
                    .unwrap_or((0, 0));
                let _ = state.event_tx.send(StoreEvent::SessionUpdated {
                    session_id: id.clone(),
                    status: "completed".to_string(),
                    total_steps: steps,
                    total_tokens: tokens,
                });
            }
        }

        ids.len()
    }

    /// Drain buffered hook events from ~/.rewind/hooks/buffer.jsonl
    /// These accumulate when the hook script fires but the server isn't running.
    async fn drain_hook_buffer(state: &AppState) -> usize {
        let buffer_path = std::env::var("HOME")
            .map(|h| std::path::PathBuf::from(h).join(".rewind/hooks/buffer.jsonl"))
            .unwrap_or_default();

        if !buffer_path.exists() {
            return 0;
        }

        let content = match std::fs::read_to_string(&buffer_path) {
            Ok(c) => c,
            Err(_) => return 0,
        };

        let mut count = 0;
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(envelope) = serde_json::from_str::<hooks::HookEventEnvelope>(line) {
                hooks::process_hook_event(state, envelope).await;
                count += 1;
            }
        }

        // Clear the buffer file after successful drain
        if count > 0 {
            let _ = std::fs::write(&buffer_path, "");
        }

        count
    }

    fn build_router(self) -> Router {
        let api_routes = api::routes(self.state.clone());
        let eval_routes = eval_api::routes(self.state.clone());
        let hook_routes = hooks::routes(self.state.clone());
        let ws_route = ws::routes(self.state.clone());

        let otlp_routes = otlp_ingest::routes(self.state.clone());

        // Compose protected routes (state-bearing APIs, WS, OTLP ingest) and
        // attach auth middleware at this layer. `/_rewind/health` and the SPA
        // static fallback remain open so liveness probes and the UI shell can
        // load without a token.
        let protected = Router::new()
            .merge(otlp_routes)
            .nest("/api", api_routes)
            .nest("/api/eval", eval_routes)
            .nest("/api/hooks", hook_routes)
            .merge(ws_route)
            .layer(axum::middleware::from_fn_with_state(
                self.state.clone(),
                auth::auth_middleware,
            ));

        let app = Router::new()
            .route("/_rewind/health", axum::routing::get(rewind_health))
            .merge(protected);

        if self.dev_mode {
            app
        } else {
            app.fallback(spa::static_handler)
        }
    }
}

/// Health check endpoint at `/_rewind/health` — used by the Python SDK to detect
/// if the Rewind proxy/web server is alive before redirecting LLM traffic.
async fn rewind_health() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}
