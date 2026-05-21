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
    _ALL_REPLAY_VARS = (
        "REWIND_SESSION_ID",
        "REWIND_REPLAY_CONTEXT_ID",
        "REWIND_REPLAY_CONTEXT_TIMELINE_ID",
    )

    def _replay_env(self, **overrides):
        """Build a clean env dict with replay vars set unless overridden."""
        base = {k: v for k, v in os.environ.items() if k not in self._ALL_REPLAY_VARS}
        base.update({
            "REWIND_SESSION_ID": "s-replay",
            "REWIND_REPLAY_CONTEXT_ID": "ctx-replay",
            "REWIND_REPLAY_CONTEXT_TIMELINE_ID": "tl-fork",
        })
        base.update(overrides)
        return {k: v for k, v in base.items() if v is not None}

    def test_replay_env_skips_session_start(self):
        env = self._replay_env(REWIND_URL=self.base_url)
        with mock.patch.dict(os.environ, env, clear=True):
            self.assertTrue(_is_replay_dispatch())
            with setup(name="replay-handler", base_url=self.base_url) as client:
                self.assertIsInstance(client, ExplicitClient)
                self.assertTrue(is_installed())
        # No phantom session was created on /api/sessions/start.
        self.assertEqual(_MockHandler.sessions_started, [])
        self.assertEqual(_MockHandler.sessions_ended, [])

    def test_all_three_required_for_replay_mode(self):
        # Each replay var, individually unset, breaks replay-mode detection.
        # Without all three, intercept._install warns about an undefined
        # recording target — better to fall through to normal session start.
        for omit in self._ALL_REPLAY_VARS:
            with self.subTest(omit=omit):
                env = self._replay_env(**{omit: None})
                with mock.patch.dict(os.environ, env, clear=True):
                    self.assertFalse(
                        _is_replay_dispatch(),
                        f"missing {omit} should disable replay mode",
                    )

    def test_partial_replay_env_falls_through_to_session_start(self):
        # Sanity: with only sessionid + replayctxid (no timeline), the
        # connector starts a fresh session instead of half-attaching.
        env = self._replay_env(REWIND_REPLAY_CONTEXT_TIMELINE_ID=None)
        with mock.patch.dict(os.environ, env, clear=True):
            with setup(name="incomplete-replay", base_url=self.base_url):
                pass
        self.assertEqual(len(_MockHandler.sessions_started), 1)
        self.assertEqual(_MockHandler.sessions_started[0]["name"], "incomplete-replay")


def _fake_req(netloc: str):
    """Build the minimal duck-typed RewindRequest a Predicates.is_llm_call needs."""
    class _Parts:
        pass

    parts = _Parts()
    parts.netloc = netloc
    req = type("R", (), {})()
    req.url_parts = parts
    return req


class TestHostPredicates(_ConnectorTestBase):
    """Behavioral coverage — assert what is_llm_call returns, not on private state."""

    def _capture_predicates_via_setup(self, **setup_kwargs):
        captured = {}

        def fake_install(predicates=None):
            captured["predicates"] = predicates

        with mock.patch("rewind_agent.connector.install", side_effect=fake_install), \
             mock.patch("rewind_agent.connector.uninstall"), \
             mock.patch("rewind_agent.connector.is_installed", return_value=False):
            with setup(name="capture", base_url=self.base_url, **setup_kwargs):
                pass
        return captured["predicates"]

    def test_kwarg_hosts_match_via_substring(self):
        preds = self._capture_predicates_via_setup(
            llm_hosts=("llm-gateway.example.com",),
        )
        self.assertIsInstance(preds, _HostPredicates)
        # The kwarg host matches — including substring containment.
        self.assertTrue(preds.is_llm_call(_fake_req("llm-gateway.example.com")))
        self.assertTrue(preds.is_llm_call(_fake_req("private-llm-gateway.example.com")))
        # Hosts not in kwarg AND not on the strict-by-default provider list don't match.
        self.assertFalse(preds.is_llm_call(_fake_req("unrelated.example.com")))

    def test_env_hosts_parsed_with_whitespace_and_empties_dropped(self):
        with mock.patch.dict(
            os.environ,
            {"REWIND_LLM_HOSTS": "a.example,b.example , ,c.example"},
            clear=False,
        ):
            preds = self._capture_predicates_via_setup()
        self.assertIsInstance(preds, _HostPredicates)
        # All three configured hosts match.
        self.assertTrue(preds.is_llm_call(_fake_req("a.example")))
        self.assertTrue(preds.is_llm_call(_fake_req("svc.b.example")))
        self.assertTrue(preds.is_llm_call(_fake_req("c.example:8080")))
        # The empty / whitespace-only entries did NOT become a wildcard:
        # an unrelated host that doesn't share a substring with any of
        # a/b/c.example must still miss.
        self.assertFalse(preds.is_llm_call(_fake_req("unrelated.example")))

    def test_no_hosts_uses_intercept_default_predicates(self):
        # Empty REWIND_LLM_HOSTS and no kwarg → install() gets None, which
        # tells intercept to apply its strict-by-default DefaultPredicates.
        # We do NOT wrap an empty _HostPredicates here (would silently
        # broaden matching to "no hosts" — never True for unknown hosts —
        # which IS the same in effect, but it's clearer to delegate to
        # intercept's default rather than introduce an empty wrapper.)
        with mock.patch.dict(os.environ, {"REWIND_LLM_HOSTS": ""}, clear=False):
            preds = self._capture_predicates_via_setup()
        self.assertIsNone(preds)

    def test_predicate_falls_through_to_default_provider_list(self):
        # When custom hosts don't match, the parent DefaultPredicates handles
        # known providers like api.openai.com — verify the chain.
        preds = _HostPredicates(("custom-gw.example",))
        self.assertTrue(preds.is_llm_call(_fake_req("custom-gw.example")))
        self.assertTrue(preds.is_llm_call(_fake_req("api.openai.com")))
        self.assertFalse(preds.is_llm_call(_fake_req("unrelated.com")))


class TestPublicExport(unittest.TestCase):
    def test_module_attribute(self):
        self.assertTrue(hasattr(rewind_agent, "connector"))
        self.assertTrue(callable(rewind_agent.connector.setup))


if __name__ == "__main__":
    unittest.main()
