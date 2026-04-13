//! OTLP trace ingestion endpoint: `POST /v1/traces` and `POST /api/import/otel`.
//!
//! Accepts `ExportTraceServiceRequest` (protobuf), creates Rewind sessions from the spans.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use rewind_otel::ingest;

use crate::{AppState, StoreEvent};

/// Build OTLP ingestion routes.
pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/v1/traces", post(otlp_ingest_traces))
        .route("/api/import/otel", post(otlp_ingest_traces))
        .with_state(state)
}

/// Handler for OTLP trace ingestion.
///
/// Accepts `application/x-protobuf` with optional gzip compression.
/// Returns `ExportTraceServiceResponse` (protobuf).
pub async fn otlp_ingest_traces(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    // 1. Check for gzip compression
    let is_gzip = headers
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("gzip"));

    // 2. Decode protobuf request
    let request = ingest::decode_otlp_request(&body, is_gzip)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Decode error: {e}")))?;

    // 3. Ingest into store
    let result = {
        let store = state
            .store
            .lock()
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}")))?;
        ingest::ingest_trace_request(request, &store, &ingest::IngestOptions::default())
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Ingest error: {e}")))?
    };

    // 4. Emit events for WebSocket live updates
    let _ = state.event_tx.send(StoreEvent::SessionUpdated {
        session_id: result.session_id.clone(),
        status: "completed".to_string(),
        total_steps: result.steps_created as u32,
        total_tokens: result.total_tokens,
    });

    tracing::info!(
        session_id = %result.session_id,
        spans = result.spans_ingested,
        steps = result.steps_created,
        replay = result.replay_possible,
        "Ingested OTLP trace"
    );

    // 5. Return protobuf response
    let response_bytes = ingest::encode_otlp_response(&ingest::success_response());

    Ok((
        StatusCode::OK,
        [("content-type", "application/x-protobuf")],
        response_bytes,
    ))
}
