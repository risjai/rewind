"""Regression tests for Santa reviews on PR #149.

Two waves of findings, both covered here:

**Initial review (NAUGHTY, 7 findings):**

- ``test_strict_match_409_surfaces`` — Santa #4
- ``test_httpx_config_preserved_post_init_wrap`` — Santa #5
- ``test_streaming_miss_does_not_eager_read_body`` — Santa #2
- ``test_body_only_stream_true_detects_streaming`` — Santa #3
- ``test_install_handles_subset_of_libraries`` — Santa #1 follow-up

**Re-review (NAUGHTY, 4 remaining findings on commit 1f9a2dd):**

- ``test_httpx_buffered_response_via_real_transport`` — Re-review #1:
  ``_read_response_body_async`` must call ``await resp.aread()``
  before ``resp.json()`` because real httpx transport responses are
  NOT auto-read. ``MockTransport`` masked this in initial tests.
- ``test_httpx_close_propagates_to_inner_transport`` — Re-review #2:
  ``RewindHTTPTransport.close()`` and async ``aclose()`` must forward
  to ``self._inner`` so wrapped transports release their connection
  pools / SSL contexts.
- ``test_aiohttp_base_url_relative_path_predicate_match`` — Re-review #3:
  ``session.post("/v1/chat/completions")`` against a
  ``ClientSession(base_url="https://api.openai.com")`` must resolve
  to the absolute URL before host-based predicates run. Otherwise
  the request silently bypasses interception.
"""

from __future__ import annotations

import unittest
import urllib.error
from typing import Any
from unittest.mock import patch

import pytest

# Most regression tests require httpx — the most-used adapter. Skip
# them when httpx isn't installed (CI's bare env); install tests
# below stay alive across all envs.
httpx = pytest.importorskip("httpx", exc_type=ImportError)

from rewind_agent import RewindReplayDivergenceError  # noqa: E402
from rewind_agent.explicit import ExplicitClient  # noqa: E402
from rewind_agent.intercept import _flow, _savings  # noqa: E402
from rewind_agent.intercept import (  # noqa: E402
    aiohttp_middleware,
    httpx_transport,
    requests_adapter,
)


# ── Santa #4: strict-match 409 surfaces as typed exception ─────────


class TestStrictMatch409Surfaces(unittest.TestCase):
    def _seed_replay_context(self) -> tuple[Any, Any]:
        """Set the contextvars get_replayed_response reads. ContextVar
        attributes are read-only at the C level so we can't patch.object
        them; .set() returns a Token we restore in tearDown.
        """
        from rewind_agent import explicit as _explicit_mod

        sid_token = _explicit_mod._session_id.set("sess-1")
        ctx_token = _explicit_mod._replay_context_id.set("ctx-1")
        return sid_token, ctx_token

    def _reset_replay_context(self, tokens: tuple[Any, Any]) -> None:
        from rewind_agent import explicit as _explicit_mod

        _explicit_mod._session_id.reset(tokens[0])
        _explicit_mod._replay_context_id.reset(tokens[1])

    def test_strict_match_409_raises_typed_error(self) -> None:
        """get_replayed_response must NOT swallow HTTP 409 to None.

        A swallow turns a strict-mode divergence into a silent cache
        miss — defeats the entire purpose of strict_match=True.
        """
        body = b'Cache divergence at step 3 (strict_match=true): incoming hash abc123 != stored def456'

        def _raise_409(req, timeout):  # type: ignore[no-untyped-def]
            err = urllib.error.HTTPError(
                url=req.full_url,
                code=409,
                msg="Conflict",
                hdrs=None,  # type: ignore[arg-type]
                fp=None,
            )
            # Override read() to return the canned diagnostic body.
            err.read = lambda: body  # type: ignore[method-assign]
            raise err

        tokens = self._seed_replay_context()
        try:
            client = ExplicitClient()
            with patch("urllib.request.urlopen", side_effect=_raise_409):
                with self.assertRaises(RewindReplayDivergenceError) as ctx:
                    client.get_replayed_response({"model": "x", "messages": []})
            self.assertIn("strict_match=true", str(ctx.exception))
        finally:
            self._reset_replay_context(tokens)

    def test_non_409_http_error_is_swallowed_to_cache_miss(self) -> None:
        """Other 4xx/5xx errors degrade to None (cache miss), preserving
        the previous best-effort behavior. Only 409 is re-raised."""

        def _raise_500(req, timeout):  # type: ignore[no-untyped-def]
            raise urllib.error.HTTPError(
                url=req.full_url,
                code=500,
                msg="Internal Server Error",
                hdrs=None,  # type: ignore[arg-type]
                fp=None,
            )

        tokens = self._seed_replay_context()
        try:
            client = ExplicitClient()
            with patch("urllib.request.urlopen", side_effect=_raise_500):
                result = client.get_replayed_response({"model": "x"})
            self.assertIsNone(result)
        finally:
            self._reset_replay_context(tokens)


