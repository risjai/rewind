"""HTTP transport interception for replay (Tier 1 of the Universal Replay
Architecture).

This package is the wire-format-agnostic alternative to the SDK monkey-patch
path in ``rewind_agent.patch``. It intercepts at the transport layer of
common Python HTTP clients (httpx, requests, aiohttp) so any agent making
HTTP-shaped LLM calls can record + replay without per-call-site changes.

## Quickstart

>>> from rewind_agent import intercept
>>> intercept.install()
>>> # Any subsequent httpx.Client, requests.Session, or
>>> # aiohttp.ClientSession routes through cache-then-live automatically.

## What's available

**Public API** — what most users need:

- :func:`install` — patch all importable HTTP libraries; idempotent,
  missing-library tolerant.
- :func:`uninstall` — reverse :func:`install`. Mainly for tests.
- :func:`is_installed` — check current install status.
- :func:`savings` — read the process-lifetime cache-hit savings counter
  (cache hits, tokens saved, USD estimate).
- :class:`DefaultPredicates` — strict-by-default predicate set; subclass
  to extend with custom gateway hosts. Pass to ``install(predicates=…)``.
- :class:`Predicates` — Protocol type for fully custom routing.
- :class:`SavingsSnapshot` — frozen dataclass returned by :func:`savings`.

**Lower-level building blocks** — the Phase 0 primitives, exported for
operators writing custom adapters or extending behavior:

- :class:`RewindRequest` — normalized request shape passed to predicates.
- :func:`detect_streaming` — heuristic for "does the agent expect SSE?"
- :func:`is_json_content` — content-type guard for body-sniffing.
- :func:`synthetic_sse_for_cache_hit` — wrap a buffered response body as
  a single SSE event + ``[DONE]`` sentinel.
- :data:`SSE_DONE_SENTINEL` — the literal ``b"data: [DONE]\\n\\n"``.

The transport adapters themselves (``httpx_transport``,
``requests_adapter``, ``aiohttp_middleware``) are package modules with
``patch_*`` / ``unpatch_*`` functions. Most users go through
:func:`install` / :func:`uninstall` and never reach for them directly.
"""

from rewind_agent.intercept._core import (
    SSE_DONE_SENTINEL,
    detect_streaming,
    is_json_content,
    synthetic_sse_for_cache_hit,
)
from rewind_agent.intercept._install import (
    install,
    is_installed,
    uninstall,
)
from rewind_agent.intercept._predicates import (
    DEFAULT_LLM_HOSTS,
    DefaultPredicates,
    Predicates,
    default_is_llm_call,
    default_is_tool_call,
)
from rewind_agent.intercept._request import RewindRequest
from rewind_agent.intercept._savings import (
    SavingsSnapshot,
    savings,
)

__all__ = [
    # Public API
    "install",
    "uninstall",
    "is_installed",
    "savings",
    "DefaultPredicates",
    "Predicates",
    "SavingsSnapshot",
    # Low-level Phase 0 primitives
    "RewindRequest",
    "detect_streaming",
    "is_json_content",
    "synthetic_sse_for_cache_hit",
    "SSE_DONE_SENTINEL",
    # Default predicate components for users who want to compose
    "DEFAULT_LLM_HOSTS",
    "default_is_llm_call",
    "default_is_tool_call",
]
