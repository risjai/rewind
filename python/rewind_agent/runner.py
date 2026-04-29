"""Operator-friendly runner library for Rewind dispatch webhooks.

**Phase 3 commit 9/13.**

A *runner* is a long-lived agent process that exposes an HTTP
webhook endpoint. The Rewind server POSTs replay-job dispatches to
that endpoint (HMAC-signed under the runner's auth token, see
``crates/rewind-web/src/dispatcher.rs``); the runner verifies the
signature, replies ``202 Accepted`` immediately, and asynchronously
runs the agent under the supplied ``replay_context_id``. As the
agent progresses, the runner POSTs ``started`` / ``progress`` /
``completed`` / ``errored`` events back to ``POST /api/replay-
jobs/{id}/events`` (authenticated with ``X-Rewind-Runner-Auth:
<token>`` — same token the dispatch was signed with).

This module ships:

- :class:`RunnerConfig` — env-loadable config (token, URL, base).
- :func:`verify_signature` — pure HMAC verification helper.
- :class:`ProgressReporter` — convenience wrapper around the
  events endpoint.
- :func:`asgi_handler` — ASGI route handler suitable for FastAPI /
  Starlette / aiohttp adapters; verifies the signature, replies
  202 immediately, dispatches a coroutine to user code via the
  ``@handle_replay`` decorator.
- :func:`handle_replay` — decorator for user code that processes
  a dispatched job. Receives a :class:`DispatchPayload` and a
  :class:`ProgressReporter`.

## Example

.. code-block:: python

    from fastapi import FastAPI, Request
    from rewind_agent import runner

    config = runner.RunnerConfig.from_env()  # reads REWIND_RUNNER_TOKEN, etc.
    app = FastAPI()

    @runner.handle_replay
    async def my_replay_handler(payload, reporter):
        # The dispatch is now in_progress (reporter.started() was
        # auto-emitted before this runs).
        from rewind_agent import intercept
        from rewind_agent.explicit import ExplicitClient

        client = ExplicitClient(base_url=payload.base_url)
        client.attach_replay_context(
            session_id=payload.session_id,
            replay_context_id=payload.replay_context_id,
        )
        intercept.install()

        # Re-execute the agent under the replay context.
        for i, step in enumerate(my_agent_run(), start=1):
            await reporter.progress(i)

        await reporter.completed()

    @app.post("/rewind-webhook")
    async def webhook(request: Request):
        body = await request.body()
        return await runner.asgi_handler(
            config=config,
            headers=dict(request.headers),
            body_bytes=body,
            handler=my_replay_handler,
        )
"""

from __future__ import annotations

import asyncio
import dataclasses
import hashlib
import hmac
import json
import logging
import os
import time
from collections import OrderedDict
from typing import Any, Awaitable, Callable, Optional

logger = logging.getLogger(__name__)


# ──────────────────────────────────────────────────────────────────
# Config
# ──────────────────────────────────────────────────────────────────


@dataclasses.dataclass(frozen=True)
class RunnerConfig:
    """Environment-loadable runner config.

    Attributes
    ----------
    auth_token:
        The runner's auth token (returned ONCE at registration).
        Used to verify inbound dispatch signatures AND to
        authenticate outbound event POSTs.
    rewind_base_url:
        The Rewind server base URL. Events are POSTed to
        ``{rewind_base_url}/api/replay-jobs/{job_id}/events``.
        Defaults from ``REWIND_URL`` env var.
    """

    auth_token: str
    rewind_base_url: str = "http://127.0.0.1:4800"

    @classmethod
    def from_env(cls) -> "RunnerConfig":
        """Read ``REWIND_RUNNER_TOKEN`` and ``REWIND_URL``.

        ``REWIND_RUNNER_TOKEN`` is required. ``REWIND_URL`` falls
        back to the local-dev default.
        """
        token = os.environ.get("REWIND_RUNNER_TOKEN")
        if not token:
            raise RuntimeError(
                "REWIND_RUNNER_TOKEN env var is required. Set it to the "
                "raw token returned at runner registration."
            )
        return cls(
            auth_token=token,
            rewind_base_url=os.environ.get("REWIND_URL", "http://127.0.0.1:4800"),
        )


# ──────────────────────────────────────────────────────────────────
# Dispatch payload
# ──────────────────────────────────────────────────────────────────


