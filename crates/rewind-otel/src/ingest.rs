//! OTel trace ingestion: convert incoming OTLP traces into Rewind sessions.
//!
//! This is the reverse of `export.rs` — it accepts `ExportTraceServiceRequest`
//! and creates Session + Timeline + Step records in the Store.

use anyhow::Result;
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use opentelemetry_proto::tonic::common::v1::any_value::Value as OtelValue;
use opentelemetry_proto::tonic::trace::v1::Span as OtelSpan;
use rewind_store::models::{
    Session, SessionSource, SessionStatus, Step, StepStatus, StepType, Timeline,
};
use rewind_store::Store;
use std::collections::HashMap;
use uuid::Uuid;

/// Options for ingesting an OTLP trace.
#[derive(Debug, Clone, Default)]
pub struct IngestOptions {
    /// Override the auto-generated session name.
    pub session_name: Option<String>,
}

/// Result of ingesting an OTLP trace.
#[derive(Debug, Clone)]
pub struct IngestResult {
    pub session_id: String,
    pub spans_ingested: usize,
    pub steps_created: usize,
    pub total_tokens: u64,
    /// True if at least one step has content blobs (making replay possible).
    pub replay_possible: bool,
}

/// Ingest an OTLP `ExportTraceServiceRequest` into the Store.
///
/// Caller must lock `Arc<Mutex<Store>>` before calling.
pub fn ingest_trace_request(
    request: ExportTraceServiceRequest,
    store: &Store,
    opts: &IngestOptions,
) -> Result<IngestResult> {
    // 1. Collect all spans from the request
    let spans = collect_spans(&request);
    let spans_ingested = spans.len();

    if spans.is_empty() {
        anyhow::bail!("No spans found in the OTLP request");
    }

    // 2. Build parent-child tree
    let children_map = build_children_map(&spans);
    // 3. Identify root span(s) — those with empty parent_span_id
    let root_spans: Vec<&OtelSpan> = spans
        .iter()
        .filter(|s| s.parent_span_id.is_empty())
        .collect();

    // 4. Derive session info from root span (or synthesize)
    let (session_name, trace_id_hex, session_start_ns, session_end_ns) = if root_spans.len() == 1 {
        let root = root_spans[0];
        (
            root.name.clone(),
            hex_encode(&root.trace_id),
            root.start_time_unix_nano,
            root.end_time_unix_nano,
        )
    } else {
        // Flat trace — multiple roots or no hierarchy.
        // Synthesize a virtual root from the span collection.
        let trace_id_hex = spans
            .first()
            .map(|s| hex_encode(&s.trace_id))
            .unwrap_or_default();
        let start = spans.iter().map(|s| s.start_time_unix_nano).min().unwrap_or(0);
        let end = spans.iter().map(|s| s.end_time_unix_nano).max().unwrap_or(0);
        (format!("imported-{}", &trace_id_hex[..8.min(trace_id_hex.len())]), trace_id_hex, start, end)
    };

    let session_name = opts
        .session_name
        .clone()
        .unwrap_or(session_name);

    // 5. Create Session
    let session_id = Uuid::new_v4().to_string();
    let created_at = nanos_to_datetime(session_start_ns);
    let updated_at = nanos_to_datetime(session_end_ns);

    // 6. Identify step-level spans: leaf spans and spans that map to step types.
    //    Strategy: if we have a clear hierarchy (root → timeline → steps), use it.
    //    Otherwise, treat all non-root spans as steps (or all spans if flat trace).
    let (timeline_spans, step_spans) = classify_spans(
        &root_spans,
        &children_map,
        &spans,
    );

    // 7. Sort step spans chronologically, assign step_number
    let mut step_spans_sorted: Vec<&OtelSpan> = step_spans;
    step_spans_sorted.sort_by(|a, b| {
        a.start_time_unix_nano
            .cmp(&b.start_time_unix_nano)
            .then_with(|| hex_encode(&a.span_id).cmp(&hex_encode(&b.span_id)))
    });

    // 8. Create timeline(s)
    let timeline_id = Uuid::new_v4().to_string();
    let timeline_label = if timeline_spans.is_empty() {
        "main".to_string()
    } else {
        timeline_spans
            .first()
            .map(|s| s.name.strip_prefix("timeline ").unwrap_or(&s.name).to_string())
            .unwrap_or_else(|| "main".to_string())
    };

    // 9. Map spans to Steps and store blobs
    let mut steps = Vec::new();
    let mut has_content = false;

    for (idx, span) in step_spans_sorted.iter().enumerate() {
        let step_number = (idx + 1) as u32;
        let step_type = infer_step_type(span);
        let model = get_string_attr(span, "gen_ai.request.model").unwrap_or_default();
        let tokens_in = get_i64_attr(span, "gen_ai.usage.input_tokens").unwrap_or(0) as u64;
        let tokens_out = get_i64_attr(span, "gen_ai.usage.output_tokens").unwrap_or(0) as u64;
        let tool_name = get_string_attr(span, "gen_ai.tool.name");
        let duration_ms = if span.end_time_unix_nano > span.start_time_unix_nano {
            (span.end_time_unix_nano - span.start_time_unix_nano) / 1_000_000
        } else {
            0
        };

        // Store content blobs if present
        let request_blob = match get_string_attr(span, "gen_ai.input.messages") {
            Some(content) => {
                let value: serde_json::Value =
                    serde_json::from_str(&content).unwrap_or(serde_json::Value::String(content));
                store.blobs.put_json(&value).unwrap_or_default()
            }
            None => String::new(),
        };

        let response_blob = match get_string_attr(span, "gen_ai.output.messages") {
            Some(content) => {
                let value: serde_json::Value =
                    serde_json::from_str(&content).unwrap_or(serde_json::Value::String(content));
                store.blobs.put_json(&value).unwrap_or_default()
            }
            None => String::new(),
        };

        if !request_blob.is_empty() || !response_blob.is_empty() {
            has_content = true;
        }

        // Error detection
        let (status, error) = if span.status.as_ref().is_some_and(|s| s.code == 2) {
            let msg = span
                .status
                .as_ref()
                .map(|s| s.message.clone())
                .unwrap_or_default();
            (
                StepStatus::Error,
                if msg.is_empty() { None } else { Some(msg) },
            )
        } else {
            (StepStatus::Success, None)
        };

        let step = Step {
            id: Uuid::new_v4().to_string(),
            timeline_id: timeline_id.clone(),
            session_id: session_id.clone(),
            step_number,
            step_type,
            status,
            created_at: nanos_to_datetime(span.start_time_unix_nano),
            duration_ms,
            tokens_in,
            tokens_out,
            model,
            request_blob,
            response_blob,
            error,
            span_id: Some(hex_encode(&span.span_id)),
            tool_name,
        };

        steps.push(step);
    }

    // 10. Persist to store
    let total_tokens: u64 = steps.iter().map(|s| s.tokens_in + s.tokens_out).sum();
    let total_steps = steps.len() as u32;

    let metadata = serde_json::json!({
        "import_source": "otel",
        "original_trace_id": trace_id_hex,
        "content_available": has_content,
        "replay_possible": has_content,
    });

    let session = Session {
        id: session_id.clone(),
        name: session_name,
        created_at,
        updated_at,
        status: SessionStatus::Completed,
        source: SessionSource::OtelImport,
        total_steps,
        total_tokens,
        metadata,
        thread_id: None,
        thread_ordinal: None,
    };

    store.create_session(&session)?;

    let timeline = Timeline {
        id: timeline_id,
        session_id: session_id.clone(),
        parent_timeline_id: None,
        fork_at_step: None,
        created_at,
        label: timeline_label,
    };

    store.create_timeline(&timeline)?;

    for step in &steps {
        store.create_step(step)?;
    }

    Ok(IngestResult {
        session_id,
        spans_ingested,
        steps_created: steps.len(),
        total_tokens,
        replay_possible: has_content,
    })
}

