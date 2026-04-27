"""``aiohttp`` middleware for rewind intercept.

Phase 1 of the Universal Replay Architecture. Patches
``aiohttp.ClientSession._request`` so any session constructed after
:func:`rewind_agent.intercept.install` routes through cache-then-live.

## Why monkey-patch and not TraceConfig

aiohttp ships ``aiohttp.TraceConfig`` for hooking into client-request
lifecycle events (``on_request_start``, ``on_response_chunk_received``,
etc.). It's the official extension point — but it can only **observe**
requests, not short-circuit them. There's no trace event that lets us
return a synthetic response in place of the live one. For Phase 1 we
need the cache-hit path to bypass the network entirely.

The plan's stated fallback was to monkey-patch ``ClientSession._request``,
which is what we do here. This is version-fragile (private API
boundary), so we test against the aiohttp range we actually support
(3.8 +) and document the fragility. If aiohttp 4.x changes the
internal signature, the patch logic in :func:`patch_aiohttp_sessions`
needs updating; the public API (``intercept.install()``) remains
stable.

## Synthetic response shape

aiohttp's ``ClientResponse`` is heavyweight and constructed by the
session as it reads the wire. For cache hits we use
:class:`_SyntheticClientResponse` which exposes the methods agents
typically reach for: ``.status``, ``.headers``, ``.json()`` (async),
``.text()`` (async), ``.read()`` (async), and async iteration via
``response.content``. That covers the OpenAI SDK, Anthropic SDK,
LangChain, and direct-aiohttp users.

## What's NOT covered

- aiohttp's ``raise_for_status`` per-session config — we always return
  status=200 on cache hit; if the original recorded a non-2xx the
  synthetic response misses that signal. v1 limitation; revisit if a
  user reports it.
- WebSocket upgrade requests — different code path
  (``ClientSession._ws_connect``); intercept doesn't touch them.
- Streaming uploads (request body as an async iterator) — v1 falls
  back to empty body for predicate / cache, same caveat as the other
  two adapters.
"""

from __future__ import annotations

import logging
from typing import Any, AsyncIterator

from rewind_agent.intercept._flow import handle_intercepted_async
from rewind_agent.intercept._predicates import DefaultPredicates, Predicates
from rewind_agent.intercept._request import RewindRequest

logger = logging.getLogger(__name__)


# Conditional import — aiohttp is the most-likely-missing of the three
# library deps because async stacks are less universal than sync.
# Bare `import aiohttp` would be flagged unused (we only call ClientSession
# directly, and the patch-target is ``ClientSession._request``); we rely
# on the from-import for ImportError detection. ``aiohttp`` is also
# referenced via the `aiohttp.ClientSession._request` attribute path the
# tests patch, but that's runtime introspection, not a static import use.
try:
    from aiohttp import ClientSession
    from multidict import CIMultiDict, CIMultiDictProxy

    AIOHTTP_AVAILABLE = True
except ImportError:  # pragma: no cover — environment-detection path
    AIOHTTP_AVAILABLE = False


_ORIGINAL_REQUEST = None
_PATCHED = False


# ── Synthetic ClientResponse ───────────────────────────────────────


class _SyntheticClientResponse:
    """Quacks-like-ClientResponse for cache-hit short-circuit.

    Implements the surface that real aiohttp consumers use:

    - ``.status`` (int)
    - ``.headers`` (CIMultiDictProxy)
    - ``.url`` (yarl.URL or str)
    - ``await .read()`` → bytes
    - ``await .text()`` → str
    - ``await .json()`` → dict
    - ``async for chunk in response.content`` → async iter over bytes

    Plus ``__aenter__`` / ``__aexit__`` so ``async with session.post(...) as resp:``
    works the same as on a real ClientResponse.

    Anything beyond this surface (cookies, history, raw connection
    access) returns ``None`` / sensible default. If a real consumer
    hits one of those it'll see a clear AttributeError or behavior
    drift; documented.
    """

    def __init__(
        self,
        *,
        status: int,
        headers: dict[str, str],
        body: bytes,
        url: str,
    ) -> None:
        self.status = status
        # CIMultiDictProxy gives case-insensitive header access matching
        # the real ClientResponse's headers attribute. Built from a
        # CIMultiDict so the underlying mutability guarantees match.
        self.headers = CIMultiDictProxy(CIMultiDict(headers))
        self._body = body
        self.url = url
        self.method = "POST"  # cache hits are always POSTs in practice
        self.content = _SyntheticStreamReader(body)

    async def __aenter__(self) -> "_SyntheticClientResponse":
        return self

    async def __aexit__(self, *exc: Any) -> None:
        return None

    async def read(self) -> bytes:
        return self._body

    async def text(self, encoding: str = "utf-8") -> str:
        return self._body.decode(encoding, errors="replace")

    async def json(self, encoding: str = "utf-8") -> Any:
        import json as _json

        return _json.loads(self._body.decode(encoding))

    def release(self) -> None:
        """Compat: real ClientResponse has a release() method that
        returns the connection to the pool. We have nothing to release;
        this is a no-op to avoid AttributeError on consumer code that
        defensively calls it.
        """
        return None

    def close(self) -> None:
        return None

    @property
    def closed(self) -> bool:
        return True

    @property
    def reason(self) -> str:
        return "OK"

    @property
    def ok(self) -> bool:
        return 200 <= self.status < 300


