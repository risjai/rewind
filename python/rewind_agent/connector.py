"""One-call connector for any agent that wants Rewind recording.

The Tier-1 ergonomic wrapper described in [docs/hdk.md](docs/hdk.md).
Composes :class:`ExplicitClient` + :func:`intercept.install` in the
correct order so callers don't have to remember the dance — and
don't hit the silent-no-op trap where intercept records nothing
because no session is active.

Usage
-----

>>> import rewind_agent
>>> with rewind_agent.connector.setup(name="my-agent"):
...     run_agent_loop()

For a custom LLM gateway hostname, pass ``llm_hosts`` (or set
``REWIND_LLM_HOSTS`` in the env):

>>> with rewind_agent.connector.setup(
...     name="my-agent",
...     llm_hosts=("llm-gateway.corp.example",),
... ):
...     ...

The yielded value is the underlying :class:`ExplicitClient`, available
for non-HTTP record paths inside the block:

>>> with rewind_agent.connector.setup(name="my-agent") as client:
...     resp = my_grpc_llm.chat(req)  # not HTTP — intercept can't see it
...     client.record_llm_call(
...         request=req, response=resp.dict(),
...         model="my-private-llm", duration_ms=duration_ms,
...     )

This is a *sync* context manager (``@contextmanager``). Async callers
should still use plain ``with``, not ``async with``: the body of the
block can be async — only the setup and teardown are synchronous.

Environment variables (env > default; kwargs override env)
----------------------------------------------------------

* ``REWIND_ENABLED`` — set to ``0`` to make ``setup()`` a no-op with
  zero overhead. Yields ``None`` instead of a client; the ``with``
  block runs unmodified.
* ``REWIND_URL`` — Rewind server URL. Default ``http://127.0.0.1:4800``.
* ``REWIND_LLM_HOSTS`` — comma-separated hostnames to treat as LLM
  gateways in addition to the strict-by-default provider list.

Replay-context interaction
--------------------------

If ``REWIND_SESSION_ID``, ``REWIND_REPLAY_CONTEXT_ID`` *and*
``REWIND_REPLAY_CONTEXT_TIMELINE_ID`` are all set in the environment
(the runner subprocess pattern documented in docs/runners.md),
``setup()`` skips creating a fresh session and lets
``intercept.install()`` attach to the existing replay context. This
makes the connector safe to drop into runner-driven replay handlers
without phantom sessions.

All three env vars are required — without ``REWIND_REPLAY_CONTEXT_TIMELINE_ID``
live cache misses during replay land on an undefined timeline. If only
some are set, the connector treats it as a misconfigured replay and
falls through to the normal session-start path so recording still works.
"""

from __future__ import annotations

import logging
import os
from contextlib import contextmanager
from typing import Iterator, Sequence

from rewind_agent.explicit import ExplicitClient
from rewind_agent.intercept import (
    DefaultPredicates,
    install,
    is_installed,
    uninstall,
)

logger = logging.getLogger(__name__)


class _HostPredicates(DefaultPredicates):
    """DefaultPredicates extended with caller-provided LLM gateway hostnames."""

    def __init__(self, hosts: Sequence[str]) -> None:
        super().__init__()
        self._hosts = tuple(h for h in (s.strip().lower() for s in hosts) if h)

    def is_llm_call(self, req) -> bool:  # type: ignore[override]
        netloc = req.url_parts.netloc.lower()
        if any(h in netloc for h in self._hosts):
            return True
        return super().is_llm_call(req)


def _resolve_hosts(llm_hosts: Sequence[str] | None) -> tuple[str, ...]:
    """Collect extra LLM gateway hostnames from kwarg or ``$REWIND_LLM_HOSTS``.

    Returns an empty tuple when nothing is configured. Callers MUST treat
    ``()`` as "use intercept's :class:`DefaultPredicates`" — do not wrap
    an empty :class:`_HostPredicates` here, that would silently broaden
    matching to no hosts (always-False fallback) which is not the same as
    the strict-by-default provider list.
    """
    if llm_hosts is not None:
        return tuple(llm_hosts)
    env = os.environ.get("REWIND_LLM_HOSTS", "")
    return tuple(h for h in env.split(",") if h.strip()) if env else ()


