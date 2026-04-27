"""Library-agnostic cache-then-live decision flow.

Phase 1 of the Universal Replay Architecture. Every transport adapter
(:mod:`.httpx_transport`, :mod:`.requests_adapter`,
:mod:`.aiohttp_middleware`) reduces its library-native request shape
to a :class:`RewindRequest` and a small set of callbacks, then hands
control here. The orchestration is identical across libraries; only
the response-construction adapters differ.

## Per-call flow

::

    Adapter
      │
      ▼
    handle_intercepted_sync (or _async)
      │
      ├── predicates.is_llm_call?  ── False ─┐
      │                                       │
      ├── predicates.is_tool_call? ── False ─┤── live() ─→ return as-is
      │                                       │           (no recording)
      │   True (LLM):                         │
      │      • parse req body for cache key   │
      │      • ExplicitClient.get_replayed_response
      │           │                           │
      │           ├─ hit  → record_cache_hit savings
      │           │         + synth (buffered or SSE) ──→ return synthetic resp
      │           │
      │           └─ miss → live() ──→ parse tokens/model
      │                              ──→ ExplicitClient.record_llm_call
      │                              ──→ return live resp
      │
      │   True (tool):
      │      • live() → record_tool_call → return live resp

The two predicates fire in order; ``is_llm_call`` wins ties (LLM-call
routing supersedes tool-call routing — typical for gateways that
expose both behind the same prefix). Neither firing falls through
to the pass-through path.

## Why a callback architecture

Per-library response synthesis is the only thing that has to change
between adapters. Putting it behind two callbacks
(``synth_buffered`` / ``synth_streaming``) lets this module own all
the orchestration — predicate routing, cache lookup, recording,
savings accounting, error handling — while staying out of the typing
mess of cross-library response types.

The adapter passes:

- ``live`` — invoke the underlying transport, return the library-native
  response. Adapter handles its own retry/timeout/etc semantics here.
- ``synth_buffered(body, headers)`` — build a library-native response
  from cached bytes for non-streaming clients.
- ``synth_streaming(body, headers)`` — build a library-native streaming
  response from cached bytes; should emit a single SSE event followed
  by ``[DONE]`` (uses :func:`.synthetic_sse_for_cache_hit`).
- ``parse_response`` — JSON-decode the live response body for token
  extraction. Returns ``None`` if parse fails (we still record, just
  with token counts of 0).

## Recording on miss vs hit

- **Miss** records via ``ExplicitClient.record_llm_call`` (or
  ``record_tool_call``). The recording is best-effort — failures
  log a warning but never break the live response path.
- **Hit** records the cache-hit savings via :mod:`._savings` and
  returns a synthetic response. The Rust side already wrote a
  replayed step to the database when ``get_replayed_response``
  returned a hit, so we don't double-record on the Python side.

## Async correctness

The async path uses ``ExplicitClient`` async variants
(``get_replayed_response_async``, ``record_llm_call_async``) so we
never block the event loop. Sync recording is dispatched on a
ThreadPoolExecutor by the ExplicitClient's internals — safe to call
from sync transport adapters even when an event loop is running
elsewhere in the process.
"""

from __future__ import annotations

import json
import logging
import time
from typing import Any, Awaitable, Callable

from rewind_agent.explicit import ExplicitClient
from rewind_agent.intercept._core import detect_streaming
from rewind_agent.intercept._predicates import Predicates
from rewind_agent.intercept._request import RewindRequest
from rewind_agent.intercept._savings import record_cache_hit
from rewind_agent.intercept._tokens import extract_tokens_and_model

logger = logging.getLogger(__name__)


# Type aliases. Python's typing for generic library responses is
# clumsy; we use Any here and document the contract in docstrings.
Response = Any
SyntheticResponse = Any


# Module-level ExplicitClient. The intercept layer is process-wide;
# constructing a fresh client per call would multiply HTTP overhead
# (each call to the rewind server includes a session lookup). Lazily
# initialized so importing this module doesn't crash before
# ``rewind_agent`` is configured.
_explicit_client: ExplicitClient | None = None


