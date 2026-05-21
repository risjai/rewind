"""Tests for rewind_agent.runner — the operator-facing runner library
that processes dispatch webhooks."""

from __future__ import annotations

import asyncio
import json
from typing import Any

import pytest

from rewind_agent import runner


# ──────────────────────────────────────────────────────────────────
# ProgressReporter
# ──────────────────────────────────────────────────────────────────


def test_progress_reporter_builds_url_from_base_url() -> None:
    reporter = runner.ProgressReporter(
        "job-x", base_url="http://dispatcher-supplied.example"
    )
    assert reporter._url == "http://dispatcher-supplied.example/api/replay-jobs/job-x/events"


def test_progress_reporter_strips_trailing_slash() -> None:
    reporter = runner.ProgressReporter(
        "job-x", base_url="http://example.com/"
    )
    assert reporter._url == "http://example.com/api/replay-jobs/job-x/events"


# ──────────────────────────────────────────────────────────────────
# DispatchPayload
# ──────────────────────────────────────────────────────────────────


def _canonical_body(**overrides: object) -> dict[str, object]:
    base: dict[str, object] = {
        "job_id": "j",
        "session_id": "s",
        "replay_context_id": "r",
        "replay_context_timeline_id": "tl-fork",
        "source_timeline_id": "tl-source",
        "at_step": 2,
        "base_url": "http://x.example",
        "dispatch_token": "tok",
    }
    base.update(overrides)
    return base


def test_dispatch_payload_decodes_canonical_body() -> None:
    body = _canonical_body()
    payload = runner.DispatchPayload.from_json(body)
    assert payload.job_id == "j"
    assert payload.session_id == "s"
    assert payload.replay_context_id == "r"
    assert payload.replay_context_timeline_id == "tl-fork"
    assert payload.source_timeline_id == "tl-source"
    assert payload.at_step == 2
    assert payload.base_url == "http://x.example"
    assert payload.dispatch_token == "tok"


def test_dispatch_payload_source_differs_from_fork() -> None:
    """source_timeline_id (read target) and replay_context_timeline_id
    (write target) are independent fields."""
    body = _canonical_body(
        replay_context_timeline_id="write-fork",
        source_timeline_id="read-source",
    )
    payload = runner.DispatchPayload.from_json(body)
    assert payload.source_timeline_id == "read-source"
    assert payload.replay_context_timeline_id == "write-fork"


def test_dispatch_payload_tolerates_extra_unknown_keys() -> None:
    """Forward-compat: future server versions may add fields. Extra
    keys in the body are ignored, not rejected."""
    body = _canonical_body(
        future_field="ignored",
        another_future_field={"nested": [1, 2, 3]},
    )
    payload = runner.DispatchPayload.from_json(body)
    assert payload.job_id == "j"
    assert payload.at_step == 2


# ──────────────────────────────────────────────────────────────────
# asgi_handler — end-to-end with a mocked event endpoint
# ──────────────────────────────────────────────────────────────────


def test_asgi_handler_invalid_dispatch_body_returns_400() -> None:
    @runner.handle_replay
    async def handler(p, r) -> None:
        pass

    async def run():
        status, resp = await runner.asgi_handler(
            body_bytes=b'{"missing_required_fields": true}',
            handler=handler,
        )
        assert status == 400
        assert "error" in resp

    asyncio.run(run())


def test_asgi_handler_dispatches_user_code_on_valid_request(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    body = json.dumps(_canonical_body()).encode()

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
    body = json.dumps(_canonical_body()).encode()

    received_events: list[dict[str, Any]] = []

    async def stub_post(self, body: dict[str, Any]) -> None:
        received_events.append(body)

    monkeypatch.setattr(runner.ProgressReporter, "_post", stub_post)

    @runner.handle_replay
    async def handler(payload, reporter) -> None:
        raise RuntimeError("agent fell over")

    async def run():
        status, _ = await runner.asgi_handler(
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


def test_asgi_handler_uses_payload_base_url_for_reporter(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The reporter built by asgi_handler should prefer the dispatch
    payload's base_url over the default."""
    body = json.dumps(
        _canonical_body(base_url="http://from-dispatch.example")
    ).encode()

    captured_reporter: list[runner.ProgressReporter] = []

    async def stub_post(self, body: dict[str, Any]) -> None:
        pass

    monkeypatch.setattr(runner.ProgressReporter, "_post", stub_post)

    @runner.handle_replay
    async def handler(payload, reporter) -> None:
        captured_reporter.append(reporter)

    async def run():
        await runner.asgi_handler(body_bytes=body, handler=handler)
        await asyncio.sleep(0.05)

    asyncio.run(run())

    assert len(captured_reporter) == 1
    assert "from-dispatch.example" in captured_reporter[0]._url


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


def test_install_bootstraps_only_when_all_three_env_vars_set(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """PR #171 round 2: bootstrap requires all three env vars
    (REWIND_SESSION_ID + REWIND_REPLAY_CONTEXT_ID +
    REWIND_REPLAY_CONTEXT_TIMELINE_ID).

    Previously, the bootstrap fired with only the first two and treated
    the timeline as optional with a WARN. That contract diverged from
    connector.setup() and caused a clobber bug — see
    test_intercept_install.TestEnvVarBootstrapContract for full coverage.
    Here we just sanity-check the subprocess path: missing timeline
    means no attach.
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
        # Without REWIND_REPLAY_CONTEXT_TIMELINE_ID, no attach happens.
        assert _session_id.get() is None
        assert _replay_context_id.get() is None
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
    """Partial env-var subset (any 1 or 2 of 3) → WARN + skip attach.

    The new contract requires all three together; a partial subset is
    operator misconfiguration that should fail loudly rather than
    half-attach with a warning (which used to leave live cache misses
    on an undefined timeline).
    """
    from rewind_agent.explicit import _replay_context_id, _session_id, _timeline_id
    from rewind_agent.intercept import _install

    monkeypatch.setenv("REWIND_SESSION_ID", "only-session")
    monkeypatch.delenv("REWIND_REPLAY_CONTEXT_ID", raising=False)
    monkeypatch.delenv("REWIND_REPLAY_CONTEXT_TIMELINE_ID", raising=False)
    _session_id.set(None)
    _replay_context_id.set(None)
    _timeline_id.set(None)

    with caplog.at_level("WARNING"):
        _install._bootstrap_replay_context_from_env()

    assert _session_id.get() is None
    assert _replay_context_id.get() is None
    assert _timeline_id.get() is None
    assert any(
        "set together" in r.message for r in caplog.records
    ), f"records={[r.message for r in caplog.records]}"
