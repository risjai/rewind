"""Tests for the one-call connector (rewind_agent.connector.setup)."""

import os
import threading
import unittest
from http.server import HTTPServer, BaseHTTPRequestHandler
from unittest import mock

import rewind_agent
from rewind_agent.connector import _HostPredicates, _is_replay_dispatch, setup
from rewind_agent.explicit import (
    ExplicitClient,
    _replay_context_id,
    _session_id,
    _timeline_id,
)
from rewind_agent.intercept import is_installed, uninstall


class _MockHandler(BaseHTTPRequestHandler):
    """Minimal mock — only what the connector needs."""

    sessions_started: list = []
    sessions_ended: list = []

    def do_POST(self):  # noqa: N802 — stdlib API
        import json

        length = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(length)) if length else {}

        if self.path == "/api/sessions/start":
            _MockHandler.sessions_started.append(body)
            self._respond(201, {
                "session_id": f"s-{len(_MockHandler.sessions_started)}",
                "root_timeline_id": f"tl-{len(_MockHandler.sessions_started)}",
            })
        elif self.path.endswith("/end"):
            _MockHandler.sessions_ended.append(self.path)
            self._respond(200, {"session_id": self.path.split("/")[3]})
        else:
            self._respond(404, {"error": "unhandled"})

    def _respond(self, status, body):
        import json

        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(body).encode())

    def log_message(self, *_):  # silence
        pass


class _ConnectorTestBase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = HTTPServer(("127.0.0.1", 0), _MockHandler)
        cls.port = cls.server.server_address[1]
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()
        cls.base_url = f"http://127.0.0.1:{cls.port}"

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()

    def setUp(self):
        _MockHandler.sessions_started = []
        _MockHandler.sessions_ended = []
        _session_id.set(None)
        _timeline_id.set(None)
        _replay_context_id.set(None)
        # Make sure intercept is not lingering from a prior test.
        if is_installed():
            uninstall()


class TestKillSwitch(_ConnectorTestBase):
    def test_disabled_via_env_yields_none_and_no_http(self):
        with mock.patch.dict(os.environ, {"REWIND_ENABLED": "0"}, clear=False):
            with setup(name="off", base_url=self.base_url) as client:
                self.assertIsNone(client)
                self.assertFalse(is_installed())
        self.assertEqual(_MockHandler.sessions_started, [])

    def test_disabled_via_kwarg(self):
        with setup(name="off", base_url=self.base_url, enabled=False) as client:
            self.assertIsNone(client)
        self.assertEqual(_MockHandler.sessions_started, [])


class TestSessionLifecycle(_ConnectorTestBase):
    def test_starts_session_and_installs_intercept(self):
        with setup(name="my-agent", base_url=self.base_url) as client:
            self.assertIsInstance(client, ExplicitClient)
            self.assertEqual(_session_id.get(), "s-1")
            self.assertEqual(_timeline_id.get(), "tl-1")
            self.assertTrue(is_installed())

        # Cleanup happened.
        self.assertIsNone(_session_id.get())
        self.assertFalse(is_installed())
        self.assertEqual(len(_MockHandler.sessions_started), 1)
        self.assertEqual(_MockHandler.sessions_started[0]["name"], "my-agent")
        self.assertEqual(len(_MockHandler.sessions_ended), 1)

    def test_does_not_uninstall_intercept_if_already_installed(self):
        # Simulate: operator already called intercept.install() at startup.
        from rewind_agent.intercept import install as intercept_install
        intercept_install()
        try:
            with setup(name="reentrant", base_url=self.base_url):
                self.assertTrue(is_installed())
            # We were not the installer, so we must NOT have uninstalled.
            self.assertTrue(is_installed())
        finally:
            uninstall()

    def test_propagates_thread_id_and_metadata(self):
        with setup(
            name="threaded",
            base_url=self.base_url,
            thread_id="conv-42",
            metadata={"app": "test"},
        ):
            pass
        body = _MockHandler.sessions_started[0]
        self.assertEqual(body.get("thread_id"), "conv-42")
        self.assertEqual(body.get("metadata"), {"app": "test"})