/// Build an empty `ExportTraceServiceResponse` (success, no partial failures).
pub fn success_response() -> ExportTraceServiceResponse {
    ExportTraceServiceResponse {
        partial_success: None,
    }
}

/// Decode an OTLP protobuf request from raw bytes (with optional gzip decompression).
pub fn decode_otlp_request(body: &[u8], is_gzip: bool) -> Result<ExportTraceServiceRequest> {
    use prost::Message;

    let bytes = if is_gzip {
        use flate2::read::GzDecoder;
        use std::io::Read;
        let mut decoder = GzDecoder::new(body);
        let mut decompressed = Vec::new();
        decoder
            .read_to_end(&mut decompressed)
            .map_err(|e| anyhow::anyhow!("gzip decompression failed: {e}"))?;
        decompressed
    } else {
        body.to_vec()
    };

    ExportTraceServiceRequest::decode(bytes.as_slice())
        .map_err(|e| anyhow::anyhow!("protobuf decode failed: {e}"))
}

/// Encode an `ExportTraceServiceResponse` to protobuf bytes.
pub fn encode_otlp_response(response: &ExportTraceServiceResponse) -> Vec<u8> {
    use prost::Message;
    response.encode_to_vec()
}

// ── Internal helpers ─────────────────────────────────────

/// Collect all spans from the OTLP request into a flat vec.
fn collect_spans(request: &ExportTraceServiceRequest) -> Vec<OtelSpan> {
    let mut spans = Vec::new();
    for rs in &request.resource_spans {
        for ss in &rs.scope_spans {
            for span in &ss.spans {
                spans.push(span.clone());
            }
        }
    }
    spans
}

