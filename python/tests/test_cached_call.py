"""Tests for ``rewind_agent.cached_call.cached_llm_call``.

The decorator is the Tier 2 / Phase 2 surface. These tests verify:

- Sync + async function support
- Cache hit returns cached value; miss calls live and records
- Custom ``extract_model`` / ``extract_tokens`` / ``cache_key`` plumbing
- Strict-match divergence propagates through the decorator
- Composition with intercept.install() — no double-recording on miss
- Generator / async-generator decoration raises TypeError
- Non-JSON-able args degrade safely via _safe_repr
- Pydantic / openai SDK return types serialize via model_dump()
- Pathological return types log warning and store repr
"""

from __future__ import annotations

import asyncio
import unittest
from typing import Any
from unittest.mock import patch

from rewind_agent import cached_llm_call
from rewind_agent.cached_call import (
    _build_request_payload,
    _default_cache_key,
    _safe_repr,
    _to_json_serializable,
    is_cached_llm_call_active,
)
from rewind_agent.explicit import ExplicitClient


# ── Test fixture: stub ExplicitClient like the intercept tests do ──


class _CacheHarness:
    """Patch ExplicitClient methods so the decorator's cache lookup
    + recording goes through controllable hooks instead of a real
    Rewind server.
    """

    def __init__(self) -> None:
        # None = miss; any value = hit returning that value
        self.cache_response: Any | None = None
        self.recorded_calls: list[dict[str, Any]] = []

    def __enter__(self) -> "_CacheHarness":
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
                side_effect=self._record_sync,
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

    async def _async_get(self, req: Any = None) -> Any:
        return self.cache_response

    def _record_sync(self, request: Any, response: Any, **kw: Any) -> int | None:
        self.recorded_calls.append({"request": request, "response": response, **kw})
        return 1

    async def _record_async(
        self, request: Any, response: Any, **kw: Any
    ) -> int | None:
        self.recorded_calls.append({"request": request, "response": response, **kw})
        return 1


# ── Sync function tests ────────────────────────────────────────────


class TestSyncDecorator(unittest.TestCase):
    def test_cache_miss_calls_function_and_records(self) -> None:
        with _CacheHarness() as h:
            calls = []

            @cached_llm_call()
            def my_fn(question: str) -> dict:
                calls.append(question)
                return {"answer": f"echo: {question}"}

            result = my_fn("hello")
            self.assertEqual(result, {"answer": "echo: hello"})
            self.assertEqual(calls, ["hello"])
            self.assertEqual(len(h.recorded_calls), 1)
            self.assertEqual(h.recorded_calls[0]["response"], {"answer": "echo: hello"})

    def test_cache_hit_returns_cached_without_calling_function(self) -> None:
        with _CacheHarness() as h:
            h.cache_response = {"answer": "from cache"}
            calls = []

            @cached_llm_call()
            def my_fn(question: str) -> dict:
                calls.append(question)
                return {"answer": "live"}

            result = my_fn("hello")
            self.assertEqual(result, {"answer": "from cache"})
            # User function never invoked.
            self.assertEqual(calls, [])
            # No recording (server already wrote replayed step on hit).
            self.assertEqual(len(h.recorded_calls), 0)

    def test_extract_tokens_and_model_reach_record(self) -> None:
        with _CacheHarness() as h:

            @cached_llm_call(
                extract_model=lambda call, ret: ret["model"],
                extract_tokens=lambda call, ret: (
                    ret["usage"]["prompt_tokens"],
                    ret["usage"]["completion_tokens"],
                ),
            )
            def my_fn(question: str) -> dict:
                return {
                    "answer": "x",
                    "model": "gpt-4o",
                    "usage": {"prompt_tokens": 12, "completion_tokens": 4},
                }

            my_fn("hi")
            kw = h.recorded_calls[0]
            self.assertEqual(kw["model"], "gpt-4o")
            self.assertEqual(kw["tokens_in"], 12)
            self.assertEqual(kw["tokens_out"], 4)

    def test_custom_cache_key_overrides_default(self) -> None:
        with _CacheHarness() as h:

            @cached_llm_call(cache_key=lambda *a, **kw: "fixed-key")
            def my_fn(question: str) -> dict:
                return {"answer": question}

            my_fn("hello")
            request = h.recorded_calls[0]["request"]
            self.assertEqual(request["cache_key"], "fixed-key")

    def test_default_cache_key_is_stable_across_runs(self) -> None:
        # Same args → same key. Stability is the cache's contract.
        k1 = _default_cache_key("fn", ("a", 1), {"k": "v"})
        k2 = _default_cache_key("fn", ("a", 1), {"k": "v"})
        self.assertEqual(k1, k2)
        # Different args → different key.
        k3 = _default_cache_key("fn", ("a", 2), {"k": "v"})
        self.assertNotEqual(k1, k3)
        # kwargs order doesn't matter.
        k4 = _default_cache_key("fn", (), {"a": 1, "b": 2})
        k5 = _default_cache_key("fn", (), {"b": 2, "a": 1})
        self.assertEqual(k4, k5)

    def test_failing_extract_tokens_does_not_break_call(self) -> None:
        """A buggy extract_tokens shouldn't take down the user's
        function — log warning, record with zeros, return live result.
        """
        with _CacheHarness() as h:

            @cached_llm_call(extract_tokens=lambda call, ret: 1 / 0)  # boom
            def my_fn(q: str) -> dict:
                return {"answer": q}

            result = my_fn("ok")
            self.assertEqual(result, {"answer": "ok"})
            kw = h.recorded_calls[0]
            self.assertEqual(kw["tokens_in"], 0)
            self.assertEqual(kw["tokens_out"], 0)

    def test_strict_match_divergence_propagates(self) -> None:
        """RewindReplayDivergenceError raised by get_replayed_response
        must surface to the caller, NOT be turned into a cache miss.
        """
        from rewind_agent import RewindReplayDivergenceError

        with patch.object(
            ExplicitClient,
            "get_replayed_response",
            side_effect=RewindReplayDivergenceError("diverged at step 3"),
        ):

            @cached_llm_call()
            def my_fn(q: str) -> dict:
                return {"answer": "should not be called"}

            with self.assertRaises(RewindReplayDivergenceError):
                my_fn("hello")