@dataclasses.dataclass(frozen=True)
class DispatchPayload:
    """Decoded body of a dispatch webhook from the Rewind server.

    Mirrors ``crates/rewind-web/src/dispatcher.rs::DispatchBody``.

    **Review #154 F2:** ``replay_context_timeline_id`` is the timeline
    the replay context targets — runners pass it to
    ``ExplicitClient.attach_replay_context`` so live cache misses
    record into the fork.

    **Added 2026-04-29:** ``at_step`` is the original fork-point of
    the replay-context's timeline — i.e. the step number the user
    clicked Run replay at in the dashboard. Distinct from the
    replay-context's ``from_step`` (always 0 because the agent
    re-runs from scratch). Runners use ``at_step`` to drive
    multi-turn replay: when ``at_step > 1``, fetch the source
    timeline's steps 1..at_step-1, reconstruct the conversation
    history, and invoke the agent at the right turn so edits to
    user messages in turn 2+ actually take effect.

    Defaults to ``None`` for back-compat with older servers (pre
    v0.14.8) that don't send the field.
    """

    job_id: str
    session_id: str
    replay_context_id: str
    replay_context_timeline_id: str
    base_url: str
    at_step: Optional[int] = None

    @classmethod
    def from_json(cls, body: dict[str, Any]) -> "DispatchPayload":
        return cls(
            job_id=body["job_id"],
            session_id=body["session_id"],
            replay_context_id=body["replay_context_id"],
            # Tolerate older dispatch payloads (pre-#154) by defaulting
            # to empty so the runner can still process them — but a
            # missing timeline id will result in `_timeline_id` not
            # being set, with the documented consequence.
            replay_context_timeline_id=body.get("replay_context_timeline_id", ""),
            base_url=body["base_url"],
            # Tolerate older servers (pre v0.14.8) that don't send
            # at_step. Runners that depend on it for multi-turn
            # replay should branch on `payload.at_step is not None`.
            at_step=body.get("at_step"),
        )


# ──────────────────────────────────────────────────────────────────
# Signature verification
# ──────────────────────────────────────────────────────────────────


SIGNATURE_HEADER = "X-Rewind-Signature"
JOB_ID_HEADER = "X-Rewind-Job-Id"
TIMESTAMP_HEADER = "X-Rewind-Timestamp"

#: Maximum allowable clock skew between dispatcher and runner
#: (review #154 F5). Rejects captured-and-replayed dispatches whose
#: timestamp is outside this window.
TIMESTAMP_TOLERANCE_SECS = 300  # 5 minutes


def compute_signature(
    token: str,
    job_id: str,
    body_bytes: bytes,
    timestamp: Optional[int] = None,
) -> str:
    """Compute the canonical signature for a dispatch.

    Mirrors ``crates/rewind-web/src/dispatcher.rs::compute_signature``.

    **Review #154 F5:** the signed input now includes a unix-seconds
    timestamp so replays of captured dispatches outside the tolerance
    window are rejected. ``timestamp=None`` (the default) preserves
    the legacy two-line input ``job_id || "\\n" || body`` for tests
    that pin against the pre-#154 reference vector; production
    dispatch always passes a timestamp.

    Recipe (with timestamp): ``HMAC-SHA256(token, timestamp || "\\n"
    || job_id || "\\n" || body)``, hex.
    """
    mac = hmac.new(token.encode("utf-8"), digestmod=hashlib.sha256)
    if timestamp is not None:
        mac.update(str(timestamp).encode("utf-8"))
        mac.update(b"\n")
    mac.update(job_id.encode("utf-8"))
    mac.update(b"\n")
    mac.update(body_bytes)
    return mac.hexdigest()


def verify_signature(
    *,
    config: RunnerConfig,
    headers: dict[str, str],
    body_bytes: bytes,
    now: Optional[int] = None,
) -> tuple[bool, Optional[str]]:
    """Constant-time-compare the supplied signature against the canonical one.

    Returns ``(ok, reason)``. ``ok=True`` means the request is
    authentic. ``reason`` is a short human-readable hint when
    verification fails (don't echo it back to untrusted callers).

    **Review #154 F5:** also enforces the timestamp tolerance window.
    A request whose ``X-Rewind-Timestamp`` is more than
    :data:`TIMESTAMP_TOLERANCE_SECS` seconds away from ``now`` is
    refused, defeating long-window replay attacks even if the
    attacker captured a previously-valid signature.
    """
    job_id = _header(headers, JOB_ID_HEADER)
    if not job_id:
        return False, "missing X-Rewind-Job-Id"

    sig_header = _header(headers, SIGNATURE_HEADER)
    if not sig_header or not sig_header.startswith("sha256="):
        return False, "missing or malformed X-Rewind-Signature"
    supplied = sig_header[len("sha256=") :]

    ts_header = _header(headers, TIMESTAMP_HEADER)
    timestamp: Optional[int] = None
    if ts_header is not None:
        try:
            timestamp = int(ts_header)
        except ValueError:
            return False, "malformed X-Rewind-Timestamp"
        current = now if now is not None else int(time.time())
        if abs(current - timestamp) > TIMESTAMP_TOLERANCE_SECS:
            return False, (
                f"timestamp outside tolerance: |{current} - {timestamp}| "
                f"> {TIMESTAMP_TOLERANCE_SECS}s"
            )

    expected = compute_signature(config.auth_token, job_id, body_bytes, timestamp)
    if not hmac.compare_digest(supplied, expected):
        return False, "signature mismatch"
    return True, None


