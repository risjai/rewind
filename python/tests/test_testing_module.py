"""Tests for the public ``rewind_agent.testing`` module (Phase 0 commit 4).

Promoting test infrastructure from ``python/tests/`` (private) to a public
module is a real semver commitment. These tests lock the public API contract:
import-by-name (catches accidental rename), lifecycle, payload shape,
``wait_for_session`` timeout semantics, and the ``_unstable`` boundary.
"""

import threading
import time
import unittest
import urllib.error
import urllib.request


class TestPublicSurface(unittest.TestCase):
    """Lock the names of the stable public symbols.

    If a refactor accidentally renames or removes one of these, this test
    fires before downstream consumers (sf-rewind, integration smoke tests)
    break. New stable symbols should be ADDED here, not silently introduced.
    """

    def test_stable_symbols_importable(self):
        # Import via the documented module path.
        from rewind_agent.testing import (
            StubRewindServer,
            make_dispatch_payload,
            wait_for_session,
        )
        self.assertTrue(callable(StubRewindServer))
        self.assertTrue(callable(make_dispatch_payload))
        self.assertTrue(callable(wait_for_session))

    def test_dunder_all_lists_stable_symbols_only(self):
        import rewind_agent.testing as testing
        self.assertIn("StubRewindServer", testing.__all__)
        self.assertIn("make_dispatch_payload", testing.__all__)
        self.assertIn("wait_for_session", testing.__all__)
        # Internal/unstable surfaces must NOT appear in __all__.
        for name in testing.__all__:
            self.assertFalse(
                name.startswith("_"),
                f"{name} starts with '_'; private symbols don't belong in __all__",
            )


class TestStubRewindServer(unittest.TestCase):
    """``StubRewindServer`` is the in-process Rewind server that downstream
    tests bind ``ExplicitClient`` against. The contract: start it, point a
    client at ``server.base_url``, run normal recording calls, stop it on
    teardown.
    """

    def test_start_stop_lifecycle_via_context_manager(self):
        from rewind_agent.testing import StubRewindServer

        with StubRewindServer() as server:
            self.assertTrue(server.base_url.startswith("http://127.0.0.1:"))
            # Server is responsive while inside the block.
            req = urllib.request.Request(
                f"{server.base_url}/api/sessions/start",
                data=b'{"name": "lifecycle"}',
                headers={"Content-Type": "application/json"},
                method="POST",
            )
            with urllib.request.urlopen(req, timeout=2.0) as resp:
                self.assertIn(resp.status, (200, 201))

        # After exit, the server is shut down — connections fail with a
        # transport error specifically, not just any Exception. Tighten
        # the assertion so a regression that surfaces as a different
        # exception class (e.g. a hang masked by KeyboardInterrupt)
        # actually fails the test.
        with self.assertRaises(urllib.error.URLError):
            with urllib.request.urlopen(
                f"{server.base_url}/api/sessions/start", timeout=0.5
            ):
                pass

    def test_explicit_client_can_record_against_server(self):
        from rewind_agent import ExplicitClient
        from rewind_agent.testing import StubRewindServer

        with StubRewindServer() as server:
            client = ExplicitClient(server.base_url)
            with client.session("stub-test"):
                step = client.record_tool_call(
                    "ping", {"x": 1}, "ok", duration_ms=5,
                )
                self.assertIsNotNone(step)
            self.assertGreaterEqual(len(server.recorded_steps), 1)

    def test_recorded_steps_observable_for_assertions(self):
        """Tests need to assert what was recorded. Stub exposes a list of
        captured step dicts."""
        from rewind_agent import ExplicitClient
        from rewind_agent.testing import StubRewindServer

        with StubRewindServer() as server:
            client = ExplicitClient(server.base_url)
            with client.session("assert-test"):
                client.record_tool_call("foo", {}, "out", duration_ms=1)
                client.record_tool_call("bar", {}, "out", duration_ms=1)

            tool_names = [s.get("tool_name") for s in server.recorded_steps]
            self.assertEqual(tool_names, ["foo", "bar"])


class TestMakeDispatchPayload(unittest.TestCase):
    """``make_dispatch_payload`` builds a runner-compatible
    :class:`DispatchPayload` for tests that exercise replay handlers.

    The contract: required fields have sensible defaults so a single
    ``make_dispatch_payload(session_id="s")`` call produces a valid payload;
    callers override individually.
    """

    def test_returns_dispatch_payload_instance(self):
        from rewind_agent.runner import DispatchPayload
        from rewind_agent.testing import make_dispatch_payload

        payload = make_dispatch_payload(session_id="s-1")
        self.assertIsInstance(payload, DispatchPayload)
        self.assertEqual(payload.session_id, "s-1")

    def test_defaults_populate_required_fields(self):
        from rewind_agent.testing import make_dispatch_payload

        payload = make_dispatch_payload(session_id="s-1")
        # Every required DispatchPayload field is non-empty.
        self.assertTrue(payload.job_id)
        self.assertTrue(payload.replay_context_id)
        self.assertTrue(payload.replay_context_timeline_id)
        self.assertTrue(payload.source_timeline_id)
        self.assertTrue(payload.base_url)
        self.assertTrue(payload.dispatch_token)
        # at_step defaults to 1 (replay-from-start).
        self.assertEqual(payload.at_step, 1)

    def test_overrides_apply(self):
        from rewind_agent.testing import make_dispatch_payload

        payload = make_dispatch_payload(
            session_id="sess",
            job_id="job-42",
            at_step=7,
            base_url="http://my.test:1234",
        )
        self.assertEqual(payload.job_id, "job-42")
        self.assertEqual(payload.at_step, 7)
        self.assertEqual(payload.base_url, "http://my.test:1234")


class TestWaitForSession(unittest.TestCase):
    """``wait_for_session`` polls the stub for a session and returns when
    it appears. Used by integration-style tests that bring up an agent in
    a thread and need to assert "the agent recorded its session" without
    relying on internal sync primitives.
    """

    def test_returns_session_when_recorded(self):
        from rewind_agent import ExplicitClient
        from rewind_agent.testing import StubRewindServer, wait_for_session

        with StubRewindServer() as server:
            client = ExplicitClient(server.base_url)

            def record_in_thread():
                # Tiny delay to exercise the polling path.
                time.sleep(0.05)
                with client.session("background"):
                    client.record_tool_call(
                        "ping", {}, "ok", duration_ms=1,
                    )

            t = threading.Thread(target=record_in_thread)
            t.start()
            try:
                session = wait_for_session(server, name="background", timeout=2.0)
                self.assertIsNotNone(session)
                self.assertEqual(session["name"], "background")
            finally:
                t.join()

    def test_raises_timeout_when_no_session_recorded(self):
        from rewind_agent.testing import StubRewindServer, wait_for_session

        with StubRewindServer() as server:
            with self.assertRaises(TimeoutError):
                wait_for_session(server, name="never-happens", timeout=0.2)


class TestUnstableBoundary(unittest.TestCase):
    """``rewind_agent.testing._unstable`` exists as an explicit signal that
    APIs underneath it may break in any 0.x release. Promoting a symbol
    OUT of ``_unstable`` is a deliberate decision; this test makes sure
    the boundary is real (the submodule importable) and not just
    documentation."""

    def test_unstable_submodule_importable(self):
        import rewind_agent.testing._unstable  # noqa: F401

    def test_unstable_not_in_dunder_all(self):
        import rewind_agent.testing as testing
        self.assertNotIn("_unstable", testing.__all__)


if __name__ == "__main__":
    unittest.main()
