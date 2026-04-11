pub mod api;
pub mod eval_api;
mod polling;
mod spa;
mod ws;

use anyhow::Result;
use axum::Router;
use rewind_store::Store;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

pub use api::routes as api_routes;

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

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Mutex<Store>>,
    pub event_tx: broadcast::Sender<StoreEvent>,
}

pub struct WebServer {
    state: AppState,
    dev_mode: bool,
}

impl WebServer {
    pub fn new(store: Arc<Mutex<Store>>, event_tx: broadcast::Sender<StoreEvent>) -> Self {
        WebServer {
            state: AppState { store, event_tx },
            dev_mode: false,
        }
    }

    pub fn new_standalone(store: Store) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        WebServer {
            state: AppState {
                store: Arc::new(Mutex::new(store)),
                event_tx,
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
        let app = self.build_router();

        tracing::info!("Rewind Web UI listening on http://{}", addr);
        println!();
        println!("  \x1b[36;1m⏪ Rewind Web UI\x1b[0m");
        println!("  \x1b[2mDashboard:\x1b[0m \x1b[33mhttp://{}\x1b[0m", addr);
        println!();

        // Start background SQLite polling for live updates
        tokio::spawn(polling::start_polling(
            poll_store,
            poll_tx,
            std::time::Duration::from_millis(300),
        ));

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;
        Ok(())
    }

    fn build_router(self) -> Router {
        let api_routes = api::routes(self.state.clone());
        let eval_routes = eval_api::routes(self.state.clone());
        let ws_route = ws::routes(self.state.clone());

        let app = Router::new()
            .nest("/api", api_routes)
            .nest("/api/eval", eval_routes)
            .merge(ws_route);

        if self.dev_mode {
            app
        } else {
            app.fallback(spa::static_handler)
        }
    }
}
