"""Tests for rewind_agent.runner — the operator-facing runner library
that processes dispatch webhooks (Phase 3 commit 9/13)."""

from __future__ import annotations

import asyncio
import json
import time
from typing import Any

import pytest

from rewind_agent import runner


@pytest.fixture(autouse=True)
def _reset_replay_protection():
    """Each test gets a clean process-level idempotency cache so
    same-job-id reuse across tests doesn't trigger duplicate
    short-circuiting (Review #154 F5).

    Note: the lock is also reset because asyncio.Lock() instances
    are bound to the event loop they're created in; asyncio.run()
    creates a new loop per test, so a lock from a previous test
    would deadlock or panic in the next."""
    runner.reset_replay_protection_for_tests()
    yield
    runner.reset_replay_protection_for_tests()


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


def test_compute_signature_matches_rust_reference_legacy() -> None:
    """Pre-#154 two-line input — kept for backward compat.
    Pinned vs ``compute_signature_legacy_no_timestamp_matches_python_reference``.
    """
    sig = runner.compute_signature("secret", "job-1", b"body")
    assert sig == "52fd281254f1f940a5f2ad83ebce5bfbf92f77187afa03db6902bd328abd31f9"


def test_compute_signature_matches_rust_reference_with_timestamp() -> None:
    """Review #154 F5 wire format. Pinned vs
    ``compute_signature_matches_python_reference_with_timestamp`` on the Rust side.
    """
    sig = runner.compute_signature("secret", "job-1", b"body", timestamp=1700000000)
    assert sig == "ea61914f63b2516960203b8bf3f4e8ee5c9a379ca941c8bd6edef2a1681944bb"


def test_verify_signature_accepts_canonical_signature_with_timestamp() -> None:
    cfg = runner.RunnerConfig(auth_token="my-token")
    body = b'{"job_id":"x","session_id":"y","replay_context_id":"z","base_url":"u"}'
    ts = int(time.time())
    sig = runner.compute_signature(cfg.auth_token, "x", body, timestamp=ts)
    headers = {
        "X-Rewind-Job-Id": "x",
        "X-Rewind-Signature": f"sha256={sig}",
        "X-Rewind-Timestamp": str(ts),
    }
    ok, reason = runner.verify_signature(config=cfg, headers=headers, body_bytes=body)
    assert ok, f"expected ok, got reason={reason}"


def test_verify_signature_accepts_legacy_signature_without_timestamp() -> None:
    """Pre-#154 dispatchers (none in production, but defensively
    handled): no X-Rewind-Timestamp header → fall back to legacy
    two-line input. Once the dispatcher always emits a timestamp,
    this path becomes opt-in for legacy clients only."""
    cfg = runner.RunnerConfig(auth_token="my-token")
    body = b"body"
    sig = runner.compute_signature(cfg.auth_token, "x", body)  # no timestamp
    headers = {
        "X-Rewind-Job-Id": "x",
        "X-Rewind-Signature": f"sha256={sig}",
    }
    ok, _ = runner.verify_signature(config=cfg, headers=headers, body_bytes=body)
    assert ok


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
# Review #154 F5 — replay protection
# ──────────────────────────────────────────────────────────────────


def test_verify_signature_rejects_stale_timestamp() -> None:
    """Captured-and-replayed dispatch with timestamp older than
    the tolerance window must be refused even if the signature
    itself is valid for that timestamp."""
    cfg = runner.RunnerConfig(auth_token="my-token")
    body = b"body"
    stale_ts = int(time.time()) - runner.TIMESTAMP_TOLERANCE_SECS - 60
    sig = runner.compute_signature(cfg.auth_token, "x", body, timestamp=stale_ts)
    headers = {
        "X-Rewind-Job-Id": "x",
        "X-Rewind-Signature": f"sha256={sig}",
        "X-Rewind-Timestamp": str(stale_ts),
    }
    ok, reason = runner.verify_signature(config=cfg, headers=headers, body_bytes=body)
    assert not ok
    assert "tolerance" in reason


def test_verify_signature_rejects_future_timestamp() -> None:
    """Future-dated dispatches outside the tolerance are also refused
    (defensive against clock drift attacks + dispatch-time races)."""
    cfg = runner.RunnerConfig(auth_token="my-token")
    body = b"body"
    future_ts = int(time.time()) + runner.TIMESTAMP_TOLERANCE_SECS + 60
    sig = runner.compute_signature(cfg.auth_token, "x", body, timestamp=future_ts)
    headers = {
        "X-Rewind-Job-Id": "x",
        "X-Rewind-Signature": f"sha256={sig}",
        "X-Rewind-Timestamp": str(future_ts),
    }
    ok, _ = runner.verify_signature(config=cfg, headers=headers, body_bytes=body)
    assert not ok


