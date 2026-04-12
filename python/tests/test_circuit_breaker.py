"""Tests for the mid-session proxy circuit breaker."""

import os
import tempfile
import threading
import time
import unittest

# ── Step 1: Error detection tests ──────────────────────────────


class TestIsConnectionError(unittest.TestCase):
    """Test _is_connection_error() classifies exceptions correctly."""

    def _make_error(self, cls_name, cause=None):
        """Create an exception with a specific class name (without importing SDKs)."""
        exc_cls = type(cls_name, (Exception,), {})
        exc = exc_cls("test error")
        if cause:
            exc.__cause__ = cause
        return exc

    def test_api_connection_error_detected(self):
        from rewind_agent.circuit_breaker import _is_connection_error
        err = self._make_error("APIConnectionError")
        self.assertTrue(_is_connection_error(err))

    def test_api_timeout_error_detected(self):
        from rewind_agent.circuit_breaker import _is_connection_error
        err = self._make_error("APITimeoutError")
        self.assertTrue(_is_connection_error(err))

    def test_regular_api_error_not_detected(self):
        from rewind_agent.circuit_breaker import _is_connection_error
        err = self._make_error("BadRequestError")
        self.assertFalse(_is_connection_error(err))

    def test_raw_connection_refused_detected(self):
        from rewind_agent.circuit_breaker import _is_connection_error
        err = ConnectionRefusedError("Connection refused")
        self.assertTrue(_is_connection_error(err))

    def test_generic_exception_not_detected(self):
        from rewind_agent.circuit_breaker import _is_connection_error
        err = ValueError("something went wrong")
        self.assertFalse(_is_connection_error(err))

    def test_wrapped_connect_error_detected(self):
        """An exception wrapping httpx.ConnectError in __cause__."""
        from rewind_agent.circuit_breaker import _is_connection_error
        cause = self._make_error("ConnectError")
        err = self._make_error("SomeSDKError", cause=cause)
        self.assertTrue(_is_connection_error(err))

    def test_wrapped_connection_error_detected(self):
        from rewind_agent.circuit_breaker import _is_connection_error
        cause = ConnectionError("connection lost")
        err = self._make_error("SomeSDKError", cause=cause)
        self.assertTrue(_is_connection_error(err))


# ── Step 3: State machine tests ────────────────────────────────


def _make_conn_error():
    """Create a fake APIConnectionError."""
    cls = type("APIConnectionError", (Exception,), {})
    return cls("Connection refused")


