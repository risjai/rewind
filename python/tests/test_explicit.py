"""Tests for the explicit recording API client."""

import asyncio
import json
import threading
import unittest
from http.server import HTTPServer, BaseHTTPRequestHandler

from rewind_agent.explicit import (
    ExplicitClient,
    _session_id,
    _timeline_id,
    _replay_context_id,
    _serialize_args,
    _serialize_result,
    _safe_json,
)


class MockRewindHandler(BaseHTTPRequestHandler):
    """Minimal mock of the Rewind explicit API for testing."""

    step_counter = 0
    sessions = {}
    replay_cursor = 0
    recorded_steps = []
    # Maps client_session_key -> (sid, tid). Mirrors the real server's
    # idempotent /sessions/start so tests can assert that a repeat
    # ensure_session for the same conversation_id reuses the session.
    sessions_by_client_key: dict = {}
    start_request_log: list = []

    def do_POST(self):
        content_length = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(content_length)) if content_length else {}

        if self.path == "/api/sessions/start":
            MockRewindHandler.start_request_log.append(body)
            client_key = body.get("client_session_key")
            if client_key and client_key in MockRewindHandler.sessions_by_client_key:
                sid, tid = MockRewindHandler.sessions_by_client_key[client_key]
                # Real server returns 200 (not 201) on dedup hit.
                self._respond(200, {"session_id": sid, "root_timeline_id": tid})
                return
            sid = f"test-session-{len(self.sessions)}"
            tid = f"test-timeline-{len(self.sessions)}"
            MockRewindHandler.sessions[sid] = {"timeline_id": tid}
            if client_key:
                MockRewindHandler.sessions_by_client_key[client_key] = (sid, tid)
            self._respond(201, {"session_id": sid, "root_timeline_id": tid})

        elif self.path.endswith("/end"):
            self._respond(200, {"session_id": self.path.split("/")[3]})

        elif self.path.endswith("/llm-calls") and "replay-lookup" not in self.path:
            MockRewindHandler.step_counter += 1
            MockRewindHandler.recorded_steps.append({
                "type": "llm_call",
                "model": body.get("model"),
                "request": body.get("request_body"),
                "response": body.get("response_body"),
            })
            self._respond(201, {"step_number": MockRewindHandler.step_counter})

        elif self.path.endswith("/tool-calls") and "replay-lookup" not in self.path:
            MockRewindHandler.step_counter += 1
            MockRewindHandler.recorded_steps.append({
                "type": "tool_call",
                "tool_name": body.get("tool_name"),
                "request": body.get("request_body"),
                "response": body.get("response_body"),
            })
            self._respond(201, {"step_number": MockRewindHandler.step_counter})

        elif "llm-calls/replay-lookup" in self.path:
            MockRewindHandler.replay_cursor += 1
            if MockRewindHandler.replay_cursor <= 2:
                self._respond(200, {
                    "hit": True,
                    "response_body": {"content": f"cached-{MockRewindHandler.replay_cursor}"},
                    "model": "gpt-4o",
                    "step_number": MockRewindHandler.replay_cursor,
                    "active_timeline_id": "tl-1",
                })
            else:
                self._respond(200, {"hit": False, "active_timeline_id": "tl-1"})

        elif "tool-calls/replay-lookup" in self.path:
            MockRewindHandler.replay_cursor += 1
            self._respond(200, {"hit": False})

        elif self.path == "/api/replay-contexts":
            self._respond(201, {
                "replay_context_id": "ctx-test-123",
                "parent_steps_count": 5,
                "fork_at_step": body.get("from_step", 0),
            })

        elif self.path.endswith("/fork"):
            self._respond(201, {"fork_timeline_id": "fork-tl-1"})

        else:
            self._respond(404, {"error": f"unknown path: {self.path}"})

    def do_GET(self):
        if "/timelines" in self.path:
            self._respond(200, [
                {"id": "tl-root", "parent_timeline_id": None, "session_id": "s1"},
            ])
        elif "/steps" in self.path:
            self._respond(200, [
                {"step_number": 1, "step_type": "llm_call", "model": "gpt-4o"},
                {"step_number": 2, "step_type": "tool_call", "tool_name": "get_pods"},
                {"step_number": 3, "step_type": "llm_call", "model": "gpt-4o"},
                {"step_number": 4, "step_type": "tool_call", "tool_name": "get_logs"},
                {"step_number": 5, "step_type": "llm_call", "model": "gpt-4o"},
            ])
        else:
            self._respond(404, {"error": "not found"})

    def do_DELETE(self):
        self._respond(200, {"released": True})

    def _respond(self, status, body):
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(body).encode())

    def log_message(self, format, *args):
        pass  # silence request logging