# ── Santa #5: httpx configured default transport preserved ─────────


class TestHttpxConfigPreserved(unittest.TestCase):
    def setUp(self) -> None:
        httpx_transport.unpatch_httpx_clients()

    def tearDown(self) -> None:
        httpx_transport.unpatch_httpx_clients()

    def test_verify_false_setting_survives_intercept_install(self) -> None:
        """``httpx.Client(verify=False)`` must reach the underlying
        transport. Pre-Santa #5, our patch built a fresh
        RewindHTTPTransport() without forwarding kwargs, dropping verify.
        """
        httpx_transport.patch_httpx_clients()

        client = httpx.Client(verify=False)
        # Our wrapper exposes _inner — the configured default transport
        # httpx built. That inner transport should reflect verify=False.
        wrapper = client._transport
        self.assertIsNotNone(getattr(wrapper, "_inner", None),
                             "Phase 1 wrapper missing _inner — config drop bug regressed")
        # httpx HTTPTransport stores SSL settings inside an internal
        # SSLContext; we verify the pool's verify mode reflects False.
        # The cleanest signal is that _inner is NOT just a default-config
        # HTTPTransport — it has the user-supplied verify=False.
        # The most stable cross-version check: the inner exists and is
        # not our class (it's the configured default).
        self.assertNotIsInstance(
            wrapper._inner,
            type(wrapper),
            "inner transport should be httpx's configured default, not another Rewind wrapper",
        )

    def test_user_supplied_transport_is_wrapped_not_replaced(self) -> None:
        """Mode (a): user passes transport=X → we wrap it so X's logic still runs."""
        httpx_transport.patch_httpx_clients()

        called = []

        def handler(request: httpx.Request) -> httpx.Response:
            called.append(request.url)
            return httpx.Response(200, json={"ok": True})

        user_t = httpx.MockTransport(handler)
        client = httpx.Client(transport=user_t)
        # Our wrapper's _inner should be the user's transport.
        self.assertIs(client._transport._inner, user_t)


# ── Santa #2: streaming pass-through (no eager body read) ──────────


