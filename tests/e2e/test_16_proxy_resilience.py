"""
E2E Phase 16: Proxy resilience — health endpoint, init-time fallthrough, circuit breaker.

Tests:
  16a. Health endpoint returns 200 with valid JSON
  16b. Init-time fallthrough to direct mode when proxy is down
  16c. Circuit breaker trips on proxy failure and records to local store
"""

import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.request
import urllib.error

sys.path.insert(0, os.path.dirname(__file__))
import mock_llm_server
from helpers import run_cli, clean_db, query_scalar, REWIND

PROXY_PORT = 8445
MOCK_PORT = 9997


def test_health_endpoint():
    """16a: GET /_rewind/health returns 200 with status=ok."""
    print("\n--- Test 16a: Health endpoint ---")
    clean_db()
    mock_url = mock_llm_server.start(port=MOCK_PORT)

    proxy_proc = subprocess.Popen(
        f"{REWIND} record --name health-test --upstream {mock_url} --port {PROXY_PORT}".split(),
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    time.sleep(2)

    try:
        req = urllib.request.Request(
            f"http://127.0.0.1:{PROXY_PORT}/_rewind/health",
            method="GET",
        )
        with urllib.request.urlopen(req, timeout=5) as resp:
            assert resp.status == 200, f"Expected 200, got {resp.status}"
            data = json.loads(resp.read())

        assert data.get("status") == "ok", f"Expected status=ok, got: {data}"
        assert "version" in data, f"Missing version field: {data}"
        assert "session" in data, f"Missing session field: {data}"
        assert "steps" in data, f"Missing steps field: {data}"
        print(f"  Health response: {data}")
        print("  PASS")
    finally:
        proxy_proc.terminate()
        proxy_proc.wait(timeout=5)
        mock_llm_server.stop()


def test_init_fallthrough():
    """16b: Python SDK falls back to direct mode when proxy is unreachable."""
    print("\n--- Test 16b: Init-time fallthrough ---")
    tmpdir = tempfile.mkdtemp()
    saved_rewind_data = os.environ.get("REWIND_DATA")
    os.environ["REWIND_DATA"] = tmpdir

    try:
        import rewind_agent.patch as pm
        # Reset state
        pm._initialized = False
        pm._mode = None
        pm._recorder = None
        pm._store = None
        pm._session_id = None
        pm._circuit_breaker = None
        pm._original_base_url = None
        pm._original_anthropic_base_url = None

        # Use a port nothing is listening on
        dead_port = 19999
        pm.init(mode="proxy", proxy_url=f"http://127.0.0.1:{dead_port}",
                session_name="fallthrough-e2e")

        assert pm._mode == "direct", f"Expected direct mode, got: {pm._mode}"
        assert pm._recorder is not None, "Recorder should be created in direct mode"
        assert pm._circuit_breaker is None, "CB should not be created on fallthrough"

        # Verify session was created with correct name
        session = pm._store.get_session(pm._session_id)
        assert session["name"] == "fallthrough-e2e", f"Wrong session name: {session['name']}"

        print(f"  Mode: {pm._mode}")
        print(f"  Session: {session['name']}")
        print("  PASS")

        pm.uninit()
    finally:
        if saved_rewind_data is None:
            os.environ.pop("REWIND_DATA", None)
        else:
            os.environ["REWIND_DATA"] = saved_rewind_data


def test_circuit_breaker_state_machine():
    """16c: Circuit breaker trips after 2 consecutive connection errors."""
    print("\n--- Test 16c: Circuit breaker state machine ---")
    tmpdir = tempfile.mkdtemp()
    saved_rewind_data = os.environ.get("REWIND_DATA")
    os.environ["REWIND_DATA"] = tmpdir

    try:
        from rewind_agent.circuit_breaker import ProxyCircuitBreaker

        cb = ProxyCircuitBreaker(
            proxy_url="http://127.0.0.1:19999",
            original_openai_url="https://api.openai.com/v1",
            original_anthropic_url="https://api.anthropic.com",
            session_name="cb-e2e",
            failure_threshold=2,
            recovery_timeout=0.5,
        )

        # Initially CLOSED
        assert cb.state == "closed"
        assert cb.should_try_proxy() is True

        # 1 failure — still CLOSED
        ConnErr = type("APIConnectionError", (Exception,), {})
        cb.record_failure(ConnErr("refused"))
        assert cb.state == "closed"

        # 2nd failure — trips to OPEN
        tripped = cb.record_failure(ConnErr("refused"))
        assert tripped is True
        assert cb.state == "open"
        assert cb.should_try_proxy() is False

        # Direct resources created
        assert cb._direct_store is not None
        assert cb._direct_recorder is not None
        session = cb._direct_store.get_session(cb._direct_session_id)
        assert "proxy-fallback" in session["name"]

        # Record a step via direct recorder
        cb._direct_recorder._record_call(
            model="gpt-4o", request_data={"model": "gpt-4o"},
            response_data={"choices": [{"message": {"content": "test"}}],
                           "usage": {"prompt_tokens": 10, "completion_tokens": 5}},
            duration_ms=100, provider="openai",
        )

        # Verify step recorded
        root_tl = cb._direct_store.get_root_timeline(cb._direct_session_id)
        steps = cb._direct_store.get_steps(root_tl["id"])
        assert len(steps) == 1, f"Expected 1 step, got {len(steps)}"
        assert steps[0]["model"] == "gpt-4o"

        # Wait for recovery timeout → HALF_OPEN
        time.sleep(0.6)
        assert cb.should_try_proxy() is True
        assert cb.state == "half_open"

        # Success → CLOSED
        cb.record_success()
        assert cb.state == "closed"
        assert cb._direct_store is None  # resources torn down

        print(f"  State transitions: closed → open → half_open → closed")
        print(f"  Steps recorded in fallback session: 1")
        print("  PASS")

        cb.teardown()
    finally:
        if saved_rewind_data is None:
            os.environ.pop("REWIND_DATA", None)
        else:
            os.environ["REWIND_DATA"] = saved_rewind_data


if __name__ == "__main__":
    test_health_endpoint()
    test_init_fallthrough()
    test_circuit_breaker_state_machine()
    print("\n=== Phase 16 COMPLETE ===")
