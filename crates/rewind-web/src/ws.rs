use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};

use crate::{AppState, StoreEvent};

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/api/ws", get(ws_handler))
        .with_state(state)
}

fn is_local_origin(origin: &str) -> bool {
    if let Some(host_port) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    {
        let host = host_port.split(':').next().unwrap_or("");
        matches!(host, "localhost" | "127.0.0.1" | "[::1]")
    } else {
        false
    }
}

async fn ws_handler(
    headers: HeaderMap,
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> Response {
    if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok())
        && !is_local_origin(origin)
    {
        return (StatusCode::FORBIDDEN, "WebSocket connections from non-localhost origins are rejected").into_response();
    }
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum ClientMessage {
    #[serde(rename = "subscribe")]
    Subscribe { session_id: String },
    #[serde(rename = "unsubscribe")]
    Unsubscribe,
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum ServerMessage {
    #[serde(rename = "step")]
    Step { data: Box<StepEventData> },
    #[serde(rename = "session_update")]
    SessionUpdate { data: SessionUpdateData },
    #[serde(rename = "subscribed")]
    Subscribed { session_id: String },
    /// Phase 3 commit 5/6: replay-job state/progress updates.
    #[serde(rename = "replay_job_update")]
    ReplayJobUpdate { data: ReplayJobUpdateData },
}

#[derive(Serialize)]
struct ReplayJobUpdateData {
    job_id: String,
    session_id: String,
    state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    progress_step: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    progress_total: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_stage: Option<String>,
}

#[derive(Serialize)]
struct StepEventData {
    id: String,
    step_number: u32,
    step_type: String,
    step_type_label: String,
    step_type_icon: String,
    status: String,
    model: String,
    duration_ms: u64,
    tokens_in: u64,
    tokens_out: u64,
    error: Option<String>,
    tool_name: Option<String>,
    response_preview: String,
    created_at: String,
}

#[derive(Serialize)]
struct SessionUpdateData {
    session_id: String,
    status: String,
    total_steps: u32,
    total_tokens: u64,
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();

    let mut subscribed_session: Option<String> = None;
    let mut event_rx = state.event_tx.subscribe();

    loop {
        tokio::select! {
            msg = receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) {
                            match client_msg {
                                ClientMessage::Subscribe { session_id } => {
                                    let ack = ServerMessage::Subscribed {
                                        session_id: session_id.clone(),
                                    };
                                    if let Ok(json) = serde_json::to_string(&ack) {
                                        let _ = sender.send(Message::Text(json.into())).await;
                                    }
                                    subscribed_session = Some(session_id);
                                }
                                ClientMessage::Unsubscribe => {
                                    subscribed_session = None;
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
            event = event_rx.recv() => {
                if let Ok(event) = event {
                    let msg = match &event {
                        StoreEvent::StepCreated { session_id, step } => {
                            if subscribed_session.as_deref() == Some(session_id) {
                                let preview = {
                                    let store = state.store.lock().unwrap();
                                    crate::api::extract_preview_from_store(
                                        &store,
                                        &step.response_blob,
                                        step.response_blob_format,
                                    )
                                };
                                Some(ServerMessage::Step {
                                    data: Box::new(StepEventData {
                                        id: step.id.clone(),
                                        step_number: step.step_number,
                                        step_type: step.step_type.as_str().to_string(),
                                        step_type_label: step.step_type.label().to_string(),
                                        step_type_icon: step.step_type.icon().to_string(),
                                        status: step.status.as_str().to_string(),
                                        model: step.model.clone(),
                                        duration_ms: step.duration_ms,
                                        tokens_in: step.tokens_in,
                                        tokens_out: step.tokens_out,
                                        error: step.error.clone(),
                                        tool_name: step.tool_name.clone(),
                                        response_preview: preview,
                                        created_at: step.created_at.to_rfc3339(),
                                    }),
                                })
                            } else {
                                None
                            }
                        }
                        StoreEvent::SessionUpdated { session_id, status, total_steps, total_tokens } => {
                            Some(ServerMessage::SessionUpdate {
                                data: SessionUpdateData {
                                    session_id: session_id.clone(),
                                    status: status.clone(),
                                    total_steps: *total_steps,
                                    total_tokens: *total_tokens,
                                },
                            })
                        }
                        StoreEvent::StepUpdated { .. } => {
                            // The frontend hook (useStepEdit) already
                            // invalidates react-query caches on save,
                            // so there's no need to broadcast here. A
                            // dedicated StepUpdate WS message type is
                            // a future enhancement for multi-tab sync.
                            None
                        }
                        StoreEvent::ReplayJobUpdated {
                            job_id,
                            session_id,
                            state: job_state,
                            progress_step,
                            progress_total,
                            error_message,
                            error_stage,
                        } => {
                            // Forward replay-job updates only to clients
                            // subscribed to the owning session.
                            if subscribed_session.as_deref() == Some(session_id) {
                                Some(ServerMessage::ReplayJobUpdate {
                                    data: ReplayJobUpdateData {
                                        job_id: job_id.clone(),
                                        session_id: session_id.clone(),
                                        state: job_state.clone(),
                                        progress_step: *progress_step,
                                        progress_total: *progress_total,
                                        error_message: error_message.clone(),
                                        error_stage: error_stage.clone(),
                                    },
                                })
                            } else {
                                None
                            }
                        }
                    };

                    if let Some(msg) = msg
                        && let Ok(json) = serde_json::to_string(&msg)
                            && sender.send(Message::Text(json.into())).await.is_err() {
                                break;
                            }
                }
            }
        }
    }
}