# ── Async function tests ───────────────────────────────────────────


class TestAsyncDecorator(unittest.TestCase):
    def test_async_cache_miss(self) -> None:
        async def run() -> None:
            with _CacheHarness() as h:
                calls = []

                @cached_llm_call()
                async def my_async(q: str) -> dict:
                    calls.append(q)
                    return {"answer": f"async: {q}"}

                result = await my_async("hi")
                self.assertEqual(result, {"answer": "async: hi"})
                self.assertEqual(calls, ["hi"])
                self.assertEqual(len(h.recorded_calls), 1)

        asyncio.run(run())

    def test_async_cache_hit(self) -> None:
        async def run() -> None:
            with _CacheHarness() as h:
                h.cache_response = {"answer": "async cached"}
                calls = []

                @cached_llm_call()
                async def my_async(q: str) -> dict:
                    calls.append(q)
                    return {"answer": "live"}

                result = await my_async("hi")
                self.assertEqual(result, {"answer": "async cached"})
                self.assertEqual(calls, [])
                self.assertEqual(len(h.recorded_calls), 0)

        asyncio.run(run())

    def test_async_extract_tokens(self) -> None:
        async def run() -> None:
            with _CacheHarness() as h:

                @cached_llm_call(
                    extract_model=lambda call, ret: ret.get("model", ""),
                    extract_tokens=lambda call, ret: (
                        ret["usage"]["prompt_tokens"],
                        ret["usage"]["completion_tokens"],
                    ),
                )
                async def my_async(q: str) -> dict:
                    return {
                        "answer": q,
                        "model": "claude-3-5-sonnet",
                        "usage": {"prompt_tokens": 5, "completion_tokens": 2},
                    }

                await my_async("hi")
                kw = h.recorded_calls[0]
                self.assertEqual(kw["model"], "claude-3-5-sonnet")
                self.assertEqual(kw["tokens_in"], 5)
                self.assertEqual(kw["tokens_out"], 2)

        asyncio.run(run())


# ── Generator + async-gen decoration is rejected ───────────────────


class TestGeneratorRejection(unittest.TestCase):
    def test_sync_generator_function_raises_at_decoration(self) -> None:
        with self.assertRaises(TypeError) as ctx:

            @cached_llm_call()
            def my_gen():
                yield 1
                yield 2

        self.assertIn("generator", str(ctx.exception).lower())

    def test_async_generator_function_raises_at_decoration(self) -> None:
        with self.assertRaises(TypeError):

            @cached_llm_call()
            async def my_async_gen():
                yield 1
                yield 2


