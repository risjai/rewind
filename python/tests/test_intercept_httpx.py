"""Tests for ``rewind_agent.intercept.httpx_transport``.

Exercises the cache-then-live decision flow through real
``httpx.Client`` and ``httpx.AsyncClient`` instances. The "upstream"
HTTP server is faked via ``httpx.MockTransport`` (which we wrap as
the inner transport), and the Rewind server's
:class:`ExplicitClient.get_replayed_response` is monkey-patched to
control cache hit / miss without any actual Rewind server.

## Test matrix

For each of sync (``Client``) and async (``AsyncClient``):

- predicate ``is_llm_call=False`` → request passes through, no recording
- ``is_llm_call=True`` + cache hit (non-streaming) → synthetic JSON response
- ``is_llm_call=True`` + cache hit (streaming) → synthetic SSE with [DONE]
- ``is_llm_call=True`` + cache miss → live response + record_llm_call invoked
- User-passed transport via ``transport=…`` kwarg → wrapped, still works
- Patch / unpatch idempotency
"""

from __future__ import annotations

import asyncio
import json
import unittest
from typing import Any
from unittest.mock import patch

import httpx

from rewind_agent.intercept import _flow, _savings
from rewind_agent.intercept.httpx_transport import (
    is_patched,
    patch_httpx_clients,
    unpatch_httpx_clients,
)


# ── Test fixtures ──────────────────────────────────────────────────