def _reset_mock():
    MockRewindHandler.step_counter = 0
    MockRewindHandler.sessions = {}
    MockRewindHandler.replay_cursor = 0
    MockRewindHandler.recorded_steps = []
    MockRewindHandler.sessions_by_client_key = {}
    MockRewindHandler.start_request_log = []


class TestExplicitClient(unittest.TestCase):
    """Tests with a real mock HTTP server."""

    @classmethod
    def setUpClass(cls):
        cls.server = HTTPServer(("127.0.0.1", 0), MockRewindHandler)
        cls.port = cls.server.server_address[1]
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()
        cls.client = ExplicitClient(f"http://127.0.0.1:{cls.port}")

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()

    def setUp(self):
        _reset_mock()
        _session_id.set(None)
        _timeline_id.set(None)
        _replay_context_id.set(None)

    def test_session_lifecycle(self):
        with self.client.session("test-agent"):
            sid = _session_id.get()
            self.assertIsNotNone(sid)
            self.assertTrue(sid.startswith("test-session-"))

        self.assertIsNone(_session_id.get(), "session_id should be cleared after context exit")

    def test_session_sets_timeline(self):
        with self.client.session("test"):
            tid = _timeline_id.get()
            self.assertIsNotNone(tid)
            self.assertTrue(tid.startswith("test-timeline-"))

    def test_record_llm_call(self):
        with self.client.session("test"):
            step = self.client.record_llm_call(
                {"messages": [{"role": "user", "content": "hi"}]},
                {"content": "hello"},
                model="gpt-4o",
                duration_ms=100,
            )
            self.assertEqual(step, 1)
            self.assertEqual(len(MockRewindHandler.recorded_steps), 1)
            self.assertEqual(MockRewindHandler.recorded_steps[0]["type"], "llm_call")

    def test_record_tool_call(self):
        with self.client.session("test"):
            step = self.client.record_tool_call(
                "get_pods",
                {"cluster": "mulesoft"},
                {"pods": [{"name": "head-0"}]},
                duration_ms=234,
            )
            self.assertEqual(step, 1)
            self.assertEqual(MockRewindHandler.recorded_steps[0]["type"], "tool_call")
            self.assertEqual(MockRewindHandler.recorded_steps[0]["tool_name"], "get_pods")

    def test_record_without_session_is_noop(self):
        result = self.client.record_llm_call({}, {}, model="x", duration_ms=0)
        self.assertIsNone(result, "recording without session should return None")
        self.assertEqual(len(MockRewindHandler.recorded_steps), 0)

    def test_replay_hit_and_miss(self):
        ctx = self.client.start_replay("test-session-0", timeline_id="tl-root")
        self.assertIsNotNone(ctx)
        _session_id.set("test-session-0")

        hit1 = self.client.get_replayed_response()
        self.assertIsNotNone(hit1)
        self.assertEqual(hit1["content"], "cached-1")

        hit2 = self.client.get_replayed_response()
        self.assertIsNotNone(hit2)
        self.assertEqual(hit2["content"], "cached-2")

        miss = self.client.get_replayed_response()
        self.assertIsNone(miss, "third lookup should miss")

        self.client.stop_replay()

    def test_replay_from_iteration(self):
        ctx = self.client.replay_from_iteration("test-session-0", 2)
        self.assertIsNotNone(ctx)
        self.assertEqual(ctx, "ctx-test-123")

    def test_replay_from_iteration_out_of_range(self):
        ctx = self.client.replay_from_iteration("test-session-0", 99)
        self.assertIsNone(ctx)

    def test_fork(self):
        fork_id = self.client.fork("test-session-0", at_step=2, label="experiment")
        self.assertEqual(fork_id, "fork-tl-1")

    def test_contextvars_isolation(self):
        """Verify sessions in different threads don't interfere."""
        results = {}

        def worker(name, idx):
            with self.client.session(name):
                results[idx] = _session_id.get()

        t1 = threading.Thread(target=worker, args=("agent-1", 1))
        t2 = threading.Thread(target=worker, args=("agent-2", 2))
        t1.start()
        t2.start()
        t1.join()
        t2.join()

        self.assertIsNotNone(results[1])
        self.assertIsNotNone(results[2])
        self.assertNotEqual(results[1], results[2], "sessions should have different IDs")
        self.assertIsNone(_session_id.get(), "main thread should not be affected")

    def test_cached_tool_sync(self):
        @self.client.cached_tool("add")
        def add(a: int, b: int) -> int:
            return a + b

        with self.client.session("test"):
            result = add(2, 3)
            self.assertEqual(result, 5)
            self.assertEqual(len(MockRewindHandler.recorded_steps), 1)
            self.assertEqual(MockRewindHandler.recorded_steps[0]["tool_name"], "add")

    def test_cached_tool_async(self):
        @self.client.cached_tool("async_add")
        async def async_add(a: int, b: int) -> int:
            return a + b

        async def run():
            async with self.client.session_async("test"):
                result = await async_add(2, 3)
                self.assertEqual(result, 5)

        asyncio.run(run())
        self.assertEqual(len(MockRewindHandler.recorded_steps), 1)
        self.assertEqual(MockRewindHandler.recorded_steps[0]["tool_name"], "async_add")

    def test_session_error_sends_errored_status(self):
        try:
            with self.client.session("test"):
                raise ValueError("boom")
        except ValueError:
            pass

        self.assertIsNone(_session_id.get(), "session should be cleaned up even on error")

    def test_server_unreachable_does_not_crash(self):
        bad_client = ExplicitClient("http://127.0.0.1:1")
        with bad_client.session("test"):
            result = bad_client.record_llm_call({}, {}, model="x", duration_ms=0)
            self.assertIsNone(result, "should return None, not crash")