# ── Composition with intercept.install() — no double record ────────


class TestInterceptComposition(unittest.TestCase):
    def test_contextvar_set_during_call(self) -> None:
        """When the decorator is calling the user's function, the
        contextvar must be True so intercept knows to skip recording.
        """
        with _CacheHarness():
            observed = []

            @cached_llm_call()
            def my_fn(q: str) -> dict:
                observed.append(is_cached_llm_call_active())
                return {"q": q}

            # Outside the call: False
            self.assertFalse(is_cached_llm_call_active())
            my_fn("test")
            # Outside again: False (token reset on return)
            self.assertFalse(is_cached_llm_call_active())
            # Inside the call: True (decorator set it)
            self.assertEqual(observed, [True])

    def test_contextvar_resets_on_exception(self) -> None:
        """Exception in the user's function should still reset the
        contextvar — try/finally semantics.
        """
        with _CacheHarness():

            @cached_llm_call()
            def my_fn() -> None:
                raise ValueError("boom")

            with self.assertRaises(ValueError):
                my_fn()
            self.assertFalse(is_cached_llm_call_active())

    def test_intercept_flow_skips_record_when_under_decorator(self) -> None:
        """End-to-end check: with both intercept.install() and
        cached_llm_call active, the intercept's _flow doesn't
        double-record on cache miss.
        """
        # We can't easily test this without httpx in the env, so
        # verify the underlying check works directly.
        from rewind_agent.intercept import _flow as flow_mod

        # Outside: returns False
        self.assertFalse(flow_mod._is_under_cached_llm_call())

        # Set the contextvar manually (mimicking what the decorator
        # does) and verify _flow sees it.
        from rewind_agent.cached_call import _cached_llm_call_active

        token = _cached_llm_call_active.set(True)
        try:
            self.assertTrue(flow_mod._is_under_cached_llm_call())
        finally:
            _cached_llm_call_active.reset(token)
        self.assertFalse(flow_mod._is_under_cached_llm_call())


# ── Cache key + serialization helpers ──────────────────────────────


class TestSafeRepr(unittest.TestCase):
    def test_primitives_pass_through(self) -> None:
        self.assertEqual(_safe_repr("x"), "x")
        self.assertEqual(_safe_repr(42), 42)
        self.assertEqual(_safe_repr(3.14), 3.14)
        self.assertEqual(_safe_repr(True), True)
        self.assertEqual(_safe_repr(None), None)

    def test_lists_recurse(self) -> None:
        self.assertEqual(_safe_repr([1, "a", 3.14]), [1, "a", 3.14])

    def test_dicts_recurse_with_str_keys(self) -> None:
        # Non-string keys get str()'d for stable ordering.
        result = _safe_repr({1: "a", "b": 2})
        self.assertEqual(set(result.keys()), {"1", "b"})

    def test_non_serializable_falls_back_to_repr(self) -> None:
        class _Custom:
            def __repr__(self) -> str:
                return "<Custom>"

        self.assertEqual(_safe_repr(_Custom()), "<Custom>")


class TestToJsonSerializable(unittest.TestCase):
    def test_dict_passes_through(self) -> None:
        self.assertEqual(_to_json_serializable({"a": 1}), {"a": 1})

    def test_pydantic_model_dump_used(self) -> None:
        # Mock a Pydantic-shaped object.
        class _FakePydantic:
            def model_dump(self) -> dict:
                return {"answer": "from model_dump"}

        self.assertEqual(
            _to_json_serializable(_FakePydantic()),
            {"answer": "from model_dump"},
        )

    def test_pydantic_v1_dict_method_fallback(self) -> None:
        class _PydanticV1:
            def dict(self) -> dict:
                return {"answer": "from dict"}

        self.assertEqual(
            _to_json_serializable(_PydanticV1()),
            {"answer": "from dict"},
        )

    def test_pathological_object_repr_fallback(self) -> None:
        # Use __slots__ so the class has no __dict__, forcing the
        # fallback chain through to repr(). Without __slots__, an
        # empty-attribute class would serialize to {} via __dict__
        # extraction (which is a legitimate degenerate case but not
        # what this test is exercising).
        class _NotJsonable:
            __slots__ = ()

            def __repr__(self) -> str:
                return "<NotJsonable>"

        # Should log a warning AND return repr — the call shouldn't raise.
        result = _to_json_serializable(_NotJsonable())
        self.assertEqual(result, "<NotJsonable>")