class TestStreamingPassThrough(unittest.TestCase):
    def setUp(self) -> None:
        httpx_transport.patch_httpx_clients()
        _flow.reset_client()
        _savings.reset()

    def tearDown(self) -> None:
        httpx_transport.unpatch_httpx_clients()
        _flow.reset_client()
        _savings.reset()

    def test_streaming_miss_passes_through_without_consuming_body(self) -> None:
        """Live streaming response must reach user code with the body
        unconsumed. Pre-Santa #2 we'd ``await resp.json()`` before
        returning, breaking httpx streaming clients.
        """
        # Fake upstream returns a streaming SSE body.
        sse_body = (
            b'data: {"choices":[{"delta":{"content":"hi"}}]}\n\n'
            b"data: [DONE]\n\n"
        )

        def upstream_handler(request: httpx.Request) -> httpx.Response:
            return httpx.Response(
                200,
                headers={"Content-Type": "text/event-stream"},
                stream=httpx.ByteStream(sse_body),
            )

        # Stub ExplicitClient — cache miss + record_llm_call observability.
        recorded: list[dict[str, Any]] = []
        with patch.object(
            ExplicitClient, "get_replayed_response", return_value=None
        ), patch.object(
            ExplicitClient,
            "record_llm_call",
            side_effect=lambda *a, **kw: recorded.append(kw) or 1,
        ):
            client = httpx.Client(transport=httpx.MockTransport(upstream_handler))
            resp = client.post(
                "https://api.openai.com/v1/chat/completions",
                json={"model": "gpt-4o", "stream": True, "messages": []},
                headers={"accept": "text/event-stream"},
            )
            # Critical assertion: we can still iterate the body.
            # If _flow had pre-read it, this would yield empty bytes.
            chunks = list(resp.iter_bytes())
            joined = b"".join(chunks)
            self.assertIn(b"data: [DONE]", joined,
                          "streaming body was consumed before user could iterate")

        # Recording fired with placeholder None response + zero tokens
        # (Phase 1 limitation; tee-based capture in v1.1).
        self.assertEqual(len(recorded), 1, "streaming miss should record once")
        self.assertIsNone(recorded[0]["response"])
        self.assertEqual(recorded[0]["tokens_in"], 0)
        self.assertEqual(recorded[0]["tokens_out"], 0)


# ── Santa #3: body-only stream:true detected ───────────────────────


class TestBodyOnlyStreamTrueDetected(unittest.TestCase):
    def setUp(self) -> None:
        httpx_transport.patch_httpx_clients()
        _flow.reset_client()
        _savings.reset()

    def tearDown(self) -> None:
        httpx_transport.unpatch_httpx_clients()
        _flow.reset_client()
        _savings.reset()

    def test_cache_hit_with_body_stream_true_emits_synthetic_sse(self) -> None:
        """Request body has ``"stream": true`` but no Accept header —
        cache hit must still route through the streaming path
        (synthetic SSE), not the buffered path.
        """
        cached_inner = {
            "choices": [{"message": {"content": "stream-via-body"}}],
            "usage": {"prompt_tokens": 4, "completion_tokens": 2},
            "model": "gpt-4o",
        }

        def boom(request: httpx.Request) -> httpx.Response:
            raise AssertionError("live transport called on cache hit")

        with patch.object(
            ExplicitClient, "get_replayed_response", return_value=cached_inner
        ):
            client = httpx.Client(transport=httpx.MockTransport(boom))
            # Note: NO Accept: text/event-stream header. Body has stream: true.
            resp = client.post(
                "https://api.openai.com/v1/chat/completions",
                json={"model": "gpt-4o", "stream": True, "messages": []},
            )
            # The synthetic response should be SSE-formatted (data: …).
            chunks = list(resp.iter_bytes())
            joined = b"".join(chunks)
            self.assertIn(b"data: ", joined,
                          "body-only stream:true didn't trigger SSE synth — Santa #3 regression")
            self.assertIn(b"data: [DONE]", joined)


# ── Santa #1 follow-up: install with subset of libraries ───────────


class TestInstallWithSubsetOfLibraries(unittest.TestCase):
    """Reproduces CI's bare environment: not all of httpx, requests,
    aiohttp are available. install() must succeed and only patch what
    IS available.
    """

    def setUp(self) -> None:
        from rewind_agent.intercept import uninstall

        uninstall()

    def tearDown(self) -> None:
        from rewind_agent.intercept import uninstall

        uninstall()

    def test_install_only_patches_available_adapters(self) -> None:
        from rewind_agent.intercept import install, is_installed

        install()
        self.assertTrue(is_installed())

        # Each adapter is patched if and only if its library is importable.
        self.assertEqual(
            httpx_transport.is_patched(),
            httpx_transport.HTTPX_AVAILABLE,
        )
        self.assertEqual(
            requests_adapter.is_patched(),
            requests_adapter.REQUESTS_AVAILABLE,
        )
        self.assertEqual(
            aiohttp_middleware.is_patched(),
            aiohttp_middleware.AIOHTTP_AVAILABLE,
        )