def _header(headers: dict[str, str], name: str) -> Optional[str]:
    """Case-insensitive header lookup."""
    name_l = name.lower()
    for k, v in headers.items():
        if k.lower() == name_l:
            return v
    return None


# ──────────────────────────────────────────────────────────────────
# Progress reporter
# ──────────────────────────────────────────────────────────────────


class ProgressReporter:
    """Thin wrapper around the events endpoint.

    Use this from inside a ``@handle_replay`` handler to emit
    ``started`` / ``progress`` / ``completed`` / ``errored`` events.
    Built on top of httpx (already required transitively by other
    rewind_agent modules); falls back to ``urllib`` if httpx is
    absent.

    **Review #154 F4:** the constructor takes ``base_url`` directly
    so callbacks land back at the *dispatcher's* server URL rather
    than the runner's own ``REWIND_URL`` env (they may differ when
    Rewind sits behind a proxy or rewrites). :func:`asgi_handler`
    builds the reporter from ``payload.base_url``;
    ``RunnerConfig.rewind_base_url`` is the fallback for direct/test
    usage.
    """

    def __init__(
        self,
        config: RunnerConfig,
        job_id: str,
        base_url: Optional[str] = None,
    ) -> None:
        self.config = config
        self.job_id = job_id
        url_root = (base_url or config.rewind_base_url).rstrip("/")
        self._url = f"{url_root}/api/replay-jobs/{job_id}/events"

    async def started(self) -> None:
        await self._post({"event_type": "started"})

    async def progress(
        self,
        step_number: int,
        progress_total: Optional[int] = None,
        payload: Optional[dict[str, Any]] = None,
    ) -> None:
        body: dict[str, Any] = {
            "event_type": "progress",
            "step_number": step_number,
        }
        if progress_total is not None:
            body["progress_total"] = progress_total
        if payload is not None:
            body["payload"] = payload
        await self._post(body)

    async def completed(self) -> None:
        await self._post({"event_type": "completed"})

    async def errored(
        self,
        error_message: str,
        error_stage: str = "agent",
    ) -> None:
        await self._post(
            {
                "event_type": "errored",
                "error_message": error_message,
                "error_stage": error_stage,
            }
        )

    async def _post(self, body: dict[str, Any]) -> None:
        headers = {
            "Content-Type": "application/json",
            "X-Rewind-Runner-Auth": self.config.auth_token,
        }
        body_bytes = json.dumps(body).encode("utf-8")

        # Prefer httpx (async-native); fall back to urllib in a thread.
        try:
            import httpx  # noqa: PLC0415
        except ImportError:
            await asyncio.to_thread(
                _urllib_post, self._url, headers, body_bytes
            )
            return

        try:
            async with httpx.AsyncClient(timeout=10.0) as client:
                resp = await client.post(self._url, headers=headers, content=body_bytes)
                if resp.status_code >= 400:
                    logger.warning(
                        "rewind runner: event POST %s returned %s: %s",
                        body.get("event_type"),
                        resp.status_code,
                        resp.text[:200],
                    )
        except Exception as e:  # noqa: BLE001
            logger.error("rewind runner: event POST failed: %s", e)


