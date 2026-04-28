"""Tests for rewind_agent.runner — the operator-facing runner library
that processes dispatch webhooks (Phase 3 commit 9/13)."""

from __future__ import annotations

import asyncio
import json
from typing import Any

import pytest

from rewind_agent import runner


# ──────────────────────────────────────────────────────────────────
# RunnerConfig.from_env
# ──────────────────────────────────────────────────────────────────


def test_from_env_requires_token(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("REWIND_RUNNER_TOKEN", raising=False)
    with pytest.raises(RuntimeError, match="REWIND_RUNNER_TOKEN"):
        runner.RunnerConfig.from_env()


def test_from_env_uses_defaults_for_url(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("REWIND_RUNNER_TOKEN", "abc")
    monkeypatch.delenv("REWIND_URL", raising=False)
    cfg = runner.RunnerConfig.from_env()
    assert cfg.auth_token == "abc"
    assert cfg.rewind_base_url == "http://127.0.0.1:4800"


def test_from_env_honors_rewind_url(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("REWIND_RUNNER_TOKEN", "abc")
    monkeypatch.setenv("REWIND_URL", "https://rewind.example/api")
    cfg = runner.RunnerConfig.from_env()
    assert cfg.rewind_base_url == "https://rewind.example/api"


# ──────────────────────────────────────────────────────────────────
# Signature verification (cross-checked against Rust impl)
# ──────────────────────────────────────────────────────────────────


def test_compute_signature_matches_rust_reference() -> None:
    """Pin the wire-format. Same vector as
    ``crates/rewind-web/src/dispatcher.rs::compute_signature_matches_python_reference``.
    """
    sig = runner.compute_signature("secret", "job-1", b"body")
    assert sig == "52fd281254f1f940a5f2ad83ebce5bfbf92f77187afa03db6902bd328abd31f9"


def test_verify_signature_accepts_canonical_signature() -> None:
    cfg = runner.RunnerConfig(auth_token="my-token")
    body = b'{"job_id":"x","session_id":"y","replay_context_id":"z","base_url":"u"}'
    sig = runner.compute_signature(cfg.auth_token, "x", body)
    headers = {
        "X-Rewind-Job-Id": "x",
        "X-Rewind-Signature": f"sha256={sig}",
    }
    ok, reason = runner.verify_signature(config=cfg, headers=headers, body_bytes=body)
    assert ok, f"expected ok, got reason={reason}"


def test_verify_signature_rejects_missing_job_id_header() -> None:
    cfg = runner.RunnerConfig(auth_token="my-token")
    headers = {"X-Rewind-Signature": "sha256=abc"}
    ok, reason = runner.verify_signature(config=cfg, headers=headers, body_bytes=b"")
    assert not ok
    assert "Job-Id" in reason


def test_verify_signature_rejects_missing_signature_header() -> None:
    cfg = runner.RunnerConfig(auth_token="my-token")
    headers = {"X-Rewind-Job-Id": "x"}
    ok, reason = runner.verify_signature(config=cfg, headers=headers, body_bytes=b"")
    assert not ok
    assert "Signature" in reason


def test_verify_signature_rejects_malformed_signature_prefix() -> None:
    cfg = runner.RunnerConfig(auth_token="my-token")
    headers = {
        "X-Rewind-Job-Id": "x",
        # Missing "sha256=" prefix.
        "X-Rewind-Signature": "abc123",
    }
    ok, reason = runner.verify_signature(config=cfg, headers=headers, body_bytes=b"")
    assert not ok
    assert "Signature" in reason


def test_verify_signature_rejects_tampered_body() -> None:
    cfg = runner.RunnerConfig(auth_token="my-token")
    body = b'{"k":"v"}'
    sig = runner.compute_signature(cfg.auth_token, "x", body)
    tampered_body = b'{"k":"DIFFERENT"}'
    headers = {
        "X-Rewind-Job-Id": "x",
        "X-Rewind-Signature": f"sha256={sig}",
    }
    ok, reason = runner.verify_signature(
        config=cfg, headers=headers, body_bytes=tampered_body
    )
    assert not ok
    assert "mismatch" in reason


def test_verify_signature_rejects_wrong_token() -> None:
    body = b"body"
    cfg_signing = runner.RunnerConfig(auth_token="signing-key")
    cfg_verifying = runner.RunnerConfig(auth_token="different-key")
    sig = runner.compute_signature(cfg_signing.auth_token, "x", body)
    headers = {
        "X-Rewind-Job-Id": "x",
        "X-Rewind-Signature": f"sha256={sig}",
    }
    ok, _ = runner.verify_signature(
        config=cfg_verifying, headers=headers, body_bytes=body
    )
    assert not ok


def test_verify_signature_is_case_insensitive_for_header_lookup() -> None:
    cfg = runner.RunnerConfig(auth_token="my-token")
    body = b"body"
    sig = runner.compute_signature(cfg.auth_token, "x", body)
    headers = {
        "x-rewind-job-id": "x",  # lowercase
        "x-rewind-signature": f"sha256={sig}",  # lowercase
    }
    ok, _ = runner.verify_signature(config=cfg, headers=headers, body_bytes=body)
    assert ok


# ──────────────────────────────────────────────────────────────────
# DispatchPayload
# ──────────────────────────────────────────────────────────────────


def test_dispatch_payload_decodes_canonical_body() -> None:
    body = {
        "job_id": "j",
        "session_id": "s",
        "replay_context_id": "r",
        "base_url": "http://x.example",
    }
    payload = runner.DispatchPayload.from_json(body)
    assert payload.job_id == "j"
    assert payload.session_id == "s"
    assert payload.replay_context_id == "r"
    assert payload.base_url == "http://x.example"


# ──────────────────────────────────────────────────────────────────
# asgi_handler — end-to-end with a mocked event endpoint
# ──────────────────────────────────────────────────────────────────


def test_asgi_handler_rejects_invalid_signature() -> None:
    cfg = runner.RunnerConfig(auth_token="t")
    body = json.dumps(
        {"job_id": "j", "session_id": "s", "replay_context_id": "r", "base_url": "u"}
    ).encode()

    called = False

    @runner.handle_replay
    async def handler(p, r) -> None:
        nonlocal called
        called = True

    async def run():
        status, resp = await runner.asgi_handler(
            config=cfg,
            headers={
                "X-Rewind-Job-Id": "j",
                "X-Rewind-Signature": "sha256=deadbeef",
            },
            body_bytes=body,
            handler=handler,
        )
        assert status == 401
        assert "error" in resp
        # Give the loop a tick — handler must NOT have been scheduled.
        await asyncio.sleep(0.05)
        assert not called

    asyncio.run(run())


def test_asgi_handler_invalid_dispatch_body_returns_400() -> None:
    cfg = runner.RunnerConfig(auth_token="t")
    body = b'{"missing_required_fields": true}'
    sig = runner.compute_signature(cfg.auth_token, "j", body)

    @runner.handle_replay
    async def handler(p, r) -> None:
        pass

    async def run():
        status, resp = await runner.asgi_handler(
            config=cfg,
            headers={
                "X-Rewind-Job-Id": "j",
                "X-Rewind-Signature": f"sha256={sig}",
            },
            body_bytes=body,
            handler=handler,
        )
        assert status == 400
        assert "error" in resp

    asyncio.run(run())


def test_asgi_handler_dispatches_user_code_on_valid_request(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cfg = runner.RunnerConfig(auth_token="t")
    body = json.dumps(
        {"job_id": "j", "session_id": "s", "replay_context_id": "r", "base_url": "u"}
    ).encode()
    sig = runner.compute_signature(cfg.auth_token, "j", body)

    # Stub the ProgressReporter._post so we don't hit the network.
    received_events: list[dict[str, Any]] = []

    async def stub_post(self, body: dict[str, Any]) -> None:
        received_events.append(body)

    monkeypatch.setattr(runner.ProgressReporter, "_post", stub_post)

    handler_called_with: list[runner.DispatchPayload] = []

    @runner.handle_replay
    async def handler(payload: runner.DispatchPayload, reporter: runner.ProgressReporter) -> None:
        handler_called_with.append(payload)
        await reporter.progress(1, progress_total=3)
        await reporter.progress(2)
        await reporter.completed()

    async def run():
        status, resp = await runner.asgi_handler(
            config=cfg,
            headers={
                "X-Rewind-Job-Id": "j",
                "X-Rewind-Signature": f"sha256={sig}",
            },
            body_bytes=body,
            handler=handler,
        )
        assert status == 202
        assert resp == {"job_id": "j", "accepted": True}

        for _ in range(30):
            if received_events and received_events[-1].get("event_type") == "completed":
                break
            await asyncio.sleep(0.05)
        else:
            raise AssertionError(f"handler did not finish; events={received_events}")

    asyncio.run(run())

    assert len(handler_called_with) == 1
    assert handler_called_with[0].job_id == "j"
    types = [e["event_type"] for e in received_events]
    assert types == ["started", "progress", "progress", "completed"]
    assert received_events[1]["step_number"] == 1
    assert received_events[1]["progress_total"] == 3


def test_asgi_handler_emits_errored_when_user_code_raises(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cfg = runner.RunnerConfig(auth_token="t")
    body = json.dumps(
        {"job_id": "j", "session_id": "s", "replay_context_id": "r", "base_url": "u"}
    ).encode()
    sig = runner.compute_signature(cfg.auth_token, "j", body)

    received_events: list[dict[str, Any]] = []

    async def stub_post(self, body: dict[str, Any]) -> None:
        received_events.append(body)

    monkeypatch.setattr(runner.ProgressReporter, "_post", stub_post)

    @runner.handle_replay
    async def handler(payload, reporter) -> None:
        raise RuntimeError("agent fell over")

    async def run():
        status, _ = await runner.asgi_handler(
            config=cfg,
            headers={
                "X-Rewind-Job-Id": "j",
                "X-Rewind-Signature": f"sha256={sig}",
            },
            body_bytes=body,
            handler=handler,
        )
        assert status == 202

        for _ in range(30):
            if any(e.get("event_type") == "errored" for e in received_events):
                break
            await asyncio.sleep(0.05)
        else:
            raise AssertionError(f"errored event never emitted; got: {received_events}")

    asyncio.run(run())

    err = next(e for e in received_events if e["event_type"] == "errored")
    assert "agent fell over" in err["error_message"]
    assert err["error_stage"] == "agent"


# ──────────────────────────────────────────────────────────────────
# attach_replay_context (env-var bootstrap)
# ──────────────────────────────────────────────────────────────────


def test_attach_replay_context_sets_contextvars() -> None:
    from rewind_agent.explicit import (
        ExplicitClient,
        _replay_context_id,
        _session_id,
    )

    client = ExplicitClient(base_url="http://127.0.0.1:4800")
    client.attach_replay_context(
        session_id="sess-attach", replay_context_id="ctx-attach"
    )
    assert _session_id.get() == "sess-attach"
    assert _replay_context_id.get() == "ctx-attach"


def test_install_bootstraps_from_env(monkeypatch: pytest.MonkeyPatch) -> None:
    """``intercept.install()`` reads REWIND_SESSION_ID +
    REWIND_REPLAY_CONTEXT_ID and attaches before patching.
    """
    from rewind_agent.explicit import _replay_context_id, _session_id
    from rewind_agent.intercept import _install

    monkeypatch.setenv("REWIND_SESSION_ID", "boot-sess")
    monkeypatch.setenv("REWIND_REPLAY_CONTEXT_ID", "boot-ctx")
    monkeypatch.setenv("REWIND_URL", "http://127.0.0.1:4800")

    _install._INSTALLED = False
    _session_id.set(None)
    _replay_context_id.set(None)

    try:
        _install._bootstrap_replay_context_from_env()
        assert _session_id.get() == "boot-sess"
        assert _replay_context_id.get() == "boot-ctx"
    finally:
        _session_id.set(None)
        _replay_context_id.set(None)


def test_install_partial_env_logs_warning_and_skips(
    monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture
) -> None:
    from rewind_agent.explicit import _replay_context_id, _session_id
    from rewind_agent.intercept import _install

    monkeypatch.setenv("REWIND_SESSION_ID", "only-session")
    monkeypatch.delenv("REWIND_REPLAY_CONTEXT_ID", raising=False)
    _session_id.set(None)
    _replay_context_id.set(None)

    with caplog.at_level("WARNING"):
        _install._bootstrap_replay_context_from_env()

    assert _session_id.get() is None
    assert _replay_context_id.get() is None
    assert any(
        "must be set together" in r.message for r in caplog.records
    ), f"records={[r.message for r in caplog.records]}"
