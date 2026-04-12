"""Tests for proxy mode init-time health check and fallthrough to direct mode."""

import json
import os
import tempfile
import threading
import time
import unittest
from http.server import HTTPServer, BaseHTTPRequestHandler

from rewind_agent.patch import (
    _proxy_is_healthy,
    _init_proxy,
    init,
    uninit,
)
import rewind_agent.patch as patch_module


def _find_free_port():
    """Find an unused TCP port."""
    import socket
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class MockHealthHandler(BaseHTTPRequestHandler):
    """Responds to /_rewind/health with 200 and valid JSON."""

    def do_GET(self):
        if self.path == "/_rewind/health":
            body = json.dumps({"status": "ok", "session": "test-session", "steps": 0}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_response(404)
            self.end_headers()

    def log_message(self, format, *args):
        pass  # suppress logs in test output


class SlowHealthHandler(BaseHTTPRequestHandler):
    """Responds to /_rewind/health after a 2-second delay (exceeds timeout)."""

    def do_GET(self):
        time.sleep(2)
        body = json.dumps({"status": "ok"}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format, *args):
        pass


class NonRewindHandler(BaseHTTPRequestHandler):
    """Returns 200 but with a non-Rewind JSON body (missing "status": "ok")."""

    def do_GET(self):
        body = json.dumps({"service": "something-else"}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format, *args):
        pass


class TestProxyIsHealthy(unittest.TestCase):
    """Test the _proxy_is_healthy() health check function."""

    def test_unreachable_port_returns_false(self):
        """When nothing is listening, health check returns False quickly."""
        port = _find_free_port()
        result = _proxy_is_healthy(f"http://127.0.0.1:{port}", timeout=0.3)
        self.assertFalse(result)

    def test_healthy_server_returns_true(self):
        """When a mock server responds to /_rewind/health, returns True."""
        port = _find_free_port()
        server = HTTPServer(("127.0.0.1", port), MockHealthHandler)
        t = threading.Thread(target=server.handle_request, daemon=True)
        t.start()
        try:
            result = _proxy_is_healthy(f"http://127.0.0.1:{port}", timeout=1.0)
            self.assertTrue(result)
        finally:
            server.server_close()
            t.join(timeout=2)

    def test_slow_server_returns_false(self):
        """When proxy responds slower than timeout, health check returns False."""
        port = _find_free_port()
        server = HTTPServer(("127.0.0.1", port), SlowHealthHandler)
        t = threading.Thread(target=server.handle_request, daemon=True)
        t.start()
        try:
            result = _proxy_is_healthy(f"http://127.0.0.1:{port}", timeout=0.3)
            self.assertFalse(result)
        finally:
            server.server_close()
            t.join(timeout=3)

    def test_non_rewind_server_returns_false(self):
        """When a non-Rewind service returns 200 but wrong body, returns False."""
        port = _find_free_port()
        server = HTTPServer(("127.0.0.1", port), NonRewindHandler)
        t = threading.Thread(target=server.handle_request, daemon=True)
        t.start()
        try:
            result = _proxy_is_healthy(f"http://127.0.0.1:{port}", timeout=1.0)
            self.assertFalse(result)
        finally:
            server.server_close()
            t.join(timeout=2)


class TestInitProxyFallthrough(unittest.TestCase):
    """Test that _init_proxy falls through to direct mode when proxy is unreachable."""

    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        # Reset global state
        patch_module._initialized = False
        patch_module._mode = None
        patch_module._recorder = None
        patch_module._store = None
        patch_module._session_id = None
        patch_module._original_base_url = None
        patch_module._original_anthropic_base_url = None
        # Save original env vars
        self._saved_env = {}
        for key in ("OPENAI_BASE_URL", "ANTHROPIC_BASE_URL", "REWIND_DATA"):
            self._saved_env[key] = os.environ.get(key)
        # Point store to temp dir
        os.environ["REWIND_DATA"] = self.tmpdir

    def tearDown(self):
        # Clean up
        if patch_module._initialized:
            try:
                uninit()
            except Exception:
                pass
        patch_module._initialized = False
        patch_module._mode = None
        patch_module._recorder = None
        patch_module._store = None
        patch_module._session_id = None
        # Restore env
        for key, val in self._saved_env.items():
            if val is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = val

    def test_fallthrough_to_direct_when_proxy_down(self):
        """When proxy is unreachable, _init_proxy falls back to direct mode."""
        port = _find_free_port()
        fell_through = _init_proxy(f"http://127.0.0.1:{port}", auto_patch=True)

        self.assertTrue(fell_through)
        # Recorder should be created (direct mode creates one)
        self.assertIsNotNone(patch_module._recorder)
        # Store should be created
        self.assertIsNotNone(patch_module._store)
        # OPENAI_BASE_URL should NOT point to the dead proxy
        openai_url = os.environ.get("OPENAI_BASE_URL")
        self.assertTrue(
            openai_url is None or str(port) not in openai_url,
            f"OPENAI_BASE_URL should not point to dead proxy, got: {openai_url}"
        )

    def test_fallthrough_preserves_session_name(self):
        """When falling through, the session is created with the caller's name, not 'default'."""
        port = _find_free_port()
        _init_proxy(f"http://127.0.0.1:{port}", auto_patch=True, session_name="my-agent")

        # The session should be named "my-agent", not "default"
        store = patch_module._store
        session = store.get_session(patch_module._session_id)
        self.assertEqual(session["name"], "my-agent")

    def test_init_fallthrough_sets_mode_to_direct(self):
        """init(mode='proxy') with no proxy switches _mode to 'direct'."""
        port = _find_free_port()
        init(mode="proxy", proxy_url=f"http://127.0.0.1:{port}", session_name="test")

        self.assertEqual(patch_module._mode, "direct")
        self.assertTrue(patch_module._initialized)

    def test_proxy_mode_when_proxy_up(self):
        """When proxy is healthy, _init_proxy sets up proxy mode normally."""
        port = _find_free_port()
        server = HTTPServer(("127.0.0.1", port), MockHealthHandler)
        t = threading.Thread(target=server.handle_request, daemon=True)
        t.start()
        try:
            fell_through = _init_proxy(f"http://127.0.0.1:{port}", auto_patch=False)

            self.assertFalse(fell_through)
            # ENV should point to proxy
            self.assertEqual(
                os.environ.get("OPENAI_BASE_URL"),
                f"http://127.0.0.1:{port}/v1",
            )
            self.assertEqual(
                os.environ.get("ANTHROPIC_BASE_URL"),
                f"http://127.0.0.1:{port}/anthropic",
            )
        finally:
            server.server_close()
            t.join(timeout=2)

    def test_env_vars_not_corrupted_on_fallthrough(self):
        """Both OPENAI_BASE_URL and ANTHROPIC_BASE_URL are clean after fallthrough."""
        os.environ["OPENAI_BASE_URL"] = "https://api.openai.com/v1"
        os.environ["ANTHROPIC_BASE_URL"] = "https://api.anthropic.com"

        port = _find_free_port()
        _init_proxy(f"http://127.0.0.1:{port}", auto_patch=True)

        # Original values should be preserved (not overwritten by proxy)
        self.assertEqual(os.environ.get("OPENAI_BASE_URL"), "https://api.openai.com/v1")
        self.assertEqual(os.environ.get("ANTHROPIC_BASE_URL"), "https://api.anthropic.com")

    def test_uninit_after_fallthrough_cleans_up(self):
        """uninit() correctly tears down direct mode after a proxy fallthrough."""
        port = _find_free_port()
        # Simulate init(mode="proxy") with dead proxy
        patch_module._mode = "proxy"
        fell_through = _init_proxy(f"http://127.0.0.1:{port}", auto_patch=True)
        if fell_through:
            patch_module._mode = "direct"
        patch_module._initialized = True

        # Verify we're in direct mode with a recorder
        self.assertIsNotNone(patch_module._recorder)
        self.assertIsNotNone(patch_module._store)

        # uninit should clean up direct mode state
        uninit()

        self.assertFalse(patch_module._initialized)
        self.assertIsNone(patch_module._mode)
        self.assertIsNone(patch_module._recorder)
        self.assertIsNone(patch_module._store)


if __name__ == "__main__":
    unittest.main()