class TestSerializationHelpers(unittest.TestCase):
    def test_safe_json_primitives(self):
        self.assertEqual(_safe_json(42), 42)
        self.assertEqual(_safe_json("hello"), "hello")
        self.assertIsNone(_safe_json(None))
        self.assertEqual(_safe_json(True), True)

    def test_safe_json_complex(self):
        self.assertEqual(_safe_json([1, {"a": 2}]), [1, {"a": 2}])
        self.assertEqual(_safe_json({"x": [1, 2]}), {"x": [1, 2]})

    def test_safe_json_non_serializable(self):
        result = _safe_json(object())
        self.assertIsInstance(result, str)

    def test_serialize_args(self):
        result = _serialize_args((1, "hello"), {"key": "val"})
        self.assertEqual(result["args"], [1, "hello"])
        self.assertEqual(result["kwargs"], {"key": "val"})

    def test_serialize_result(self):
        self.assertEqual(_serialize_result({"pods": []}), {"pods": []})
        self.assertEqual(_serialize_result("plain string"), "plain string")


class TestAsyncSession(unittest.TestCase):
    """Test async session management."""

    @classmethod
    def setUpClass(cls):
        cls.server = HTTPServer(("127.0.0.1", 0), MockRewindHandler)
        cls.port = cls.server.server_address[1]
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()
        cls.client = ExplicitClient(f"http://127.0.0.1:{cls.port}")

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()

    def setUp(self):
        _reset_mock()
        _session_id.set(None)

    def test_async_session_lifecycle(self):
        async def run():
            async with self.client.session_async("test-async"):
                sid = _session_id.get()
                self.assertIsNotNone(sid)

                step = await self.client.record_llm_call_async(
                    {}, {"content": "hi"}, model="gpt-4o", duration_ms=100
                )
                self.assertEqual(step, 1)

            self.assertIsNone(_session_id.get())

        asyncio.run(run())

    def test_async_record_tool_call(self):
        async def run():
            async with self.client.session_async("test"):
                step = await self.client.record_tool_call_async(
                    "search", {"q": "test"}, {"results": []}, duration_ms=50
                )
                self.assertEqual(step, 1)

        asyncio.run(run())

    def test_async_replay(self):
        async def run():
            _session_id.set("test-session-0")
            self.client.start_replay("test-session-0", timeline_id="tl-root")

            hit = await self.client.get_replayed_response_async()
            self.assertIsNotNone(hit)
            self.assertEqual(hit["content"], "cached-1")

            self.client.stop_replay()

        asyncio.run(run())


