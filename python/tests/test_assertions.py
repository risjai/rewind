"""Tests for the assertions module — baseline querying and regression checks."""

import tempfile
import unittest
import uuid

from rewind_agent.store import Store
from rewind_agent.assertions import Assertions, AssertionResult


def _now_rfc3339():
    from datetime import datetime, timezone
    return datetime.now(timezone.utc).isoformat()


def _seed_session(store: Store, name: str = "test-session"):
    """Create a session with 3 steps for testing."""
    session_id = str(uuid.uuid4())
    timeline_id = str(uuid.uuid4())
    now = _now_rfc3339()

    store._conn.execute(
        "INSERT INTO sessions (id, name, created_at, updated_at, status, total_steps, total_tokens) "
        "VALUES (?, ?, ?, ?, 'completed', 3, 500)",
        (session_id, name, now, now),
    )
    store._conn.execute(
        "INSERT INTO timelines (id, session_id, created_at, label) "
        "VALUES (?, ?, ?, 'main')",
        (timeline_id, session_id, now),
    )

    # 3 steps: LLM call, tool result, LLM call
    for i, (stype, model, tin, tout, err) in enumerate([
        ("llm_call", "gpt-4o", 100, 20, None),
        ("tool_result", "tool", 0, 0, None),
        ("llm_call", "gpt-4o", 200, 40, None),
    ], start=1):
        store._conn.execute(
            "INSERT INTO steps (id, timeline_id, session_id, step_number, step_type, "
            "status, created_at, duration_ms, tokens_in, tokens_out, model, "
            "request_blob, response_blob, error) "
            "VALUES (?, ?, ?, ?, ?, 'success', ?, 100, ?, ?, ?, '', '', ?)",
            (str(uuid.uuid4()), timeline_id, session_id, i, stype, now, tin, tout, model, err),
        )
    store._conn.commit()
    return session_id, timeline_id


def _seed_baseline(store: Store, session_id: str, timeline_id: str, name: str = "test-baseline"):
    """Create a baseline from a session's steps."""
    baseline_id = str(uuid.uuid4())
    now = _now_rfc3339()

    store._conn.execute(
        "INSERT INTO baselines (id, name, source_session_id, source_timeline_id, "
        "created_at, step_count, total_tokens) VALUES (?, ?, ?, ?, ?, 3, 500)",
        (baseline_id, name, session_id, timeline_id, now),
    )

    rows = store._conn.execute(
        "SELECT step_number, step_type, status, model, tokens_in, tokens_out, error "
        "FROM steps WHERE timeline_id = ? ORDER BY step_number",
        (timeline_id,),
    ).fetchall()

    for r in rows:
        store._conn.execute(
            "INSERT INTO baseline_steps (id, baseline_id, step_number, step_type, "
            "expected_status, expected_model, tokens_in, tokens_out, has_error) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (str(uuid.uuid4()), baseline_id, r[0], r[1], r[2], r[3], r[4], r[5], 1 if r[6] else 0),
        )
    store._conn.commit()
    return baseline_id


class TestAssertions(unittest.TestCase):
    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.store = Store(self.tmpdir)
        self.assertions = Assertions(self.store)

    def test_list_baselines_empty(self):
        baselines = self.assertions.list_baselines()
        self.assertEqual(baselines, [])

    def test_get_baseline_not_found(self):
        self.assertIsNone(self.assertions.get_baseline("nonexistent"))

    def test_list_and_get_baseline(self):
        session_id, timeline_id = _seed_session(self.store)
        _seed_baseline(self.store, session_id, timeline_id, "my-baseline")

        baselines = self.assertions.list_baselines()
        self.assertEqual(len(baselines), 1)
        self.assertEqual(baselines[0].name, "my-baseline")
        self.assertEqual(baselines[0].step_count, 3)

        bl = self.assertions.get_baseline("my-baseline")
        self.assertIsNotNone(bl)
        self.assertEqual(bl.name, "my-baseline")

    def test_get_baseline_steps(self):
        session_id, timeline_id = _seed_session(self.store)
        baseline_id = _seed_baseline(self.store, session_id, timeline_id)

        steps = self.assertions.get_baseline_steps(baseline_id)
        self.assertEqual(len(steps), 3)
        self.assertEqual(steps[0].step_type, "llm_call")
        self.assertEqual(steps[0].expected_model, "gpt-4o")
        self.assertEqual(steps[1].step_type, "tool_result")
        self.assertEqual(steps[2].tokens_in, 200)

    def test_check_self_passes(self):
        """Checking a session against its own baseline should pass."""
        session_id, timeline_id = _seed_session(self.store)
        _seed_baseline(self.store, session_id, timeline_id, "self-check")

        result = self.assertions.check("self-check", session_id)
        self.assertIsInstance(result, AssertionResult)
        self.assertTrue(result.passed)
        self.assertEqual(result.failed_checks, 0)
        self.assertEqual(len(result.step_results), 3)
        for sr in result.step_results:
            self.assertEqual(sr.verdict, "pass")

    def test_check_latest(self):
        """Check using 'latest' session ID."""
        session_id, timeline_id = _seed_session(self.store)
        _seed_baseline(self.store, session_id, timeline_id, "latest-check")

        result = self.assertions.check("latest-check", "latest")
        self.assertTrue(result.passed)

    def test_check_missing_steps_fails(self):
        """If actual session has fewer steps, missing steps should fail."""
        session_id, timeline_id = _seed_session(self.store)
        _seed_baseline(self.store, session_id, timeline_id, "missing-test")

        # Delete the last step
        self.store._conn.execute(
            "DELETE FROM steps WHERE timeline_id = ? AND step_number = 3",
            (timeline_id,),
        )
        self.store._conn.commit()

        result = self.assertions.check("missing-test", session_id)
        self.assertFalse(result.passed)
        self.assertEqual(result.step_results[-1].verdict, "missing")

    def test_check_new_error_fails(self):
        """If a previously successful step now errors, it should fail."""
        session_id, timeline_id = _seed_session(self.store)
        _seed_baseline(self.store, session_id, timeline_id, "error-test")

        # Add error to step 3
        self.store._conn.execute(
            "UPDATE steps SET status = 'error', error = 'hallucination' "
            "WHERE timeline_id = ? AND step_number = 3",
            (timeline_id,),
        )
        self.store._conn.commit()

        result = self.assertions.check("error-test", session_id)
        self.assertFalse(result.passed)
        step3 = result.step_results[2]
        self.assertEqual(step3.verdict, "fail")

    def test_check_baseline_not_found(self):
        with self.assertRaises(ValueError):
            self.assertions.check("nonexistent")


if __name__ == "__main__":
    unittest.main()