def test_verify_signature_rejects_malformed_timestamp() -> None:
    cfg = runner.RunnerConfig(auth_token="my-token")
    headers = {
        "X-Rewind-Job-Id": "x",
        "X-Rewind-Signature": "sha256=anything",
        "X-Rewind-Timestamp": "not-a-number",
    }
    ok, reason = runner.verify_signature(config=cfg, headers=headers, body_bytes=b"")
    assert not ok
    assert "malformed" in reason


def test_asgi_handler_replay_attack_short_circuits_with_duplicate(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A second dispatch with the same job_id (within tolerance) must
    NOT re-launch the user handler — process-level idempotency."""
    runner.reset_replay_protection_for_tests()
    cfg = runner.RunnerConfig(auth_token="t")
    body = json.dumps(
        {
            "job_id": "replay-job-1",
            "session_id": "s",
            "replay_context_id": "r",
            "replay_context_timeline_id": "tl",
            "base_url": "http://x",
        }
    ).encode()
    ts = int(time.time())
    sig = runner.compute_signature(cfg.auth_token, "replay-job-1", body, timestamp=ts)
    headers = {
        "X-Rewind-Job-Id": "replay-job-1",
        "X-Rewind-Signature": f"sha256={sig}",
        "X-Rewind-Timestamp": str(ts),
    }

    invocations = []

    async def stub_post(self, body: dict[str, Any]) -> None:
        pass

    monkeypatch.setattr(runner.ProgressReporter, "_post", stub_post)

    @runner.handle_replay
    async def handler(p, r) -> None:
        invocations.append(p.job_id)

    async def run():
        # First arrival: 202 + handler runs.
        s1, b1 = await runner.asgi_handler(
            config=cfg, headers=headers, body_bytes=body, handler=handler
        )
        assert s1 == 202
        assert b1["accepted"] is True

        # Second (replay) arrival: 200 + duplicate flag, handler does NOT run.
        s2, b2 = await runner.asgi_handler(
            config=cfg, headers=headers, body_bytes=body, handler=handler
        )
        assert s2 == 200
        assert b2["accepted"] is False
        assert b2["duplicate"] is True

        # Let the first handler scheduling complete.
        await asyncio.sleep(0.05)

    asyncio.run(run())
    assert invocations == ["replay-job-1"]


# ──────────────────────────────────────────────────────────────────
# Review #154 F4 — ProgressReporter uses dispatch payload base_url
# ──────────────────────────────────────────────────────────────────


def test_progress_reporter_prefers_explicit_base_url_over_config() -> None:
    """The reporter's URL must reflect the dispatch payload's base_url,
    not the runner's local config — they diverge when Rewind runs
    behind a proxy."""
    cfg = runner.RunnerConfig(
        auth_token="t",
        rewind_base_url="http://runner-side-config.example",
    )
    reporter = runner.ProgressReporter(
        cfg, "job-x", base_url="http://dispatcher-supplied.example"
    )
    assert reporter._url == "http://dispatcher-supplied.example/api/replay-jobs/job-x/events"


def test_progress_reporter_falls_back_to_config_when_base_url_omitted() -> None:
    cfg = runner.RunnerConfig(
        auth_token="t",
        rewind_base_url="http://config.example",
    )
    reporter = runner.ProgressReporter(cfg, "job-x")
    assert reporter._url == "http://config.example/api/replay-jobs/job-x/events"


# ──────────────────────────────────────────────────────────────────
# DispatchPayload
# ──────────────────────────────────────────────────────────────────


def test_dispatch_payload_decodes_canonical_body() -> None:
    body = {
        "job_id": "j",
        "session_id": "s",
        "replay_context_id": "r",
        "replay_context_timeline_id": "tl-fork",
        "base_url": "http://x.example",
    }
    payload = runner.DispatchPayload.from_json(body)
    assert payload.job_id == "j"
    assert payload.session_id == "s"
    assert payload.replay_context_id == "r"
    assert payload.replay_context_timeline_id == "tl-fork"
    assert payload.base_url == "http://x.example"


def test_dispatch_payload_tolerates_missing_timeline_id_for_back_compat() -> None:
    """Older dispatchers (pre-#154) don't include
    replay_context_timeline_id; runner library decodes them anyway
    with an empty string. attach_replay_context will then leave
    _timeline_id unset (logged as a soft warning in user code)."""
    body = {
        "job_id": "j",
        "session_id": "s",
        "replay_context_id": "r",
        "base_url": "http://x.example",
    }
    payload = runner.DispatchPayload.from_json(body)
    assert payload.replay_context_timeline_id == ""


def test_dispatch_payload_decodes_at_step() -> None:
    """v0.14.8+ servers include `at_step` in the dispatch body — the
    fork-point of the replay-context's timeline. Runners use it to
    drive multi-turn replay (start the agent at the right turn so
    edits to user messages in turn 2+ take effect)."""
    body = {
        "job_id": "j",
        "session_id": "s",
        "replay_context_id": "r",
        "replay_context_timeline_id": "tl-fork",
        "at_step": 4,
        "base_url": "http://x.example",
    }
    payload = runner.DispatchPayload.from_json(body)
    assert payload.at_step == 4


def test_dispatch_payload_at_step_defaults_to_none_for_back_compat() -> None:
    """Older servers (pre v0.14.8) don't send `at_step`. The SDK
    decodes the body anyway; runners that need at_step branch on
    `payload.at_step is not None` and fall back to single-turn
    behavior when it's missing."""
    body = {
        "job_id": "j",
        "session_id": "s",
        "replay_context_id": "r",
        "replay_context_timeline_id": "tl-fork",
        "base_url": "http://x.example",
    }
    payload = runner.DispatchPayload.from_json(body)
    assert payload.at_step is None


def test_dispatch_payload_tolerates_extra_unknown_keys() -> None:
    """Forward-compat: future server versions may add fields. Older
    runner SDKs hitting newer servers must keep working — extra
    keys in the body are ignored, not rejected. This pins the
    contract that lets us add fields without breaking deployed
    runners (no HMAC concern: the server signs the wire bytes;
    the runner verifies against raw `request.body`, so extra
    fields are part of the signed payload on both ends)."""
    body = {
        "job_id": "j",
        "session_id": "s",
        "replay_context_id": "r",
        "replay_context_timeline_id": "tl-fork",
        "at_step": 4,
        "base_url": "http://x.example",
        "future_field_added_in_v0_14_99": "ignored",
        "another_future_field": {"nested": [1, 2, 3]},
    }
    payload = runner.DispatchPayload.from_json(body)
    assert payload.job_id == "j"
    assert payload.at_step == 4


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
    from rewind_agent.explicit import _replay_context_id, _session_id, _timeline_id
    from rewind_agent.intercept import _install

    monkeypatch.setenv("REWIND_SESSION_ID", "boot-sess")
    monkeypatch.setenv("REWIND_REPLAY_CONTEXT_ID", "boot-ctx")
    monkeypatch.delenv("REWIND_REPLAY_CONTEXT_TIMELINE_ID", raising=False)
    monkeypatch.setenv("REWIND_URL", "http://127.0.0.1:4800")

    _install._INSTALLED = False
    _session_id.set(None)
    _replay_context_id.set(None)
    _timeline_id.set(None)

    try:
        _install._bootstrap_replay_context_from_env()
        assert _session_id.get() == "boot-sess"
        assert _replay_context_id.get() == "boot-ctx"
        # Without REWIND_REPLAY_CONTEXT_TIMELINE_ID, _timeline_id
        # stays unset (with a logged WARN — see the documented
        # subprocess-bootstrap caveat).
        assert _timeline_id.get() is None
    finally:
        _session_id.set(None)
        _replay_context_id.set(None)
        _timeline_id.set(None)


def test_install_bootstraps_with_timeline_id_from_env(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Review #154 round 2: env-var bootstrap also honors
    REWIND_REPLAY_CONTEXT_TIMELINE_ID. Subprocess-bootstrap paths
    that previously left _timeline_id unset now propagate the fork
    timeline so live cache misses record into the right place."""
    from rewind_agent.explicit import _replay_context_id, _session_id, _timeline_id
    from rewind_agent.intercept import _install

    monkeypatch.setenv("REWIND_SESSION_ID", "boot-sess")
    monkeypatch.setenv("REWIND_REPLAY_CONTEXT_ID", "boot-ctx")
    monkeypatch.setenv("REWIND_REPLAY_CONTEXT_TIMELINE_ID", "boot-fork-tl")
    monkeypatch.setenv("REWIND_URL", "http://127.0.0.1:4800")

    _install._INSTALLED = False
    _session_id.set(None)
    _replay_context_id.set(None)
    _timeline_id.set(None)

    try:
        _install._bootstrap_replay_context_from_env()
        assert _session_id.get() == "boot-sess"
        assert _replay_context_id.get() == "boot-ctx"
        assert _timeline_id.get() == "boot-fork-tl"
    finally:
        _session_id.set(None)
        _replay_context_id.set(None)
        _timeline_id.set(None)


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