def _urllib_post(url: str, headers: dict[str, str], body: bytes) -> None:
    """Sync fallback when httpx isn't installed."""
    import urllib.error  # noqa: PLC0415
    import urllib.request  # noqa: PLC0415

    req = urllib.request.Request(url, data=body, headers=headers, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            if resp.status >= 400:
                logger.warning("rewind runner: event POST returned %s", resp.status)
    except urllib.error.URLError as e:
        logger.error("rewind runner: event POST failed: %s", e)


# ──────────────────────────────────────────────────────────────────
# ASGI handler + decorator
# ──────────────────────────────────────────────────────────────────


HandlerFn = Callable[[DispatchPayload, ProgressReporter], Awaitable[None]]


def handle_replay(fn: HandlerFn) -> HandlerFn:
    """Marker decorator for user code that processes a dispatch.

    The decorator currently just returns the function unchanged —
    it exists so the docs and examples show a clean attribution
    point and so future versions can attach metadata or wrap with
    automatic error reporting.
    """
    return fn


#: Recently-seen job_ids for replay protection (Review #154 F5).
#: Bounded to 10_000 entries to cap memory; the dispatcher keeps
#: job_ids unique per dispatch attempt so the only way for a job_id
#: to repeat is a replay attack against the runner's webhook.
_RECENT_JOB_IDS_CAP = 10_000
_recent_job_ids: "OrderedDict[str, float]" = OrderedDict()
_recent_lock: Optional[asyncio.Lock] = None


def _get_recent_lock() -> asyncio.Lock:
    """Lazy-create the lock so module import doesn't require a loop."""
    global _recent_lock
    if _recent_lock is None:
        _recent_lock = asyncio.Lock()
    return _recent_lock


def reset_replay_protection_for_tests() -> None:
    """Reset the seen-cache + lock. Test-only — never call in prod."""
    global _recent_lock
    _recent_job_ids.clear()
    _recent_lock = None


async def _seen_job_id(job_id: str) -> bool:
    """Return True if this job_id was already accepted recently.

    The dispatcher emits each `job_id` exactly once per real
    dispatch; a duplicate is either a captured-replay attack OR a
    legitimate retry from the dispatcher (which we currently don't
    do, but defensive coding leaves room for v3.1 retries — when
    that lands we'll switch to a delivery_id field).
    """
    async with _get_recent_lock():
        if job_id in _recent_job_ids:
            return True
        _recent_job_ids[job_id] = time.time()
        # Bound the cache.
        while len(_recent_job_ids) > _RECENT_JOB_IDS_CAP:
            _recent_job_ids.popitem(last=False)
        return False


async def asgi_handler(
    *,
    config: RunnerConfig,
    headers: dict[str, str],
    body_bytes: bytes,
    handler: HandlerFn,
    auto_emit_started: bool = True,
) -> tuple[int, dict[str, Any]]:
    """Verify the signature, dispatch the handler, return ``(status, body)``.

    Plug this into your web framework. FastAPI example in the
    module docstring above; aiohttp / Starlette adapt the same way.

    The handler runs as a background task — this function returns
    ``(202, {"job_id": ...})`` immediately on signature success so
    the Rewind dispatcher's 5-second timeout is satisfied.

    **Review #154 F5:** before invoking the handler, the function
    enforces:

    1. Timestamp tolerance via :func:`verify_signature` (rejects
       replays older than :data:`TIMESTAMP_TOLERANCE_SECS`).
    2. Process-level idempotency via the ``job_id`` seen-cache —
       a captured signed dispatch within the timestamp window
       still cannot re-launch the agent because the second arrival
       hits the cache and short-circuits with 200 + ``duplicate``.

    **Review #154 F4:** the reporter handed to user code is built
    from ``payload.base_url`` so event callbacks land at the
    dispatcher's server, not the runner's local config.
    """
    ok, reason = verify_signature(
        config=config, headers=headers, body_bytes=body_bytes
    )
    if not ok:
        logger.warning("rewind runner: signature rejection — %s", reason)
        return 401, {"error": "signature verification failed"}

    try:
        body = json.loads(body_bytes)
        payload = DispatchPayload.from_json(body)
    except (ValueError, KeyError) as e:
        return 400, {"error": f"invalid dispatch body: {e}"}

    # F5: process-level idempotency. The signed input changes per
    # dispatch (timestamp varies), so an attacker can't forge a fresh
    # one — but if they captured a signed dispatch INSIDE the 5-min
    # tolerance window, the timestamp check passes. The seen-cache
    # closes that residual window at the runner.
    if await _seen_job_id(payload.job_id):
        logger.warning(
            "rewind runner: duplicate dispatch for job %s — likely replay attempt; ignoring",
            payload.job_id,
        )
        return 200, {"job_id": payload.job_id, "accepted": False, "duplicate": True}

    reporter = ProgressReporter(config, payload.job_id, base_url=payload.base_url)

    async def _run() -> None:
        if auto_emit_started:
            await reporter.started()
        try:
            await handler(payload, reporter)
        except Exception as e:  # noqa: BLE001
            logger.exception("rewind runner: handler raised")
            try:
                await reporter.errored(
                    error_message=f"handler raised: {e}",
                    error_stage="agent",
                )
            except Exception:
                logger.exception("rewind runner: errored event POST also failed")

    # Fire-and-forget: the asyncio task runs to completion in the
    # background while we return 202 immediately.
    asyncio.create_task(_run())
    return 202, {"job_id": payload.job_id, "accepted": True}
