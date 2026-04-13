"""Tests for the recorder — uses mock SDK objects, no real API calls."""

import json
import os
import sqlite3
import tempfile
import threading
import unittest
from unittest.mock import MagicMock

from rewind_agent.store import Store
from rewind_agent.recorder import (
    Recorder,
    _extract_openai_usage,
    _extract_anthropic_usage,
    _OpenAIStreamWrapper,
    estimate_cost,
    _estimate_cost_from_steps,
)


class TestUsageExtraction(unittest.TestCase):
    def test_openai_usage(self):
        resp = {"usage": {"prompt_tokens": 100, "completion_tokens": 50}}
        self.assertEqual(_extract_openai_usage(resp), (100, 50))

    def test_anthropic_usage(self):
        resp = {"usage": {"input_tokens": 200, "output_tokens": 75}}
        self.assertEqual(_extract_anthropic_usage(resp), (200, 75))

    def test_missing_usage(self):
        self.assertEqual(_extract_openai_usage({}), (0, 0))
        self.assertEqual(_extract_anthropic_usage({}), (0, 0))


class TestRecorderNonStreaming(unittest.TestCase):
    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.store = Store(root=self.tmpdir)
        sid, tid = self.store.create_session("test")
        self.session_id = sid
        self.timeline_id = tid
        self.recorder = Recorder(self.store, sid, tid)

    def tearDown(self):
        self.store.close()

    def test_record_call_creates_step(self):
        self.recorder._record_call(
            model="gpt-4o",
            request_data={"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]},
            response_data={
                "choices": [{"message": {"role": "assistant", "content": "hello"}}],
                "usage": {"prompt_tokens": 10, "completion_tokens": 5},
            },
            duration_ms=250,
            provider="openai",
        )

        conn = sqlite3.connect(os.path.join(self.tmpdir, "rewind.db"))
        row = conn.execute(
            "SELECT model, tokens_in, tokens_out, status, step_number FROM steps WHERE session_id = ?",
            (self.session_id,),
        ).fetchone()
        self.assertEqual(row[0], "gpt-4o")
        self.assertEqual(row[1], 10)
        self.assertEqual(row[2], 5)
        self.assertEqual(row[3], "success")
        self.assertEqual(row[4], 1)
        conn.close()

    def test_record_call_error(self):
        self.recorder._record_call(
            model="gpt-4o",
            request_data={"model": "gpt-4o"},
            response_data=None,
            duration_ms=100,
            error="Connection timeout",
            provider="openai",
        )

        conn = sqlite3.connect(os.path.join(self.tmpdir, "rewind.db"))
        row = conn.execute(
            "SELECT status, error FROM steps WHERE session_id = ?",
            (self.session_id,),
        ).fetchone()
        self.assertEqual(row[0], "error")
        self.assertEqual(row[1], "Connection timeout")
        conn.close()

    def test_step_counter_increments(self):
        for i in range(5):
            self.recorder._record_call(
                model="gpt-4o",
                request_data={"call": i},
                response_data={"choices": [], "usage": {"prompt_tokens": 1, "completion_tokens": 1}},
                duration_ms=10,
                provider="openai",
            )

        conn = sqlite3.connect(os.path.join(self.tmpdir, "rewind.db"))
        rows = conn.execute(
            "SELECT step_number FROM steps WHERE session_id = ? ORDER BY step_number",
            (self.session_id,),
        ).fetchall()
        self.assertEqual([r[0] for r in rows], [1, 2, 3, 4, 5])
        conn.close()

    def test_session_stats_updated(self):
        self.recorder._record_call(
            model="gpt-4o",
            request_data={"model": "gpt-4o"},
            response_data={"choices": [], "usage": {"prompt_tokens": 100, "completion_tokens": 50}},
            duration_ms=300,
            provider="openai",
        )

        conn = sqlite3.connect(os.path.join(self.tmpdir, "rewind.db"))
        row = conn.execute(
            "SELECT total_steps, total_tokens FROM sessions WHERE id = ?",
            (self.session_id,),
        ).fetchone()
        self.assertEqual(row[0], 1)
        self.assertEqual(row[1], 150)
        conn.close()

    def test_anthropic_recording(self):
        self.recorder._record_call(
            model="claude-3-5-sonnet-20241022",
            request_data={"model": "claude-3-5-sonnet-20241022", "messages": []},
            response_data={
                "content": [{"type": "text", "text": "hello"}],
                "usage": {"input_tokens": 20, "output_tokens": 10},
            },
            duration_ms=400,
            provider="anthropic",
        )

        conn = sqlite3.connect(os.path.join(self.tmpdir, "rewind.db"))
        row = conn.execute(
            "SELECT model, tokens_in, tokens_out FROM steps WHERE session_id = ?",
            (self.session_id,),
        ).fetchone()
        self.assertEqual(row[0], "claude-3-5-sonnet-20241022")
        self.assertEqual(row[1], 20)
        self.assertEqual(row[2], 10)
        conn.close()

    def test_recording_failure_does_not_raise(self):
        """If the store is broken, _record_call should log but not raise."""
        self.store.close()  # Break the store
        # This should NOT raise
        self.recorder._record_call(
            model="gpt-4o",
            request_data={},
            response_data={},
            duration_ms=0,
            provider="openai",
        )

    def test_concurrent_recording(self):
        """Multiple threads recording simultaneously should not corrupt data."""
        errors = []

        def record(n):
            try:
                self.recorder._record_call(
                    model="gpt-4o",
                    request_data={"call": n},
                    response_data={"choices": [], "usage": {"prompt_tokens": 1, "completion_tokens": 1}},
                    duration_ms=1,
                    provider="openai",
                )
            except Exception as e:
                errors.append(str(e))

        threads = [threading.Thread(target=record, args=(i,)) for i in range(50)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        self.assertEqual(len(errors), 0, f"Errors: {errors}")

        conn = sqlite3.connect(os.path.join(self.tmpdir, "rewind.db"))
        count = conn.execute("SELECT COUNT(*) FROM steps WHERE session_id = ?", (self.session_id,)).fetchone()[0]
        self.assertEqual(count, 50)

        # Step numbers should be unique
        rows = conn.execute(
            "SELECT step_number FROM steps WHERE session_id = ? ORDER BY step_number",
            (self.session_id,),
        ).fetchall()
        step_nums = [r[0] for r in rows]
        self.assertEqual(len(set(step_nums)), 50)
        conn.close()


class TestStreamWrapper(unittest.TestCase):
    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.store = Store(root=self.tmpdir)
        sid, tid = self.store.create_session("stream-test")
        self.recorder = Recorder(self.store, sid, tid)
        self.session_id = sid

    def tearDown(self):
        self.store.close()

    def test_openai_stream_wrapper_records_on_completion(self):
        """Stream wrapper should record a step after all chunks are consumed."""
        # Create mock chunks
        chunks = []
        for text in ["Hello", " world", "!"]:
            chunk = MagicMock()
            chunk.model_dump.return_value = {
                "choices": [{"delta": {"content": text}, "finish_reason": None}],
            }
            chunks.append(chunk)

        # Final chunk with usage
        final = MagicMock()
        final.model_dump.return_value = {
            "choices": [{"delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 20, "completion_tokens": 10},
        }
        chunks.append(final)

        stream = iter(chunks)
        import time
        wrapper = _OpenAIStreamWrapper(stream, self.recorder, "gpt-4o", {"model": "gpt-4o"}, time.perf_counter())

        # Consume the stream
        collected = list(wrapper)
        self.assertEqual(len(collected), 4)

        # Verify step was recorded
        conn = sqlite3.connect(os.path.join(self.tmpdir, "rewind.db"))
        row = conn.execute(
            "SELECT model, tokens_in, tokens_out, status FROM steps WHERE session_id = ?",
            (self.session_id,),
        ).fetchone()
        self.assertIsNotNone(row, "Stream wrapper should have recorded a step")
        self.assertEqual(row[0], "gpt-4o")
        self.assertEqual(row[1], 20)
        self.assertEqual(row[2], 10)
        self.assertEqual(row[3], "success")

        # Verify response blob has accumulated content
        resp_hash = conn.execute(
            "SELECT response_blob FROM steps WHERE session_id = ?",
            (self.session_id,),
        ).fetchone()[0]
        resp = json.loads(self.store.blobs.get(resp_hash))
        self.assertEqual(resp["choices"][0]["message"]["content"], "Hello world!")
        conn.close()


class TestReplayCached(unittest.TestCase):
    """Bug 2 fix: _try_replay_cached should NOT create step records."""

    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.store = Store(root=self.tmpdir)
        sid, tid = self.store.create_session("replay-test")
        self.session_id = sid
        self.timeline_id = tid

        # Record 3 steps on the main timeline
        for i in range(1, 4):
            req = self.store.blobs.put_json({"call": i})
            resp = self.store.blobs.put_json({"result": f"response-{i}"})
            self.store.create_step(
                session_id=sid, timeline_id=tid, step_number=i,
                step_type="llm_call", status="success", model="gpt-4o",
                duration_ms=100, tokens_in=10, tokens_out=5,
                request_blob=req, response_blob=resp, error=None,
            )

        parent_steps = self.store.get_steps(tid)
        # Create fork timeline
        fork_tid = self.store.create_fork_timeline(sid, tid, 2, "replayed")
        self.fork_tid = fork_tid

        self.recorder = Recorder(
            self.store, sid, fork_tid,
            replay_steps=parent_steps, fork_at_step=2,
        )

    def tearDown(self):
        self.store.close()

    def test_cached_replay_does_not_create_steps(self):
        """Cached replay should return data but not create step records."""
        result = self.recorder._try_replay_cached("openai")
        self.assertIsNotNone(result, "Should return cached response")
        self.assertEqual(result["result"], "response-1")

        conn = sqlite3.connect(os.path.join(self.tmpdir, "rewind.db"))
        fork_steps = conn.execute(
            "SELECT count(*) FROM steps WHERE timeline_id = ?",
            (self.fork_tid,),
        ).fetchone()[0]
        self.assertEqual(fork_steps, 0, "No steps should be created on the fork timeline for cached replays")
        conn.close()

    def test_cached_replay_advances_counter(self):
        """Step counter should advance even without creating records."""
        self.assertEqual(self.recorder._step_counter, 0)
        self.recorder._try_replay_cached("openai")
        self.assertEqual(self.recorder._step_counter, 1)
        self.recorder._try_replay_cached("openai")
        self.assertEqual(self.recorder._step_counter, 2)

    def test_live_step_after_cache_gets_correct_number(self):
        """After 2 cached replays, the next live step should be step 3."""
        self.recorder._try_replay_cached("openai")
        self.recorder._try_replay_cached("openai")

        # Beyond fork point — returns None
        self.assertIsNone(self.recorder._try_replay_cached("openai"))

        # Record a live step
        self.recorder._record_call(
            model="gpt-4o", request_data={"live": True},
            response_data={"choices": [], "usage": {"prompt_tokens": 5, "completion_tokens": 3}},
            duration_ms=50, provider="openai",
        )

        conn = sqlite3.connect(os.path.join(self.tmpdir, "rewind.db"))
        row = conn.execute(
            "SELECT step_number FROM steps WHERE timeline_id = ?",
            (self.fork_tid,),
        ).fetchone()
        self.assertEqual(row[0], 3, "Live step after 2 cached should be step 3")
        conn.close()


class TestSpanIdAssignment(unittest.TestCase):
    """Bug 3 fix: _record_call should assign span_id from the ContextVar."""

    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.store = Store(root=self.tmpdir)
        sid, tid = self.store.create_session("span-test")
        self.session_id = sid
        self.timeline_id = tid
        self.recorder = Recorder(self.store, sid, tid)

    def tearDown(self):
        self.store.close()

    def test_record_call_picks_up_span_id(self):
        """When _current_span_id is set, _record_call should use it."""
        from rewind_agent.hooks import _current_span_id

        span_id = self.store.create_span(
            session_id=self.session_id,
            timeline_id=self.timeline_id,
            span_type="agent",
            name="test-agent",
        )

        token = _current_span_id.set(span_id)
        try:
            self.recorder._record_call(
                model="gpt-4o",
                request_data={"model": "gpt-4o"},
                response_data={"choices": [], "usage": {"prompt_tokens": 5, "completion_tokens": 3}},
                duration_ms=100,
                provider="openai",
            )
        finally:
            _current_span_id.reset(token)

        conn = sqlite3.connect(os.path.join(self.tmpdir, "rewind.db"))
        row = conn.execute(
            "SELECT span_id FROM steps WHERE session_id = ?",
            (self.session_id,),
        ).fetchone()
        self.assertEqual(row[0], span_id, "Step should have the span_id from ContextVar")
        conn.close()

    def test_record_call_without_span_has_null(self):
        """When no span is active, span_id should be NULL."""
        self.recorder._record_call(
            model="gpt-4o",
            request_data={"model": "gpt-4o"},
            response_data={"choices": [], "usage": {"prompt_tokens": 5, "completion_tokens": 3}},
            duration_ms=100,
            provider="openai",
        )

        conn = sqlite3.connect(os.path.join(self.tmpdir, "rewind.db"))
        row = conn.execute(
            "SELECT span_id FROM steps WHERE session_id = ?",
            (self.session_id,),
        ).fetchone()
        self.assertIsNone(row[0], "Step without active span should have NULL span_id")
        conn.close()


class TestMonkeyPatching(unittest.TestCase):
    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.store = Store(root=self.tmpdir)
        sid, tid = self.store.create_session("patch-test")
        self.recorder = Recorder(self.store, sid, tid)

    def tearDown(self):
        self.recorder.unpatch_all()
        self.store.close()

    def test_unpatch_restores_originals(self):
        """unpatch_all should restore original methods."""
        try:
            from openai.resources.chat.completions import Completions
            original = Completions.create
            self.recorder._patch_openai_sync()
            self.assertNotEqual(Completions.create, original)
            self.recorder.unpatch_all()
            self.assertEqual(Completions.create, original)
        except ImportError:
            self.skipTest("openai not installed")

    def test_anthropic_stream_patched(self):
        """Bug 4 fix: Messages.stream should be patched alongside Messages.create."""
        try:
            from anthropic.resources.messages import Messages
        except ImportError:
            self.skipTest("anthropic not installed")

        if not hasattr(Messages, "stream"):
            self.skipTest("Messages.stream not available")

        original = Messages.stream
        self.recorder.patch_all()
        self.assertIn("anthropic_stream_sync", self.recorder._originals)
        self.assertNotEqual(Messages.stream, original, "stream() should be patched")
        self.recorder.unpatch_all()
        self.assertEqual(Messages.stream, original, "stream() should be unpatched")

    def test_openai_patches_preserved_with_agents_sdk(self):
        """Bug 1 fix: OpenAI patches should NOT be removed when Agents SDK is installed."""
        try:
            from openai.resources.chat.completions import Completions
        except ImportError:
            self.skipTest("openai not installed")

        original = Completions.create
        self.recorder.patch_all()
        self.assertIn("openai_sync", self.recorder._originals)
        self.assertIn("openai_async", self.recorder._originals)
        self.assertNotEqual(Completions.create, original, "Patches should be active on the class")


class TestPricing(unittest.TestCase):
    def test_known_model_gpt4o(self):
        cost = estimate_cost("gpt-4o", 1_000_000, 1_000_000)
        self.assertAlmostEqual(cost, 12.50, places=2)

    def test_known_model_gpt4o_mini(self):
        cost = estimate_cost("gpt-4o-mini", 1_000_000, 1_000_000)
        self.assertAlmostEqual(cost, 0.75, places=2)

    def test_known_model_claude_sonnet(self):
        cost = estimate_cost("claude-sonnet-4-20250514", 1_000_000, 1_000_000)
        self.assertAlmostEqual(cost, 18.00, places=2)

    def test_unknown_model_default(self):
        cost = estimate_cost("unknown-model", 1_000_000, 1_000_000)
        self.assertAlmostEqual(cost, 4.00, places=2)

    def test_zero_tokens(self):
        self.assertEqual(estimate_cost("gpt-4o", 0, 0), 0.0)

    def test_case_insensitive(self):
        cost = estimate_cost("GPT-4o", 1_000_000, 0)
        self.assertAlmostEqual(cost, 2.50, places=2)


class TestEstimateCostFromSteps(unittest.TestCase):
    def test_cached_steps_only(self):
        steps = [
            {"step_number": 1, "model": "gpt-4o", "tokens_in": 500, "tokens_out": 200},
            {"step_number": 2, "model": "gpt-4o", "tokens_in": 300, "tokens_out": 100},
            {"step_number": 3, "model": "gpt-4o", "tokens_in": 400, "tokens_out": 150},
        ]
        cost = _estimate_cost_from_steps(steps, fork_at_step=2)
        # Only steps 1 and 2 are cached
        expected = estimate_cost("gpt-4o", 500, 200) + estimate_cost("gpt-4o", 300, 100)
        self.assertAlmostEqual(cost, round(expected, 2), places=2)

    def test_no_steps(self):
        self.assertEqual(_estimate_cost_from_steps([], 5), 0.0)


class TestReplaySavingsTracking(unittest.TestCase):
    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.store = Store(root=self.tmpdir)
        sid, tid = self.store.create_session("test-replay")
        self.session_id = sid
        self.timeline_id = tid

        # Create parent steps with blob data
        self.parent_steps = []
        for i in range(1, 4):
            resp_data = json.dumps({"choices": [{"message": {"content": f"response {i}"}}]})
            blob_hash = self.store.blobs.put(resp_data.encode())
            step = {
                "step_number": i,
                "model": "gpt-4o",
                "tokens_in": 100 * i,
                "tokens_out": 50 * i,
                "duration_ms": 500 * i,
                "response_blob": blob_hash,
            }
            self.parent_steps.append(step)

    def test_counters_increment_on_cache_hit(self):
        recorder = Recorder(
            self.store, self.session_id, self.timeline_id,
            replay_steps=self.parent_steps, fork_at_step=2,
        )
        # First cache hit
        result = recorder._try_replay_cached("openai")
        self.assertIsNotNone(result)
        self.assertEqual(recorder._cached_steps_count, 1)
        self.assertEqual(recorder._cached_tokens, 100 + 50)  # step 1
        self.assertEqual(recorder._cached_duration_ms, 500)

        # Second cache hit
        result = recorder._try_replay_cached("openai")
        self.assertIsNotNone(result)
        self.assertEqual(recorder._cached_steps_count, 2)
        self.assertEqual(recorder._cached_tokens, (100 + 50) + (200 + 100))
        self.assertEqual(recorder._cached_duration_ms, 500 + 1000)

        # Third call is beyond fork point — live
        result = recorder._try_replay_cached("openai")
        self.assertIsNone(result)
        # Counters unchanged
        self.assertEqual(recorder._cached_steps_count, 2)

    def test_no_replay_mode_no_tracking(self):
        recorder = Recorder(self.store, self.session_id, self.timeline_id)
        result = recorder._try_replay_cached("openai")
        self.assertIsNone(result)
        self.assertEqual(recorder._cached_steps_count, 0)


if __name__ == "__main__":
    unittest.main()
