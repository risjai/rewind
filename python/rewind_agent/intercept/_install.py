"""Install orchestrator for the Phase 1 transport adapters.

Single-call entry point — :func:`install` patches every importable
HTTP library so any subsequent client construction routes through
cache-then-live. Idempotent and missing-library tolerant.

## Design constraints

The orchestrator never raises on missing libraries. Production agents
typically have ONE of httpx / requests / aiohttp installed (the LLM
client they actually use), and dragging in the other two as runtime
dependencies would force users to install bloat they'll never run.
Instead, each adapter's ``patch_*`` function checks its own
``*_AVAILABLE`` flag and silently no-ops when the library isn't
importable. ``install()`` calls all three; absent libraries log a
``DEBUG`` line and skip.

Idempotency is required because:

1. **Library re-import.** Hot-reload tooling (jupyter, ipython auto-
   reload, watchexec) can re-execute a module that calls ``install()``;
   double-patching would break the original __init__ chain.
2. **Multiple entry points.** A pytest conftest, a service main, AND
   an MCP server may all want to call ``install()`` defensively.
3. **Re-install with new predicates.** Operators experimenting in a
   REPL want to swap predicates without restarting; first call
   :func:`uninstall`, then :func:`install` with the new predicates.

## Custom predicates

Pass a :class:`Predicates` (Protocol) implementer to ``install``.
The most common pattern is to subclass :class:`DefaultPredicates`
and add custom hosts:

>>> from rewind_agent.intercept import install, DefaultPredicates
>>> class CorpPredicates(DefaultPredicates):
...     def is_llm_call(self, req):
...         host = req.url_parts.netloc.lower()
...         if host.endswith(".llm-gateway.corp.example"):
...             return True
...         return super().is_llm_call(req)
>>> install(predicates=CorpPredicates())
"""

from __future__ import annotations

import logging

from rewind_agent.intercept._predicates import Predicates
from rewind_agent.intercept import (
    aiohttp_middleware,
    httpx_transport,
    requests_adapter,
)

logger = logging.getLogger(__name__)


_INSTALLED = False


def install(predicates: Predicates | None = None) -> None:
    """Patch all importable HTTP transports for cache-then-live routing.

    Single-call setup. Subsequent client construction (``httpx.Client``,
    ``requests.Session``, ``aiohttp.ClientSession``) gets our adapters
    automatically — no per-call-site changes required.

    Idempotent: second call is a no-op (no double-patching, no error).
    To switch predicates after install, call :func:`uninstall` first.

    Library availability:

    - **httpx** — patched if importable (most common; OpenAI ≥ 1.0,
      Anthropic SDK).
    - **requests** — patched if importable (legacy / homegrown LLM
      clients).
    - **aiohttp** — patched if importable (pure-async stacks).

    Missing libraries silently skip. ``install()`` always succeeds
    even when no LLM-using libraries are present (unusual but
    legitimate for tests / config-time imports).

    Parameters
    ----------
    predicates:
        Optional :class:`Predicates` implementer that decides which
        requests get the record/replay treatment. Defaults to
        :class:`DefaultPredicates` (strict-by-default; matches only
        the listed LLM provider hosts). Custom predicates are applied
        across ALL three adapters consistently.

    Examples
    --------

    Basic install with defaults:

    >>> from rewind_agent import intercept
    >>> intercept.install()

    Custom gateway predicate:

    >>> from rewind_agent.intercept import DefaultPredicates
    >>> class MyPredicates(DefaultPredicates):
    ...     def is_llm_call(self, req):
    ...         if "my-corp.example" in req.url_parts.netloc:
    ...             return True
    ...         return super().is_llm_call(req)
    >>> intercept.install(predicates=MyPredicates())
    """
    global _INSTALLED
    if _INSTALLED:
        logger.debug("rewind: intercept.install() already active; ignoring")
        return

    httpx_transport.patch_httpx_clients(predicates)
    requests_adapter.patch_requests_sessions(predicates)
    aiohttp_middleware.patch_aiohttp_sessions(predicates)

    _INSTALLED = True
    _log_install_status()


def uninstall() -> None:
    """Reverse :func:`install`. Mainly for tests; production agents
    rarely uninstall the intercept layer.

    Idempotent: safe to call when not installed. Restores each library's
    original ``__init__`` / ``send`` / ``_request`` so subsequent client
    construction uses the unmodified transport.

    Pre-existing client instances (constructed during the install
    window) keep their adapter reference — we don't try to mutate
    live instances. Documented behavior across each adapter.
    """
    global _INSTALLED
    if not _INSTALLED:
        return

    httpx_transport.unpatch_httpx_clients()
    requests_adapter.unpatch_requests_sessions()
    aiohttp_middleware.unpatch_aiohttp_sessions()

    _INSTALLED = False
    logger.debug("rewind: intercept.uninstall() complete")


def is_installed() -> bool:
    """True if :func:`install` has been called and not since
    :func:`uninstall`'d.
    """
    return _INSTALLED


def _log_install_status() -> None:
    """One-line summary of which adapters actually patched. Visible at
    DEBUG level; bumps to INFO if NO adapter patched (suspicious — agent
    probably has none of httpx/requests/aiohttp installed).
    """
    patched = []
    if httpx_transport.HTTPX_AVAILABLE and httpx_transport.is_patched():
        patched.append("httpx")
    if requests_adapter.REQUESTS_AVAILABLE and requests_adapter.is_patched():
        patched.append("requests")
    if aiohttp_middleware.AIOHTTP_AVAILABLE and aiohttp_middleware.is_patched():
        patched.append("aiohttp")

    if patched:
        logger.debug(
            "rewind: intercept.install() patched: %s", ", ".join(patched)
        )
    else:
        logger.info(
            "rewind: intercept.install() patched no transports — "
            "neither httpx, requests, nor aiohttp is importable. "
            "Install one to enable record/replay."
        )