class TestRequestPayload(unittest.TestCase):
    def test_default_payload_shape_is_stable(self) -> None:
        # Review #2 fix: payload now contains ONLY identity fields
        # (_rewind_decorator, fn_name, cache_key). args/kwargs are
        # absent so unstable arg reprs (object IDs etc) can't poison
        # the server-side hash.
        payload = _build_request_payload(
            "my_module.my_fn", ("a",), {"k": "v"}, cache_key=None
        )
        self.assertEqual(payload["_rewind_decorator"], "cached_llm_call")
        self.assertEqual(payload["fn_name"], "my_module.my_fn")
        self.assertIsInstance(payload["cache_key"], str)
        self.assertEqual(len(payload["cache_key"]), 64)  # SHA-256 hex
        # Args/kwargs intentionally absent — see Review #2 fix.
        self.assertNotIn("args_repr", payload)
        self.assertNotIn("kwargs_repr", payload)
        # Payload has exactly 3 fields; any drift would change the
        # server-side hash and break cross-version cache hits.
        self.assertEqual(set(payload.keys()), {"_rewind_decorator", "fn_name", "cache_key"})

    def test_custom_cache_key_replaces_default(self) -> None:
        payload = _build_request_payload(
            "fn", (), {}, cache_key=lambda: "user-supplied"
        )
        self.assertEqual(payload["cache_key"], "user-supplied")

    def test_custom_cache_key_failure_falls_back_to_default(self) -> None:
        # If the custom function raises, we don't break the call —
        # we fall back to the default derivation.
        def boom() -> str:
            raise RuntimeError("user error")

        payload = _build_request_payload("fn", (), {}, cache_key=boom)
        self.assertEqual(len(payload["cache_key"]), 64)  # default sha256


# ── Review #151 regression coverage ────────────────────────────────


class TestCustomCacheKeyIsolation(unittest.TestCase):
    """Review #151 #2: custom cache_key MUST control server-side
    cache identity — the request_payload sent to the server must NOT
    include args/kwargs whose repr varies across processes
    (memory-address-bearing objects). Two calls with the same custom
    cache_key but different unhashable arg objects must produce
    identical payloads.
    """

    def test_custom_cache_key_payload_is_independent_of_arg_reprs(self) -> None:
        """The whole point of custom cache_key. Two `object()` instances
        have different reprs (different memory addresses), so if our
        payload baked args_repr in, the hash would differ.
        """
        client1 = object()  # different objects, different reprs
        client2 = object()
        self.assertNotEqual(repr(client1), repr(client2))

        payload1 = _build_request_payload(
            "chat", (client1, "hi"), {}, cache_key=lambda client, q: q
        )
        payload2 = _build_request_payload(
            "chat", (client2, "hi"), {}, cache_key=lambda client, q: q
        )

        # Identical payloads — the unhashable client arg is invisible
        # to the cache. SAME hash on the server.
        self.assertEqual(payload1, payload2,
                         "custom cache_key didn't isolate cache identity from "
                         "non-stable arg reprs — Review #151 #2 regression")
        self.assertEqual(payload1["cache_key"], "hi")

    def test_decorator_with_custom_cache_key_records_stable_request_across_clients(self) -> None:
        """End-to-end: two calls through the decorator, with the same
        custom cache_key but different unhashable client args, produce
        the same request payload at record time. The server would see
        identical request_hashes → cache hit on the second call.
        """
        with _CacheHarness() as h:
            # cache_key ignores the first positional arg (client),
            # uses only the second (question).
            @cached_llm_call(cache_key=lambda client, q: q)
            def chat(client: Any, q: str) -> dict:
                return {"answer": q}

            client1 = object()
            client2 = object()

            # Both calls miss (cache_response stays None) → both record.
            chat(client1, "hello")
            chat(client2, "hello")

            self.assertEqual(len(h.recorded_calls), 2)
            req1 = h.recorded_calls[0]["request"]
            req2 = h.recorded_calls[1]["request"]
            self.assertEqual(req1, req2,
                             "two recordings with same custom cache_key but "
                             "different client objects produced different requests")

    def test_default_cache_key_payload_independent_of_unstable_object_args(self) -> None:
        """Even WITHOUT a custom cache_key, the default key derivation
        uses _safe_repr which falls back to repr(). Pathological
        objects with address-bearing reprs (the WHOLE reason custom
        cache_key exists) WILL produce different default keys across
        processes — that's expected and documented. This test pins
        the behavior so a future "smart" default-key change doesn't
        silently shift it.
        """
        # Two distinct object instances → different repr → different
        # default cache_key. Documented behavior.
        client1 = object()
        client2 = object()
        payload1 = _build_request_payload("fn", (client1,), {}, cache_key=None)
        payload2 = _build_request_payload("fn", (client2,), {}, cache_key=None)
        self.assertNotEqual(
            payload1["cache_key"], payload2["cache_key"],
            "default cache key collided across distinct object args — "
            "should differ until user supplies custom cache_key"
        )


