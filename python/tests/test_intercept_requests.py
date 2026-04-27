"""Tests for ``rewind_agent.intercept.requests_adapter``.

Exercises cache-then-live routing through real ``requests.Session``
instances. The "upstream" HTTP delivery is faked by patching
``HTTPAdapter.send`` (the superclass our RewindHTTPAdapter delegates
to on cache miss); the Rewind server's
``ExplicitClient.get_replayed_response`` is patched too, just like
the httpx tests.
"""

from __future__ import annotations

import unittest
from typing import Any
from unittest.mock import patch

import pytest

# Skip the module when requests isn't installed. See test_intercept_httpx.py
# for the rationale (optional dep; CI installs it; importorskip is the
# fallback for stripped dev envs).
requests = pytest.importorskip("requests")
from requests.adapters import HTTPAdapter  # noqa: E402
from requests.models import Response  # noqa: E402

from rewind_agent.intercept import _flow, _savings  # noqa: E402
from rewind_agent.intercept.requests_adapter import (  # noqa: E402
    is_patched,
    patch_requests_sessions,
    unpatch_requests_sessions,
)


# ── Fake Rewind server fixture (mirrors test_intercept_httpx.py) ───


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
                "get_replayed_response",
                side_effect=lambda req=None: self.cache_response,
            ),
            patch.object(
                ExplicitClient,
                "record_llm_call",
                side_effect=self._record,
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


def _make_fake_upstream_response(body: dict[str, Any]) -> Response:
    """Build a ``requests.Response`` for ``HTTPAdapter.send`` to "return"
    when monkey-patched."""
    import json as _json

    resp = Response()
    resp.status_code = 200
    resp._content = _json.dumps(body).encode("utf-8")  # type: ignore[attr-defined]
    resp._content_consumed = True  # type: ignore[attr-defined]
    resp.encoding = "utf-8"
    resp.headers["Content-Type"] = "application/json"
    resp.url = "https://api.openai.com/v1/chat/completions"
    return resp


# ── Tests ──────────────────────────────────────────────────────────


class TestPatchLifecycle(unittest.TestCase):
    def tearDown(self) -> None:
        unpatch_requests_sessions()

    def test_patch_is_idempotent(self) -> None:
        self.assertFalse(is_patched())
        patch_requests_sessions()
        self.assertTrue(is_patched())
        patch_requests_sessions()  # no-op
        self.assertTrue(is_patched())

    def test_unpatch_restores_original(self) -> None:
        original = requests.Session.__init__
        patch_requests_sessions()
        self.assertNotEqual(requests.Session.__init__, original)
        unpatch_requests_sessions()
        self.assertEqual(requests.Session.__init__, original)


class TestSessionIntercept(unittest.TestCase):
    def setUp(self) -> None:
        patch_requests_sessions()

    def tearDown(self) -> None:
        unpatch_requests_sessions()

    def test_non_llm_request_passes_through(self) -> None:
        with _FakeRewindServer() as srv:
            session = requests.Session()
            with patch.object(
                HTTPAdapter,
                "send",
                return_value=_make_fake_upstream_response({"ok": True}),
            ):
                resp = session.post("https://example.com/foo", json={"q": 1})
            self.assertEqual(resp.status_code, 200)
            # Predicate False → no recording.
            self.assertEqual(len(srv.recorded_calls), 0)

    def test_cache_miss_records_via_explicit_client(self) -> None:
        with _FakeRewindServer() as srv:
            srv.cache_response = None
            session = requests.Session()
            with patch.object(
                HTTPAdapter,
                "send",
                return_value=_make_fake_upstream_response(
                    {
                        "choices": [
                            {"message": {"role": "assistant", "content": "live req"}}
                        ],
                        "usage": {"prompt_tokens": 7, "completion_tokens": 3},
                        "model": "gpt-4o",
                    }
                ),
            ):
                resp = session.post(
                    "https://api.openai.com/v1/chat/completions",
                    json={
                        "model": "gpt-4o",
                        "messages": [{"role": "user", "content": "hi"}],
                    },
                )
            self.assertEqual(resp.status_code, 200)
            self.assertEqual(
                resp.json()["choices"][0]["message"]["content"], "live req"
            )
            self.assertEqual(len(srv.recorded_calls), 1)
            req, response, model, tokens_in, tokens_out, _ = srv.recorded_calls[0]
            self.assertEqual(model, "gpt-4o")
            self.assertEqual(tokens_in, 7)
            self.assertEqual(tokens_out, 3)
            self.assertEqual(req["model"], "gpt-4o")

    def test_cache_hit_returns_synthetic_response(self) -> None:
        with _FakeRewindServer() as srv:
            cached = {
                "choices": [
                    {"message": {"role": "assistant", "content": "from cache"}}
                ],
                "usage": {"prompt_tokens": 4, "completion_tokens": 2},
                "model": "gpt-4o",
            }
            srv.cache_response = cached
            session = requests.Session()

            # Boom if HTTPAdapter.send is reached — cache hit must
            # short-circuit before going to the wire.
            with patch.object(
                HTTPAdapter,
                "send",
                side_effect=AssertionError(
                    "live HTTPAdapter.send called on cache hit"
                ),
            ):
                resp = session.post(
                    "https://api.openai.com/v1/chat/completions",
                    json={"model": "gpt-4o", "messages": []},
                )
            self.assertEqual(resp.status_code, 200)
            self.assertEqual(
                resp.json()["choices"][0]["message"]["content"], "from cache"
            )
            self.assertEqual(len(srv.recorded_calls), 0)
            # Savings counter ticked.
            snap = _savings.savings()
            self.assertEqual(snap.cache_hits, 1)
            self.assertEqual(snap.tokens_saved_in, 4)
            self.assertEqual(snap.tokens_saved_out, 2)

    def test_streaming_cache_hit_emits_synthetic_sse(self) -> None:
        with _FakeRewindServer() as srv:
            srv.cache_response = {
                "choices": [{"message": {"content": "stream req"}}],
                "usage": {"prompt_tokens": 3, "completion_tokens": 1},
                "model": "gpt-4o",
            }
            session = requests.Session()
            with patch.object(
                HTTPAdapter,
                "send",
                side_effect=AssertionError("live called on streaming cache hit"),
            ):
                # Streaming signal: stream=True kwarg on session.post().
                # The PreparedRequest carries Accept header; the adapter
                # gets stream_arg=True via Session.send forwarding.
                resp = session.post(
                    "https://api.openai.com/v1/chat/completions",
                    json={"model": "gpt-4o", "stream": True, "messages": []},
                    headers={"Accept": "text/event-stream"},
                    stream=True,
                )
            self.assertEqual(resp.headers.get("Content-Type"), "text/event-stream")
            # Iter_content should yield the SSE bytes; our synthetic
            # body is one chunk + [DONE].
            chunks = list(resp.iter_content(chunk_size=None))
            joined = b"".join(chunks)
            self.assertIn(b"data: ", joined)
            self.assertIn(b'"stream req"', joined)
            self.assertIn(b"data: [DONE]\n\n", joined)

    def test_pre_existing_session_keeps_default_adapter(self) -> None:
        # Sessions built BEFORE patch_requests_sessions don't get our
        # adapter. Documented behavior.
        unpatch_requests_sessions()
        with _FakeRewindServer() as srv:
            session = requests.Session()  # built pre-patch
            patch_requests_sessions()
            with patch.object(
                HTTPAdapter,
                "send",
                return_value=_make_fake_upstream_response(
                    {
                        "choices": [{"message": {"content": "x"}}],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1},
                        "model": "gpt-4o",
                    }
                ),
            ):
                resp = session.post(
                    "https://api.openai.com/v1/chat/completions",
                    json={"model": "gpt-4o", "messages": []},
                )
            self.assertEqual(resp.status_code, 200)
            # Old session never wrapped → no recording.
            self.assertEqual(len(srv.recorded_calls), 0)


if __name__ == "__main__":
    unittest.main()