def _get_client() -> ExplicitClient:
    """Lazy ExplicitClient singleton. Reused across all intercepted calls."""
    global _explicit_client
    if _explicit_client is None:
        _explicit_client = ExplicitClient()
    return _explicit_client


def reset_client() -> None:
    """Test hook — drop the cached singleton so the next call re-reads
    environment / config. Public via the test surface, not the user API.
    """
    global _explicit_client
    _explicit_client = None


def _decode_body(body: bytes) -> Any:
    """Best-effort JSON decode of a request body.

    Returns the parsed value or ``None`` if the body is empty / not
    JSON. The cache-key match is computed server-side over the
    canonical-hash of the request body, so a non-JSON body still hits
    the cache correctly — we only need a JSON value when ExplicitClient
    expects one (which it does, for the wire format).
    """
    if not body:
        return None
    try:
        return json.loads(body)
    except (json.JSONDecodeError, ValueError):
        return None


# ── Sync flow ──────────────────────────────────────────────────────


def handle_intercepted_sync(
    req: RewindRequest,
    *,
    predicates: Predicates,
    live: Callable[[], Response],
    synth_buffered: Callable[[bytes, dict[str, str]], SyntheticResponse],
    synth_streaming: Callable[[bytes, dict[str, str]], SyntheticResponse],
    is_streaming: bool,
) -> Response:
    """Drive a single intercepted request through cache lookup and recording.

    All five callbacks are sync. ``live()`` is the library's actual
    transport invocation; the adapter is responsible for ensuring it's
    safe to call from this context (e.g. an asyncio adapter would wrap
    its async client in a thread executor before calling here).

    Returns the library-native response object — either the live one
    or a ``synth_*`` synthetic. The caller treats both identically.

    Streaming detection (Santa #3): the adapter passes ``is_streaming``
    derived from transport-level signals (``stream=True`` kwarg, Accept:
    text/event-stream header). We OR that with the body-aware
    :func:`._core.detect_streaming` heuristic so a request with
    ``{"stream": true}`` in the JSON body but no Accept header still
    routes through the streaming path.
    """
    streaming = is_streaming or detect_streaming(req)

    if predicates.is_llm_call(req):
        return _handle_llm_sync(
            req,
            live=live,
            synth_buffered=synth_buffered,
            synth_streaming=synth_streaming,
            is_streaming=streaming,
        )

    if predicates.is_tool_call(req):
        return _handle_tool_sync(req, live=live)

    return live()


def _handle_llm_sync(
    req: RewindRequest,
    *,
    live: Callable[[], Response],
    synth_buffered: Callable[[bytes, dict[str, str]], SyntheticResponse],
    synth_streaming: Callable[[bytes, dict[str, str]], SyntheticResponse],
    is_streaming: bool,
) -> Response:
    request_value = _decode_body(req.body)
    client = _get_client()

    cached = client.get_replayed_response(request_value)
    if cached is not None:
        return _serve_cache_hit_sync(
            cached_response=cached,
            request_value=request_value,
            synth_buffered=synth_buffered,
            synth_streaming=synth_streaming,
            is_streaming=is_streaming,
        )

    return _serve_cache_miss_sync(
        client=client,
        request_value=request_value,
        live=live,
        is_streaming=is_streaming,
    )


def _serve_cache_hit_sync(
    *,
    cached_response: Any,
    request_value: Any,
    synth_buffered: Callable[[bytes, dict[str, str]], SyntheticResponse],
    synth_streaming: Callable[[bytes, dict[str, str]], SyntheticResponse],
    is_streaming: bool,
) -> SyntheticResponse:
    # The cached response from get_replayed_response is the inner body
    # (Phase 0 server-side already unwrapped the envelope before
    # returning). Re-encode to bytes for the adapter callbacks; the
    # synth functions will package it as either a buffered JSON
    # response or a synthetic SSE stream.
    body_bytes = json.dumps(cached_response).encode("utf-8")

    # Best-effort tokens-saved accounting. The cached response carries
    # the original usage block (if the recording included one), so we
    # extract from there for the most accurate count.
    tokens_in, tokens_out, model = extract_tokens_and_model(
        request_value, cached_response
    )
    record_cache_hit(model=model, tokens_in=tokens_in, tokens_out=tokens_out)

    headers = (
        {"content-type": "text/event-stream"}
        if is_streaming
        else {"content-type": "application/json"}
    )

    if is_streaming:
        return synth_streaming(body_bytes, headers)
    return synth_buffered(body_bytes, headers)