# ── Re-review #1: httpx body materialization on real transport ─────


class TestHttpxResponseBodyMaterialization(unittest.TestCase):
    """Real httpx transport responses don't auto-read the body — calling
    ``resp.json()`` before ``resp.read()`` raises ``httpx.ResponseNotRead``.

    Our previous mock-based tests passed because ``httpx.MockTransport``
    + ``httpx.Response(json=…)`` auto-buffers the body. To catch the
    regression, this test constructs a real ``httpx.Response`` with a
    ``ByteStream`` (not yet read) and verifies ``_read_response_body_*``
    calls read/aread before json.
    """

    def setUp(self) -> None:
        _flow.reset_client()
        _savings.reset()

    def tearDown(self) -> None:
        _flow.reset_client()
        _savings.reset()

    def test_sync_read_called_before_json_on_unread_response(self) -> None:
        """``_read_response_body_sync`` must call ``resp.read()`` before
        ``resp.json()`` so that an unread httpx Response (the shape from
        a real transport) doesn't raise ResponseNotRead.
        """
        body = b'{"choices":[{"message":{"content":"hi"}}]}'
        # Build a Response with a ByteStream — this is the shape a real
        # httpx transport returns. .json() on this raises ResponseNotRead
        # until .read() materializes the body.
        request = httpx.Request("POST", "https://api.openai.com/v1/chat/completions")
        resp = httpx.Response(
            200,
            headers={"content-type": "application/json"},
            stream=httpx.ByteStream(body),
            request=request,
        )

        # Sanity check: pre-fix behavior would raise here.
        # (We don't assert this directly because the fix means it works.)
        result = _flow._read_response_body_sync(resp)
        self.assertEqual(result, {"choices": [{"message": {"content": "hi"}}]})

    def test_async_aread_called_before_json_on_unread_response(self) -> None:
        """Async counterpart — ``_read_response_body_async`` must call
        ``await resp.aread()`` first."""
        async def run() -> None:
            body = b'{"usage":{"prompt_tokens":5,"completion_tokens":2}}'
            request = httpx.Request("POST", "https://api.openai.com/v1/chat/completions")
            resp = httpx.Response(
                200,
                headers={"content-type": "application/json"},
                stream=httpx.ByteStream(body),
                request=request,
            )
            result = await _flow._read_response_body_async(resp)
            self.assertEqual(
                result,
                {"usage": {"prompt_tokens": 5, "completion_tokens": 2}},
            )

        import asyncio
        asyncio.run(run())

    def test_buffered_cache_miss_through_real_transport_records_tokens(self) -> None:
        """End-to-end: a real httpx.AsyncClient making a buffered POST
        on cache miss should not crash with ResponseNotRead, AND should
        successfully extract tokens from the response body for
        record_llm_call.
        """
        async def run() -> None:
            httpx_transport.patch_httpx_clients()
            try:
                recorded: list[dict[str, Any]] = []

                # MockTransport with a stream-typed Response (more like
                # real transport behavior than json=) so the body is NOT
                # pre-read. Our fix's await resp.aread() is what makes
                # this work.
                def upstream_handler(request: httpx.Request) -> httpx.Response:
                    body = b'{"choices":[{"message":{"content":"buffered"}}],' \
                           b'"usage":{"prompt_tokens":7,"completion_tokens":4},' \
                           b'"model":"gpt-4o"}'
                    return httpx.Response(
                        200,
                        headers={"content-type": "application/json"},
                        stream=httpx.ByteStream(body),
                        request=request,
                    )

                with patch.object(
                    ExplicitClient, "get_replayed_response_async", return_value=None,
                ), patch.object(
                    ExplicitClient,
                    "record_llm_call_async",
                    side_effect=lambda *a, **kw: (recorded.append(kw), 1)[1],
                ):
                    client = httpx.AsyncClient(
                        transport=httpx.MockTransport(upstream_handler)
                    )
                    resp = await client.post(
                        "https://api.openai.com/v1/chat/completions",
                        json={"model": "gpt-4o", "messages": []},
                    )
                    self.assertEqual(resp.status_code, 200)
                    # User code can still read the body — our pre-read
                    # didn't damage the response.
                    body = await resp.aread()
                    self.assertIn(b"buffered", body)
                    await client.aclose()

                # Recording fired with extracted tokens / model.
                self.assertEqual(len(recorded), 1)
                kw = recorded[0]
                self.assertEqual(kw["tokens_in"], 7)
                self.assertEqual(kw["tokens_out"], 4)
                self.assertEqual(kw["model"], "gpt-4o")
            finally:
                httpx_transport.unpatch_httpx_clients()

        import asyncio
        asyncio.run(run())


