"""HTTP transport interception for replay (Tier 1 of the Universal Replay
Architecture).

This package is the wire-format-agnostic alternative to the SDK monkey-patch
path in ``rewind_agent.patch``. It intercepts at the transport layer of
common Python HTTP clients (httpx, requests, aiohttp) so any agent making
HTTP-shaped LLM calls can record + replay without per-call-site changes.

Step 0.2 (this commit) lays down the core primitives:

- :mod:`._request` — normalized ``RewindRequest`` dataclass, the single
  request shape passed to ``is_llm_call``/``is_tool_call`` predicates
  across all three transport adapters.
- :mod:`._core` — streaming detection and synthetic SSE emission. The
  proxy/server now stores responses as a ``ResponseEnvelope`` (status +
  headers + body); the intercept layer reads the envelope on cache hit
  and re-emits it as either a buffered response or a single-chunk SSE
  stream depending on whether the agent asked for streaming.

The actual transport adapters land in Tier 1:

- ``httpx_transport.py`` — wraps ``httpx.HTTPTransport`` /
  ``httpx.AsyncHTTPTransport`` and patches ``httpx.Client.__init__`` so
  late-bound clients still get intercepted.
- ``requests_adapter.py`` — subclass of ``requests.adapters.HTTPAdapter``
  with a ``requests.Session.__init__`` patch.
- ``aiohttp_middleware.py`` — ``aiohttp.ClientSession`` middleware via
  the ``trace_configs`` API.

Public surface (for now, just the request type and core helpers — full
``install()`` arrives with the transport adapters):

>>> from rewind_agent.intercept import RewindRequest, detect_streaming
>>> req = RewindRequest(url="https://api.openai.com/v1/chat/completions",
...                     method="POST",
...                     headers={"content-type": "application/json"},
...                     body=b'{"stream":true,"messages":[]}')
>>> detect_streaming(req)
True
"""

from rewind_agent.intercept._core import (
    detect_streaming,
    is_json_content,
    synthetic_sse_for_cache_hit,
    SSE_DONE_SENTINEL,
)
from rewind_agent.intercept._request import RewindRequest

__all__ = [
    "RewindRequest",
    "detect_streaming",
    "is_json_content",
    "synthetic_sse_for_cache_hit",
    "SSE_DONE_SENTINEL",
]