class TestCircuitBreakerStateMachine(unittest.TestCase):
    """Test ProxyCircuitBreaker state transitions."""

    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self._saved_rewind_data = os.environ.get("REWIND_DATA")
        os.environ["REWIND_DATA"] = self.tmpdir

    def tearDown(self):
        if self._saved_rewind_data is None:
            os.environ.pop("REWIND_DATA", None)
        else:
            os.environ["REWIND_DATA"] = self._saved_rewind_data

    def _make_cb(self, **kwargs):
        from rewind_agent.circuit_breaker import ProxyCircuitBreaker
        defaults = dict(
            proxy_url="http://127.0.0.1:8443",
            original_openai_url="https://api.openai.com/v1",
            original_anthropic_url="https://api.anthropic.com",
            session_name="test-session",
            failure_threshold=2,
            recovery_timeout=30.0,
        )
        defaults.update(kwargs)
        return ProxyCircuitBreaker(**defaults)

    def test_initial_state_is_closed(self):
        cb = self._make_cb()
        self.assertEqual(cb.state, "closed")

    def test_single_failure_stays_closed(self):
        cb = self._make_cb(failure_threshold=2)
        cb.record_failure(_make_conn_error())
        self.assertEqual(cb.state, "closed")
        self.assertEqual(cb.failure_count, 1)

    def test_two_failures_trips_to_open(self):
        cb = self._make_cb(failure_threshold=2)
        cb.record_failure(_make_conn_error())
        tripped = cb.record_failure(_make_conn_error())
        self.assertTrue(tripped)
        self.assertEqual(cb.state, "open")

    def test_success_resets_failure_count(self):
        cb = self._make_cb(failure_threshold=2)
        cb.record_failure(_make_conn_error())
        self.assertEqual(cb.failure_count, 1)
        cb.record_success()
        self.assertEqual(cb.failure_count, 0)
        self.assertEqual(cb.state, "closed")

    def test_open_creates_direct_store_and_recorder(self):
        cb = self._make_cb(failure_threshold=2)
        cb.record_failure(_make_conn_error())
        cb.record_failure(_make_conn_error())
        self.assertEqual(cb.state, "open")
        self.assertIsNotNone(cb._direct_store)
        self.assertIsNotNone(cb._direct_recorder)
        self.assertIsNotNone(cb._direct_session_id)
        cb.teardown()

    def test_should_try_proxy_false_when_open(self):
        cb = self._make_cb(failure_threshold=2)
        cb.record_failure(_make_conn_error())
        cb.record_failure(_make_conn_error())
        self.assertFalse(cb.should_try_proxy())
        cb.teardown()

    def test_should_try_proxy_true_when_closed(self):
        cb = self._make_cb()
        self.assertTrue(cb.should_try_proxy())

    def test_recovery_timeout_transitions_to_half_open(self):
        cb = self._make_cb(failure_threshold=2, recovery_timeout=0.1)
        cb.record_failure(_make_conn_error())
        cb.record_failure(_make_conn_error())
        self.assertEqual(cb.state, "open")
        time.sleep(0.15)
        # should_try_proxy checks timeout and transitions
        self.assertTrue(cb.should_try_proxy())
        self.assertEqual(cb.state, "half_open")
        cb.teardown()

    def test_half_open_success_closes_circuit(self):
        cb = self._make_cb(failure_threshold=2, recovery_timeout=0.1)
        cb.record_failure(_make_conn_error())
        cb.record_failure(_make_conn_error())
        time.sleep(0.15)
        cb.should_try_proxy()  # transitions to half_open
        self.assertEqual(cb.state, "half_open")
        cb.record_success()
        self.assertEqual(cb.state, "closed")
        # Direct resources should be torn down
        self.assertIsNone(cb._direct_store)
        self.assertIsNone(cb._direct_recorder)

    def test_half_open_failure_reopens(self):
        cb = self._make_cb(failure_threshold=2, recovery_timeout=0.1)
        cb.record_failure(_make_conn_error())
        cb.record_failure(_make_conn_error())
        time.sleep(0.15)
        cb.should_try_proxy()  # transitions to half_open
        cb.record_failure(_make_conn_error())
        self.assertEqual(cb.state, "open")
        cb.teardown()

    def test_close_tears_down_direct_resources(self):
        cb = self._make_cb(failure_threshold=2)
        cb.record_failure(_make_conn_error())
        cb.record_failure(_make_conn_error())
        self.assertIsNotNone(cb._direct_store)
        cb.teardown()
        self.assertIsNone(cb._direct_store)
        self.assertIsNone(cb._direct_recorder)

    def test_fallback_session_name_includes_suffix(self):
        cb = self._make_cb(failure_threshold=2, session_name="my-agent")
        cb.record_failure(_make_conn_error())
        cb.record_failure(_make_conn_error())
        # Check the session was created with the fallback suffix
        store = cb._direct_store
        session = store.get_session(cb._direct_session_id)
        self.assertIn("proxy-fallback", session["name"])
        cb.teardown()

    def test_thread_safety_concurrent_failures(self):
        cb = self._make_cb(failure_threshold=2)
        errors = []

        def fail():
            try:
                cb.record_failure(_make_conn_error())
            except Exception as e:
                errors.append(e)

        threads = [threading.Thread(target=fail) for _ in range(10)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        self.assertEqual(errors, [])
        # Should be OPEN (at least 2 failures happened)
        self.assertEqual(cb.state, "open")
        cb.teardown()


# ── Step 7: Integration with patch.py ──────────────────────────

import json
from http.server import HTTPServer, BaseHTTPRequestHandler


class MockHealthHandler(BaseHTTPRequestHandler):
    """Responds to /_rewind/health with 200."""

    def do_GET(self):
        if self.path == "/_rewind/health":
            body = json.dumps({"status": "ok", "session": "test", "steps": 0}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_response(404)
            self.end_headers()

    def log_message(self, format, *args):
        pass


def _find_free_port():
    import socket
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class TestCircuitBreakerIntegration(unittest.TestCase):
    """Test that patch.py wires the circuit breaker correctly."""

    def setUp(self):
        import rewind_agent.patch as pm
        self.pm = pm
        self.tmpdir = tempfile.mkdtemp()
        self._saved_env = {}
        for key in ("OPENAI_BASE_URL", "ANTHROPIC_BASE_URL", "REWIND_DATA"):
            self._saved_env[key] = os.environ.get(key)
        os.environ["REWIND_DATA"] = self.tmpdir
        # Reset global state
        pm._initialized = False
        pm._mode = None
        pm._recorder = None
        pm._store = None
        pm._session_id = None
        pm._circuit_breaker = None
        pm._original_base_url = None
        pm._original_anthropic_base_url = None

    def tearDown(self):
        if self.pm._initialized:
            try:
                self.pm.uninit()
            except Exception:
                pass
        self.pm._initialized = False
        self.pm._mode = None
        self.pm._circuit_breaker = None
        for key, val in self._saved_env.items():
            if val is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = val

    def test_init_proxy_creates_circuit_breaker(self):
        """init(mode='proxy') with healthy proxy creates _circuit_breaker."""
        port = _find_free_port()
        server = HTTPServer(("127.0.0.1", port), MockHealthHandler)
        t = threading.Thread(target=server.handle_request, daemon=True)
        t.start()
        try:
            self.pm.init(mode="proxy", proxy_url=f"http://127.0.0.1:{port}",
                         session_name="cb-test")
            self.assertEqual(self.pm._mode, "proxy")
            self.assertIsNotNone(self.pm._circuit_breaker)
            self.assertEqual(self.pm._circuit_breaker.state, "closed")
            self.assertEqual(self.pm._circuit_breaker.session_name, "cb-test")
        finally:
            server.server_close()
            t.join(timeout=2)

    def test_uninit_cleans_up_circuit_breaker(self):
        """uninit() tears down the circuit breaker."""
        port = _find_free_port()
        server = HTTPServer(("127.0.0.1", port), MockHealthHandler)
        t = threading.Thread(target=server.handle_request, daemon=True)
        t.start()
        try:
            self.pm.init(mode="proxy", proxy_url=f"http://127.0.0.1:{port}")
            self.assertIsNotNone(self.pm._circuit_breaker)
            self.pm.uninit()
            self.assertIsNone(self.pm._circuit_breaker)
        finally:
            server.server_close()
            t.join(timeout=2)

    def test_fallthrough_does_not_create_circuit_breaker(self):
        """init(mode='proxy') with dead proxy falls through — no CB created."""
        port = _find_free_port()
        self.pm.init(mode="proxy", proxy_url=f"http://127.0.0.1:{port}")
        # Should have fallen through to direct mode
        self.assertEqual(self.pm._mode, "direct")
        self.assertIsNone(self.pm._circuit_breaker)


if __name__ == "__main__":
    unittest.main()
