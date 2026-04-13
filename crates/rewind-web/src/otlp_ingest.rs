//! OTLP trace ingestion endpoint: `POST /v1/traces` and `POST /api/import/otel`.
//!
//! Accepts `ExportTraceServiceRequest` (protobuf or JSON), creates Rewind sessions from the spans.

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
/// Accepts `application/x-protobuf` (with optional gzip) or `application/json`.
/// Returns `ExportTraceServiceResponse` (protobuf) with `X-Rewind-Session-Id` header.
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

    // 2. Check content type for JSON vs protobuf
    let is_json = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("json"));

    // 3. Decode request
    let request = if is_json {
        ingest::decode_otlp_json_request(&body)
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("JSON decode error: {e}")))?
    } else {
        ingest::decode_otlp_request(&body, is_gzip)
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("Protobuf decode error: {e}")))?
    };

    // 4. Read optional session name from header
    let session_name = headers
        .get("x-rewind-session-name")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let opts = ingest::IngestOptions { session_name };

    // 5. Ingest into store
    let result = {
        let store = state
            .store
            .lock()
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}")))?;
        ingest::ingest_trace_request(request, &store, &opts)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Ingest error: {e}")))?
    };

    // 6. Emit events for WebSocket live updates
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

    // 7. Return protobuf response with session_id header
    let response_bytes = ingest::encode_otlp_response(&ingest::success_response());

    let response = axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/x-protobuf")
        .header("x-rewind-session-id", &result.session_id)
        .body(axum::body::Body::from(response_bytes))
        .unwrap();

    Ok(response)
}