/// Build a map: parent_span_id → list of child spans.
fn build_children_map(spans: &[OtelSpan]) -> HashMap<Vec<u8>, Vec<usize>> {
    let mut map: HashMap<Vec<u8>, Vec<usize>> = HashMap::new();
    for (idx, span) in spans.iter().enumerate() {
        if !span.parent_span_id.is_empty() {
            map.entry(span.parent_span_id.clone())
                .or_default()
                .push(idx);
        }
    }
    map
}

/// Classify spans into timeline-level and step-level.
///
/// Hierarchy patterns:
/// - Root → Timeline → Steps  (Rewind-exported traces)
/// - Root → Steps             (flat traces, no timeline layer)
/// - All roots (flat)         (no hierarchy at all)
fn classify_spans<'a>(
    root_spans: &[&'a OtelSpan],
    children_map: &HashMap<Vec<u8>, Vec<usize>>,
    all_spans: &'a [OtelSpan],
) -> (Vec<&'a OtelSpan>, Vec<&'a OtelSpan>) {
    if root_spans.len() != 1 {
        // Flat trace — all spans are steps
        return (Vec::new(), all_spans.iter().collect());
    }

    let root = root_spans[0];
    let root_children_idx = match children_map.get(&root.span_id) {
        Some(idx) => idx.clone(),
        None => {
            // Root with no children — just the root is a step
            return (Vec::new(), vec![root]);
        }
    };

    // Check if root's children look like timeline spans (have their own children
    // that look like steps). Heuristic: if a child's name starts with "timeline "
    // and it has children, treat it as a timeline span.
    let mut timeline_spans = Vec::new();
    let mut step_spans = Vec::new();

    let has_timeline_layer = root_children_idx.iter().any(|&idx| {
        let child = &all_spans[idx];
        child.name.starts_with("timeline ")
            && children_map.contains_key(&child.span_id)
    });

    if has_timeline_layer {
        // Root → Timeline → Steps
        for &child_idx in &root_children_idx {
            let child = &all_spans[child_idx];
            if child.name.starts_with("timeline ") {
                timeline_spans.push(child);
                // Grandchildren are steps
                if let Some(grandchildren) = children_map.get(&child.span_id) {
                    for &gc_idx in grandchildren {
                        step_spans.push(&all_spans[gc_idx]);
                    }
                }
            } else {
                // Non-timeline child of root — treat as step
                step_spans.push(child);
            }
        }
    } else {
        // Root → Steps (no timeline layer)
        for &child_idx in &root_children_idx {
            step_spans.push(&all_spans[child_idx]);
        }
    }

    (timeline_spans, step_spans)
}

/// Infer `StepType` from an OTel span's name and attributes.
fn infer_step_type(span: &OtelSpan) -> StepType {
    // Check span name patterns (from Rewind export format)
    if span.name.starts_with("gen_ai.chat") || span.name.starts_with("gen_ai.chat ") {
        return StepType::LlmCall;
    }
    if span.name.starts_with("tool.execute") {
        return StepType::ToolCall;
    }
    if span.name.starts_with("tool.result") {
        return StepType::ToolResult;
    }
    if span.name == "user.prompt" || span.name.starts_with("user.prompt") {
        return StepType::UserPrompt;
    }
    if span.name.starts_with("hook.event") {
        return StepType::HookEvent;
    }

    // Check attributes as fallback
    if get_string_attr(span, "gen_ai.request.model").is_some()
        || get_string_attr(span, "gen_ai.operation.name").as_deref() == Some("chat")
    {
        return StepType::LlmCall;
    }
    if get_string_attr(span, "gen_ai.tool.name").is_some() {
        return StepType::ToolCall;
    }

    // Default: treat as HookEvent for unknown spans
    StepType::HookEvent
}