class TestEnsureSession(unittest.TestCase):
    """Tests for ensure_session (one session per conversation)."""

    @classmethod
    def setUpClass(cls):
        cls.server = HTTPServer(("127.0.0.1", 0), MockRewindHandler)
        cls.port = cls.server.server_address[1]
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()
        cls.client = ExplicitClient(f"http://127.0.0.1:{cls.port}")

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()

    def setUp(self):
        _reset_mock()
        _session_id.set(None)
        _timeline_id.set(None)
        self.client._session_cache.clear()

    def test_ensure_session_creates_on_first_call(self):
        self.client.ensure_session("conv-1", name="test-agent")
        sid = _session_id.get()
        self.assertIsNotNone(sid)
        self.assertIn("conv-1", self.client._session_cache)

    def test_ensure_session_sends_client_session_key(self):
        """SDK must pass conversation_id as client_session_key so the
        server can dedup across replicas. Without this header field,
        the multi-replica fix on the server is moot."""
        self.client.ensure_session("conv-key-1", name="test-agent")
        self.assertEqual(len(MockRewindHandler.start_request_log), 1)
        body = MockRewindHandler.start_request_log[0]
        self.assertEqual(
            body.get("client_session_key"),
            "conv-key-1",
            f"expected client_session_key='conv-key-1' in start request, got {body}",
        )

    def test_ensure_session_dedups_across_clients_via_server_key(self):
        """Two ExplicitClient instances (simulating two Ray Serve
        replicas) hitting the same server with the same conversation
        id must end up with the same session_id, even though their
        local caches are independent."""
        from rewind_agent.explicit import ExplicitClient
        client_b = ExplicitClient(self.client.base_url)

        self.client.ensure_session("conv-shared")
        sid_a = _session_id.get()

        # Simulate a second replica: its cache is empty, but the
        # server's idempotency on client_session_key returns the
        # session created by client A.
        _session_id.set(None)
        client_b.ensure_session("conv-shared")
        sid_b = _session_id.get()

        self.assertEqual(sid_a, sid_b, "both replicas must converge on the same session")
        self.assertEqual(
            len(MockRewindHandler.sessions),
            1,
            "server must have created exactly one session for the shared conversation_id",
        )

    def test_ensure_session_reuses_on_second_call(self):
        self.client.ensure_session("conv-1")
        sid1 = _session_id.get()

        _session_id.set(None)
        self.client.ensure_session("conv-1")
        sid2 = _session_id.get()

        self.assertEqual(sid1, sid2, "second call should reuse same session")
        self.assertEqual(len(MockRewindHandler.sessions), 1, "only one session created on server")

    def test_ensure_session_different_conversations(self):
        self.client.ensure_session("conv-1")
        sid1 = _session_id.get()

        self.client.ensure_session("conv-2")
        sid2 = _session_id.get()

        self.assertNotEqual(sid1, sid2, "different conversations should get different sessions")
        self.assertEqual(len(self.client._session_cache), 2)

    def test_ensure_session_sets_contextvars(self):
        self.client.ensure_session("conv-1")
        self.assertIsNotNone(_session_id.get())
        self.assertIsNotNone(_timeline_id.get())

    def test_clear_session_resets_contextvars(self):
        self.client.ensure_session("conv-1")
        self.assertIsNotNone(_session_id.get())

        self.client.clear_session()
        self.assertIsNone(_session_id.get())
        self.assertIsNone(_timeline_id.get())

    def test_cache_eviction(self):
        import rewind_agent.explicit as mod
        old_ttl = mod._SESSION_CACHE_TTL
        mod._SESSION_CACHE_TTL = 0  # expire immediately

        self.client.ensure_session("conv-old")
        self.assertIn("conv-old", self.client._session_cache)

        import time
        time.sleep(0.01)
        self.client.ensure_session("conv-new")
        self.assertNotIn("conv-old", self.client._session_cache, "stale entry should be evicted")
        self.assertIn("conv-new", self.client._session_cache)

        mod._SESSION_CACHE_TTL = old_ttl

    def test_ensure_session_then_record(self):
        self.client.ensure_session("conv-1")
        step = self.client.record_llm_call(
            {"msg": "hi"}, {"content": "hello"},
            model="gpt-4o", duration_ms=100,
        )
        self.assertEqual(step, 1)
        self.assertEqual(len(MockRewindHandler.recorded_steps), 1)


class TestExplicitClientBaseUrlResolution(unittest.TestCase):
    """Cover the __init__ resolution order: kwarg > $REWIND_URL > default."""

    def test_default_base_url(self):
        import os
        from unittest import mock

        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop("REWIND_URL", None)
            client = ExplicitClient()
            self.assertEqual(client.base_url, "http://127.0.0.1:4800")

    def test_env_var_used_when_kwarg_absent(self):
        import os
        from unittest import mock

        with mock.patch.dict(
            os.environ, {"REWIND_URL": "http://rewind.test:9999/"}, clear=False
        ):
            client = ExplicitClient()
            # Trailing slash gets stripped to match the existing convention.
            self.assertEqual(client.base_url, "http://rewind.test:9999")

    def test_kwarg_wins_over_env_var(self):
        import os
        from unittest import mock

        with mock.patch.dict(
            os.environ, {"REWIND_URL": "http://rewind.from-env:9999"}, clear=False
        ):
            client = ExplicitClient(base_url="http://rewind.from-kwarg:7777")
            self.assertEqual(client.base_url, "http://rewind.from-kwarg:7777")


