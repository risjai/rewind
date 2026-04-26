"""Shared cache-then-live decision logic and synthetic SSE primitives.

Used by all three transport adapters in ``rewind_agent.intercept``. The
adapters are thin shims over their library's transport API; the meaningful
behavior lives here so it stays consistent across libraries.

Step 0.2 surface:

- :func:`detect_streaming` — does the agent expect SSE chunks back?
- :func:`is_json_content` — is the request body safe to inspect for JSON
  fields like ``"stream": true``?
- :func:`synthetic_sse_for_cache_hit` — given an envelope's body bytes,
  emit a single-chunk SSE stream so the agent's ``async for chunk`` loop
  sees a well-formed stream on cache hit. v1 fidelity: one synthetic chunk
  + ``[DONE]`` sentinel, not real chunk-level replay.

The transport adapters are responsible for:

- Buffering streaming request bodies into ``RewindRequest.body`` bytes.
- Re-emitting an equivalent body iterator for the live (cache-miss) path
  so the upstream client doesn't see a consumed stream.
- Calling ``ExplicitClient.get_replayed_response`` with the buffered body
  and ``RewindRequest`` for content validation.
- On cache miss, calling the underlying transport, recording via
  ``ExplicitClient.record_llm_call``.

That orchestration logic ships with each transport adapter (Tier 1) — this
module owns only the reusable detection/emission primitives.
"""

from __future__ import annotations

import json
from typing import Iterator, AsyncIterator

from rewind_agent.intercept._request import RewindRequest


# Wire format constants. The SSE delimiter is "\n\n" between events;
# OpenAI/Anthropic/most providers terminate the stream with "data: [DONE]\n\n".
SSE_DONE_SENTINEL = b"data: [DONE]\n\n"

# Content types where we'll inspect the body for `"stream": true`. Outside
# this set, we don't parse — predicate callers explicitly opt into JSON
# semantics by setting the right content-type. The list is conservative:
# multipart, octet-stream, form-urlencoded, etc. are skipped.
_JSON_CONTENT_TYPES: frozenset[str] = frozenset(
    {
        "application/json",
        "application/vnd.api+json",
        "application/ld+json",
    }
)

# Body-sniff size cap. Even when content-type is JSON we don't want to
# pay quadratic deserialization cost for huge prompts; ``stream: true``
# would always appear within the first 8 KB on any reasonable client.
_BODY_SNIFF_LIMIT_BYTES = 8 * 1024


def is_json_content(req: RewindRequest) -> bool:
    """True if the request body should be parsed as JSON for stream-flag
    detection. Conservatively false for missing/unknown content-types so
    we don't accidentally JSON-parse a multipart upload.
    """
    ct = req.content_type()
    if ct is None:
        # No content-type → if there's a body, assume JSON (OpenAI clients
        # often omit the header). If no body, the question is moot.
        return bool(req.body)
    return ct in _JSON_CONTENT_TYPES


def detect_streaming(req: RewindRequest) -> bool:
    """Does the caller expect a streaming response?

    Two signals:

    1. **Explicit flag on the request** — ``RewindRequest.stream`` is True.
       Adapters set this from library-native signals (httpx ``stream=True``
       context manager, openai SDK's ``stream`` argument, etc.).
    2. **``Accept: text/event-stream`` header** — works for any HTTP
       client across all content types. Reliable signal regardless of
       request body shape.
    3. **``"stream": true`` in JSON request body** — only inspected when
       content-type is JSON-ish (see :func:`is_json_content`). Skipped
       for multipart / form bodies / unknown content types where parsing
       would be wrong or expensive.

    All three are OR-combined. Returns ``True`` if any signal fires.
    """
    if req.stream:
        return True

    accept = req.header("accept")
    if accept is not None:
        # Multiple accept values comma-separated, optional q= weights;
        # we only need to know SSE is among them.
        if "text/event-stream" in accept.lower():
            return True

    if not is_json_content(req):
        return False

    body = req.body
    if not body:
        return False
    # Cheap pre-filter: if the literal substring isn't there, no JSON
    # parse is needed. This is wrong for `"stream":true` with no space
    # vs `"stream" : true` with surrounding whitespace, but JSON serializers
    # virtually always emit one of two canonical forms (no-whitespace from
    # Python's `json.dumps`, comma+space from JS-style serializers). Try
    # both before paying for a parse.
    sniff = body[:_BODY_SNIFF_LIMIT_BYTES]
    if b'"stream"' not in sniff:
        return False

    # Cheap structural test: parse only if the substring is present. Catch
    # all parse errors — a truncated body during streaming, a non-JSON
    # body that smelled like one, etc. — and treat as "not streaming"
    # rather than raise.
    try:
        parsed = json.loads(sniff)
    except (json.JSONDecodeError, ValueError):
        return False
    return isinstance(parsed, dict) and parsed.get("stream") is True


def synthetic_sse_for_cache_hit(body: bytes) -> bytes:
    """Build a single-chunk SSE stream from a recorded response body.

    Used by the transport adapters when a streaming request hits the
    cache. The recorded body is the *final assembled* JSON response (e.g.
    the OpenAI Chat Completion object after the SSE stream ended) — see
    Step 0.3 contract in the proxy. We emit it as one ``data: <body>``
    event followed by ``data: [DONE]`` so an SSE consumer's ``async for
    chunk`` loop terminates cleanly.

    v1 fidelity:
    - Single chunk, not real chunk-level replay. SDKs that depend on
      *partial* chunk delivery for token-by-token UI behavior will see
      the whole response in one chunk on cache hit. Real chunk-level
      replay is a follow-up.
    - The body is emitted verbatim. If the recording captured a
      non-JSON body for some reason, the sniff will still construct a
      valid SSE event (every byte goes after ``data: `` and before
      ``\\n\\n``); a streaming JSON parser may raise downstream on a
      malformed body, which is the same failure mode as live.

    Returns the full SSE-formatted bytes ready to write to the response
    body. The transport adapters set ``Content-Type: text/event-stream``
    on the synthesized response.
    """
    # Per RFC, SSE event data lines are separated by single \n inside an
    # event, and events are separated by \n\n. JSON bodies frequently
    # contain newlines (pretty-printed) which would split into multiple
    # data lines — that's actually fine per the SSE spec (data: lines
    # are concatenated by clients), but for v1 we strip and inline so
    # consumers using lazy line-readers see one event.
    inline_body = body.replace(b"\r\n", b"\n").replace(b"\n", b"")
    return b"data: " + inline_body + b"\n\n" + SSE_DONE_SENTINEL


def iter_synthetic_sse_chunks(body: bytes) -> Iterator[bytes]:
    """Sync iterator yielding SSE chunks for a buffered cache-hit body.

    Yields one chunk for the body and one chunk for the ``[DONE]``
    sentinel. Splitting into two chunks (vs the single ``bytes`` returned
    by :func:`synthetic_sse_for_cache_hit`) lets transport adapters that
    insist on chunk granularity (e.g. requests' ``iter_content``) report
    realistic stream events.
    """
    inline_body = body.replace(b"\r\n", b"\n").replace(b"\n", b"")
    yield b"data: " + inline_body + b"\n\n"
    yield SSE_DONE_SENTINEL


async def aiter_synthetic_sse_chunks(body: bytes) -> AsyncIterator[bytes]:
    """Async iterator variant for httpx / aiohttp / asyncio-based clients."""
    inline_body = body.replace(b"\r\n", b"\n").replace(b"\n", b"")
    yield b"data: " + inline_body + b"\n\n"
    yield SSE_DONE_SENTINEL
