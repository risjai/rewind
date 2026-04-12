"""
OpenTelemetry trace exporter for Rewind sessions.

All OTel imports are inside function bodies so the core SDK remains
zero-dependency. Users install with: pip install rewind-agent[otel]
"""

from __future__ import annotations

import hashlib
import json
import time
from typing import Any


def _check_otel() -> None:
    """Raise ImportError with install instructions if OTel is not available."""
    try:
        import opentelemetry.sdk  # noqa: F401
    except ImportError:
        raise ImportError(
            "OpenTelemetry export requires extra dependencies.\n"
            "Install with: pip install rewind-agent[otel]"
        ) from None


def _trace_id_from_session(session_id: str) -> int:
    """Deterministic 128-bit trace ID from session ID. Matches Rust: SHA-256[0..16]."""
    h = hashlib.sha256(session_id.encode()).digest()
    return int.from_bytes(h[:16], "big")


def _span_id_from_id(id_str: str) -> int:
    """Deterministic 64-bit span ID from an ID string. Matches Rust: SHA-256[0..8]."""
    h = hashlib.sha256(id_str.encode()).digest()
    return int.from_bytes(h[:8], "big")


def _infer_provider(model: str) -> str:
    """Infer LLM provider from model name. Matches Rust implementation."""
    m = model.lower()
    stripped = m.rsplit("/", 1)[-1]
    if any(stripped.startswith(p) for p in ("gpt-", "o1", "o3", "o4")) or "davinci" in stripped or "turbo" in stripped:
        return "openai"
    if stripped.startswith("claude"):
        return "anthropic"
    if stripped.startswith("gemini"):
        return "google"
    if stripped.startswith(("mistral", "mixtral")):
        return "mistral"
    if stripped.startswith(("llama", "meta-llama")):
        return "meta"
    # Fallback: check prefix
    if m.startswith("openai/"):
        return "openai"
    if m.startswith("anthropic/"):
        return "anthropic"
    if m.startswith(("google/", "models/gemini")):
        return "google"
    return "unknown"


def _iso_to_ns(iso_str: str) -> int:
    """Convert ISO 8601 timestamp string to nanoseconds since epoch."""
    from datetime import datetime, timezone
    # Handle both with and without fractional seconds
    for fmt in ("%Y-%m-%dT%H:%M:%S.%f", "%Y-%m-%dT%H:%M:%S"):
        try:
            dt = datetime.strptime(iso_str.rstrip("Z"), fmt).replace(tzinfo=timezone.utc)
            return int(dt.timestamp() * 1_000_000_000)
        except ValueError:
            continue
    # Fallback: current time
    return time.time_ns()