class _StepAwareHandler(BaseHTTPRequestHandler):
    """Mock that serves per-step lookups via the existing list endpoint.

    Mirrors the real Rust server: GET /api/sessions/{id}/steps?timeline=…
    returns the full step list; with ``&include_blobs=1`` the response
    bodies include ``request_body`` and ``response_body``. Per-step
    fetch in commit 3 is built on top of this list endpoint (no new
    Rust route — Python-only filter by step_number).
    """

    fixture_steps: list[dict] = []
    # Test instrumentation: lets tests assert which paths were touched.
    paths_called: list[str] = []
    timelines_path_called: bool = False
    # Override the /timelines response for tests that exercise edge
    # cases (empty list / non-empty list with no root / etc.). None
    # = use the default 2-timeline fixture.
    timelines_override: list[dict] | None = None

    def do_GET(self):  # noqa: N802 — stdlib API
        _StepAwareHandler.paths_called.append(self.path)
        if "/timelines" in self.path:
            _StepAwareHandler.timelines_path_called = True
            if _StepAwareHandler.timelines_override is not None:
                self._respond(200, _StepAwareHandler.timelines_override)
                return
            self._respond(200, [
                {"id": "tl-root", "parent_timeline_id": None, "session_id": "s1"},
                {"id": "tl-fork", "parent_timeline_id": "tl-root", "session_id": "s1"},
            ])
            return
        if "/steps" in self.path:
            include_blobs = "include_blobs=1" in self.path
            steps = []
            for s in _StepAwareHandler.fixture_steps:
                copy = {
                    "step_number": s["step_number"],
                    "step_type": s["step_type"],
                    "model": s.get("model"),
                    "tool_name": s.get("tool_name"),
                }
                if include_blobs:
                    copy["request_body"] = s.get("request_body")
                    copy["response_body"] = s.get("response_body")
                steps.append(copy)
            self._respond(200, steps)
            return
        self._respond(404, {"error": "not found"})

    def _respond(self, status, body):
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(body).encode())

    def log_message(self, *_):
        pass