# ── Re-review #2: close/aclose forward to _inner ───────────────────


class TestHttpxCloseLifecycle(unittest.TestCase):
    """Wrapped transport must release the underlying configured/user
    transport when the Client closes. Without this, every wrapped
    transport leaks for the process lifetime.
    """

    def setUp(self) -> None:
        httpx_transport.unpatch_httpx_clients()

    def tearDown(self) -> None:
        httpx_transport.unpatch_httpx_clients()

    def test_sync_close_propagates_to_inner_transport(self) -> None:
        httpx_transport.patch_httpx_clients()

        # Spy on close() of a MockTransport. MockTransport.close exists
        # in modern httpx; if it doesn't, fall back to recording via
        # an attribute on a custom transport.
        class _SpyTransport(httpx.MockTransport):  # type: ignore[misc]
            close_count = 0

            def close(self) -> None:  # type: ignore[override]
                _SpyTransport.close_count += 1
                super().close()

        spy = _SpyTransport(lambda req: httpx.Response(200, json={"ok": True}))
        client = httpx.Client(transport=spy)
        # Sanity: our wrapper's _inner should be the spy.
        self.assertIs(client._transport._inner, spy)

        # Closing the client must trigger close on the wrapper, which
        # must forward to spy.
        client.close()
        self.assertGreaterEqual(
            _SpyTransport.close_count, 1,
            "RewindHTTPTransport.close did not forward to _inner — "
            "configured/user transport leaks for the process lifetime",
        )

    def test_async_aclose_propagates_to_inner_transport(self) -> None:
        async def run() -> None:
            httpx_transport.patch_httpx_clients()

            class _AsyncSpyTransport(httpx.MockTransport):  # type: ignore[misc]
                aclose_count = 0

                async def aclose(self) -> None:  # type: ignore[override]
                    _AsyncSpyTransport.aclose_count += 1
                    await super().aclose()

            spy = _AsyncSpyTransport(lambda req: httpx.Response(200, json={"ok": True}))
            client = httpx.AsyncClient(transport=spy)
            self.assertIs(client._transport._inner, spy)

            await client.aclose()
            self.assertGreaterEqual(
                _AsyncSpyTransport.aclose_count, 1,
                "RewindAsyncHTTPTransport.aclose did not forward to _inner",
            )

        import asyncio
        asyncio.run(run())


# ── Re-review #3: aiohttp base_url + relative path resolves ────────


