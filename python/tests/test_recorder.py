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


if __name__ == "__main__":
    unittest.main()
