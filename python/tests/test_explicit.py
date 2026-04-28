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


if __name__ == "__main__":
    unittest.main()
