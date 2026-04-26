"""Tests for ``rewind_agent.intercept._core`` and ``._request`` (Step 0.2).

The transport adapters (httpx/requests/aiohttp) live in Tier 1 and have
their own integration tests; this file covers only the pure helpers that
adapters compose with.
"""

from __future__ import annotations

import asyncio
import json

from rewind_agent.intercept import (
    RewindRequest,
    detect_streaming,
    is_json_content,
    synthetic_sse_for_cache_hit,
    SSE_DONE_SENTINEL,
)
from rewind_agent.intercept._core import (
    iter_synthetic_sse_chunks,
    aiter_synthetic_sse_chunks,
)


# ── RewindRequest accessors ────────────────────────────────────────────


def test_rewind_request_url_parts_lazy() -> None:
    req = RewindRequest(
        url="https://api.openai.com:9443/v1/chat/completions?token=abc",
        method="POST",
    )
    parts = req.url_parts
    assert parts.scheme == "https"
    assert parts.hostname == "api.openai.com"
    assert parts.port == 9443
    assert parts.path == "/v1/chat/completions"
    assert parts.query == "token=abc"


def test_rewind_request_header_lookup_case_insensitive() -> None:
    req = RewindRequest(
        url="https://x.example/y",
        method="POST",
        headers={"content-type": "application/json"},
    )
    assert req.header("Content-Type") == "application/json"
    assert req.header("CONTENT-TYPE") == "application/json"
    assert req.header("missing") is None


def test_rewind_request_content_type_strips_params() -> None:
    req = RewindRequest(
        url="x",
        method="POST",
        headers={"content-type": "application/json; charset=utf-8"},
    )
    assert req.content_type() == "application/json"


def test_rewind_request_content_type_none_when_missing() -> None:
    req = RewindRequest(url="x", method="GET")
    assert req.content_type() is None


# ── is_json_content ────────────────────────────────────────────────────


def test_is_json_content_recognizes_application_json() -> None:
    req = RewindRequest(
        url="x", method="POST",
        headers={"content-type": "application/json"},
        body=b'{"messages": []}',
    )
    assert is_json_content(req) is True


def test_is_json_content_recognizes_jsonapi_variant() -> None:
    req = RewindRequest(
        url="x", method="POST",
        headers={"content-type": "application/vnd.api+json"},
        body=b'{}',
    )
    assert is_json_content(req) is True


def test_is_json_content_rejects_multipart() -> None:
    req = RewindRequest(
        url="x", method="POST",
        headers={"content-type": "multipart/form-data; boundary=abc"},
        body=b"<multipart bytes>",
    )
    assert is_json_content(req) is False


def test_is_json_content_rejects_form_urlencoded() -> None:
    req = RewindRequest(
        url="x", method="POST",
        headers={"content-type": "application/x-www-form-urlencoded"},
        body=b"a=1&b=2",
    )
    assert is_json_content(req) is False


def test_is_json_content_rejects_octet_stream() -> None:
    req = RewindRequest(
        url="x", method="POST",
        headers={"content-type": "application/octet-stream"},
        body=b"\x00\x01\x02",
    )
    assert is_json_content(req) is False


def test_is_json_content_assumes_json_when_header_missing_with_body() -> None:
    # OpenAI clients sometimes omit content-type. If a body is present we
    # speculatively call it JSON so the stream-flag detector can run.
    req = RewindRequest(url="x", method="POST", body=b'{"stream": true}')
    assert is_json_content(req) is True


def test_is_json_content_false_when_no_body_no_header() -> None:
    req = RewindRequest(url="x", method="GET")
    assert is_json_content(req) is False


# ── detect_streaming: explicit flag ────────────────────────────────────


def test_detect_streaming_explicit_flag() -> None:
    req = RewindRequest(url="x", method="POST", stream=True)
    assert detect_streaming(req) is True


# ── detect_streaming: Accept header ────────────────────────────────────


def test_detect_streaming_accept_sse_header() -> None:
    req = RewindRequest(
        url="x", method="POST",
        headers={"accept": "text/event-stream"},
    )
    assert detect_streaming(req) is True


def test_detect_streaming_accept_sse_among_multiple() -> None:
    # Real Accept headers often list multiple types with q= weights.
    req = RewindRequest(
        url="x", method="POST",
        headers={"accept": "application/json, text/event-stream;q=0.9, */*;q=0.5"},
    )
    assert detect_streaming(req) is True


def test_detect_streaming_accept_case_insensitive() -> None:
    req = RewindRequest(
        url="x", method="POST",
        headers={"accept": "TEXT/EVENT-STREAM"},
    )
    assert detect_streaming(req) is True


def test_detect_streaming_accept_json_only() -> None:
    req = RewindRequest(
        url="x", method="POST",
        headers={"accept": "application/json"},
    )
    assert detect_streaming(req) is False


# ── detect_streaming: stream:true in JSON body ─────────────────────────


def test_detect_streaming_json_body_stream_true() -> None:
    req = RewindRequest(
        url="x", method="POST",
        headers={"content-type": "application/json"},
        body=b'{"model": "gpt-4o", "stream": true, "messages": []}',
    )
    assert detect_streaming(req) is True


def test_detect_streaming_json_body_stream_false() -> None:
    req = RewindRequest(
        url="x", method="POST",
        headers={"content-type": "application/json"},
        body=b'{"model": "gpt-4o", "stream": false, "messages": []}',
    )
    assert detect_streaming(req) is False


def test_detect_streaming_json_body_stream_absent() -> None:
    req = RewindRequest(
        url="x", method="POST",
        headers={"content-type": "application/json"},
        body=b'{"model": "gpt-4o", "messages": []}',
    )
    assert detect_streaming(req) is False


