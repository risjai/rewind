pub mod api;
pub mod eval_api;
pub mod hooks;
pub mod otlp_ingest;
mod polling;
mod spa;
pub mod transcript;
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
}

pub struct WebServer {
    state: AppState,
    dev_mode: bool,
}

impl WebServer {
    pub fn new(store: Arc<Mutex<Store>>, event_tx: broadcast::Sender<StoreEvent>) -> Self {
        let otel_config = OtelConfig::from_env();
        WebServer {
            state: AppState {
                store,
                event_tx,
                hooks: Arc::new(HookIngestionState::new()),
                otel_config,
            },
            dev_mode: false,
        }
    }

    pub fn new_standalone(store: Store) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        let otel_config = OtelConfig::from_env();
        WebServer {
            state: AppState {
                store: Arc::new(Mutex::new(store)),
                event_tx,
                hooks: Arc::new(HookIngestionState::new()),
                otel_config,
            },
            dev_mode: false,
        }
    }

    pub fn dev_mode(mut self, dev: bool) -> Self {
        self.dev_mode = dev;
        self
    }

    pub fn state(&self) -> AppState {
        self.state.clone()
    }

    pub async fn run(self, addr: SocketAddr) -> Result<()> {
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

        println!();

        // Start background SQLite polling for live updates
        tokio::spawn(polling::start_polling(
            poll_store,
            poll_tx,
            std::time::Duration::from_millis(300),
        ));

        // Start background transcript sync: creates LlmCall steps + aggregates tokens
        let transcript_state = drain_state;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                if let Err(e) = transcript::sync_transcript_steps(&transcript_state) {
                    tracing::error!("Transcript sync error: {e}");
                }
            }
        });

        axum::serve(listener, app).await?;
        Ok(())
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

        let app = Router::new()
            .route("/_rewind/health", axum::routing::get(rewind_health))
            .merge(otlp_routes)
            .nest("/api", api_routes)
            .nest("/api/eval", eval_routes)
            .nest("/api/hooks", hook_routes)
            .merge(ws_route);

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