class TestGetStep(unittest.TestCase):
    """Tests for ExplicitClient.get_step / get_step_sync (Phase 0 commit 3).

    Replay handlers today reach into private SDK helpers or hit raw HTTP
    to fetch step content. A typed public helper closes that gap.
    """

    @classmethod
    def setUpClass(cls):
        cls.server = HTTPServer(("127.0.0.1", 0), _StepAwareHandler)
        cls.port = cls.server.server_address[1]
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()
        cls.client = ExplicitClient(f"http://127.0.0.1:{cls.port}")

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()

    def setUp(self):
        _StepAwareHandler.paths_called = []
        _StepAwareHandler.timelines_path_called = False
        _StepAwareHandler.timelines_override = None
        _StepAwareHandler.fixture_steps = [
            {
                "step_number": 1,
                "step_type": "llm_call",
                "model": "gpt-4o",
                "request_body": {"messages": [{"role": "user", "content": "hi"}]},
                "response_body": {"choices": [{"message": {"content": "hello"}}]},
            },
            {
                "step_number": 2,
                "step_type": "tool_call",
                "tool_name": "get_pods",
                "request_body": {"args": ["dev"]},
                "response_body": "pod-1\npod-2",
            },
            {
                "step_number": 3,
                "step_type": "llm_call",
                "model": "gpt-4o",
                "request_body": {"messages": [{"role": "user", "content": "next"}]},
                "response_body": {"choices": [{"message": {"content": "ok"}}]},
            },
        ]

    def test_returns_typed_step_response(self):
        from rewind_agent.explicit import StepResponse

        step = self.client.get_step_sync("s1", step_number=2)

        self.assertIsInstance(step, StepResponse)
        self.assertEqual(step.step_number, 2)
        self.assertEqual(step.step_type, "tool_call")
        self.assertEqual(step.request_body, {"args": ["dev"]})
        self.assertEqual(step.response_body, "pod-1\npod-2")

    def test_returns_llm_call_with_request_body(self):
        step = self.client.get_step_sync("s1", step_number=1)
        self.assertEqual(step.step_type, "llm_call")
        self.assertEqual(step.model, "gpt-4o")
        self.assertEqual(
            step.request_body,
            {"messages": [{"role": "user", "content": "hi"}]},
        )

    def test_unknown_step_raises_step_not_found(self):
        from rewind_agent.explicit import StepNotFoundError

        with self.assertRaises(StepNotFoundError):
            self.client.get_step_sync("s1", step_number=999)

    def test_explicit_timeline_id_passes_through(self):
        # When `timeline_id` is provided we must NOT auto-resolve via
        # /timelines — the caller's choice wins. Verify two things:
        # (1) the /timelines endpoint was NOT hit; (2) the caller-supplied
        # timeline_id reached the server in the /steps query string.
        step = self.client.get_step_sync("s1", timeline_id="tl-fork", step_number=1)
        self.assertEqual(step.step_number, 1)
        self.assertFalse(
            _StepAwareHandler.timelines_path_called,
            "auto-resolve via /timelines must be skipped when caller passes timeline_id",
        )
        steps_paths = [p for p in _StepAwareHandler.paths_called if "/steps" in p]
        self.assertEqual(len(steps_paths), 1, f"expected one /steps call, got {steps_paths}")
        self.assertIn(
            "timeline=tl-fork",
            steps_paths[0],
            f"caller-supplied timeline_id missing from server URL: {steps_paths[0]}",
        )

    def test_default_timeline_resolution_hits_timelines_endpoint(self):
        # Counterpart to the above: when timeline_id is omitted we DO hit
        # /timelines once to find the root timeline. Locks the contract so
        # a refactor that loses auto-resolve is caught.
        self.client.get_step_sync("s1", step_number=1)
        self.assertTrue(
            _StepAwareHandler.timelines_path_called,
            "auto-resolve via /timelines must run when caller omits timeline_id",
        )
        steps_paths = [p for p in _StepAwareHandler.paths_called if "/steps" in p]
        self.assertEqual(len(steps_paths), 1)
        # Auto-resolved root timeline reached the server URL.
        self.assertIn("timeline=tl-root", steps_paths[0])

    def test_async_get_step(self):
        from rewind_agent.explicit import StepResponse

        async def run():
            step = await self.client.get_step("s1", step_number=3)
            self.assertIsInstance(step, StepResponse)
            self.assertEqual(step.step_number, 3)
            self.assertEqual(step.step_type, "llm_call")

        asyncio.run(run())

    def test_step_response_is_immutable(self):
        import dataclasses
        from rewind_agent.explicit import StepResponse

        step = self.client.get_step_sync("s1", step_number=1)
        # Frozen dataclass: assignment raises FrozenInstanceError specifically.
        # Don't use bare Exception here — that would let unrelated regressions
        # (e.g. AttributeError because StepResponse stops being a dataclass)
        # mask the actual contract.
        with self.assertRaises(dataclasses.FrozenInstanceError):
            step.step_number = 999  # type: ignore[misc]
        # And StepResponse is the public type used in __all__.
        self.assertTrue(hasattr(rewind_agent_module, "StepResponse"))

    def test_package_root_re_exports(self):
        import rewind_agent
        self.assertTrue(hasattr(rewind_agent, "StepResponse"))
        self.assertTrue(hasattr(rewind_agent, "StepNotFoundError"))
        self.assertTrue(hasattr(rewind_agent, "RewindServerError"))

    def test_empty_timelines_raises_step_not_found(self):
        """An empty timelines list = unknown / freshly-empty session.
        That's true 'absence', not server data inconsistency, so it raises
        StepNotFoundError (NOT RewindServerError)."""
        from rewind_agent.explicit import StepNotFoundError

        _StepAwareHandler.timelines_override = []
        with self.assertRaises(StepNotFoundError):
            self.client.get_step_sync("s1", step_number=1)

    def test_non_empty_timelines_with_no_root_raises_server_error(self):
        """Non-empty list with NO entry having parent_timeline_id=None is
        server data inconsistency. Per round-3 fix, this raises
        RewindServerError so callers can distinguish it from real
        absences and decide to retry / log / page."""
        from rewind_agent.explicit import RewindServerError

        _StepAwareHandler.timelines_override = [
            {"id": "tl-orphan-1", "parent_timeline_id": "tl-missing", "session_id": "s1"},
            {"id": "tl-orphan-2", "parent_timeline_id": "tl-missing", "session_id": "s1"},
        ]
        with self.assertRaises(RewindServerError):
            self.client.get_step_sync("s1", step_number=1)

    def test_session_id_is_url_quoted(self):
        """Round-3 fix: session_id, like timeline_id, is URL-quoted on
        the way out so reserved characters in opaque IDs don't break
        the URL or open a path-traversal-shaped surface."""
        # Use a session id that contains URL-reserved characters. The
        # mock handler ignores the session id for routing — it serves
        # the same fixture regardless — so the test verifies what hit
        # the wire, not server-side behavior.
        weird_sid = "s/with?reserved#chars"
        try:
            self.client.get_step_sync(weird_sid, step_number=1)
        except Exception:
            pass  # The mock 404s for unrecognized paths; we only care about the URL.

        # Both /timelines and /steps requests should have the session_id
        # percent-encoded.
        self.assertTrue(
            _StepAwareHandler.paths_called,
            "expected at least one GET to reach the server",
        )
        for p in _StepAwareHandler.paths_called:
            self.assertNotIn(
                "s/with",
                p,
                f"raw session_id leaked into URL: {p}",
            )
            self.assertIn(
                "s%2Fwith%3Freserved%23chars",
                p,
                f"session_id was not properly URL-quoted in: {p}",
            )


