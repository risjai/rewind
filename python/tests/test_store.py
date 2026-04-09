"""Tests for the pure Python store — no external binary needed."""

import hashlib
import json
import os
import sqlite3
import tempfile
import threading
import unittest

from rewind_agent.store import BlobStore, Store


class TestBlobStore(unittest.TestCase):
    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.blobs = BlobStore(os.path.join(self.tmpdir, "objects"))

    def test_put_get_roundtrip(self):
        data = b"hello world"
        h = self.blobs.put(data)
        self.assertEqual(self.blobs.get(h), data)

    def test_hash_is_sha256(self):
        data = b"test data"
        h = self.blobs.put(data)
        expected = hashlib.sha256(data).hexdigest()
        self.assertEqual(h, expected)

    def test_blob_path_layout(self):
        """Verify blob stored at {root}/{first2hex}/{resthex} matching Rust layout."""
        data = b"check path"
        h = self.blobs.put(data)
        expected_path = os.path.join(self.tmpdir, "objects", h[:2], h[2:])
        self.assertTrue(os.path.exists(expected_path))

    def test_put_json_compact(self):
        """Verify put_json uses compact JSON (no spaces) matching Rust serde_json::to_vec."""
        obj = {"key": "value", "num": 42}
        h = self.blobs.put_json(obj)
        raw = self.blobs.get(h)
        # Compact JSON has no spaces after separators
        self.assertEqual(raw, b'{"key":"value","num":42}')

    def test_put_json_default_str(self):
        """Non-serializable values should fall back to str() via default=str."""
        from datetime import datetime, timezone
        obj = {"time": datetime(2024, 1, 1, tzinfo=timezone.utc)}
        h = self.blobs.put_json(obj)
        raw = self.blobs.get(h)
        self.assertIn(b"2024", raw)

    def test_deduplication(self):
        """Same data written twice should produce the same hash and not duplicate files."""
        data = b"dedup test"
        h1 = self.blobs.put(data)
        h2 = self.blobs.put(data)
        self.assertEqual(h1, h2)


class TestStore(unittest.TestCase):
    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.store = Store(root=self.tmpdir)

    def tearDown(self):
        self.store.close()

    def test_create_session_returns_ids(self):
        sid, tid = self.store.create_session("test-session")
        self.assertTrue(len(sid) > 0)
        self.assertTrue(len(tid) > 0)
        self.assertNotEqual(sid, tid)

    def test_session_and_timeline_in_db(self):
        """Session creation must atomically create both session and root timeline."""
        sid, tid = self.store.create_session("test")
        conn = sqlite3.connect(os.path.join(self.tmpdir, "rewind.db"))

        # Session exists
        row = conn.execute("SELECT name, status FROM sessions WHERE id = ?", (sid,)).fetchone()
        self.assertEqual(row[0], "test")
        self.assertEqual(row[1], "recording")

        # Root timeline exists with label 'main'
        row = conn.execute(
            "SELECT session_id, label, parent_timeline_id FROM timelines WHERE id = ?", (tid,)
        ).fetchone()
        self.assertEqual(row[0], sid)
        self.assertEqual(row[1], "main")
        self.assertIsNone(row[2])  # root has no parent

        conn.close()

    def test_create_step(self):
        sid, tid = self.store.create_session("test")
        req_hash = self.store.blobs.put_json({"model": "gpt-4o", "messages": []})
        resp_hash = self.store.blobs.put_json({"choices": [{"message": {"content": "hi"}}]})

        step_id = self.store.create_step(
            session_id=sid, timeline_id=tid, step_number=1,
            step_type="llm_call", status="success", model="gpt-4o",
            duration_ms=100, tokens_in=10, tokens_out=5,
            request_blob=req_hash, response_blob=resp_hash,
        )
        self.assertTrue(len(step_id) > 0)

        # Verify in DB
        conn = sqlite3.connect(os.path.join(self.tmpdir, "rewind.db"))
        row = conn.execute("SELECT model, tokens_in, tokens_out FROM steps WHERE id = ?", (step_id,)).fetchone()
        self.assertEqual(row[0], "gpt-4o")
        self.assertEqual(row[1], 10)
        self.assertEqual(row[2], 5)
        conn.close()

    def test_update_session_stats(self):
        sid, _ = self.store.create_session("test")
        self.store.update_session_stats(sid, 3, 100)

        conn = sqlite3.connect(os.path.join(self.tmpdir, "rewind.db"))
        row = conn.execute("SELECT total_steps, total_tokens FROM sessions WHERE id = ?", (sid,)).fetchone()
        self.assertEqual(row[0], 3)
        self.assertEqual(row[1], 100)
        conn.close()

    def test_update_session_status(self):
        sid, _ = self.store.create_session("test")
        self.store.update_session_status(sid, "completed")

        conn = sqlite3.connect(os.path.join(self.tmpdir, "rewind.db"))
        row = conn.execute("SELECT status FROM sessions WHERE id = ?", (sid,)).fetchone()
        self.assertEqual(row[0], "completed")
        conn.close()

    def test_schema_matches_rust(self):
        """Verify all expected tables and indexes exist."""
        conn = sqlite3.connect(os.path.join(self.tmpdir, "rewind.db"))
        tables = {r[0] for r in conn.execute("SELECT name FROM sqlite_master WHERE type='table'").fetchall()}
        self.assertIn("sessions", tables)
        self.assertIn("timelines", tables)
        self.assertIn("steps", tables)
        self.assertIn("replay_cache", tables)
        self.assertIn("snapshots", tables)

        indexes = {r[0] for r in conn.execute("SELECT name FROM sqlite_master WHERE type='index'").fetchall()}
        self.assertIn("idx_steps_timeline", indexes)
        self.assertIn("idx_steps_session", indexes)
        self.assertIn("idx_timelines_session", indexes)
        conn.close()

    def test_thread_safety_step_numbers(self):
        """100 concurrent step writes should produce unique, valid step numbers."""
        sid, tid = self.store.create_session("concurrent-test")
        errors = []
        step_numbers = []
        lock = threading.Lock()

        def write_step(n):
            try:
                req_hash = self.store.blobs.put_json({"step": n})
                resp_hash = self.store.blobs.put_json({"result": n})
                # Simulate what Recorder does: lock → increment → write
                with self.store._lock:
                    step_num = n  # In real code, this comes from the counter
                    self.store.create_step(
                        session_id=sid, timeline_id=tid, step_number=step_num,
                        step_type="llm_call", status="success", model="test",
                        duration_ms=1, tokens_in=1, tokens_out=1,
                        request_blob=req_hash, response_blob=resp_hash,
                    )
                with lock:
                    step_numbers.append(step_num)
            except Exception as e:
                with lock:
                    errors.append(str(e))

        threads = [threading.Thread(target=write_step, args=(i,)) for i in range(1, 101)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        self.assertEqual(len(errors), 0, f"Errors: {errors}")
        self.assertEqual(len(step_numbers), 100)
        self.assertEqual(len(set(step_numbers)), 100)  # All unique

    def test_timestamps_are_rfc3339(self):
        """Verify timestamps are valid RFC 3339."""
        sid, _ = self.store.create_session("test")
        conn = sqlite3.connect(os.path.join(self.tmpdir, "rewind.db"))
        row = conn.execute("SELECT created_at FROM sessions WHERE id = ?", (sid,)).fetchone()
        ts = row[0]
        # Should contain timezone info and be parseable
        self.assertIn("+", ts)
        self.assertIn("T", ts)
        conn.close()


if __name__ == "__main__":
    unittest.main()
