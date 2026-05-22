"""``make_dispatch_payload`` — build a runner-compatible
:class:`DispatchPayload` for tests.

Public API: imported via ``from rewind_agent.testing import make_dispatch_payload``.
"""

from __future__ import annotations

from rewind_agent.runner import DispatchPayload


def make_dispatch_payload(
    *,
    session_id: str,
    job_id: str = "job-test",
    replay_context_id: str = "ctx-test",
    replay_context_timeline_id: str = "tl-fork",
    source_timeline_id: str | None = None,
    base_url: str = "http://127.0.0.1:4800",
    at_step: int = 1,
    dispatch_token: str = "tok-test",
) -> DispatchPayload:
    """Build a :class:`DispatchPayload` with sensible test defaults.

    All fields default to non-empty placeholders so a single
    ``make_dispatch_payload(session_id="s")`` call produces a valid
    payload. Override individually when a test needs to pin a specific
    value (e.g. ``at_step=5`` to exercise mid-turn replay).

    By convention, ``source_timeline_id`` defaults to
    ``replay_context_timeline_id`` (the ``ReuseContext`` shape — the
    common case in tests). Set them differently to exercise the
    ``CreateAndDispatch`` shape where edits live on the source timeline.
    """
    return DispatchPayload(
        job_id=job_id,
        session_id=session_id,
        replay_context_id=replay_context_id,
        replay_context_timeline_id=replay_context_timeline_id,
        source_timeline_id=source_timeline_id or replay_context_timeline_id,
        base_url=base_url,
        at_step=at_step,
        dispatch_token=dispatch_token,
    )
