"""
Langfuse trace importer for Rewind.

Fetches a trace from Langfuse by ID, converts observations to OTLP-compatible
JSON, and POSTs to Rewind's /v1/traces endpoint for ingestion.

Can be used:
  1. As a subprocess: `python3 -m rewind_agent.langfuse_import` (called by Rust CLI)
  2. In-process via `import_from_langfuse()` (called by Python SDK)

Uses urllib.request — zero external dependencies.
"""

from __future__ import annotations

import base64
import hashlib
import json
import logging
import os
import sys
from datetime import datetime, timezone
from typing import Optional

import urllib.request
import urllib.error

logger = logging.getLogger("rewind_agent")

DEFAULT_LANGFUSE_HOST = "https://cloud.langfuse.com"
DEFAULT_REWIND_ENDPOINT = "http://127.0.0.1:4800"


def import_from_langfuse(
    trace_id: str,
    public_key: Optional[str] = None,
    secret_key: Optional[str] = None,
    host: str = DEFAULT_LANGFUSE_HOST,
    session_name: Optional[str] = None,
    rewind_endpoint: str = DEFAULT_REWIND_ENDPOINT,
) -> str:
    """Fetch a Langfuse trace and import it into Rewind.

    Args:
        trace_id: The Langfuse trace ID.
        public_key: Langfuse public key (or LANGFUSE_PUBLIC_KEY env).
        secret_key: Langfuse secret key (or LANGFUSE_SECRET_KEY env).
        host: Langfuse host URL.
        session_name: Override the session name in Rewind.
        rewind_endpoint: Rewind server URL.

    Returns:
        The created Rewind session ID.
    """
    pk = public_key or os.environ.get("LANGFUSE_PUBLIC_KEY")
    sk = secret_key or os.environ.get("LANGFUSE_SECRET_KEY")
    if not pk or not sk:
        raise RuntimeError(
            "Langfuse API keys required. Set LANGFUSE_PUBLIC_KEY and "
            "LANGFUSE_SECRET_KEY, or pass public_key/secret_key."
        )

    trace_data = _fetch_trace(host, pk, sk, trace_id)
    otlp_json = _convert_to_otlp(trace_data)

    from .otel_import import import_otel
    return import_otel(
        json_data=otlp_json,
        session_name=session_name or trace_data.get("name") or f"langfuse-{trace_id[:8]}",
        endpoint=rewind_endpoint,
    )


def _fetch_trace(host: str, public_key: str, secret_key: str, trace_id: str) -> dict:
    """Fetch a trace with embedded observations from the Langfuse API."""
    url = f"{host.rstrip('/')}/api/public/traces/{trace_id}"

    credentials = base64.b64encode(f"{public_key}:{secret_key}".encode()).decode()
    headers = {
        "Authorization": f"Basic {credentials}",
        "Accept": "application/json",
    }

    req = urllib.request.Request(url, headers=headers)

    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            data = json.loads(resp.read())
    except urllib.error.HTTPError as e:
        if e.code == 404:
            raise ValueError(f"Trace not found in Langfuse: {trace_id}") from e
        if e.code == 401:
            raise RuntimeError("Langfuse authentication failed. Check your API keys.") from e
        raise RuntimeError(f"Langfuse API error ({e.code}): {e.read().decode()[:500]}") from e
    except urllib.error.URLError as e:
        raise ConnectionError(
            f"Failed to connect to Langfuse at {host}. Error: {e}"
        ) from e

    observations = data.get("observations")
    if not observations:
        observations = _fetch_observations_paginated(host, public_key, secret_key, trace_id)
        data["observations"] = observations

    return data


def _fetch_observations_paginated(
    host: str, public_key: str, secret_key: str, trace_id: str
) -> list:
    """Fallback: fetch observations via the paginated v1 endpoint."""
    credentials = base64.b64encode(f"{public_key}:{secret_key}".encode()).decode()
    all_obs = []
    page = 1

    while True:
        url = (
            f"{host.rstrip('/')}/api/public/observations"
            f"?traceId={trace_id}&page={page}&limit=500"
        )
        req = urllib.request.Request(url, headers={
            "Authorization": f"Basic {credentials}",
            "Accept": "application/json",
        })
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                result = json.loads(resp.read())
        except urllib.error.HTTPError as e:
            raise RuntimeError(
                f"Langfuse observations API error ({e.code}) for trace {trace_id}"
            ) from e
        except urllib.error.URLError as e:
            raise ConnectionError(
                f"Failed to fetch observations from Langfuse at {host}: {e}"
            ) from e

        all_obs.extend(result.get("data", []))
        meta = result.get("meta", {})
        if page >= meta.get("totalPages", 1):
            break
        page += 1

    return all_obs


def _convert_to_otlp(trace_data: dict) -> dict:
    """Convert a Langfuse trace+observations dict to an OTLP ExportTraceServiceRequest JSON."""
    observations = trace_data.get("observations", [])
    trace_id_hex = _stable_id(trace_data.get("id", "unknown"), 16)

    spans = []
    for obs in observations:
        span = _observation_to_span(obs, trace_id_hex)
        if span:
            spans.append(span)

    if not spans:
        raise ValueError("Langfuse trace has no observations to import.")

    return {
        "resourceSpans": [{
            "scopeSpans": [{
                "spans": spans,
            }],
        }],
    }


