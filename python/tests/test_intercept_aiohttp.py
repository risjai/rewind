"""Tests for ``rewind_agent.intercept.aiohttp_middleware``.

Exercises cache-then-live routing through real ``aiohttp.ClientSession``
instances. The "upstream" HTTP path is faked by patching the ORIGINAL
``ClientSession._request`` (the one our patch saves as
``_ORIGINAL_REQUEST``); on cache hit, our patched ``_request`` returns
a synthetic response WITHOUT touching the original at all.

The test fixture mirrors test_intercept_httpx.py's _FakeRewindServer
pattern: patches ExplicitClient methods to control cache hit / miss
and capture record_llm_call invocations.
"""

from __future__ import annotations

import asyncio
import json
import unittest
from typing import Any
from unittest.mock import patch

import aiohttp

from rewind_agent.intercept import _flow, _savings
from rewind_agent.intercept.aiohttp_middleware import (
    _SyntheticClientResponse,
    is_patched,
    patch_aiohttp_sessions,
    unpatch_aiohttp_sessions,
)


# ── Fake Rewind server fixture (async-aware) ───────────────────────


class _FakeRewindServer:
    def __init__(self) -> None:
        self.cache_response: dict[str, Any] | None = None
        self.recorded_calls: list[tuple[Any, ...]] = []

    def __enter__(self) -> "_FakeRewindServer":
        _flow.reset_client()
        _savings.reset()
        from rewind_agent.explicit import ExplicitClient

        self._patches = [
            patch.object(
                ExplicitClient,
                "get_replayed_response_async",
                side_effect=self._async_get,
            ),
            patch.object(
                ExplicitClient,
                "record_llm_call_async",
                side_effect=self._record_async,
            ),
            # Sync versions stubbed too in case anything reaches them.
            patch.object(
                ExplicitClient,
                "get_replayed_response",
                side_effect=lambda req=None: self.cache_response,
            ),
            patch.object(
                ExplicitClient,
                "record_llm_call",
                side_effect=lambda *a, **kw: 1,
            ),
        ]
        for p in self._patches:
            p.start()
        return self

    def __exit__(self, *exc: Any) -> None:
        for p in self._patches:
            p.stop()
        _flow.reset_client()
        _savings.reset()

    async def _async_get(self, req: Any = None) -> dict[str, Any] | None:
        return self.cache_response

    async def _record_async(
        self,
        request: Any,
        response: Any,
        *,
        model: str,
        duration_ms: int,
        tokens_in: int = 0,
        tokens_out: int = 0,
        client_step_id: str | None = None,
    ) -> int | None:
        self.recorded_calls.append(
            (request, response, model, tokens_in, tokens_out, duration_ms)
        )
        return 1


def _make_fake_upstream_response(body: dict[str, Any]) -> _SyntheticClientResponse:
    """Build a synthetic ClientResponse to stand in for the live path.

    Returned by the patched-original ``_request`` to simulate the real
    upstream response. Lets us test cache-miss recording without a real
    network or aiohttp test server.
    """
    return _SyntheticClientResponse(
        status=200,
        headers={"Content-Type": "application/json"},
        body=json.dumps(body).encode("utf-8"),
        url="https://api.openai.com/v1/chat/completions",
    )


# ── Patch lifecycle ────────────────────────────────────────────────


class TestPatchLifecycle(unittest.TestCase):
    def tearDown(self) -> None:
        unpatch_aiohttp_sessions()

    def test_patch_is_idempotent(self) -> None:
        self.assertFalse(is_patched())
        patch_aiohttp_sessions()
        self.assertTrue(is_patched())
        patch_aiohttp_sessions()  # no-op
        self.assertTrue(is_patched())

    def test_unpatch_restores_original(self) -> None:
        original = aiohttp.ClientSession._request
        patch_aiohttp_sessions()
        self.assertNotEqual(aiohttp.ClientSession._request, original)
        unpatch_aiohttp_sessions()
        self.assertEqual(aiohttp.ClientSession._request, original)


# ── Session intercept ──────────────────────────────────────────────


