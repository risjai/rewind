"""E2E test for the Phase 3 runner-replay flow (commit 11/13).

Spins up a real Rewind server (with REWIND_RUNNER_SECRET_KEY +
REWIND_AUTH_TOKEN) and a stub runner HTTP endpoint, then exercises
the full lifecycle:

  1. CLI registers the stub runner via `/api/runners` and captures
     the raw token.
  2. We seed a session with a couple of recorded steps so a real
     replay context can be created.
  3. Server dispatches a job (shape A: server forks + creates ctx).
  4. Stub runner receives the dispatch with HMAC signature, posts
     `started` -> `progress` -> `completed` events back via
     `/api/replay-jobs/{id}/events` with `X-Rewind-Runner-Auth`.
  5. We assert the job lands in the `completed` state.

Skips when the rewind binary isn't built or `REWIND_RUNNER_SECRET_KEY`
can't be set in the test environment.
"""

from __future__ import annotations

import asyncio
import base64
import contextlib
import hashlib
import hmac
import json
import os
import secrets
import socket
import subprocess
import sys
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, HTTPServer
from typing import Optional

import pytest

try:
    import requests  # noqa: PLC0415
except ImportError:  # pragma: no cover
    pytest.skip("requests library not installed", allow_module_level=True)


def _resolve_rewind_bin() -> str:
    """Find ./target/release/rewind from any working directory.

    pytest may run from python/ or repo root; locate by walking up.
    """
    candidates = [
        "./target/release/rewind",
        "../target/release/rewind",
        os.path.join(os.path.dirname(__file__), "..", "..", "..", "target", "release", "rewind"),
    ]
    for c in candidates:
        if os.path.exists(c):
            return os.path.abspath(c)
    return candidates[0]  # caller will pytest.skip


REWIND_BIN = _resolve_rewind_bin()
TEST_AUTH_TOKEN = "phase3-e2e-bearer-token"


# ─────────────────────────────────────────────────────────────────
# Stub runner: receives webhook, replies 202, calls back
# ─────────────────────────────────────────────────────────────────


class _StubRunner:
    """Tiny HTTP server that pretends to be a runner."""

    def __init__(self, runner_token_holder: list[str]) -> None:
        self.received: list[dict] = []
        self.token_holder = runner_token_holder
        self.server = self._build_server()
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def url(self) -> str:
        host, port = self.server.server_address
        return f"http://{host}:{port}/wh"

    def _build_server(self) -> HTTPServer:
        outer = self

        class Handler(BaseHTTPRequestHandler):
            # Suppress default access logging.
            def log_message(self, *_args, **_kwargs) -> None:  # noqa: D401
                return

            def do_POST(self):  # noqa: N802
                length = int(self.headers.get("Content-Length", "0"))
                body = self.rfile.read(length)
                job_id = self.headers.get("X-Rewind-Job-Id", "")
                signature = self.headers.get("X-Rewind-Signature", "")
                outer.received.append(
                    {
                        "job_id": job_id,
                        "signature": signature,
                        "body": body.decode("utf-8"),
                    }
                )
                self.send_response(202)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(b'{"accepted": true}')

                # Async-style: spawn a thread that posts events back
                # so we don't block the dispatcher.
                threading.Thread(
                    target=outer._post_events_back,
                    args=(json.loads(body), outer.token_holder[0]),
                    daemon=True,
                ).start()

        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
        listener.close()
        return HTTPServer(("127.0.0.1", port), Handler)

    @staticmethod
    def _post_events_back(payload: dict, raw_token: str) -> None:
        time.sleep(0.05)
        base = payload["base_url"].rstrip("/")
        url = f"{base}/api/replay-jobs/{payload['job_id']}/events"
        headers = {
            "Content-Type": "application/json",
            "X-Rewind-Runner-Auth": raw_token,
        }
        for event in [
            {"event_type": "started"},
            {"event_type": "progress", "step_number": 1, "progress_total": 2},
            {"event_type": "progress", "step_number": 2, "progress_total": 2},
            {"event_type": "completed"},
        ]:
            try:
                requests.post(url, headers=headers, json=event, timeout=5)
                time.sleep(0.05)
            except Exception:
                pass

    def stop(self) -> None:
        self.server.shutdown()
        self.thread.join(timeout=5)


# ─────────────────────────────────────────────────────────────────
# Rewind server fixture
# ─────────────────────────────────────────────────────────────────


def _free_port() -> int:
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