def _enabled(enabled: bool | None) -> bool:
    if enabled is not None:
        return enabled
    return os.environ.get("REWIND_ENABLED", "1") != "0"


def _is_replay_dispatch() -> bool:
    """Detect runner-subprocess replay env vars.

    When all three are set, intercept.install() will attach to the existing
    replay context — we must NOT start a fresh session in that case.

    All three (``REWIND_SESSION_ID``, ``REWIND_REPLAY_CONTEXT_ID``,
    ``REWIND_REPLAY_CONTEXT_TIMELINE_ID``) are required for replay mode to
    activate. If only some are set, the bootstrap path inside
    ``intercept._install`` warns that live cache misses will not have a
    defined recording target — which is a half-broken replay. Treat
    "partial replay env" as "not replay" so the connector falls through
    to the normal session-start path and recording works correctly.
    """
    # Truthiness, not `is not None`: an empty-string env var is the same
    # as unset for our purposes (`REWIND_SESSION_ID=""` is operator
    # misconfiguration, not a valid id). Don't "fix" this to `is not None`.
    return bool(
        os.environ.get("REWIND_SESSION_ID")
        and os.environ.get("REWIND_REPLAY_CONTEXT_ID")
        and os.environ.get("REWIND_REPLAY_CONTEXT_TIMELINE_ID")
    )


@contextmanager
def setup(
    name: str,
    *,
    base_url: str | None = None,
    llm_hosts: Sequence[str] | None = None,
    enabled: bool | None = None,
    thread_id: str | None = None,
    metadata: dict | None = None,
) -> Iterator[ExplicitClient | None]:
    """Connect any agent to Rewind for the duration of a ``with`` block.

    Starts a session, installs HTTP intercept (with custom predicates
    when ``llm_hosts`` is set), yields the :class:`ExplicitClient` for
    use inside the block, and tears both down on exit.

    Parameters
    ----------
    name:
        Session name shown in ``rewind show`` and the dashboard.
    base_url:
        Rewind server URL. ``None`` (default) consults
        :class:`ExplicitClient`'s resolution: ``$REWIND_URL``, then
        ``http://127.0.0.1:4800``.
    llm_hosts:
        Sequence of hostnames to treat as LLM gateways. ``None``
        (default) reads ``$REWIND_LLM_HOSTS``; empty / unset falls
        through to intercept's strict-by-default provider list.
    enabled:
        ``None`` (default) reads ``$REWIND_ENABLED`` (any value other
        than ``"0"`` is on); ``True`` forces on; ``False`` forces off.
        When off, ``setup()`` is a true no-op — yields ``None``, no HTTP,
        no install.
    thread_id, metadata:
        Forwarded to :meth:`ExplicitClient.session`.

    Yields
    ------
    ExplicitClient | None
        The recording client, or ``None`` when disabled.
    """
    if not _enabled(enabled):
        yield None
        return

    # base_url resolution lives in ExplicitClient.__init__ so all callers
    # share a single source of truth (kwarg > $REWIND_URL > localhost).
    client = ExplicitClient(base_url=base_url)
    hosts = _resolve_hosts(llm_hosts)
    predicates = _HostPredicates(hosts) if hosts else None

    if _is_replay_dispatch():
        # Runner-driven replay: intercept.install() will attach to the
        # existing replay context via env vars. Don't create a phantom
        # session.
        already_installed = is_installed()
        install(predicates=predicates)
        try:
            yield client
        finally:
            if not already_installed:
                uninstall()
        return

    with client.session(name, thread_id=thread_id, metadata=metadata):
        already_installed = is_installed()
        install(predicates=predicates)
        try:
            yield client
        finally:
            if not already_installed:
                uninstall()