def _observation_to_span(obs: dict, trace_id_hex: str) -> dict | None:
    """Convert a single Langfuse observation to an OTLP span dict."""
    obs_id = obs.get("id", "")
    if not obs_id:
        return None

    span_id_hex = _stable_id(obs_id, 8)
    parent_obs_id = obs.get("parentObservationId")
    parent_span_id_hex = _stable_id(parent_obs_id, 8) if parent_obs_id else ""

    name = _infer_span_name(obs)

    start_ns = _iso_to_nanos(obs.get("startTime", ""))
    end_time = obs.get("endTime") or obs.get("startTime", "")
    end_ns = _iso_to_nanos(end_time)

    attributes = _build_attributes(obs)

    status = None
    level = obs.get("level", "")
    status_msg = obs.get("statusMessage", "")
    if level == "ERROR" or status_msg:
        status = {"code": 2, "message": status_msg or "error"}

    return {
        "traceId": trace_id_hex,
        "spanId": span_id_hex,
        "parentSpanId": parent_span_id_hex,
        "name": name,
        "kind": 1,  # INTERNAL
        "startTimeUnixNano": str(start_ns),
        "endTimeUnixNano": str(end_ns),
        "attributes": attributes,
        "status": status,
    }


def _infer_span_name(obs: dict) -> str:
    obs_type = obs.get("type", "SPAN")
    model = obs.get("model") or obs.get("providedModelName") or ""
    obs_name = obs.get("name") or ""

    if obs_type == "GENERATION":
        return f"gen_ai.chat {model}" if model else "gen_ai.chat unknown"
    if obs_type == "TOOL":
        return f"tool.execute {obs_name}" if obs_name else "tool.execute unknown"
    return obs_name or f"langfuse.{obs_type.lower()}"


def _build_attributes(obs: dict) -> list:
    attrs = []
    obs_type = obs.get("type", "SPAN")

    model = obs.get("model") or obs.get("providedModelName")
    if model:
        attrs.append(_str_attr("gen_ai.request.model", model))

    if obs_type == "GENERATION":
        attrs.append(_str_attr("gen_ai.operation.name", "chat"))

    usage = obs.get("usageDetails") or obs.get("usage") or {}
    input_tokens = usage.get("input") or usage.get("promptTokens") or 0
    output_tokens = usage.get("output") or usage.get("completionTokens") or 0
    if input_tokens:
        attrs.append(_int_attr("gen_ai.usage.input_tokens", int(input_tokens)))
    if output_tokens:
        attrs.append(_int_attr("gen_ai.usage.output_tokens", int(output_tokens)))

    obs_input = obs.get("input")
    if obs_input is not None:
        content = obs_input if isinstance(obs_input, str) else json.dumps(obs_input, default=str)
        attrs.append(_str_attr("gen_ai.input.messages", content))

    obs_output = obs.get("output")
    if obs_output is not None:
        content = obs_output if isinstance(obs_output, str) else json.dumps(obs_output, default=str)
        attrs.append(_str_attr("gen_ai.output.messages", content))

    tool_name = obs.get("name")
    if obs_type in ("TOOL",) and tool_name:
        attrs.append(_str_attr("gen_ai.tool.name", tool_name))

    return attrs


def _str_attr(key: str, value: str) -> dict:
    return {"key": key, "value": {"stringValue": value}}


def _int_attr(key: str, value: int) -> dict:
    return {"key": key, "value": {"intValue": str(value)}}


def _stable_id(input_str: str, byte_len: int) -> str:
    """Deterministic hex ID from a string, like Rewind's SHA-256 truncation."""
    h = hashlib.sha256(input_str.encode()).digest()
    return h[:byte_len].hex()


def _iso_to_nanos(iso_str: str) -> int:
    if not iso_str:
        return 0
    try:
        iso_str = iso_str.replace("Z", "+00:00")
        dt = datetime.fromisoformat(iso_str)
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=timezone.utc)
        epoch = datetime(1970, 1, 1, tzinfo=timezone.utc)
        delta = dt - epoch
        return int(delta.total_seconds() * 1_000_000_000)
    except (ValueError, TypeError):
        return 0


# ── Subprocess Entry Point ───────────────────────────────────────

def main():
    """Entry point for `python3 -m rewind_agent.langfuse_import`."""
    raw = sys.stdin.read()
    try:
        payload = json.loads(raw)
    except json.JSONDecodeError as e:
        result = {"error": f"Invalid stdin JSON: {e}"}
        print(json.dumps(result))
        sys.exit(1)

    try:
        session_id = import_from_langfuse(
            trace_id=payload["trace_id"],
            public_key=payload.get("public_key"),
            secret_key=payload.get("secret_key"),
            host=payload.get("host", DEFAULT_LANGFUSE_HOST),
            session_name=payload.get("session_name"),
            rewind_endpoint=payload.get("rewind_endpoint", DEFAULT_REWIND_ENDPOINT),
        )
        result = {"session_id": session_id}
    except Exception as e:
        result = {"error": str(e)}
        print(json.dumps(result))
        sys.exit(1)

    print(json.dumps(result))


if __name__ == "__main__":
    main()