class TestGetStepServerErrors(unittest.TestCase):
    """Round-2 santa-review fix: get_step distinguishes 'step absent'
    (StepNotFoundError) from 'transport / server failure'
    (RewindServerError). Replay handlers can decide whether to retry
    transient infra failures vs treat the step as genuinely missing."""

    def test_unreachable_server_raises_rewind_server_error(self):
        from rewind_agent.explicit import (
            ExplicitClient,
            RewindServerError,
            StepNotFoundError,
        )

        # Port 1 is reserved/closed on standard hosts — guaranteed
        # connection refused. The exception type we want is
        # RewindServerError, NOT StepNotFoundError.
        client = ExplicitClient("http://127.0.0.1:1")
        with self.assertRaises(RewindServerError):
            client.get_step_sync("any-session", step_number=1)

        # Defensive: verify it's NOT StepNotFoundError (which would let
        # callers silently swallow infra failures).
        try:
            client.get_step_sync("any-session", step_number=1)
        except StepNotFoundError:
            self.fail(
                "get_step_sync masked transport failure as StepNotFoundError"
            )
        except RewindServerError:
            pass


class TestModuleCachedToolStableWrapper(unittest.TestCase):
    """Round-2 santa-review fix: module-level cached_tool used to call
    `client.cached_tool(name)(func)` on EVERY invocation, allocating a
    fresh wrapper per call. After the fix, the inner wrapper is built
    once per (client, func) pair and reused. Locks the contract so a
    refactor that loses the cache is caught."""

    def setUp(self):
        from rewind_agent import explicit as mod
        mod.set_default_client(None)
        # Also clear the wrapper cache so test order doesn't matter.
        mod._module_cached_tool_wrappers.clear()
        _session_id.set(None)
        _timeline_id.set(None)
        _replay_context_id.set(None)

    def tearDown(self):
        from rewind_agent import explicit as mod
        mod.set_default_client(None)

    def test_call_path_uses_cached_wrapper_not_per_call_rebuild(self):
        """Genuinely lock the perf claim: instrument ExplicitClient.cached_tool
        to count invocations and assert it ran exactly ONCE across N calls
        of the decorated function. A regression that reverts sync_wrapper /
        async_wrapper to `client.cached_tool(name)(func)(*args)` per call
        would show N invocations instead of 1."""
        from rewind_agent.explicit import (
            ExplicitClient,
            cached_tool as module_cached_tool,
            set_default_client,
        )

        @module_cached_tool("count_check")
        def add(a: int, b: int) -> int:
            return a + b

        server = HTTPServer(("127.0.0.1", 0), MockRewindHandler)
        port = server.server_address[1]
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            client = ExplicitClient(f"http://127.0.0.1:{port}")
            set_default_client(client)

            # Wrap the real cached_tool method to count invocations.
            real_cached_tool = client.cached_tool
            invocations = {"count": 0}

            def counting_cached_tool(name):
                invocations["count"] += 1
                return real_cached_tool(name)

            client.cached_tool = counting_cached_tool  # type: ignore[method-assign]

            with client.session("call-path-cache-test"):
                # Five calls of the decorated function.
                for i in range(5):
                    self.assertEqual(add(i, i), 2 * i)

            # If sync_wrapper rebuilds the wrapper each call,
            # invocations["count"] would be 5. With caching, exactly 1.
            self.assertEqual(
                invocations["count"],
                1,
                f"client.cached_tool was invoked {invocations['count']} times "
                f"across 5 add() calls; module-level cached_tool must build "
                f"the wrapper once per (client, func) pair and reuse it.",
            )
        finally:
            server.shutdown()


# Module reference used by TestGetStep.test_step_response_is_immutable
import rewind_agent as rewind_agent_module  # noqa: E402