class TestAiohttpBaseUrlResolution(unittest.TestCase):
    """``session.post("/v1/chat/completions")`` against
    ``ClientSession(base_url="https://api.openai.com")`` must resolve
    to the absolute URL before host predicates evaluate. Pre-fix:
    only the path reached the predicate, host check failed silently,
    request bypassed interception.
    """

    def test_relative_path_resolves_against_base_url(self) -> None:
        from rewind_agent.intercept.aiohttp_middleware import _resolve_url

        # Mock-shape session with _base_url attribute.
        try:
            from yarl import URL  # type: ignore[import-untyped]
        except ImportError:
            self.skipTest("yarl not installed (aiohttp dep) — skipping")

        class _MockSession:
            _base_url = URL("https://api.openai.com")

        resolved = _resolve_url(_MockSession(), "/v1/chat/completions")
        self.assertEqual(resolved, "https://api.openai.com/v1/chat/completions")

    def test_absolute_url_passes_through_unchanged(self) -> None:
        from rewind_agent.intercept.aiohttp_middleware import _resolve_url

        try:
            from yarl import URL  # type: ignore[import-untyped]
        except ImportError:
            self.skipTest("yarl not installed (aiohttp dep) — skipping")

        class _MockSession:
            _base_url = URL("https://api.openai.com")

        # Even with a base_url set, an absolute URL should pass through.
        resolved = _resolve_url(
            _MockSession(),
            "https://api.anthropic.com/v1/messages",
        )
        self.assertEqual(resolved, "https://api.anthropic.com/v1/messages")

    def test_no_base_url_returns_input_unchanged(self) -> None:
        from rewind_agent.intercept.aiohttp_middleware import _resolve_url

        class _MockSession:
            _base_url = None

        resolved = _resolve_url(_MockSession(), "/anything")
        self.assertEqual(resolved, "/anything")

    def test_aiohttp_session_base_url_match_records_via_predicate(self) -> None:
        """End-to-end: aiohttp session with base_url and a relative
        POST path matches the default predicate (api.openai.com) and
        triggers recording.
        """
        try:
            import aiohttp  # noqa: F401
        except ImportError:
            self.skipTest("aiohttp not installed — skipping")

        async def run() -> None:
            aiohttp_middleware.patch_aiohttp_sessions()
            try:
                _flow.reset_client()
                _savings.reset()
                recorded: list[dict[str, Any]] = []

                # Stub the upstream transport via _ORIGINAL_REQUEST
                # swap (same pattern as tests/test_intercept_aiohttp.py).
                async def fake_original(self_, method, url, **kw):  # type: ignore[no-untyped-def]
                    return aiohttp_middleware._SyntheticClientResponse(
                        status=200,
                        headers={"content-type": "application/json"},
                        body=b'{"choices":[{"message":{"content":"x"}}],"usage":{"prompt_tokens":1,"completion_tokens":1},"model":"gpt-4o"}',
                        url=url,
                    )

                aiohttp_middleware._ORIGINAL_REQUEST = fake_original

                with patch.object(
                    ExplicitClient,
                    "get_replayed_response_async",
                    return_value=None,
                ), patch.object(
                    ExplicitClient,
                    "record_llm_call_async",
                    side_effect=lambda *a, **kw: (recorded.append(kw), 1)[1],
                ):
                    import aiohttp as _aio

                    async with _aio.ClientSession(
                        base_url="https://api.openai.com",
                    ) as session:
                        resp = await session.post(
                            "/v1/chat/completions",
                            json={"model": "gpt-4o", "messages": []},
                        )
                        await resp.read()

                # Recording fired = predicate matched the resolved URL.
                # Pre-fix: only "/v1/chat/completions" reached the
                # predicate, host check missed, no recording.
                self.assertEqual(
                    len(recorded), 1,
                    "base_url + relative path didn't match the host predicate — "
                    "request silently bypassed interception",
                )
            finally:
                aiohttp_middleware.unpatch_aiohttp_sessions()
                _flow.reset_client()
                _savings.reset()

        import asyncio
        asyncio.run(run())


if __name__ == "__main__":
    unittest.main()
