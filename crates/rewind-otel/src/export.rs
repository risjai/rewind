use anyhow::{Context, Result};
use opentelemetry::trace::{SpanKind, TraceContextExt, TraceFlags, TraceState, Tracer, TracerProvider};
use opentelemetry::KeyValue;
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig, WithTonicConfig};
use opentelemetry_sdk::trace::{SdkTracerProvider, SpanLimits};
use opentelemetry_sdk::Resource;
use sha2::{Digest, Sha256};
use std::time::SystemTime;

use crate::attributes::{self, OtelSpanKind};
use crate::extract::SessionExportData;

/// OTLP export protocol.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Protocol {
    Grpc,
    Http,
}

/// Configuration for the OTel exporter.
#[derive(Debug, Clone)]
pub struct ExportConfig {
    pub endpoint: String,
    pub protocol: Protocol,
    pub headers: Vec<(String, String)>,
    pub include_content: bool,
}

impl Default for ExportConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:4318".to_string(),
            protocol: Protocol::Http,
            headers: Vec::new(),
            include_content: false,
        }
    }
}

/// Generate a deterministic 16-byte trace ID from a session ID.
/// Uses SHA-256(session_id)[0..16].
pub fn trace_id_from_session(session_id: &str) -> opentelemetry::trace::TraceId {
    let mut hasher = Sha256::new();
    hasher.update(session_id.as_bytes());
    let hash = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    opentelemetry::trace::TraceId::from_bytes(bytes)
}

/// Generate a deterministic 8-byte span ID from a step/timeline ID.
/// Uses SHA-256(id)[0..8].
pub fn span_id_from_id(id: &str) -> opentelemetry::trace::SpanId {
    let mut hasher = Sha256::new();
    hasher.update(id.as_bytes());
    let hash = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&hash[..8]);
    opentelemetry::trace::SpanId::from_bytes(bytes)
}

/// Convert a chrono DateTime to SystemTime.
fn to_system_time(dt: &chrono::DateTime<chrono::Utc>) -> SystemTime {
    let ts = dt.timestamp();
    let nanos = dt.timestamp_subsec_nanos();
    SystemTime::UNIX_EPOCH + std::time::Duration::new(ts as u64, nanos)
}

/// Export session data to an OTLP endpoint.
///
/// Returns the number of spans exported.
pub fn export_to_otlp(data: &SessionExportData, config: &ExportConfig) -> Result<usize> {
    let provider = build_provider(config)?;
    let span_count = emit_spans(&provider, data, config)?;

    provider
        .shutdown()
        .context("Failed to flush OTel provider")?;

    Ok(span_count)
}

/// Export session data to stdout (for --dry-run).
///
/// Returns the number of spans exported.
pub fn export_to_stdout(data: &SessionExportData, config: &ExportConfig) -> Result<usize> {
    let provider = build_stdout_provider()?;
    let span_count = emit_spans(&provider, data, config)?;

    provider
        .shutdown()
        .context("Failed to flush stdout provider")?;

    Ok(span_count)
}

fn build_provider(config: &ExportConfig) -> Result<SdkTracerProvider> {
    let exporter = match config.protocol {
        Protocol::Grpc => {
            let mut metadata = tonic::metadata::MetadataMap::new();
            for (key, value) in &config.headers {
                let key = key.parse::<tonic::metadata::MetadataKey<tonic::metadata::Ascii>>()
                    .context("Invalid gRPC metadata key")?;
                let value = value.parse().context("Invalid gRPC metadata value")?;
                metadata.insert(key, value);
            }
            let mut builder = opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(&config.endpoint);
            if !metadata.is_empty() {
                builder = builder.with_metadata(metadata);
            }
            builder.build().context("Failed to build gRPC exporter")?
        }
        Protocol::Http => {
            let mut headers = std::collections::HashMap::new();
            for (key, value) in &config.headers {
                headers.insert(key.clone(), value.clone());
            }
            let mut builder = opentelemetry_otlp::SpanExporter::builder()
                .with_http()
                .with_endpoint(&config.endpoint);
            if !headers.is_empty() {
                builder = builder.with_headers(headers);
            }
            builder.build().context("Failed to build HTTP exporter")?
        }
    };

    let resource = Resource::builder()
        .with_service_name("rewind")
        .with_attributes([KeyValue::new(
            "service.version",
            env!("CARGO_PKG_VERSION"),
        )])
        .build();

    Ok(SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .with_span_limits(SpanLimits::default())
        .build())
}

fn build_stdout_provider() -> Result<SdkTracerProvider> {
    let exporter = opentelemetry_stdout::SpanExporter::default();
    let resource = Resource::builder()
        .with_service_name("rewind")
        .with_attributes([KeyValue::new(
            "service.version",
            env!("CARGO_PKG_VERSION"),
        )])
        .build();

    Ok(SdkTracerProvider::builder()
        .with_simple_exporter(exporter)
        .with_resource(resource)
        .build())
}