def test_detect_streaming_skips_body_parse_for_multipart() -> None:
    """Even if a multipart body coincidentally contains the literal
    substring "stream":true (e.g. inside a form field), we don't parse
    multipart as JSON — content-type guard prevents it. Negative test
    for review N4 in the plan."""
    req = RewindRequest(
        url="x", method="POST",
        headers={"content-type": "multipart/form-data; boundary=abc"},
        body=b'--abc\nContent-Disposition: form-data; name="stream"\n\ntrue\n--abc--',
    )
    # The exact substring '"stream"' might not appear (form serializer
    # strips quotes) but even if it did, multipart skips body sniffing.
    assert detect_streaming(req) is False


def test_detect_streaming_handles_truncated_json_body() -> None:
    """A streaming upload whose body got partially buffered shouldn't
    panic detect_streaming. The malformed JSON parse should fall back
    to "not streaming"."""
    req = RewindRequest(
        url="x", method="POST",
        headers={"content-type": "application/json"},
        body=b'{"stream": true, "messages":',  # truncated
    )
    # No panic; falls back to false.
    assert detect_streaming(req) is False


def test_detect_streaming_substring_pre_filter() -> None:
    """The pre-filter for the literal `"stream"` substring saves a JSON
    parse on the common case where the field is absent. Cover both
    branches."""
    # Field present → parse runs.
    has_field = RewindRequest(
        url="x", method="POST",
        headers={"content-type": "application/json"},
        body=b'{"stream": true}',
    )
    assert detect_streaming(has_field) is True
    # Field absent → no parse needed.
    no_field = RewindRequest(
        url="x", method="POST",
        headers={"content-type": "application/json"},
        body=b'{"model": "gpt-4o"}',
    )
    assert detect_streaming(no_field) is False


def test_detect_streaming_respects_sniff_limit() -> None:
    """`stream: true` past the sniff limit is not detected. This is by
    design — real LLM clients put control fields at the top of the JSON,
    not buried inside megabyte-long context strings."""
    huge_prefix = b'{"messages": [{"content": "' + (b"x" * 10_000) + b'"}],'
    req = RewindRequest(
        url="x", method="POST",
        headers={"content-type": "application/json"},
        body=huge_prefix + b' "stream": true}',
    )
    # The "stream" literal is past the sniff limit so we don't see it.
    # Conservative miss is the right call here.
    assert detect_streaming(req) is False


# ── synthetic_sse_for_cache_hit ────────────────────────────────────────


def test_synthetic_sse_emits_data_then_done() -> None:
    body = b'{"choices":[{"message":{"content":"hello"}}]}'
    sse = synthetic_sse_for_cache_hit(body)
    assert sse == b"data: " + body + b"\n\n" + SSE_DONE_SENTINEL


def test_synthetic_sse_inlines_newlines() -> None:
    """Pretty-printed JSON bodies with embedded newlines must collapse
    to a single SSE data line so naive line-readers see one event."""
    pretty_body = b'{\n  "choices": [\n    {"message": {"content": "hi"}}\n  ]\n}'
    sse = synthetic_sse_for_cache_hit(pretty_body)
    # No \n inside the data: line itself.
    data_line = sse.split(b"\n\n", 1)[0]
    assert b"\n" not in data_line[len(b"data: "):]
    # CRLF normalized to LF too — Windows-recorded bodies must work.
    crlf_body = b'{\r\n"a": 1\r\n}'
    crlf_sse = synthetic_sse_for_cache_hit(crlf_body)
    assert b"\r\n" not in crlf_sse[len(b"data: "):crlf_sse.index(b"\n\n")]


def test_synthetic_sse_round_trip_recoverable_json() -> None:
    """An SSE consumer should be able to extract the original JSON from
    the synthesized chunk by stripping the `data: ` prefix and parsing.
    """
    payload = {"choices": [{"message": {"content": "hello"}}]}
    body = json.dumps(payload).encode("utf-8")
    sse = synthetic_sse_for_cache_hit(body)
    # Pull out the first event's data line.
    first_event, _ = sse.split(b"\n\n", 1)
    assert first_event.startswith(b"data: ")
    payload_back = json.loads(first_event[len(b"data: "):])
    assert payload_back == payload


def test_synthetic_sse_done_sentinel_terminates() -> None:
    sse = synthetic_sse_for_cache_hit(b"{}")
    assert sse.endswith(SSE_DONE_SENTINEL)


def test_synthetic_sse_empty_body() -> None:
    """Edge case — empty body. SSE consumers should still see a clean
    [DONE] terminator."""
    sse = synthetic_sse_for_cache_hit(b"")
    assert sse == b"data: \n\n" + SSE_DONE_SENTINEL


# ── iter_synthetic_sse_chunks (sync iterator) ──────────────────────────


def test_iter_synthetic_sse_yields_two_chunks() -> None:
    chunks = list(iter_synthetic_sse_chunks(b"{}"))
    assert len(chunks) == 2
    assert chunks[0] == b"data: {}\n\n"
    assert chunks[1] == SSE_DONE_SENTINEL


# ── aiter_synthetic_sse_chunks (async iterator) ────────────────────────
#
# Using asyncio.run() directly rather than pytest-asyncio so the SDK's
# test suite stays dependency-free for non-async test files.


def test_aiter_synthetic_sse_yields_two_chunks() -> None:
    async def collect() -> list[bytes]:
        out: list[bytes] = []
        async for chunk in aiter_synthetic_sse_chunks(b'{"a": 1}'):
            out.append(chunk)
        return out

    chunks = asyncio.run(collect())
    assert len(chunks) == 2
    assert chunks[0] == b'data: {"a": 1}\n\n'
    assert chunks[1] == SSE_DONE_SENTINEL
