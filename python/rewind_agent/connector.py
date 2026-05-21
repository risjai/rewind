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
...     client.record_llm_call(req, resp.dict(), model="...", duration_ms=...)

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

If ``REWIND_SESSION_ID`` and ``REWIND_REPLAY_CONTEXT_ID`` are set in
the environment (the runner subprocess pattern documented in
docs/runners.md), ``setup()`` skips creating a fresh session and lets
``intercept.install()`` attach to the existing replay context. This
makes the connector safe to drop into runner-driven replay handlers
without phantom sessions.
"""

from __future__ import annotations

import logging
import os
from contextlib import contextmanager
from typing import Iterable, Iterator

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

    def __init__(self, hosts: tuple[str, ...]) -> None:
        super().__init__()
        self._hosts = tuple(h for h in (s.strip().lower() for s in hosts) if h)

    def is_llm_call(self, req) -> bool:  # type: ignore[override]
        netloc = req.url_parts.netloc.lower()
        if any(h in netloc for h in self._hosts):
            return True
        return super().is_llm_call(req)


def _resolve_hosts(llm_hosts: Iterable[str] | None) -> tuple[str, ...]:
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

    When these are set, intercept.install() will attach to the existing
    replay context — we must NOT start a fresh session in that case.
    """
    return bool(
        os.environ.get("REWIND_SESSION_ID")
        and os.environ.get("REWIND_REPLAY_CONTEXT_ID")
    )


@contextmanager
def setup(
    name: str,
    *,
    base_url: str | None = None,
    llm_hosts: Iterable[str] | None = None,
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
        Rewind server URL. Defaults to ``$REWIND_URL`` or
        ``http://127.0.0.1:4800``.
    llm_hosts:
        Iterable of hostnames to treat as LLM gateways. Defaults to
        the parsed value of ``$REWIND_LLM_HOSTS`` or no extras.
    enabled:
        Override the ``$REWIND_ENABLED`` kill switch.
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

    url = base_url or os.environ.get("REWIND_URL", "http://127.0.0.1:4800")
    client = ExplicitClient(base_url=url)
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