/// Extract a string attribute from an OTel span.
fn get_string_attr(span: &OtelSpan, key: &str) -> Option<String> {
    span.attributes.iter().find_map(|kv| {
        if kv.key == key {
            kv.value.as_ref().and_then(|v| match &v.value {
                Some(OtelValue::StringValue(s)) => Some(s.clone()),
                _ => None,
            })
        } else {
            None
        }
    })
}

/// Extract an i64 attribute from an OTel span.
fn get_i64_attr(span: &OtelSpan, key: &str) -> Option<i64> {
    span.attributes.iter().find_map(|kv| {
        if kv.key == key {
            kv.value.as_ref().and_then(|v| match &v.value {
                Some(OtelValue::IntValue(i)) => Some(*i),
                _ => None,
            })
        } else {
            None
        }
    })
}

/// Encode bytes as lowercase hex string.
fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Convert nanoseconds since epoch to `chrono::DateTime<Utc>`.
fn nanos_to_datetime(nanos: u64) -> chrono::DateTime<chrono::Utc> {
    let secs = (nanos / 1_000_000_000) as i64;
    let subsec_nanos = (nanos % 1_000_000_000) as u32;
    chrono::DateTime::from_timestamp(secs, subsec_nanos).unwrap_or_default()
}

// ── Test helpers ─────────────────────────────────────────

/// Build an OTel span for testing.
#[cfg(test)]
fn make_otel_span(
    name: &str,
    trace_id: &[u8],
    span_id: &[u8],
    parent_span_id: &[u8],
    attrs: Vec<opentelemetry_proto::tonic::common::v1::KeyValue>,
) -> OtelSpan {
    use opentelemetry_proto::tonic::trace::v1::span::SpanKind;
    use opentelemetry_proto::tonic::trace::v1::Status as OtelStatus;

    OtelSpan {
        trace_id: trace_id.to_vec(),
        span_id: span_id.to_vec(),
        parent_span_id: parent_span_id.to_vec(),
        name: name.to_string(),
        kind: SpanKind::Internal as i32,
        start_time_unix_nano: 1_700_000_000_000_000_000,
        end_time_unix_nano: 1_700_000_001_500_000_000,
        attributes: attrs,
        status: Some(OtelStatus {
            code: 0,
            message: String::new(),
        }),
        ..Default::default()
    }
}

#[cfg(test)]
fn make_string_attr(key: &str, value: &str) -> opentelemetry_proto::tonic::common::v1::KeyValue {
    use opentelemetry_proto::tonic::common::v1::AnyValue;
    opentelemetry_proto::tonic::common::v1::KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(OtelValue::StringValue(value.to_string())),
        }),
    }
}

#[cfg(test)]
fn make_int_attr(key: &str, value: i64) -> opentelemetry_proto::tonic::common::v1::KeyValue {
    use opentelemetry_proto::tonic::common::v1::AnyValue;
    opentelemetry_proto::tonic::common::v1::KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(OtelValue::IntValue(value)),
        }),
    }
}