class _SyntheticStreamReader:
    """Minimal aiohttp.StreamReader stand-in for cache-hit responses.

    Real aiohttp consumers do ``async for chunk in response.content`` or
    ``await response.content.read()``. Both are supported here against
    the buffered cached body.
    """

    def __init__(self, body: bytes) -> None:
        self._body = body
        self._consumed = False

    async def read(self, n: int = -1) -> bytes:
        if self._consumed:
            return b""
        self._consumed = True
        return self._body

    async def readany(self) -> bytes:
        return await self.read()

    async def __aiter__(self) -> AsyncIterator[bytes]:
        if self._consumed:
            return
        self._consumed = True
        yield self._body


# ── Patch logic ────────────────────────────────────────────────────


def patch_aiohttp_sessions(predicates: Predicates | None = None) -> None:
    """Patch ``aiohttp.ClientSession._request`` for cache-then-live routing.

    Idempotent. The patched ``_request`` builds a :class:`RewindRequest`
    from the call args, runs through ``handle_intercepted_async``, and
    either returns our synthetic ClientResponse stand-in (cache hit) or
    delegates to the original ``_request`` (cache miss).

    Safe when aiohttp isn't installed; returns silently.
    """
    global _ORIGINAL_REQUEST, _PATCHED
    if not AIOHTTP_AVAILABLE:
        logger.debug("rewind: aiohttp not installed; skipping aiohttp patch")
        return
    if _PATCHED:
        return

    preds = predicates if predicates is not None else DefaultPredicates()
    _ORIGINAL_REQUEST = ClientSession._request

    async def patched_request(
        self: ClientSession,
        method: str,
        str_or_url: Any,
        **kwargs: Any,
    ) -> Any:
        # Build RewindRequest from the call. data / json / params are
        # the typical body-carrying kwargs; we serialize them the same
        # way aiohttp would (via `data=` byte-encoded, `json=` JSON-encoded).
        #
        # Phase 1 (Santa re-review #3): resolve relative URLs against
        # ClientSession._base_url. Without this,
        # ``session.post("/v1/chat/completions")`` with
        # ``ClientSession(base_url="https://api.openai.com")`` would
        # build a RewindRequest whose URL is just the path. Host-based
        # predicates (the default) wouldn't match
        # ``api.openai.com``, so the request bypasses interception.
        # Replicates the same logic aiohttp's ClientSession._request
        # uses internally before issuing the request.
        url = _resolve_url(self, str_or_url)
        method_upper = method.upper()
        body = _extract_body_bytes(kwargs)
        headers = _normalize_headers(kwargs.get("headers"))
        accept = headers.get("accept", "")
        is_stream = "text/event-stream" in accept.lower()

        req = RewindRequest(
            url=url,
            method=method_upper,
            headers=headers,
            body=body,
            stream=is_stream,
        )

        async def live() -> Any:
            # Resolve the original at CALL time, not closure-capture time.
            # This keeps tests honest: a test that swaps in a fake
            # `_ORIGINAL_REQUEST` between patch and call sees the swap
            # take effect, and production behavior is unchanged
            # (the value is set once at install and never moves).
            current_original = _get_original_request()
            return await current_original(self, method, str_or_url, **kwargs)

        def synth_buffered(body_bytes: bytes, response_headers: dict[str, str]) -> Any:
            return _SyntheticClientResponse(
                status=200,
                headers=response_headers,
                body=body_bytes,
                url=url,
            )

        def synth_streaming(body_bytes: bytes, response_headers: dict[str, str]) -> Any:
            from rewind_agent.intercept._core import synthetic_sse_for_cache_hit

            sse_body = synthetic_sse_for_cache_hit(body_bytes)
            return _SyntheticClientResponse(
                status=200,
                headers=response_headers,
                body=sse_body,
                url=url,
            )

        return await handle_intercepted_async(
            req,
            predicates=preds,
            live=live,
            synth_buffered=synth_buffered,
            synth_streaming=synth_streaming,
            is_streaming=req.stream,
        )

    ClientSession._request = patched_request  # type: ignore[method-assign]
    _PATCHED = True
    logger.debug("rewind: patched aiohttp.ClientSession._request")