class TestSessionIntercept(unittest.TestCase):
    def setUp(self) -> None:
        patch_aiohttp_sessions()

    def tearDown(self) -> None:
        unpatch_aiohttp_sessions()

    def test_non_llm_request_passes_through(self) -> None:
        async def run() -> None:
            with _FakeRewindServer() as srv:
                async with aiohttp.ClientSession() as session:
                    # Patch the ORIGINAL _request (saved by our patch
                    # on install) to return a fake "live" response.
                    # Our patched _request always runs first, then
                    # delegates to the original on cache miss / non-LLM.
                    from rewind_agent.intercept import aiohttp_middleware

                    async def fake_original(self_, method, url, **kwargs):
                        return _make_fake_upstream_response({"ok": True})

                    aiohttp_middleware._ORIGINAL_REQUEST = fake_original
                    resp = await session.post(
                        "https://example.com/foo", json={"q": 1}
                    )
                    self.assertEqual(resp.status, 200)
                # Predicate False → no recording.
                self.assertEqual(len(srv.recorded_calls), 0)

        asyncio.run(run())

    def test_cache_miss_records(self) -> None:
        async def run() -> None:
            with _FakeRewindServer() as srv:
                srv.cache_response = None  # explicit miss
                async with aiohttp.ClientSession() as session:
                    from rewind_agent.intercept import aiohttp_middleware

                    async def fake_original(self_, method, url, **kwargs):
                        return _make_fake_upstream_response(
                            {
                                "choices": [
                                    {"message": {"content": "live aio"}}
                                ],
                                "usage": {"prompt_tokens": 6, "completion_tokens": 3},
                                "model": "gpt-4o",
                            }
                        )

                    aiohttp_middleware._ORIGINAL_REQUEST = fake_original
                    resp = await session.post(
                        "https://api.openai.com/v1/chat/completions",
                        json={"model": "gpt-4o", "messages": []},
                    )
                    body = await resp.json()
                self.assertEqual(body["choices"][0]["message"]["content"], "live aio")
                self.assertEqual(len(srv.recorded_calls), 1)
                _, _, model, tokens_in, tokens_out, _ = srv.recorded_calls[0]
                self.assertEqual(model, "gpt-4o")
                self.assertEqual(tokens_in, 6)
                self.assertEqual(tokens_out, 3)

        asyncio.run(run())

    def test_cache_hit_returns_synthetic(self) -> None:
        async def run() -> None:
            with _FakeRewindServer() as srv:
                srv.cache_response = {
                    "choices": [{"message": {"content": "from cache aio"}}],
                    "usage": {"prompt_tokens": 4, "completion_tokens": 2},
                    "model": "gpt-4o",
                }
                async with aiohttp.ClientSession() as session:
                    from rewind_agent.intercept import aiohttp_middleware

                    async def boom(self_, method, url, **kwargs):
                        raise AssertionError(
                            "live _request called on cache hit"
                        )

                    aiohttp_middleware._ORIGINAL_REQUEST = boom
                    resp = await session.post(
                        "https://api.openai.com/v1/chat/completions",
                        json={"model": "gpt-4o", "messages": []},
                    )
                    body = await resp.json()
                self.assertEqual(body["choices"][0]["message"]["content"], "from cache aio")
                self.assertEqual(len(srv.recorded_calls), 0)
                snap = _savings.savings()
                self.assertEqual(snap.cache_hits, 1)

        asyncio.run(run())

    def test_streaming_cache_hit_emits_synthetic_sse(self) -> None:
        async def run() -> None:
            with _FakeRewindServer() as srv:
                srv.cache_response = {
                    "choices": [{"message": {"content": "stream aio"}}],
                    "usage": {"prompt_tokens": 3, "completion_tokens": 1},
                    "model": "gpt-4o",
                }
                async with aiohttp.ClientSession() as session:
                    from rewind_agent.intercept import aiohttp_middleware

                    async def boom(self_, method, url, **kwargs):
                        raise AssertionError(
                            "live _request called on streaming cache hit"
                        )

                    aiohttp_middleware._ORIGINAL_REQUEST = boom
                    resp = await session.post(
                        "https://api.openai.com/v1/chat/completions",
                        json={"model": "gpt-4o", "stream": True, "messages": []},
                        headers={"accept": "text/event-stream"},
                    )
                    # Read the synthetic SSE body via response.read().
                    body = await resp.read()
                self.assertIn(b"data: ", body)
                self.assertIn(b'"stream aio"', body)
                self.assertIn(b"data: [DONE]\n\n", body)

        asyncio.run(run())

    def test_async_iter_content_on_cache_hit(self) -> None:
        """Streaming cache hit should support `async for chunk in resp.content`."""

        async def run() -> None:
            with _FakeRewindServer() as srv:
                srv.cache_response = {
                    "choices": [{"message": {"content": "iter aio"}}],
                    "usage": {"prompt_tokens": 2, "completion_tokens": 1},
                    "model": "gpt-4o",
                }
                async with aiohttp.ClientSession() as session:
                    from rewind_agent.intercept import aiohttp_middleware

                    async def boom(self_, method, url, **kwargs):
                        raise AssertionError("live should not be called")

                    aiohttp_middleware._ORIGINAL_REQUEST = boom
                    resp = await session.post(
                        "https://api.openai.com/v1/chat/completions",
                        json={"model": "gpt-4o", "stream": True, "messages": []},
                        headers={"accept": "text/event-stream"},
                    )
                    chunks: list[bytes] = []
                    async for chunk in resp.content:
                        chunks.append(chunk)
                joined = b"".join(chunks)
                self.assertIn(b'"iter aio"', joined)
                self.assertIn(b"data: [DONE]\n\n", joined)

        asyncio.run(run())


# ── _SyntheticClientResponse surface ───────────────────────────────


class TestSyntheticClientResponse(unittest.TestCase):
    def test_supports_async_with(self) -> None:
        async def run() -> None:
            resp = _SyntheticClientResponse(
                status=200,
                headers={"Content-Type": "application/json"},
                body=b'{"hello": "world"}',
                url="https://example.com/",
            )
            async with resp as r:
                data = await r.json()
            self.assertEqual(data, {"hello": "world"})

        asyncio.run(run())

    def test_text_decode(self) -> None:
        async def run() -> None:
            resp = _SyntheticClientResponse(
                status=200, headers={}, body=b"hi", url=""
            )
            text = await resp.text()
            self.assertEqual(text, "hi")

        asyncio.run(run())

    def test_headers_case_insensitive(self) -> None:
        resp = _SyntheticClientResponse(
            status=200,
            headers={"Content-Type": "application/json"},
            body=b"",
            url="",
        )
        self.assertEqual(resp.headers.get("content-type"), "application/json")
        self.assertEqual(resp.headers.get("CONTENT-TYPE"), "application/json")

    def test_release_and_close_are_safe(self) -> None:
        resp = _SyntheticClientResponse(status=200, headers={}, body=b"", url="")
        # No-ops, but defensive consumers call them.
        resp.release()
        resp.close()
        self.assertTrue(resp.closed)


if __name__ == "__main__":
    unittest.main()