#[cfg(test)]
fn make_request(spans: Vec<OtelSpan>) -> ExportTraceServiceRequest {
    use opentelemetry_proto::tonic::trace::v1::ResourceSpans;
    use opentelemetry_proto::tonic::trace::v1::ScopeSpans;
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            scope_spans: vec![ScopeSpans {
                spans,
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Unit tests for helpers ──

    #[test]
    fn test_nanos_to_datetime() {
        let dt = nanos_to_datetime(1_700_000_000_000_000_000);
        assert_eq!(dt.timestamp(), 1_700_000_000);
    }

    #[test]
    fn test_nanos_to_datetime_zero() {
        let dt = nanos_to_datetime(0);
        assert_eq!(dt.timestamp(), 0);
    }

    #[test]
    fn test_infer_step_type_llm_by_name() {
        let span = make_otel_span("gen_ai.chat gpt-4o", &[1; 16], &[1; 8], &[], vec![]);
        assert_eq!(infer_step_type(&span), StepType::LlmCall);
    }

    #[test]
    fn test_infer_step_type_tool_call_by_name() {
        let span = make_otel_span("tool.execute search_web", &[1; 16], &[2; 8], &[], vec![]);
        assert_eq!(infer_step_type(&span), StepType::ToolCall);
    }

    #[test]
    fn test_infer_step_type_tool_result_by_name() {
        let span = make_otel_span("tool.result search_web", &[1; 16], &[3; 8], &[], vec![]);
        assert_eq!(infer_step_type(&span), StepType::ToolResult);
    }

    #[test]
    fn test_infer_step_type_user_prompt_by_name() {
        let span = make_otel_span("user.prompt", &[1; 16], &[4; 8], &[], vec![]);
        assert_eq!(infer_step_type(&span), StepType::UserPrompt);
    }

    #[test]
    fn test_infer_step_type_llm_by_attribute() {
        let span = make_otel_span(
            "some_custom_name",
            &[1; 16],
            &[5; 8],
            &[],
            vec![make_string_attr("gen_ai.request.model", "gpt-4o")],
        );
        assert_eq!(infer_step_type(&span), StepType::LlmCall);
    }

    #[test]
    fn test_infer_step_type_tool_by_attribute() {
        let span = make_otel_span(
            "some_custom_name",
            &[1; 16],
            &[6; 8],
            &[],
            vec![make_string_attr("gen_ai.tool.name", "calculator")],
        );
        assert_eq!(infer_step_type(&span), StepType::ToolCall);
    }

    #[test]
    fn test_infer_step_type_unknown_defaults_to_hook_event() {
        let span = make_otel_span("something_else", &[1; 16], &[7; 8], &[], vec![]);
        assert_eq!(infer_step_type(&span), StepType::HookEvent);
    }

    #[test]
    fn test_get_string_attr_found() {
        let span = make_otel_span(
            "test",
            &[1; 16],
            &[1; 8],
            &[],
            vec![make_string_attr("gen_ai.request.model", "gpt-4o")],
        );
        assert_eq!(
            get_string_attr(&span, "gen_ai.request.model"),
            Some("gpt-4o".to_string())
        );
    }

    #[test]
    fn test_get_string_attr_missing() {
        let span = make_otel_span("test", &[1; 16], &[1; 8], &[], vec![]);
        assert_eq!(get_string_attr(&span, "gen_ai.request.model"), None);
    }

    #[test]
    fn test_get_i64_attr_found() {
        let span = make_otel_span(
            "test",
            &[1; 16],
            &[1; 8],
            &[],
            vec![make_int_attr("gen_ai.usage.input_tokens", 500)],
        );
        assert_eq!(get_i64_attr(&span, "gen_ai.usage.input_tokens"), Some(500));
    }

    #[test]
    fn test_collect_spans_from_request() {
        let span1 = make_otel_span("span1", &[1; 16], &[1; 8], &[], vec![]);
        let span2 = make_otel_span("span2", &[1; 16], &[2; 8], &[1; 8], vec![]);
        let request = make_request(vec![span1, span2]);
        let spans = collect_spans(&request);
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn test_build_children_map() {
        let span1 = make_otel_span("root", &[1; 16], &[1; 8], &[], vec![]);
        let span2 = make_otel_span("child", &[1; 16], &[2; 8], &[1; 8], vec![]);
        let spans = vec![span1, span2];
        let map = build_children_map(&spans);
        assert_eq!(map.get(&vec![1u8; 8]).unwrap().len(), 1);
        assert!(!map.contains_key(&vec![2u8; 8])); // child has no children
    }

    #[test]
    fn test_classify_flat_trace() {
        // All spans have empty parent — flat trace
        let span1 = make_otel_span("span1", &[1; 16], &[1; 8], &[], vec![]);
        let span2 = make_otel_span("span2", &[1; 16], &[2; 8], &[], vec![]);
        let spans = vec![span1, span2];
        let children_map = build_children_map(&spans);
        let roots: Vec<&OtelSpan> = spans.iter().collect();

        let (tl_spans, step_spans) =
            classify_spans(&roots, &children_map, &spans);
        assert!(tl_spans.is_empty());
        assert_eq!(step_spans.len(), 2);
    }

    #[test]
    fn test_classify_root_with_steps() {
        // Root → step1, step2 (no timeline layer)
        let root = make_otel_span("session test", &[1; 16], &[1; 8], &[], vec![]);
        let step1 = make_otel_span("gen_ai.chat gpt-4o", &[1; 16], &[2; 8], &[1; 8], vec![]);
        let step2 = make_otel_span("tool.execute search", &[1; 16], &[3; 8], &[1; 8], vec![]);
        let spans = vec![root, step1, step2];
        let children_map = build_children_map(&spans);
        let roots: Vec<&OtelSpan> = vec![&spans[0]];

        let (tl_spans, step_spans) =
            classify_spans(&roots, &children_map, &spans);
        assert!(tl_spans.is_empty());
        assert_eq!(step_spans.len(), 2); // root's children are steps
    }

    #[test]
    fn test_classify_with_timeline_layer() {
        // Root → timeline main → step1, step2
        let root = make_otel_span("session test", &[1; 16], &[1; 8], &[], vec![]);
        let tl = make_otel_span("timeline main", &[1; 16], &[2; 8], &[1; 8], vec![]);
        let step1 = make_otel_span("gen_ai.chat gpt-4o", &[1; 16], &[3; 8], &[2; 8], vec![]);
        let step2 = make_otel_span("tool.execute search", &[1; 16], &[4; 8], &[2; 8], vec![]);
        let spans = vec![root, tl, step1, step2];
        let children_map = build_children_map(&spans);
        let roots: Vec<&OtelSpan> = vec![&spans[0]];

        let (tl_spans, step_spans) =
            classify_spans(&roots, &children_map, &spans);
        assert_eq!(tl_spans.len(), 1);
        assert_eq!(tl_spans[0].name, "timeline main");
        assert_eq!(step_spans.len(), 2);
    }

    #[test]
    fn test_success_response() {
        let resp = success_response();
        assert!(resp.partial_success.is_none());
    }

    // ── Integration test: full ingest pipeline ──

    #[test]
    fn test_ingest_full_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();

        // Build a trace: root → timeline → 2 steps (LLM call + tool call)
        let root = make_otel_span("session test-import", &[1; 16], &[1; 8], &[], vec![]);
        let tl = make_otel_span("timeline main", &[1; 16], &[2; 8], &[1; 8], vec![]);

        let mut step1 = make_otel_span(
            "gen_ai.chat gpt-4o",
            &[1; 16],
            &[3; 8],
            &[2; 8],
            vec![
                make_string_attr("gen_ai.request.model", "gpt-4o"),
                make_int_attr("gen_ai.usage.input_tokens", 100),
                make_int_attr("gen_ai.usage.output_tokens", 50),
            ],
        );
        step1.start_time_unix_nano = 1_700_000_000_000_000_000;
        step1.end_time_unix_nano = 1_700_000_001_500_000_000;

        let mut step2 = make_otel_span(
            "tool.execute search_web",
            &[1; 16],
            &[4; 8],
            &[2; 8],
            vec![make_string_attr("gen_ai.tool.name", "search_web")],
        );
        step2.start_time_unix_nano = 1_700_000_002_000_000_000;
        step2.end_time_unix_nano = 1_700_000_002_500_000_000;

        let request = make_request(vec![root, tl, step1, step2]);

        let result = ingest_trace_request(request, &store, &IngestOptions::default()).unwrap();

        assert_eq!(result.spans_ingested, 4);
        assert_eq!(result.steps_created, 2);
        assert!(!result.replay_possible); // no content blobs

        // Verify session was created
        let session = store.get_session(&result.session_id).unwrap().unwrap();
        assert_eq!(session.source, SessionSource::OtelImport);
        assert_eq!(session.status, SessionStatus::Completed);
        assert_eq!(session.total_steps, 2);
        assert_eq!(session.total_tokens, 150); // 100 + 50

        // Verify timeline
        let timelines = store.get_timelines(&result.session_id).unwrap();
        assert_eq!(timelines.len(), 1);
        assert_eq!(timelines[0].label, "main");

        // Verify steps
        let steps = store.get_steps(&timelines[0].id).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].step_type, StepType::LlmCall);
        assert_eq!(steps[0].model, "gpt-4o");
        assert_eq!(steps[0].tokens_in, 100);
        assert_eq!(steps[0].tokens_out, 50);
        assert_eq!(steps[0].step_number, 1);
        assert_eq!(steps[1].step_type, StepType::ToolCall);
        assert_eq!(steps[1].tool_name.as_deref(), Some("search_web"));
        assert_eq!(steps[1].step_number, 2);
    }

    #[test]
    fn test_ingest_with_content_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();

        let step = make_otel_span(
            "gen_ai.chat gpt-4o",
            &[1; 16],
            &[1; 8],
            &[], // flat trace — no parent
            vec![
                make_string_attr("gen_ai.request.model", "gpt-4o"),
                make_string_attr(
                    "gen_ai.input.messages",
                    r#"[{"role":"user","content":"hello"}]"#,
                ),
                make_string_attr(
                    "gen_ai.output.messages",
                    r#"[{"role":"assistant","content":"hi"}]"#,
                ),
            ],
        );

        let request = make_request(vec![step]);
        let result = ingest_trace_request(request, &store, &IngestOptions::default()).unwrap();

        assert!(result.replay_possible);
        assert_eq!(result.steps_created, 1);

        // Verify blobs were stored
        let timelines = store.get_timelines(&result.session_id).unwrap();
        let steps = store.get_steps(&timelines[0].id).unwrap();
        assert!(!steps[0].request_blob.is_empty());
        assert!(!steps[0].response_blob.is_empty());

        // Verify metadata marks content as available
        let session = store.get_session(&result.session_id).unwrap().unwrap();
        assert_eq!(session.metadata["content_available"], true);
        assert_eq!(session.metadata["replay_possible"], true);
    }

    #[test]
    fn test_ingest_with_session_name_override() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();

        let span = make_otel_span("gen_ai.chat gpt-4o", &[1; 16], &[1; 8], &[], vec![]);
        let request = make_request(vec![span]);

        let opts = IngestOptions {
            session_name: Some("my-custom-name".to_string()),
        };
        let result = ingest_trace_request(request, &store, &opts).unwrap();

        let session = store.get_session(&result.session_id).unwrap().unwrap();
        assert_eq!(session.name, "my-custom-name");
    }

    #[test]
    fn test_ingest_empty_request_fails() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();

        let request = ExportTraceServiceRequest {
            resource_spans: vec![],
        };
        let result = ingest_trace_request(request, &store, &IngestOptions::default());
        assert!(result.is_err());
    }

    #[test]
    fn test_ingest_error_step() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();

        use opentelemetry_proto::tonic::trace::v1::Status as OtelStatus;

        let mut span = make_otel_span(
            "gen_ai.chat gpt-4o",
            &[1; 16],
            &[1; 8],
            &[],
            vec![make_string_attr("gen_ai.request.model", "gpt-4o")],
        );
        span.status = Some(OtelStatus {
            code: 2, // ERROR
            message: "rate_limit_exceeded".to_string(),
        });

        let request = make_request(vec![span]);
        let result = ingest_trace_request(request, &store, &IngestOptions::default()).unwrap();

        let timelines = store.get_timelines(&result.session_id).unwrap();
        let steps = store.get_steps(&timelines[0].id).unwrap();
        assert_eq!(steps[0].status, StepStatus::Error);
        assert_eq!(steps[0].error.as_deref(), Some("rate_limit_exceeded"));
    }

    #[test]
    fn test_ingest_chronological_ordering() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();

        // Create spans in reverse chronological order
        let mut span_late = make_otel_span("gen_ai.chat late", &[1; 16], &[1; 8], &[], vec![]);
        span_late.start_time_unix_nano = 1_700_000_002_000_000_000;
        span_late.end_time_unix_nano = 1_700_000_003_000_000_000;

        let mut span_early = make_otel_span("gen_ai.chat early", &[1; 16], &[2; 8], &[], vec![]);
        span_early.start_time_unix_nano = 1_700_000_000_000_000_000;
        span_early.end_time_unix_nano = 1_700_000_001_000_000_000;

        // Insert late first, early second
        let request = make_request(vec![span_late, span_early]);
        let result = ingest_trace_request(request, &store, &IngestOptions::default()).unwrap();

        let timelines = store.get_timelines(&result.session_id).unwrap();
        let steps = store.get_steps(&timelines[0].id).unwrap();

        // Step 1 should be the earlier span
        assert_eq!(steps[0].step_number, 1);
        assert!(steps[0].created_at < steps[1].created_at);
        assert_eq!(steps[1].step_number, 2);
    }
}
