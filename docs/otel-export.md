# OpenTelemetry Export

Export recorded Rewind sessions as OpenTelemetry traces to any OTLP-compatible backend — Langfuse, Datadog, Grafana Tempo, Jaeger, and more.

## Quick Start

### CLI

```bash
# Export latest session to a local OTel collector
rewind export otel latest --endpoint http://localhost:4318

# Export to Langfuse
rewind export otel latest \
  --endpoint https://cloud.langfuse.com/api/public/otel \
  --header "Authorization=Bearer pk-lf-..."

# Dry run — print spans to stdout
rewind export otel latest --dry-run
```

### Python SDK

```bash
pip install rewind-agent[otel]
```

```python
import rewind_agent

# Export latest session
rewind_agent.export_otel("latest")

# Export with options
rewind_agent.export_otel(
    "latest",
    endpoint="https://cloud.langfuse.com/api/public/otel",
    headers={"Authorization": "Bearer pk-lf-..."},
    include_content=True,
    all_timelines=True,
)
```

### Web Dashboard

Click the **Export** button in the session detail stats bar. Requires the server to be started with `REWIND_OTEL_ENDPOINT` set:

```bash
REWIND_OTEL_ENDPOINT=http://localhost:4318 rewind web
```

## CLI Reference

```
rewind export otel <SESSION>
```

| Flag | Env Var | Default | Description |
|:-----|:--------|:--------|:------------|
| `--endpoint` | `OTEL_EXPORTER_OTLP_ENDPOINT` | `http://localhost:4318` | OTLP endpoint URL |
| `--protocol` | `OTEL_EXPORTER_OTLP_PROTOCOL` | `http` | `http` or `grpc` |
| `--header` | `OTEL_EXPORTER_OTLP_HEADERS` | — | `KEY=VALUE` (repeatable, comma-separated in env) |
| `--timeline` | — | main | Export a specific timeline ID |
| `--all-timelines` | — | — | Export all timelines (including forks) |
| `--include-content` | — | — | Include full request/response messages |
| `--dry-run` | — | — | Print spans to stdout instead of sending |

`<SESSION>` accepts a full session ID, a prefix, or `latest`.

## Web API

```
POST /api/sessions/{id}/export/otel
```

The OTLP destination is configured server-side (not per-request) via environment variables:

| Env Var | Description |
|:--------|:------------|
| `REWIND_OTEL_ENDPOINT` | OTLP endpoint URL (required) |
| `REWIND_OTEL_PROTOCOL` | `http` or `grpc` (default: `http`) |
| `REWIND_OTEL_HEADERS` | Comma-separated `KEY=VALUE` pairs |

Request body:

```json
{
  "include_content": false,
  "timeline_id": null,
  "all_timelines": false
}
```

Response:

```json
{
  "spans_exported": 3,
  "trace_id": "f79f6e866edbed7e..."
}
```

Returns `501` if `REWIND_OTEL_ENDPOINT` is not configured.

## Span Hierarchy

Exported traces follow this structure:

```
session {name}              [SpanKind: INTERNAL]
  timeline {label}          [SpanKind: INTERNAL]
    gen_ai.chat {model}     [SpanKind: CLIENT]     -- LLM calls
    tool.execute {name}     [SpanKind: INTERNAL]    -- tool invocations
    tool.result {name}      [SpanKind: INTERNAL]    -- tool responses
    user.prompt             [SpanKind: INTERNAL]    -- user messages
    hook.event {type}       [SpanKind: INTERNAL]    -- Claude Code hooks
```

For forked sessions, each timeline appears as a sibling under the session root when using `--all-timelines`.

## GenAI Semantic Conventions

LLM call spans use the [OpenTelemetry GenAI semantic conventions](https://opentelemetry.io/docs/specs/semconv/gen-ai/):

| Attribute | Source |
|:----------|:-------|
| `gen_ai.operation.name` | `"chat"` |
| `gen_ai.system` | Inferred from model name (`openai`, `anthropic`, `google`, etc.) |
| `gen_ai.request.model` | Model name from the step |
| `gen_ai.response.model` | From response payload |
| `gen_ai.usage.input_tokens` | Recorded token count |
| `gen_ai.usage.output_tokens` | Recorded token count |
| `gen_ai.request.temperature` | From request payload |
| `gen_ai.request.max_tokens` | From request payload |
| `gen_ai.response.finish_reasons` | From response payload |
| `gen_ai.response.id` | From response payload |

Content attributes (`gen_ai.input.messages`, `gen_ai.output.messages`) are only included with `--include-content`.

## Privacy

By default, **no message content is exported** — only metadata (model, tokens, timing, tool names). To include full request/response messages, explicitly pass `--include-content`. This is opt-in because LLM conversations may contain sensitive data.

## Supported Backends

Any backend that accepts OTLP traces over HTTP or gRPC:

- [Langfuse](https://langfuse.com) — `https://cloud.langfuse.com/api/public/otel`
- [Jaeger](https://www.jaegertracing.io) — `http://localhost:4318` (with OTLP receiver)
- [Grafana Tempo](https://grafana.com/oss/tempo/) — via Grafana Alloy or OTLP endpoint
- [Datadog](https://www.datadoghq.com) — via Datadog Agent OTLP ingestion
- [Honeycomb](https://www.honeycomb.io) — `https://api.honeycomb.io`
- Any [OpenTelemetry Collector](https://opentelemetry.io/docs/collector/)

## Deterministic Trace IDs

Re-exporting the same session always produces the same trace ID (`SHA-256(session_id)[0..16]`). This means:

- Duplicate exports overwrite rather than create new traces (in backends that support it)
- You can correlate CLI and Python exports of the same session
- Trace IDs are predictable for automation and CI workflows