def _serve_cache_miss_sync(
    *,
    client: ExplicitClient,
    request_value: Any,
    live: Callable[[], Response],
    is_streaming: bool,
) -> Response:
    started = time.monotonic()
    resp = live()
    duration_ms = int((time.monotonic() - started) * 1000)

    # Phase 1 (Santa #2) — STREAMING MISS PASS-THROUGH.
    #
    # For streaming responses, do NOT pre-read the body. Pre-reading
    # via resp.json() / resp.text would consume the stream before user
    # code can iterate it (httpx raises ResponseNotRead; requests
    # marks _content_consumed=True; aiohttp closes the connection).
    # Phase 1 contract: live streams pass through immediately.
    #
    # Trade-off for v1: recording happens with placeholder
    # ``response_value=None`` and zero tokens, since we can't safely
    # capture the body without a tee wrapper around the response
    # stream. Tee-based streaming recording (matching the Rust
    # proxy's `handle_streaming_response`) is documented as a
    # follow-up — see the v1.1 limitation notes in the PR description.
    if is_streaming:
        try:
            client.record_llm_call(
                request=request_value,
                response=None,
                model=_model_from_request(request_value),
                duration_ms=duration_ms,
                tokens_in=0,
                tokens_out=0,
            )
        except Exception as exc:  # pragma: no cover — defensive
            logger.warning("rewind: record_llm_call (streaming miss) failed: %s", exc)
        return resp

    # Non-streaming path: it's safe to pre-read the body for token
    # extraction. Adapters expose a parsed-JSON body via several
    # candidate shapes; see _read_response_body_sync.
    response_value = _read_response_body_sync(resp)
    tokens_in, tokens_out, model = extract_tokens_and_model(
        request_value, response_value
    )

    try:
        client.record_llm_call(
            request=request_value,
            response=response_value,
            model=model,
            duration_ms=duration_ms,
            tokens_in=tokens_in,
            tokens_out=tokens_out,
        )
    except Exception as exc:  # pragma: no cover — defensive
        logger.warning("rewind: record_llm_call failed: %s", exc)

    return resp


def _model_from_request(request_value: Any) -> str:
    """Best-effort model extraction from the request when we can't
    read the response (streaming pass-through). Matches the fallback
    used in :func:`._tokens.extract_tokens_and_model`."""
    if isinstance(request_value, dict):
        model = request_value.get("model")
        if isinstance(model, str):
            return model
    return ""


def _handle_tool_sync(
    req: RewindRequest,
    *,
    live: Callable[[], Response],
) -> Response:
    request_value = _decode_body(req.body)
    client = _get_client()

    started = time.monotonic()
    resp = live()
    duration_ms = int((time.monotonic() - started) * 1000)
    response_value = _read_response_body_sync(resp)

    try:
        client.record_tool_call(
            tool_name=_tool_name_from_request(req),
            request=request_value,
            response=response_value,
            duration_ms=duration_ms,
        )
    except Exception as exc:  # pragma: no cover — defensive
        logger.warning("rewind: record_tool_call failed: %s", exc)

    return resp