class TestReplayDispatch(_ConnectorTestBase):
    def test_replay_env_skips_session_start(self):
        with mock.patch.dict(
            os.environ,
            {
                "REWIND_SESSION_ID": "s-replay",
                "REWIND_REPLAY_CONTEXT_ID": "ctx-replay",
                "REWIND_REPLAY_CONTEXT_TIMELINE_ID": "tl-fork",
                "REWIND_URL": self.base_url,
            },
            clear=False,
        ):
            self.assertTrue(_is_replay_dispatch())
            with setup(name="replay-handler", base_url=self.base_url) as client:
                self.assertIsInstance(client, ExplicitClient)
                self.assertTrue(is_installed())
        # No phantom session was created on /api/sessions/start.
        self.assertEqual(_MockHandler.sessions_started, [])
        self.assertEqual(_MockHandler.sessions_ended, [])

    def test_partial_replay_env_does_not_trigger_replay_path(self):
        # Only one of the two required vars set → not a replay dispatch.
        with mock.patch.dict(
            os.environ,
            {"REWIND_SESSION_ID": "s-only"},
            clear=False,
        ), mock.patch.dict(os.environ, {"REWIND_REPLAY_CONTEXT_ID": ""}, clear=False):
            os.environ.pop("REWIND_REPLAY_CONTEXT_ID", None)
            self.assertFalse(_is_replay_dispatch())


class TestHostPredicates(_ConnectorTestBase):
    def test_hosts_from_kwarg(self):
        captured = {}

        def fake_install(predicates=None):
            captured["predicates"] = predicates

        with mock.patch("rewind_agent.connector.install", side_effect=fake_install), \
             mock.patch("rewind_agent.connector.uninstall"), \
             mock.patch("rewind_agent.connector.is_installed", return_value=False):
            with setup(
                name="custom",
                base_url=self.base_url,
                llm_hosts=("llm-gateway.example.com",),
            ):
                pass

        preds = captured["predicates"]
        self.assertIsInstance(preds, _HostPredicates)
        self.assertEqual(preds._hosts, ("llm-gateway.example.com",))

    def test_hosts_from_env(self):
        captured = {}

        def fake_install(predicates=None):
            captured["predicates"] = predicates

        with mock.patch.dict(
            os.environ,
            {"REWIND_LLM_HOSTS": "a.example,b.example , ,c.example"},
            clear=False,
        ), mock.patch("rewind_agent.connector.install", side_effect=fake_install), \
             mock.patch("rewind_agent.connector.uninstall"), \
             mock.patch("rewind_agent.connector.is_installed", return_value=False):
            with setup(name="env-hosts", base_url=self.base_url):
                pass

        preds = captured["predicates"]
        self.assertIsInstance(preds, _HostPredicates)
        # Empty entries dropped, surrounding whitespace stripped.
        self.assertEqual(
            preds._hosts,
            ("a.example", "b.example", "c.example"),
        )

    def test_no_hosts_uses_default_predicates(self):
        captured = {}

        def fake_install(predicates=None):
            captured["predicates"] = predicates

        with mock.patch.dict(os.environ, {"REWIND_LLM_HOSTS": ""}, clear=False), \
             mock.patch("rewind_agent.connector.install", side_effect=fake_install), \
             mock.patch("rewind_agent.connector.uninstall"), \
             mock.patch("rewind_agent.connector.is_installed", return_value=False):
            with setup(name="defaults", base_url=self.base_url):
                pass

        # No custom hosts → no _HostPredicates wrapper; install gets None
        # (DefaultPredicates is applied by intercept itself).
        self.assertIsNone(captured["predicates"])

    def test_predicate_matches_substring(self):
        preds = _HostPredicates(("internal-gateway",))

        class FakeReq:
            class _Parts:
                netloc = "private-internal-gateway.example.com"

            url_parts = _Parts()

        self.assertTrue(preds.is_llm_call(FakeReq()))


class TestPublicExport(unittest.TestCase):
    def test_module_attribute(self):
        self.assertTrue(hasattr(rewind_agent, "connector"))
        self.assertTrue(callable(rewind_agent.connector.setup))


if __name__ == "__main__":
    unittest.main()
