# Langfuse Import

Import traces from [Langfuse](https://langfuse.com) into Rewind for time-travel debugging. See a broken trace in Langfuse, import it, fork at the failure, replay with the fix, and prove the fix works with LLM-as-judge scoring.

> **See also:** [OTel Import](otel-import.md) for importing from any OTLP source, [OTel Export](otel-export.md) for exporting Rewind sessions back to Langfuse.

## Quick Start

### CLI

```bash
# Set Langfuse credentials (or pass as flags)
export LANGFUSE_PUBLIC_KEY=pk-lf-...
export LANGFUSE_SECRET_KEY=sk-lf-...

# Import a trace by ID
rewind import from-langfuse --trace <trace-id>

# Import with a custom session name
rewind import from-langfuse --trace <trace-id> --name "debug-issue-42"

# Self-hosted Langfuse
rewind import from-langfuse --trace <trace-id> --host https://langfuse.internal.company.com
```

### Python SDK

```python
import rewind_agent

session_id = rewind_agent.import_from_langfuse(
    trace_id="abc123",
    public_key="pk-lf-...",       # or LANGFUSE_PUBLIC_KEY env
    secret_key="sk-lf-...",       # or LANGFUSE_SECRET_KEY env
    host="https://cloud.langfuse.com",  # default
    session_name="debug-issue-42",      # optional
)

# Now debug it
# rewind show <session_id>
# rewind replay <session_id> --from 5
```

## How It Works

1. **Fetch** — Calls Langfuse REST API (`GET /api/public/traces/{traceId}`) with embedded observations
2. **Convert** — Maps Langfuse observations to OTLP spans with `gen_ai.*` semantic conventions
3. **Ingest** — POSTs the OTLP JSON to Rewind's `/v1/traces` endpoint (reuses the existing OTel ingestion pipeline)
4. **Debug** — The imported session is browsable, forkable, and replayable (if content was included)

## Langfuse Field Mapping

| Langfuse Field | Rewind Step Field | Notes |
|:---|:---|:---|
| `observation.type == "GENERATION"` | `StepType::LlmCall` | LLM call with model, tokens |
| `observation.type == "SPAN"` | `StepType::HookEvent` | Generic span |
| `observation.type == "TOOL"` | `StepType::ToolCall` | Tool invocation |
| `observation.model` | `step.model` | e.g., `gpt-4o` |
| `observation.usageDetails.input` | `step.tokens_in` | Input token count |
| `observation.usageDetails.output` | `step.tokens_out` | Output token count |
| `observation.input` | `step.request_blob` | Stored in blob store |
| `observation.output` | `step.response_blob` | Stored in blob store |
| `observation.startTime` | `step.created_at` | ISO 8601 → nanoseconds |
| `observation.endTime` | `step.duration_ms` | Computed from start/end delta |
| `observation.statusMessage` | `step.error` | On error observations |
| `observation.parentObservationId` | span parent-child tree | Hierarchical nesting |
| `trace.name` | `session.name` | Overridable with `--name` |

## Replay Support

If Langfuse observations include `input` and `output` fields (which they do for most GENERATION observations), the imported session is **fully replayable**:

```bash
# Import
rewind import from-langfuse --trace abc123

# Fork at the failure point
rewind replay latest --from 12
# Steps 1-11: served from imported content (0 tokens, 0ms)
# Step 12+: live LLM calls with your fix

# Prove the fix works
rewind eval score latest -e correctness --compare-timelines
```

## CLI Reference

```
rewind import from-langfuse [OPTIONS]
```

| Flag | Env Var | Description |
|:-----|:--------|:------------|
| `--trace <ID>` | — | Langfuse trace ID (required) |
| `--public-key <KEY>` | `LANGFUSE_PUBLIC_KEY` | Langfuse public key |
| `--secret-key <KEY>` | `LANGFUSE_SECRET_KEY` | Langfuse secret key |
| `--host <URL>` | `LANGFUSE_HOST` | Langfuse host (default: `https://cloud.langfuse.com`) |
| `--name <NAME>` | — | Override session name |

## Authentication

Uses HTTP Basic Auth with your Langfuse API keys. Works with both Langfuse Cloud and self-hosted instances.

The `langfuse` Python package is **not** required — Rewind calls the REST API directly using `urllib.request`.

## Related

- [OTel Import](otel-import.md) — Import from any OTLP source
- [OTel Export](otel-export.md) — Export Rewind sessions to Langfuse/Datadog/Grafana
- [Replay and Forking](replay-and-forking.md) — Fork, replay, and diff workflows
- [Evaluation](evaluation.md) — LLM-as-judge scoring for fix verification