def _read_response_body_sync(resp: Any) -> Any:
    """Read a response body as JSON, materializing the body first if needed.

    Phase 1 (Santa re-review #1): httpx responses returned from the
    transport layer have NOT yet had their body read — calling
    ``resp.json()`` directly raises ``httpx.ResponseNotRead``. We must
    call ``resp.read()`` first to materialize the body bytes. The
    test suite previously masked this bug because ``httpx.MockTransport``
    auto-reads the body via ``Response(json=...)`` construction, but
    real httpx transports (the ones users hit in production) leave
    the response unread.

    Adapter is expected to set ``_rewind_response_body`` on the
    response when it has one cheaply available; that fast path
    skips the read.

    Order of attempts:

    1. ``resp._rewind_response_body`` (intercept-set, fastest)
    2. ``resp.read()`` then ``resp.json()`` (httpx pattern)
    3. ``resp.json()`` directly (libraries that auto-buffer like
       requests)
    4. ``resp.text`` string fallback
    """
    cached = getattr(resp, "_rewind_response_body", None)
    if cached is not None:
        return cached

    # Materialize body first if the response supports it. httpx exposes
    # ``read()``; requests already has ``_content`` populated by the
    # time we see it; aiohttp (handled in async variant) needs ``read()``
    # too. Calling read() on an already-read response is idempotent in
    # all three.
    read = getattr(resp, "read", None)
    if callable(read):
        try:
            read()
        except Exception:
            # If read() fails (network already closed, partial read,
            # weird custom response), fall through to json()/text and
            # let those handle it. We never propagate the read failure
            # because record_llm_call is best-effort.
            pass

    json_method = getattr(resp, "json", None)
    if callable(json_method):
        try:
            return json_method()
        except Exception:
            # Catch broadly — httpx.ResponseNotRead, json.JSONDecodeError,
            # ValueError on truncated body, etc. Fall through.
            pass

    text = getattr(resp, "text", None)
    if isinstance(text, str):
        try:
            return json.loads(text)
        except (json.JSONDecodeError, ValueError):
            return None

    return None


# ── Async flow ─────────────────────────────────────────────────────


async def handle_intercepted_async(
    req: RewindRequest,
    *,
    predicates: Predicates,
    live: Callable[[], Awaitable[Response]],
    synth_buffered: Callable[[bytes, dict[str, str]], SyntheticResponse],
    synth_streaming: Callable[[bytes, dict[str, str]], SyntheticResponse],
    is_streaming: bool,
) -> Response:
    """Async counterpart to :func:`handle_intercepted_sync`.

    ``live`` is awaitable; ``synth_*`` callbacks remain sync because
    they're pure data construction. ``predicates`` are also sync (a
    custom predicate that needs async I/O is rare and should pre-fetch
    its data into a sync-readable cache). Streaming detection uses
    transport hints OR-combined with body-aware
    :func:`._core.detect_streaming` (Santa #3).
    """
    streaming = is_streaming or detect_streaming(req)

    if predicates.is_llm_call(req):
        return await _handle_llm_async(
            req,
            live=live,
            synth_buffered=synth_buffered,
            synth_streaming=synth_streaming,
            is_streaming=streaming,
        )

    if predicates.is_tool_call(req):
        return await _handle_tool_async(req, live=live)

    return await live()


async def _handle_llm_async(
    req: RewindRequest,
    *,
    live: Callable[[], Awaitable[Response]],
    synth_buffered: Callable[[bytes, dict[str, str]], SyntheticResponse],
    synth_streaming: Callable[[bytes, dict[str, str]], SyntheticResponse],
    is_streaming: bool,
) -> Response:
    request_value = _decode_body(req.body)
    client = _get_client()

    cached = await client.get_replayed_response_async(request_value)
    if cached is not None:
        # Cache hit branch is identical to sync — synth callbacks are
        # pure data construction, no I/O to await.
        return _serve_cache_hit_sync(
            cached_response=cached,
            request_value=request_value,
            synth_buffered=synth_buffered,
            synth_streaming=synth_streaming,
            is_streaming=is_streaming,
        )

    started = time.monotonic()
    resp = await live()
    duration_ms = int((time.monotonic() - started) * 1000)

    # Phase 1 (Santa #2): same streaming-pass-through contract as sync.
    # Pre-reading the body would consume the stream before user code
    # can iterate it. Streaming misses record with placeholder
    # response/tokens; tee-based recording deferred to v1.1.
    if is_streaming:
        try:
            await client.record_llm_call_async(
                request=request_value,
                response=None,
                model=_model_from_request(request_value),
                duration_ms=duration_ms,
                tokens_in=0,
                tokens_out=0,
            )
        except Exception as exc:  # pragma: no cover — defensive
            logger.warning(
                "rewind: record_llm_call_async (streaming miss) failed: %s", exc
            )
        return resp

    response_value = await _read_response_body_async(resp)
    tokens_in, tokens_out, model = extract_tokens_and_model(
        request_value, response_value
    )

    try:
        await client.record_llm_call_async(
            request=request_value,
            response=response_value,
            model=model,
            duration_ms=duration_ms,
            tokens_in=tokens_in,
            tokens_out=tokens_out,
        )
    except Exception as exc:  # pragma: no cover — defensive
        logger.warning("rewind: record_llm_call_async failed: %s", exc)

    return resp