class _FakeRewindServer:
    """In-process stand-in for the Rewind explicit-API server.

    Patches into :class:`ExplicitClient` to intercept
    ``get_replayed_response`` (cache hit / miss) and
    ``record_llm_call`` (recording on miss). Tests configure the
    cached response via :attr:`cache_response` and inspect recordings
    via :attr:`recorded_calls`.
    """

    def __init__(self) -> None:
        # None ⇒ cache miss, dict ⇒ cache hit returning that body.
        self.cache_response: dict[str, Any] | None = None
        # Each (request, response, model, tokens_in, tokens_out, duration_ms)
        # tuple representing one record_llm_call invocation.
        self.recorded_calls: list[tuple[Any, ...]] = []

    def __enter__(self) -> "_FakeRewindServer":
        # Reset _flow's lazy ExplicitClient and _savings counter so
        # each test starts clean.
        _flow.reset_client()
        _savings.reset()
        # Stub out the four methods we care about so no real HTTP
        # calls leave the test process.
        from rewind_agent.explicit import ExplicitClient

        self._patches = [
            patch.object(
                ExplicitClient,
                "get_replayed_response",
                side_effect=lambda req=None: self.cache_response,
            ),
            patch.object(
                ExplicitClient,
                "get_replayed_response_async",
                side_effect=self._async_get,
            ),
            patch.object(
                ExplicitClient,
                "record_llm_call",
                side_effect=self._record,
            ),
            patch.object(
                ExplicitClient,
                "record_llm_call_async",
                side_effect=self._record_async,
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

    def _record(
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


def _mock_upstream(response_body: dict[str, Any] | None = None) -> httpx.MockTransport:
    """Build a MockTransport that returns a fixed JSON response for
    every request. Stand-in for "what the live server would say"."""
    body = response_body or {
        "choices": [{"message": {"role": "assistant", "content": "live"}}],
        "usage": {"prompt_tokens": 8, "completion_tokens": 4},
        "model": "gpt-4o",
    }

    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json=body)

    return httpx.MockTransport(handler)


# ── Patch / unpatch lifecycle ──────────────────────────────────────


class TestPatchLifecycle(unittest.TestCase):
    def tearDown(self) -> None:
        # Defensive: leave httpx clean even if a test mid-way panicked.
        unpatch_httpx_clients()

    def test_patch_is_idempotent(self) -> None:
        self.assertFalse(is_patched())
        patch_httpx_clients()
        self.assertTrue(is_patched())
        # Second call is a no-op (no error, no double-wrap).
        patch_httpx_clients()
        self.assertTrue(is_patched())

    def test_unpatch_restores_original_init(self) -> None:
        original_init = httpx.Client.__init__
        patch_httpx_clients()
        # Patched.
        self.assertNotEqual(httpx.Client.__init__, original_init)
        unpatch_httpx_clients()
        # Restored.
        self.assertEqual(httpx.Client.__init__, original_init)

    def test_unpatch_without_patch_is_safe(self) -> None:
        # No exception even if we never patched.
        unpatch_httpx_clients()
        self.assertFalse(is_patched())


# ── Sync client tests ──────────────────────────────────────────────


class TestSyncClientIntercept(unittest.TestCase):
    def setUp(self) -> None:
        patch_httpx_clients()

    def tearDown(self) -> None:
        unpatch_httpx_clients()

    def test_non_llm_request_passes_through(self) -> None:
        with _FakeRewindServer() as srv:
            client = httpx.Client(transport=_mock_upstream())
            # example.com is not in DEFAULT_LLM_HOSTS → predicate False.
            resp = client.post("https://example.com/foo", json={"q": 1})
            self.assertEqual(resp.status_code, 200)
            # No recording, because predicate was False.
            self.assertEqual(len(srv.recorded_calls), 0)

    def test_cache_miss_records_via_explicit_client(self) -> None:
        with _FakeRewindServer() as srv:
            srv.cache_response = None  # explicit miss
            client = httpx.Client(transport=_mock_upstream())
            resp = client.post(
                "https://api.openai.com/v1/chat/completions",
                json={"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]},
            )
            self.assertEqual(resp.status_code, 200)
            self.assertEqual(resp.json()["choices"][0]["message"]["content"], "live")
            # Recording fired with extracted tokens + model.
            self.assertEqual(len(srv.recorded_calls), 1)
            req, response, model, tokens_in, tokens_out, _ = srv.recorded_calls[0]
            self.assertEqual(model, "gpt-4o")
            self.assertEqual(tokens_in, 8)
            self.assertEqual(tokens_out, 4)
            self.assertEqual(req["model"], "gpt-4o")

    def test_cache_hit_returns_synthetic_response(self) -> None:
        with _FakeRewindServer() as srv:
            cached = {
                "choices": [{"message": {"role": "assistant", "content": "cached!"}}],
                "usage": {"prompt_tokens": 3, "completion_tokens": 1},
                "model": "gpt-4o",
            }
            srv.cache_response = cached

            # The mock transport here would FAIL the test if hit —
            # cache hit must short-circuit before the live path.
            def boom(request: httpx.Request) -> httpx.Response:
                raise AssertionError(
                    "live transport called on cache hit — short-circuit broken"
                )

            client = httpx.Client(transport=httpx.MockTransport(boom))
            resp = client.post(
                "https://api.openai.com/v1/chat/completions",
                json={"model": "gpt-4o", "messages": []},
            )
            self.assertEqual(resp.status_code, 200)
            # Synthetic response carries the cached body.
            self.assertEqual(resp.json()["choices"][0]["message"]["content"], "cached!")
            # No recording (the Rust side already wrote the replayed step).
            self.assertEqual(len(srv.recorded_calls), 0)
            # Savings counter ticked.
            snap = _savings.savings()
            self.assertEqual(snap.cache_hits, 1)
            self.assertEqual(snap.tokens_saved_in, 3)
            self.assertEqual(snap.tokens_saved_out, 1)

    def test_streaming_cache_hit_emits_synthetic_sse(self) -> None:
        with _FakeRewindServer() as srv:
            cached = {
                "choices": [{"message": {"role": "assistant", "content": "stream!"}}],
                "usage": {"prompt_tokens": 5, "completion_tokens": 2},
                "model": "gpt-4o",
            }
            srv.cache_response = cached

            def boom(request: httpx.Request) -> httpx.Response:
                raise AssertionError("live transport called on streaming cache hit")

            client = httpx.Client(transport=httpx.MockTransport(boom))
            # Streaming signal: Accept: text/event-stream
            resp = client.post(
                "https://api.openai.com/v1/chat/completions",
                json={"model": "gpt-4o", "stream": True, "messages": []},
                headers={"accept": "text/event-stream"},
            )
            self.assertEqual(resp.status_code, 200)
            self.assertEqual(resp.headers.get("content-type"), "text/event-stream")

            chunks = list(resp.iter_bytes())
            joined = b"".join(chunks)
            # SSE event with the cached body, then [DONE] sentinel.
            self.assertIn(b"data: ", joined)
            self.assertIn(b'"stream!"', joined)
            self.assertIn(b"data: [DONE]\n\n", joined)

    def test_user_passed_transport_is_wrapped(self) -> None:
        # Verify that a user supplying transport=… (custom MockTransport
        # for tests; could be anything in production) gets wrapped, not
        # replaced. On cache miss, the user's transport delivers; on
        # cache hit, our wrapper short-circuits before reaching it.
        with _FakeRewindServer() as srv:
            srv.cache_response = None
            user_transport_called = []

            def user_handler(request: httpx.Request) -> httpx.Response:
                user_transport_called.append(request.url)
                return httpx.Response(200, json={"choices": [{"message": {"content": "from user"}}], "usage": {"prompt_tokens": 1, "completion_tokens": 1}, "model": "gpt-4o"})

            user_transport = httpx.MockTransport(user_handler)
            client = httpx.Client(transport=user_transport)
            resp = client.post(
                "https://api.openai.com/v1/chat/completions",
                json={"model": "gpt-4o", "messages": []},
            )
            # User's transport was called (delivered the response).
            self.assertEqual(len(user_transport_called), 1)
            self.assertEqual(resp.json()["choices"][0]["message"]["content"], "from user")

    def test_pre_existing_client_keeps_working(self) -> None:
        # Clients constructed BEFORE patch_httpx_clients() ran retain
        # their original transport. We don't try to mutate them — too
        # magical, would break on edge cases. Documented behavior.
        unpatch_httpx_clients()  # tear down the setUp-installed patch
        with _FakeRewindServer() as srv:
            client = httpx.Client(transport=_mock_upstream())  # constructed pre-patch
            patch_httpx_clients()  # patch AFTER
            resp = client.post(
                "https://api.openai.com/v1/chat/completions",
                json={"model": "gpt-4o", "messages": []},
            )
            self.assertEqual(resp.status_code, 200)
            # Pre-existing client did NOT get our transport, so no
            # recording fired.
            self.assertEqual(len(srv.recorded_calls), 0)


# ── Async client tests ─────────────────────────────────────────────


class TestAsyncClientIntercept(unittest.TestCase):
    def setUp(self) -> None:
        patch_httpx_clients()

    def tearDown(self) -> None:
        unpatch_httpx_clients()

    def test_async_cache_miss_records(self) -> None:
        async def run() -> None:
            with _FakeRewindServer() as srv:
                srv.cache_response = None
                client = httpx.AsyncClient(transport=_mock_upstream())
                resp = await client.post(
                    "https://api.openai.com/v1/chat/completions",
                    json={"model": "gpt-4o", "messages": []},
                )
                await client.aclose()
                self.assertEqual(resp.status_code, 200)
                self.assertEqual(len(srv.recorded_calls), 1)

        asyncio.run(run())

    def test_async_cache_hit_returns_synthetic(self) -> None:
        async def run() -> None:
            with _FakeRewindServer() as srv:
                srv.cache_response = {
                    "choices": [{"message": {"content": "cached async"}}],
                    "usage": {"prompt_tokens": 3, "completion_tokens": 1},
                    "model": "gpt-4o",
                }

                async def boom(request: httpx.Request) -> httpx.Response:
                    raise AssertionError("live called on async cache hit")

                # MockTransport accepts both sync and async handlers
                # via its handler callback.
                client = httpx.AsyncClient(transport=httpx.MockTransport(boom))
                resp = await client.post(
                    "https://api.openai.com/v1/chat/completions",
                    json={"model": "gpt-4o", "messages": []},
                )
                await client.aclose()
                body = resp.json()
                self.assertEqual(body["choices"][0]["message"]["content"], "cached async")
                self.assertEqual(len(srv.recorded_calls), 0)

        asyncio.run(run())

    def test_async_non_llm_passes_through(self) -> None:
        async def run() -> None:
            with _FakeRewindServer() as srv:
                client = httpx.AsyncClient(transport=_mock_upstream())
                resp = await client.post("https://example.com/foo", json={"q": 1})
                await client.aclose()
                self.assertEqual(resp.status_code, 200)
                self.assertEqual(len(srv.recorded_calls), 0)

        asyncio.run(run())

    def test_async_streaming_cache_hit(self) -> None:
        async def run() -> None:
            with _FakeRewindServer() as srv:
                srv.cache_response = {
                    "choices": [{"message": {"content": "stream async"}}],
                    "usage": {"prompt_tokens": 4, "completion_tokens": 2},
                    "model": "gpt-4o",
                }

                async def boom(request: httpx.Request) -> httpx.Response:
                    raise AssertionError("live called on streaming async cache hit")

                client = httpx.AsyncClient(transport=httpx.MockTransport(boom))
                resp = await client.post(
                    "https://api.openai.com/v1/chat/completions",
                    json={"model": "gpt-4o", "messages": []},
                    headers={"accept": "text/event-stream"},
                )
                self.assertEqual(resp.headers.get("content-type"), "text/event-stream")

                chunks: list[bytes] = []
                async for chunk in resp.aiter_bytes():
                    chunks.append(chunk)
                joined = b"".join(chunks)
                self.assertIn(b'"stream async"', joined)
                self.assertIn(b"data: [DONE]\n\n", joined)
                await client.aclose()

        asyncio.run(run())


if __name__ == "__main__":
    unittest.main()