/// Create OTel spans from the pre-extracted session data.
fn emit_spans(
    provider: &SdkTracerProvider,
    data: &SessionExportData,
    config: &ExportConfig,
) -> Result<usize> {
    use opentelemetry::trace::Span;

    let tracer = provider.tracer("rewind-otel");
    let trace_id = trace_id_from_session(&data.session.id);
    let mut span_count = 0;

    // Seed a root context with our deterministic trace ID so all child spans
    // inherit it instead of the SDK generating a random one.
    let root_span_ctx = opentelemetry::trace::SpanContext::new(
        trace_id,
        span_id_from_id("root-seed"), // placeholder, won't appear in output
        TraceFlags::SAMPLED,
        false,
        TraceState::default(),
    );
    let root_ctx = opentelemetry::Context::new().with_remote_span_context(root_span_ctx);

    // Session root span
    let session_start = to_system_time(&data.session.created_at);
    let session_end = to_system_time(&data.session.updated_at);

    let mut session_span = tracer
        .span_builder(format!("session {}", data.session.name))
        .with_kind(SpanKind::Internal)
        .with_start_time(session_start)
        .with_attributes(vec![
            KeyValue::new("rewind.session.id", data.session.id.clone()),
            KeyValue::new("rewind.session.name", data.session.name.clone()),
            KeyValue::new("rewind.session.source", data.session.source.as_str().to_string()),
            KeyValue::new("rewind.session.total_steps", data.session.total_steps as i64),
            KeyValue::new("rewind.session.total_tokens", data.session.total_tokens as i64),
        ])
        .start_with_context(&tracer, &root_ctx);

    // Read back the actual SDK-assigned span ID for use as parent
    let actual_session_span_id = session_span.span_context().span_id();
    session_span.end_with_timestamp(session_end);
    span_count += 1;

    // Build parent context from the actual session span ID
    let session_parent_ctx = opentelemetry::trace::SpanContext::new(
        trace_id,
        actual_session_span_id,
        TraceFlags::SAMPLED,
        false,
        TraceState::default(),
    );
    let session_otel_ctx =
        opentelemetry::Context::new().with_remote_span_context(session_parent_ctx);

    // Timeline spans + step spans
    for timeline in &data.timelines {
        let tl_start = to_system_time(&timeline.created_at);

        let mut tl_attrs = vec![
            KeyValue::new("rewind.timeline.id", timeline.id.clone()),
            KeyValue::new("rewind.timeline.label", timeline.label.clone()),
        ];
        if let Some(ref parent) = timeline.parent_timeline_id {
            tl_attrs.push(KeyValue::new("rewind.timeline.parent_id", parent.clone()));
        }
        if let Some(fork_at) = timeline.fork_at_step {
            tl_attrs.push(KeyValue::new("rewind.timeline.fork_at_step", fork_at as i64));
        }

        let mut tl_span = tracer
            .span_builder(format!("timeline {}", timeline.label))
            .with_kind(SpanKind::Internal)
            .with_start_time(tl_start)
            .with_attributes(tl_attrs)
            .start_with_context(&tracer, &session_otel_ctx);

        // Read back actual timeline span ID for step parenting
        let actual_tl_span_id = tl_span.span_context().span_id();
        span_count += 1;

        // Build parent context from actual timeline span ID
        let tl_parent_ctx = opentelemetry::trace::SpanContext::new(
            trace_id,
            actual_tl_span_id,
            TraceFlags::SAMPLED,
            false,
            TraceState::default(),
        );
        let tl_otel_ctx =
            opentelemetry::Context::new().with_remote_span_context(tl_parent_ctx);

        // Step spans under this timeline
        if let Some(steps) = data.steps_by_timeline.get(&timeline.id) {
            for step in steps {
                let name = attributes::span_name(step);
                let kind = match attributes::span_kind(step) {
                    OtelSpanKind::Client => SpanKind::Client,
                    OtelSpanKind::Internal => SpanKind::Internal,
                };

                let req_blob = data.get_blob(&step.request_blob);
                let resp_blob = data.get_blob(&step.response_blob);
                let mut attrs =
                    attributes::step_attributes(step, req_blob, resp_blob, config.include_content);

                // Mark cached/replayed steps
                if let Some(fork_at) = timeline.fork_at_step
                    && step.step_number <= fork_at
                    && timeline.parent_timeline_id.is_some()
                {
                    attrs.push(KeyValue::new("rewind.replay.cached", true));
                }

                let step_start = to_system_time(&step.created_at);
                let step_end = step_start
                    + std::time::Duration::from_millis(step.duration_ms);

                let mut step_span = tracer
                    .span_builder(name)
                    .with_kind(kind)
                    .with_start_time(step_start)
                    .with_attributes(attrs)
                    .start_with_context(&tracer, &tl_otel_ctx);

                step_span.end_with_timestamp(step_end);
                span_count += 1;
            }
        }

        // Timeline end = max(step end times), not sum of durations
        let tl_end = data
            .steps_by_timeline
            .get(&timeline.id)
            .and_then(|steps| {
                steps.iter().map(|s| {
                    to_system_time(&s.created_at)
                        + std::time::Duration::from_millis(s.duration_ms)
                }).max()
            })
            .unwrap_or(tl_start);

        tl_span.end_with_timestamp(tl_end);
    }

    Ok(span_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trace_id_deterministic() {
        let id1 = trace_id_from_session("session-abc-123");
        let id2 = trace_id_from_session("session-abc-123");
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_trace_id_different_sessions() {
        let id1 = trace_id_from_session("session-1");
        let id2 = trace_id_from_session("session-2");
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_span_id_deterministic() {
        let id1 = span_id_from_id("step-abc");
        let id2 = span_id_from_id("step-abc");
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_span_id_different_steps() {
        let id1 = span_id_from_id("step-1");
        let id2 = span_id_from_id("step-2");
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_trace_id_is_16_bytes() {
        let id = trace_id_from_session("test");
        // TraceId is always 16 bytes
        assert_ne!(id, opentelemetry::trace::TraceId::INVALID);
    }

    #[test]
    fn test_to_system_time() {
        let dt = chrono::Utc::now();
        let st = to_system_time(&dt);
        let since_epoch = st.duration_since(SystemTime::UNIX_EPOCH).unwrap();
        assert!(since_epoch.as_secs() > 0);
    }
}