class TestDefaultClient(unittest.TestCase):
    """Tests for module-level default-client discovery (Phase 0 commit 2).

    The blessed singleton lets wrapper libraries (like the planned sf-rewind)
    bind a recording client at app startup and have a module-level
    `cached_tool` decorator find it at call time, without re-implementing
    the module-global pattern in every consumer.
    """

    def setUp(self):
        # The default-client module attr is process-global. Reset before each
        # test so leakage is impossible.
        from rewind_agent import explicit as mod
        mod.set_default_client(None)
        _session_id.set(None)
        _timeline_id.set(None)
        _replay_context_id.set(None)

    def tearDown(self):
        from rewind_agent import explicit as mod
        mod.set_default_client(None)

    def test_get_returns_none_when_unset(self):
        from rewind_agent.explicit import get_default_client
        self.assertIsNone(get_default_client())

    def test_set_and_get_round_trip(self):
        from rewind_agent.explicit import (
            get_default_client,
            set_default_client,
        )
        client = ExplicitClient("http://127.0.0.1:1")
        set_default_client(client)
        self.assertIs(get_default_client(), client)

    def test_set_none_clears(self):
        from rewind_agent.explicit import (
            get_default_client,
            set_default_client,
        )
        client = ExplicitClient("http://127.0.0.1:1")
        set_default_client(client)
        set_default_client(None)
        self.assertIsNone(get_default_client())

    def test_module_cached_tool_records_when_default_client_set(self):
        """`@cached_tool` at module level decorates at import time but
        resolves the active client at *call* time. This is the load-bearing
        contract for sf-rewind's tools.py pattern."""
        # Decorate BEFORE setting the default client to lock the lazy-resolve
        # contract.
        from rewind_agent.explicit import cached_tool as module_cached_tool

        @module_cached_tool("noop_add")
        def add(a: int, b: int) -> int:
            return a + b

        server = HTTPServer(("127.0.0.1", 0), MockRewindHandler)
        port = server.server_address[1]
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            from rewind_agent.explicit import set_default_client
            client = ExplicitClient(f"http://127.0.0.1:{port}")
            set_default_client(client)
            with client.session("default-client-test"):
                result = add(2, 3)
                self.assertEqual(result, 5)
            # Recording happened against the bound default client.
            self.assertTrue(
                any(s.get("tool_name") == "noop_add" for s in MockRewindHandler.recorded_steps),
                f"expected a noop_add tool_call step, got {MockRewindHandler.recorded_steps}",
            )
        finally:
            server.shutdown()

    def test_module_cached_tool_runs_unrecorded_when_no_default_client(self):
        """If `@cached_tool` is decorated and called before
        `set_default_client(...)` is invoked, the function still runs — it
        just doesn't record. This keeps imports safe at module load."""
        from rewind_agent.explicit import cached_tool as module_cached_tool

        @module_cached_tool("unbound_add")
        def add(a: int, b: int) -> int:
            return a + b

        # No default client set; no session active.
        result = add(2, 3)
        self.assertEqual(result, 5)

    def test_module_cached_tool_async(self):
        from rewind_agent.explicit import cached_tool as module_cached_tool

        @module_cached_tool("noop_aadd")
        async def aadd(a: int, b: int) -> int:
            return a + b

        server = HTTPServer(("127.0.0.1", 0), MockRewindHandler)
        port = server.server_address[1]
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()

        async def run():
            from rewind_agent.explicit import set_default_client
            client = ExplicitClient(f"http://127.0.0.1:{port}")
            set_default_client(client)
            async with client.session_async("default-client-async-test"):
                result = await aadd(2, 3)
                self.assertEqual(result, 5)

        try:
            asyncio.run(run())
            self.assertTrue(
                any(s.get("tool_name") == "noop_aadd" for s in MockRewindHandler.recorded_steps),
                f"expected a noop_aadd tool_call step, got {MockRewindHandler.recorded_steps}",
            )
        finally:
            server.shutdown()

    def test_setup_binds_default_client_inside_block(self):
        """connector.setup() binds the default client on entry and restores
        the previous value on exit. Stack semantics, not full ContextVar
        isolation — accepted trade-off documented in the design plan."""
        from rewind_agent import connector
        from rewind_agent.explicit import (
            get_default_client,
            set_default_client,
        )

        server = HTTPServer(("127.0.0.1", 0), MockRewindHandler)
        port = server.server_address[1]
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        base_url = f"http://127.0.0.1:{port}"

        # Stack semantics: inner block restores outer client on exit.
        outer = ExplicitClient(base_url)
        set_default_client(outer)
        self.assertIs(get_default_client(), outer)

        try:
            with connector.setup(name="bind-test", base_url=base_url) as inner:
                # Inside the block, the default client is the connector's
                # client, NOT the outer one we set manually.
                self.assertIsNotNone(inner)
                self.assertIs(get_default_client(), inner)
            # On exit, the outer client is restored.
            self.assertIs(get_default_client(), outer)
        finally:
            server.shutdown()

    def test_set_default_client_rejects_wrong_type(self):
        """Cheap type check at the API boundary. Catches the common typo of
        passing a string URL instead of a constructed client."""
        from rewind_agent.explicit import set_default_client
        with self.assertRaises(TypeError):
            set_default_client("http://127.0.0.1:1")  # type: ignore[arg-type]

    def test_package_root_re_exports(self):
        import rewind_agent
        self.assertTrue(hasattr(rewind_agent, "set_default_client"))
        self.assertTrue(hasattr(rewind_agent, "get_default_client"))
        self.assertTrue(hasattr(rewind_agent, "cached_tool"))


if __name__ == "__main__":
    unittest.main()