@pytest.fixture(scope="module")
def rewind_server() -> tuple[str, str]:
    """Start the Rewind binary with a dedicated DB + crypto key.

    Yields ``(base_url, auth_token)``.
    """
    if not os.path.exists(REWIND_BIN):
        pytest.skip(f"{REWIND_BIN} not built. Run `cargo build --release`.")

    port = _free_port()
    base_url = f"http://127.0.0.1:{port}"
    db_dir = f"/tmp/rewind-e2e-{uuid.uuid4().hex[:8]}"
    os.makedirs(db_dir, exist_ok=True)

    crypto_key = base64.b64encode(secrets.token_bytes(32)).decode()
    env = {
        **os.environ,
        "REWIND_DATA_DIR": db_dir,
        "REWIND_RUNNER_SECRET_KEY": crypto_key,
        "REWIND_AUTH_TOKEN": TEST_AUTH_TOKEN,
        "REWIND_PUBLIC_URL": base_url,
        # E2E test runs against a localhost stub runner; opt out of
        # the SSRF guard for this server instance.
        "REWIND_ALLOW_LOOPBACK_WEBHOOKS": "1",
    }
    proc = subprocess.Popen(
        [REWIND_BIN, "web", "--port", str(port)],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    # Wait for /_rewind/health.
    for _ in range(60):
        try:
            r = requests.get(f"{base_url}/_rewind/health", timeout=1)
            if r.status_code == 200:
                break
        except Exception:
            pass
        time.sleep(0.25)
    else:
        proc.terminate()
        pytest.fail("rewind web server never became healthy")

    yield (base_url, TEST_AUTH_TOKEN)

    proc.terminate()
    with contextlib.suppress(Exception):
        proc.wait(timeout=5)


# ─────────────────────────────────────────────────────────────────
# The actual flow test
# ─────────────────────────────────────────────────────────────────


def _bearer(token: str) -> dict[str, str]:
    return {"Authorization": f"Bearer {token}"}


def test_full_runner_replay_flow_register_dispatch_callback(rewind_server) -> None:
    base_url, auth = rewind_server

    # 1. Register the stub-runner-to-be (we need its URL first).
    token_holder = [""]
    stub = _StubRunner(token_holder)
    try:
        # 2. Register via API.
        resp = requests.post(
            f"{base_url}/api/runners",
            headers={**_bearer(auth), "Content-Type": "application/json"},
            json={
                "name": "e2e-stub-runner",
                "mode": "webhook",
                "webhook_url": stub.url,
            },
            timeout=10,
        )
        assert resp.status_code == 201, f"register failed: {resp.status_code} {resp.text}"
        body = resp.json()
        runner_id = body["runner"]["id"]
        token_holder[0] = body["raw_token"]

        # 3. Seed a session via the explicit API.
        sess_resp = requests.post(
            f"{base_url}/api/sessions/start",
            headers={**_bearer(auth), "Content-Type": "application/json"},
            json={"name": "Phase 3 E2E"},
            timeout=10,
        )
        assert sess_resp.status_code in (200, 201), sess_resp.text
        sess_id = sess_resp.json()["session_id"]
        # Record one llm-call so the session has a step to fork from.
        record_resp = requests.post(
            f"{base_url}/api/sessions/{sess_id}/llm-calls",
            headers={**_bearer(auth), "Content-Type": "application/json"},
            json={
                "model": "stub-model",
                "request_body": {"messages": [{"role": "user", "content": "hi"}]},
                "response_body": {
                    "id": "x",
                    "choices": [{"message": {"role": "assistant", "content": "hello"}}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1},
                },
                "duration_ms": 10,
                "tokens_in": 1,
                "tokens_out": 1,
            },
            timeout=10,
        )
        assert record_resp.status_code in (200, 201), record_resp.text

        # Find the root timeline id.
        timelines_resp = requests.get(
            f"{base_url}/api/sessions/{sess_id}/timelines",
            headers=_bearer(auth),
            timeout=10,
        )
        assert timelines_resp.status_code == 200
        root_timeline_id = next(
            t["id"]
            for t in timelines_resp.json()
            if t.get("parent_timeline_id") is None
        )

        # 4. Dispatch a replay job (shape A — server forks + creates ctx).
        dispatch_resp = requests.post(
            f"{base_url}/api/sessions/{sess_id}/replay-jobs",
            headers={**_bearer(auth), "Content-Type": "application/json"},
            json={
                "runner_id": runner_id,
                "source_timeline_id": root_timeline_id,
                "at_step": 1,
            },
            timeout=10,
        )
        assert dispatch_resp.status_code == 202, dispatch_resp.text
        dispatch_body = dispatch_resp.json()
        job_id = dispatch_body["job_id"]
        assert dispatch_body["state"] == "pending"

        # 5. Wait for the stub runner to receive the dispatch.
        for _ in range(40):
            if stub.received:
                break
            time.sleep(0.1)
        assert stub.received, "stub runner never received dispatch"
        first = stub.received[0]
        assert first["job_id"] == job_id
        assert first["signature"].startswith("sha256=")
        body_json = json.loads(first["body"])
        assert body_json["job_id"] == job_id
        assert body_json["session_id"] == sess_id
        assert body_json["base_url"] == base_url

        # Verify the signature is what we'd compute locally from the raw token.
        expected_sig = hmac.new(
            token_holder[0].encode(),
            digestmod=hashlib.sha256,
        )
        expected_sig.update(job_id.encode())
        expected_sig.update(b"\n")
        expected_sig.update(first["body"].encode())
        assert first["signature"] == f"sha256={expected_sig.hexdigest()}"

        # 6. Poll job state until completed.
        for _ in range(60):
            r = requests.get(
                f"{base_url}/api/replay-jobs/{job_id}",
                headers=_bearer(auth),
                timeout=5,
            )
            if r.status_code == 200 and r.json()["state"] == "completed":
                final = r.json()
                assert final["state"] == "completed"
                assert final["progress_step"] == 2
                assert final["progress_total"] == 2
                return
            time.sleep(0.2)
        pytest.fail(
            f"job never reached completed state. last: "
            f"{requests.get(base_url + '/api/replay-jobs/' + job_id, headers=_bearer(auth)).text}"
        )
    finally:
        stub.stop()