def unpatch_aiohttp_sessions() -> None:
    """Reverse :func:`patch_aiohttp_sessions`."""
    global _ORIGINAL_REQUEST, _PATCHED
    if not AIOHTTP_AVAILABLE or not _PATCHED:
        return
    if _ORIGINAL_REQUEST is not None:
        ClientSession._request = _ORIGINAL_REQUEST  # type: ignore[method-assign]
    _ORIGINAL_REQUEST = None
    _PATCHED = False


def is_patched() -> bool:
    return _PATCHED


def _get_original_request() -> Any:
    """Module-level getter so the patched function resolves the original
    at call time. Lets tests swap in a fake by setting
    ``aiohttp_middleware._ORIGINAL_REQUEST = …`` after patch.
    """
    return _ORIGINAL_REQUEST


# ── Helpers ────────────────────────────────────────────────────────


def _resolve_url(session: Any, str_or_url: Any) -> str:
    """Resolve a relative URL against the session's ``_base_url``.

    Phase 1 (Santa re-review #3): aiohttp's
    ``ClientSession._request`` does this resolution AFTER our patched
    function runs, so if we just stringify ``str_or_url`` we'd get the
    bare path for a relative URL. Predicates that match on host (the
    default) would silently miss those requests.

    Replicates aiohttp's own logic: if the session has a non-None
    ``_base_url`` and the supplied URL is relative (doesn't start with
    a scheme), we yarl-join them. Otherwise stringify as-is.

    yarl is a hard dependency of aiohttp, so importing it inside this
    function is safe — we wouldn't be in this code path without
    aiohttp being importable.
    """
    raw = str(str_or_url)
    base = getattr(session, "_base_url", None)
    if base is None:
        return raw
    # Schemes that mean "absolute URL — no resolution needed". Includes
    # ws/wss for parity with aiohttp even though our intercept doesn't
    # touch WebSocket upgrades (different code path:
    # ``ClientSession._ws_connect``).
    if raw.startswith(("http:", "https:", "ws:", "wss:")):
        return raw
    try:
        from yarl import URL  # type: ignore[import-untyped]

        return str(base.join(URL(raw)))
    except Exception:
        # Defensive: if yarl join blows up on a malformed URL, fall back
        # to the bare string. Predicate match will fail (user-facing
        # bypass), but at least we don't crash.
        return raw


def _extract_body_bytes(kwargs: dict[str, Any]) -> bytes:
    """Materialize the request body from aiohttp's kwargs.

    Order of precedence matches aiohttp's own internal handling:

    1. ``data=`` — raw bytes / str / form data (FormData not supported
       in v1; it serializes via multipart which we don't predicate-match).
    2. ``json=`` — Python object serialized as JSON. Most LLM SDK
       calls use this path.
    3. Neither → empty body.

    Streaming uploads (``data=AsyncIterator``) fall through to empty
    bytes; documented limitation matches the other adapters.
    """
    if "data" in kwargs:
        data = kwargs["data"]
        if isinstance(data, bytes):
            return data
        if isinstance(data, str):
            return data.encode("utf-8")
        return b""  # FormData / async iterator → unsupported in v1

    if "json" in kwargs:
        import json as _json

        try:
            return _json.dumps(kwargs["json"]).encode("utf-8")
        except (TypeError, ValueError):
            return b""

    return b""


def _normalize_headers(headers: Any) -> dict[str, str]:
    """Return lowercase-keyed dict matching the predicate Protocol contract.

    aiohttp accepts headers as ``dict``, ``CIMultiDict``, or list of
    tuples; normalize all forms.
    """
    if headers is None:
        return {}
    if isinstance(headers, dict):
        return {k.lower(): v for k, v in headers.items()}
    # CIMultiDict / list of tuples
    try:
        return {k.lower(): v for k, v in headers.items()}  # type: ignore[union-attr]
    except AttributeError:
        try:
            return {k.lower(): v for k, v in headers}
        except (TypeError, ValueError):
            return {}
