"""
OpenTelemetry trace exporter for Rewind sessions.

All OTel imports are inside function bodies so the core SDK remains
zero-dependency. Users install with: pip install rewind-agent[otel]
"""

from __future__ import annotations

import hashlib
import json
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
    """Convert ISO 8601 timestamp string to nanoseconds since epoch.

    Raises ValueError if the timestamp cannot be parsed.
    """
    from datetime import datetime, timezone

    if not iso_str:
        raise ValueError("Empty timestamp string")

    # Try fromisoformat first (handles +00:00, Z, and bare timestamps)
    try:
        dt = datetime.fromisoformat(iso_str)
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=timezone.utc)
        return int(dt.timestamp() * 1_000_000_000)
    except (ValueError, TypeError):
        pass

    # Fallback: manual strptime for edge cases
    for fmt in ("%Y-%m-%dT%H:%M:%S.%f", "%Y-%m-%dT%H:%M:%S"):
        try:
            dt = datetime.strptime(iso_str.rstrip("Z"), fmt).replace(tzinfo=timezone.utc)
            return int(dt.timestamp() * 1_000_000_000)
        except ValueError:
            continue

    raise ValueError(f"Could not parse timestamp: {iso_str!r}")


def _query_session(conn, session_id: str) -> dict | None:
    """Query session with all fields needed for export (including timestamps)."""
    if session_id == "latest":
        row = conn.execute(
            "SELECT id, name, status, total_steps, total_tokens, created_at, updated_at "
            "FROM sessions ORDER BY created_at DESC LIMIT 1"
        ).fetchone()
    else:
        row = conn.execute(
            "SELECT id, name, status, total_steps, total_tokens, created_at, updated_at "
            "FROM sessions WHERE id = ? OR id LIKE ?",
            (session_id, f"{session_id}%"),
        ).fetchone()
    if not row:
        return None
    return {
        "id": row[0], "name": row[1], "status": row[2],
        "total_steps": row[3], "total_tokens": row[4],
        "created_at": row[5], "updated_at": row[6],
    }


def _query_timelines(conn, session_id: str, timeline_id: str | None, all_tl: bool) -> list[dict]:
    """Query timelines with created_at for span timestamps."""
    if all_tl:
        rows = conn.execute(
            "SELECT id, session_id, parent_timeline_id, fork_at_step, label, created_at "
            "FROM timelines WHERE session_id = ? ORDER BY created_at",
            (session_id,),
        ).fetchall()
    elif timeline_id:
        rows = conn.execute(
            "SELECT id, session_id, parent_timeline_id, fork_at_step, label, created_at "
            "FROM timelines WHERE id = ?",
            (timeline_id,),
        ).fetchall()
    else:
        rows = conn.execute(
            "SELECT id, session_id, parent_timeline_id, fork_at_step, label, created_at "
            "FROM timelines WHERE session_id = ? AND parent_timeline_id IS NULL "
            "ORDER BY created_at LIMIT 1",
            (session_id,),
        ).fetchall()
    return [
        {"id": r[0], "session_id": r[1], "parent_timeline_id": r[2],
         "fork_at_step": r[3], "label": r[4], "created_at": r[5]}
        for r in rows
    ]


def _query_steps(conn, timeline_id: str) -> list[dict]:
    """Query steps with all fields needed for export (including created_at).

    Note: The Python store schema does not have tool_name or span_id columns
    in the steps table (the Rust schema does). Tool name is extracted from
    the request/response blob when needed.
    """
    rows = conn.execute(
        "SELECT id, timeline_id, session_id, step_number, step_type, status, "
        "created_at, duration_ms, tokens_in, tokens_out, model, "
        "request_blob, response_blob, error "
        "FROM steps WHERE timeline_id = ? ORDER BY step_number",
        (timeline_id,),
    ).fetchall()
    return [
        {"id": r[0], "timeline_id": r[1], "session_id": r[2],
         "step_number": r[3], "step_type": r[4], "status": r[5],
         "created_at": r[6], "duration_ms": r[7], "tokens_in": r[8],
         "tokens_out": r[9], "model": r[10], "request_blob": r[11],
         "response_blob": r[12], "error": r[13]}
        for r in rows
    ]


def export_session(
    session_id: str,
    endpoint: str = "http://localhost:4318",
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
        endpoint: OTLP HTTP endpoint URL (the SDK appends /v1/traces automatically).
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

    # 1. Open store and read session data using direct SQL (includes all columns)
    store = Store()
    sess = _query_session(store._conn, session_id)
    if not sess:
        raise ValueError(f"Session not found: {session_id}")

    timelines = _query_timelines(store._conn, sess["id"], timeline_id, all_timelines)
    if not timelines:
        raise ValueError(f"No timelines found for session: {sess['id']}")

    # Get steps per timeline + resolve blobs
    steps_by_timeline: dict[str, list[dict]] = {}
    blobs: dict[str, Any] = {}

    for tl in timelines:
        steps = _query_steps(store._conn, tl["id"])
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

    # Session root span — with recorded timestamps
    session_start_ns = _iso_to_ns(sess["created_at"])
    session_end_ns = _iso_to_ns(sess["updated_at"])

    session_span = tracer.start_span(
        name=f"session {sess.get('name', sess['id'][:8])}",
        kind=SpanKind.INTERNAL,
        context=root_ctx,
        start_time=session_start_ns,
        attributes={
            "rewind.session.id": sess["id"],
            "rewind.session.name": sess.get("name", ""),
            "rewind.session.total_steps": sess.get("total_steps", 0),
            "rewind.session.total_tokens": sess.get("total_tokens", 0),
        },
    )
    actual_session_span_id = session_span.get_span_context().span_id
    session_span.end(end_time=session_end_ns)
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
        tl_start_ns = _iso_to_ns(tl["created_at"])

        tl_attrs: dict[str, Any] = {
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
            start_time=tl_start_ns,
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
        max_end_ns = tl_start_ns

        for step in steps:
            stype = step.get("step_type", "llm_call")
            model = step.get("model", "")
            tool_name = step.get("tool_name") or "unknown"

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

            # Create span with recorded timestamps
            start_ns = _iso_to_ns(step["created_at"])
            end_ns = start_ns + step.get("duration_ms", 0) * 1_000_000
            max_end_ns = max(max_end_ns, end_ns)

            step_span = tracer.start_span(
                name=span_name,
                kind=span_kind,
                context=tl_ctx,
                attributes=attrs,
                start_time=start_ns,
            )
            step_span.end(end_time=end_ns)
            span_count += 1

        # Timeline end = max(step end times)
        tl_span.end(end_time=max_end_ns)

    # 5. Flush and shutdown
    provider.force_flush()
    provider.shutdown()

    return span_count
