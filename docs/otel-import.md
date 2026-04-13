# OpenTelemetry Import

Import OTLP traces from external systems into Rewind for time-travel debugging. The reverse of [OTel export](otel-export.md).

> **Langfuse users:** See [Langfuse Import](langfuse-import.md) for a one-command import that fetches traces directly from Langfuse by trace ID.

## Quick Start

### CLI

```bash
# Import from a protobuf file
rewind import otel --file trace.pb

# Import from a JSON file (OTLP JSON format)
rewind import otel --json-file trace.json

# Import with a custom session name
rewind import otel --file trace.pb --name "debug-issue-42"
```

### HTTP API

```bash
# POST protobuf to the OTLP standard endpoint
curl -X POST http://localhost:4800/v1/traces \
  -H "Content-Type: application/x-protobuf" \
  --data-binary @trace.pb

# POST with gzip compression
curl -X POST http://localhost:4800/v1/traces \
  -H "Content-Type: application/x-protobuf" \
  -H "Content-Encoding: gzip" \
  --data-binary @trace.pb.gz
```

Both `/v1/traces` (OTLP standard) and `/api/import/otel` (Rewind convention) accept the same request.

### Python SDK

```python
import rewind_agent

# Import from a file (auto-detects protobuf vs JSON by extension)
session_id = rewind_agent.import_otel(file_path="trace.pb")
session_id = rewind_agent.import_otel(file_path="trace.json")

# Import with a custom session name
session_id = rewind_agent.import_otel(
    file_path="trace.pb",
    session_name="debug-issue-42",
)
```

## CLI Reference

```
rewind import otel [OPTIONS]
```

| Flag | Description |
|:-----|:------------|
| `--file <PATH>` | Import from a protobuf file (`ExportTraceServiceRequest`) |
| `--json-file <PATH>` | Import from a JSON file (OTLP JSON format) |
| `--name <NAME>` | Override the session name |

One of `--file` or `--json-file` is required.

## How It Works

1. **Decode** — Protobuf via `prost::Message::decode()`, JSON via `serde_json`
2. **Build span tree** — Group spans by `parent_span_id` relationships
3. **Identify structure** — Root span becomes the Session, timeline-level spans become Timelines, leaf spans become Steps
4. **Map attributes** — `gen_ai.*` semantic conventions map to Step fields (model, tokens, tool_name)
5. **Store content** — If `gen_ai.input.messages` / `gen_ai.output.messages` are present, they're stored as blobs (enabling replay)
6. **Assign step numbers** — Chronological order by `start_time_unix_nano`, with `span_id` tiebreaks

## Replay Support

Imported sessions can be forked and replayed — just like natively recorded sessions — **if content blobs are included**.

| Content | Replay | Behavior |
|:--------|:-------|:---------|
| `gen_ai.input/output.messages` present | Replayable | Fork at any step, cached steps served from blobs |
| No content attributes | Inspect only | Trace viewable but not forkable |
| Mixed (some steps have content) | Partial replay | Steps with blobs are cached, others go live |

After import, the CLI shows whether the session is replayable:

```
✓ Imported 12 spans → 5 steps (session: a1b2c3d4)
   🔁 Content blobs stored — session is replayable
   → View with: rewind show a1b2c3d4
```

## Langfuse Integration

Langfuse ingests OTel traces — it does not export them as OTLP. The practical integration paths:

### Path A: Dual-ship (recommended)

Configure your agent to send traces to both Langfuse and Rewind simultaneously:

```bash
# Agent sends to both backends:
# 1. Langfuse: https://cloud.langfuse.com/api/public/otel/v1/traces
# 2. Rewind:   http://localhost:4800/v1/traces
```

Use Langfuse for production dashboards. Use Rewind when something breaks and you need to fork/replay.

### Path B: Record locally, export to Langfuse

```bash
# Record locally during development
rewind_agent.init()
# ... agent runs ...

# Export to Langfuse for the team
rewind export otel latest \
  --endpoint https://cloud.langfuse.com/api/public/otel \
  --header "Authorization=Bearer pk-lf-..." \
  --include-content
```

## Round-Trip

Export a Rewind session and re-import it:

```bash
# Export to JSON
rewind export otel latest --include-content --dry-run > trace.json

# Import back
rewind import otel --json-file trace.json --name "reimported"

# Verify
rewind show latest
```

The re-imported session preserves step types, models, tokens, and content blobs.

## Span Mapping

Incoming OTel spans are mapped to Rewind step types:

| Span Pattern | Step Type |
|:-------------|:----------|
| Name starts with `gen_ai.chat` | `LlmCall` |
| Name starts with `tool.execute` | `ToolCall` |
| Name starts with `tool.result` | `ToolResult` |
| Name is `user.prompt` | `UserPrompt` |
| Has `gen_ai.request.model` attribute | `LlmCall` |
| Has `gen_ai.tool.name` attribute | `ToolCall` |
| Everything else | `HookEvent` |

## Flat Traces

Not all exporters produce a hierarchical span tree. When multiple spans have empty `parent_span_id` (flat trace), Rewind synthesizes a virtual session span using the earliest/latest timestamps and creates a single "main" timeline containing all spans as steps.