async def _handle_tool_async(
    req: RewindRequest,
    *,
    live: Callable[[], Awaitable[Response]],
) -> Response:
    request_value = _decode_body(req.body)
    client = _get_client()

    started = time.monotonic()
    resp = await live()
    duration_ms = int((time.monotonic() - started) * 1000)
    response_value = await _read_response_body_async(resp)

    try:
        await client.record_tool_call_async(
            tool_name=_tool_name_from_request(req),
            request=request_value,
            response=response_value,
            duration_ms=duration_ms,
        )
    except Exception as exc:  # pragma: no cover — defensive
        logger.warning("rewind: record_tool_call_async failed: %s", exc)

    return resp


async def _read_response_body_async(resp: Any) -> Any:
    """Async counterpart of :func:`_read_response_body_sync`.

    Phase 1 (Santa re-review #1): same body-materialization fix as the
    sync variant. Real httpx async transport responses don't have their
    body pre-read; calling ``resp.json()`` raises ``httpx.ResponseNotRead``.
    We call ``await resp.aread()`` first (httpx) or ``await resp.read()``
    (aiohttp) before attempting to parse.
    """
    cached = getattr(resp, "_rewind_response_body", None)
    if cached is not None:
        return cached

    # httpx async: response has ``aread()``; calling it materializes the
    # body bytes into resp.content so the subsequent (sync) resp.json()
    # works. aiohttp ClientResponse: has ``read()`` (which is async!)
    # that returns the body bytes. The two libraries diverge on whether
    # the read method is async, so try both signatures.
    aread = getattr(resp, "aread", None)
    if callable(aread):
        try:
            await aread()
        except Exception:
            pass
    else:
        # aiohttp ClientResponse uses ``read()`` (async) as the
        # body-materialization API.
        read = getattr(resp, "read", None)
        if callable(read):
            try:
                result = read()
                if hasattr(result, "__await__"):
                    await result
            except Exception:
                pass

    # httpx async: response.json() is sync (body buffered above).
    # aiohttp: response.json() is async.
    json_method = getattr(resp, "json", None)
    if callable(json_method):
        try:
            result = json_method()
            if hasattr(result, "__await__"):
                return await result
            return result
        except Exception:
            # ResponseNotRead, JSONDecodeError, ValueError, etc.
            pass

    text = getattr(resp, "text", None)
    if callable(text):
        try:
            text_result = text()
            if hasattr(text_result, "__await__"):
                text_result = await text_result
            return json.loads(text_result) if isinstance(text_result, str) else None
        except (json.JSONDecodeError, ValueError, TypeError):
            return None
    if isinstance(text, str):
        try:
            return json.loads(text)
        except (json.JSONDecodeError, ValueError):
            return None

    return None


def _tool_name_from_request(req: RewindRequest) -> str:
    """Best-effort tool name extraction from the request URL path.

    Most internal tool gateways embed the tool name in the path
    (e.g. ``/tools/lookup_user``). If we can't extract one, fall
    back to the path itself so the recorded step is at least
    identifiable.
    """
    path = req.url_parts.path
    if not path:
        return "unknown_tool"
    # Last non-empty path segment, falling back to the full path.
    segments = [s for s in path.split("/") if s]
    return segments[-1] if segments else path