class TestNoSessionSilentBehavior(unittest.TestCase):
    """Review #151 #1: ExplicitClient.record_llm_call returns None
    silently when no session is active (``_session_id`` contextvar
    is unset). The decorator inherits this behavior — function still
    runs and returns the live result, but recording is a no-op.

    This is consistent with the rest of the SDK (init() / intercept
    have the same precondition). Document this in the docstring; this
    test pins the contract so a future "auto-create session"
    refactor doesn't silently change it without explicit decision.
    """

    def test_no_session_function_runs_and_returns_live_result(self) -> None:
        # Reset session contextvar to simulate "user never entered
        # client.session() / called ensure_session() / called init()".
        from rewind_agent import explicit as _explicit_mod

        sid_token = _explicit_mod._session_id.set(None)
        try:
            calls = []

            @cached_llm_call()
            def my_fn(q: str) -> dict:
                calls.append(q)
                return {"answer": q}

            # Function runs, returns live result. No exception.
            result = my_fn("hello")
            self.assertEqual(result, {"answer": "hello"})
            self.assertEqual(calls, ["hello"])
        finally:
            _explicit_mod._session_id.reset(sid_token)

    def test_no_session_record_is_silent_no_op(self) -> None:
        """Call the REAL ExplicitClient.record_llm_call (no patch)
        with no active session — verify it returns None without
        raising. This is the test the reviewer asked for: shows the
        decorator's silent-no-op behavior under the actual production
        guard, not via a mock that bypasses it.
        """
        from rewind_agent import explicit as _explicit_mod
        from rewind_agent.explicit import ExplicitClient

        sid_token = _explicit_mod._session_id.set(None)
        try:
            client = ExplicitClient()
            # Real call to the real method — no patch. Returns None
            # because there's no session. No HTTP attempted (the
            # session-guard short-circuits before urllib).
            result = client.record_llm_call(
                request={"q": "hi"},
                response={"a": "ok"},
                model="gpt-4o",
                duration_ms=10,
            )
            self.assertIsNone(result)
        finally:
            _explicit_mod._session_id.reset(sid_token)

    def test_active_session_records_via_real_client_path(self) -> None:
        """Inverse of the previous test: when a session IS active,
        record_llm_call attempts the HTTP POST. We can't actually
        exercise the HTTP without a Rewind server, but we verify the
        guard passes and the code reaches the _post call (which would
        then try to talk to localhost:4800 and silently return None
        on connection refused — which is fine for this test).
        """
        from rewind_agent import explicit as _explicit_mod
        from rewind_agent.explicit import ExplicitClient

        sid_token = _explicit_mod._session_id.set("test-session-id")
        post_invocations: list[str] = []
        try:
            client = ExplicitClient()
            # Patch _post to verify it's invoked (proving the
            # session-guard passed) without making real HTTP.
            with patch.object(
                ExplicitClient,
                "_post",
                side_effect=lambda path, body: post_invocations.append(path) or {"step_number": 1},
            ):
                result = client.record_llm_call(
                    request={"q": "hi"},
                    response={"a": "ok"},
                    model="gpt-4o",
                    duration_ms=10,
                )
            self.assertEqual(result, 1, "active session should reach _post and return step_number")
            self.assertEqual(len(post_invocations), 1, "session-active path didn't call _post")
            self.assertIn("/llm-calls", post_invocations[0])
        finally:
            _explicit_mod._session_id.reset(sid_token)


if __name__ == "__main__":
    unittest.main()