def export_session(
    session_id: str,
    endpoint: str = "http://localhost:4318/v1/traces",
    headers: dict[str, str] | None = None,
    include_content: bool = False,
    service_name: str = "rewind-agent",
    timeline_id: str | None = None,
    all_timelines: bool = False,
) -> int:
    """
    Export a recorded Rewind session as OTel traces via OTLP HTTP.

    Args:
        session_id: Session ID, prefix, or "latest".
        endpoint: OTLP HTTP endpoint URL.
        headers: Optional HTTP headers (e.g. auth tokens).
        include_content: Include full request/response message content.
        service_name: OTel service name.
        timeline_id: Export a specific timeline. None = main timeline.
        all_timelines: Export all timelines (overrides timeline_id).

    Returns:
        Number of OTel spans exported.

    Raises:
        ImportError: If opentelemetry packages are not installed.
        ValueError: If session not found.
    """
    _check_otel()

    # All OTel imports inside function body
    from opentelemetry import trace
    from opentelemetry.sdk.trace import TracerProvider
    from opentelemetry.sdk.trace.export import BatchSpanProcessor
    from opentelemetry.exporter.otlp.proto.http.trace_exporter import OTLPSpanExporter
    from opentelemetry.sdk.resources import Resource, SERVICE_NAME, SERVICE_VERSION
    from opentelemetry.trace import SpanKind, NonRecordingSpan, SpanContext, TraceFlags

    from .store import Store
    from . import __version__

    # 1. Open store and read session data
    store = Store()
    sess = store.get_session(session_id)
    if not sess:
        raise ValueError(f"Session not found: {session_id}")

    # Get timelines
    if all_timelines:
        rows = store._conn.execute(
            "SELECT id, session_id, parent_timeline_id, fork_at_step, label "
            "FROM timelines WHERE session_id = ? ORDER BY created_at",
            (sess["id"],),
        ).fetchall()
        timelines = [
            {"id": r[0], "session_id": r[1], "parent_timeline_id": r[2],
             "fork_at_step": r[3], "label": r[4]}
            for r in rows
        ]
    elif timeline_id:
        row = store._conn.execute(
            "SELECT id, session_id, parent_timeline_id, fork_at_step, label "
            "FROM timelines WHERE id = ?",
            (timeline_id,),
        ).fetchone()
        if not row:
            raise ValueError(f"Timeline not found: {timeline_id}")
        timelines = [{"id": row[0], "session_id": row[1], "parent_timeline_id": row[2],
                       "fork_at_step": row[3], "label": row[4]}]
    else:
        tl = store.get_root_timeline(sess["id"])
        if not tl:
            raise ValueError(f"Session has no timelines: {sess['id']}")
        timelines = [tl]

    # Get steps per timeline + resolve blobs
    steps_by_timeline: dict[str, list[dict]] = {}
    blobs: dict[str, Any] = {}

    for tl in timelines:
        steps = store.get_steps(tl["id"])
        steps_by_timeline[tl["id"]] = steps
        for step in steps:
            for blob_key in ("request_blob", "response_blob"):
                h = step.get(blob_key, "")
                if h and h not in blobs:
                    try:
                        raw = store.blobs.get(h)
                        blobs[h] = json.loads(raw)
                    except Exception:
                        pass

    # 2. Set up OTel provider
    resource = Resource.create({
        SERVICE_NAME: service_name,
        SERVICE_VERSION: __version__,
        "rewind.session.id": sess["id"],
    })

    exporter = OTLPSpanExporter(
        endpoint=endpoint,
        headers=headers or {},
    )

    provider = TracerProvider(resource=resource)
    provider.add_span_processor(BatchSpanProcessor(exporter))
    tracer = provider.get_tracer("rewind-otel", __version__)

    det_trace_id = _trace_id_from_session(sess["id"])
    span_count = 0

    # 3. Seed root context with deterministic trace ID
    root_ctx = trace.set_span_in_context(NonRecordingSpan(SpanContext(
        trace_id=det_trace_id,
        span_id=_span_id_from_id("root-seed"),
        is_remote=True,
        trace_flags=TraceFlags(TraceFlags.SAMPLED),
    )))

    # Session root span
    session_span = tracer.start_span(
        name=f"session {sess.get('name', sess['id'][:8])}",
        kind=SpanKind.INTERNAL,
        context=root_ctx,
        attributes={
            "rewind.session.id": sess["id"],
            "rewind.session.name": sess.get("name", ""),
            "rewind.session.total_steps": sess.get("total_steps", 0),
            "rewind.session.total_tokens": sess.get("total_tokens", 0),
        },
    )
    actual_session_span_id = session_span.get_span_context().span_id
    session_span.end()
    span_count += 1

    # Parent context for timelines
    session_ctx = trace.set_span_in_context(NonRecordingSpan(SpanContext(
        trace_id=det_trace_id,
        span_id=actual_session_span_id,
        is_remote=True,
        trace_flags=TraceFlags(TraceFlags.SAMPLED),
    )))

    # 4. Timeline + step spans
    for tl in timelines:
        tl_attrs = {
            "rewind.timeline.id": tl["id"],
            "rewind.timeline.label": tl.get("label", "main"),
        }
        if tl.get("parent_timeline_id"):
            tl_attrs["rewind.timeline.parent_id"] = tl["parent_timeline_id"]
        if tl.get("fork_at_step") is not None:
            tl_attrs["rewind.timeline.fork_at_step"] = tl["fork_at_step"]

        tl_span = tracer.start_span(
            name=f"timeline {tl.get('label', 'main')}",
            kind=SpanKind.INTERNAL,
            context=session_ctx,
            attributes=tl_attrs,
        )
        actual_tl_span_id = tl_span.get_span_context().span_id
        span_count += 1

        # Parent context for steps
        tl_ctx = trace.set_span_in_context(NonRecordingSpan(SpanContext(
            trace_id=det_trace_id,
            span_id=actual_tl_span_id,
            is_remote=True,
            trace_flags=TraceFlags(TraceFlags.SAMPLED),
        )))

        steps = steps_by_timeline.get(tl["id"], [])
        for step in steps:
            stype = step.get("step_type", "llm_call")
            model = step.get("model", "")
            tool_name = step.get("tool_name") or step.get("span_id") or "unknown"

            # Span name — matches Rust attribute mapping
            if stype == "llm_call":
                span_name = f"gen_ai.chat {model or 'unknown'}"
                span_kind = SpanKind.CLIENT
            elif stype == "tool_call":
                span_name = f"tool.execute {tool_name}"
                span_kind = SpanKind.INTERNAL
            elif stype == "tool_result":
                span_name = f"tool.result {tool_name}"
                span_kind = SpanKind.INTERNAL
            elif stype == "user_prompt":
                span_name = "user.prompt"
                span_kind = SpanKind.INTERNAL
            elif stype == "hook_event":
                span_name = f"hook.event {tool_name}"
                span_kind = SpanKind.INTERNAL
            else:
                span_name = stype
                span_kind = SpanKind.INTERNAL

            # Build attributes
            attrs: dict[str, Any] = {}
            req_blob = blobs.get(step.get("request_blob", ""))
            resp_blob = blobs.get(step.get("response_blob", ""))

            if stype == "llm_call":
                attrs["gen_ai.operation.name"] = "chat"
                attrs["gen_ai.system"] = _infer_provider(model)
                if model:
                    attrs["gen_ai.request.model"] = model
                if step.get("tokens_in", 0) > 0:
                    attrs["gen_ai.usage.input_tokens"] = step["tokens_in"]
                if step.get("tokens_out", 0) > 0:
                    attrs["gen_ai.usage.output_tokens"] = step["tokens_out"]
                # From request blob
                if req_blob:
                    if "temperature" in req_blob:
                        attrs["gen_ai.request.temperature"] = req_blob["temperature"]
                    if "max_tokens" in req_blob:
                        attrs["gen_ai.request.max_tokens"] = req_blob["max_tokens"]
                    if include_content and "messages" in req_blob:
                        attrs["gen_ai.input.messages"] = json.dumps(req_blob["messages"])
                # From response blob
                if resp_blob:
                    if "model" in resp_blob:
                        attrs["gen_ai.response.model"] = resp_blob["model"]
                    if "id" in resp_blob:
                        attrs["gen_ai.response.id"] = resp_blob["id"]
                    # Finish reasons — OpenAI vs Anthropic
                    if "choices" in resp_blob:
                        reasons = [c.get("finish_reason", "") for c in resp_blob["choices"] if c.get("finish_reason")]
                        if reasons:
                            attrs["gen_ai.response.finish_reasons"] = f"[{','.join(reasons)}]"
                    elif "stop_reason" in resp_blob:
                        attrs["gen_ai.response.finish_reasons"] = f"[{resp_blob['stop_reason']}]"
                    if include_content:
                        if "choices" in resp_blob:
                            attrs["gen_ai.output.messages"] = json.dumps(resp_blob["choices"])
                        elif "content" in resp_blob:
                            attrs["gen_ai.output.messages"] = json.dumps(resp_blob["content"])

            elif stype in ("tool_call", "tool_result"):
                if step.get("tool_name"):
                    attrs["gen_ai.tool.name"] = step["tool_name"]
                attrs["gen_ai.tool.type"] = "function"

            elif stype == "hook_event":
                attrs["rewind.hook.type"] = stype
                if step.get("tool_name"):
                    attrs["rewind.hook.event_type"] = step["tool_name"]

            else:
                attrs["rewind.step.type"] = stype

            attrs["rewind.duration_ms"] = step.get("duration_ms", 0)
            if step.get("error"):
                attrs["error.type"] = step["error"]

            # Cached step marker
            if (tl.get("fork_at_step") is not None
                    and tl.get("parent_timeline_id")
                    and step.get("step_number", 0) <= tl["fork_at_step"]):
                attrs["rewind.replay.cached"] = True

            # Create span with explicit timestamps
            start_ns = _iso_to_ns(step.get("created_at", ""))
            end_ns = start_ns + step.get("duration_ms", 0) * 1_000_000

            step_span = tracer.start_span(
                name=span_name,
                kind=span_kind,
                context=tl_ctx,
                attributes=attrs,
                start_time=start_ns,
            )
            step_span.end(end_time=end_ns)
            span_count += 1

        tl_span.end()

    # 5. Flush and shutdown
    provider.force_flush()
    provider.shutdown()

    return span_count
