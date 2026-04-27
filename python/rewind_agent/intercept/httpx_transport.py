"""httpx transport adapters for rewind intercept.

Phase 1 of the Universal Replay Architecture. Subclass
``httpx.HTTPTransport`` and ``httpx.AsyncHTTPTransport`` so any
``httpx.Client`` or ``httpx.AsyncClient`` constructed after
:func:`rewind_agent.intercept.install` runs goes through cache-then-live
routing.

## Design

Two surfaces:

1. **Transport subclasses** — :class:`RewindHTTPTransport` and
   :class:`RewindAsyncHTTPTransport`. Override ``handle_request`` /
   ``handle_async_request`` to drive the request through
   :func:`._flow.handle_intercepted_sync` / ``_async``. The actual HTTP
   delivery is delegated to the inner transport (default
   ``httpx.HTTPTransport()`` if not wrapped).

2. **Client patches** — :func:`patch_httpx_clients` swaps in our
   transport class on ``httpx.Client.__init__`` /
   ``httpx.AsyncClient.__init__``. If the user passed their own
   transport via ``transport=…``, we wrap it; otherwise we install
   ours fresh. Idempotent: re-running the patch is a no-op.

## Streaming

httpx represents response bodies as either ``ByteStream`` (sync) or
``AsyncByteStream`` (async). For cache hits where the agent expects a
streaming response, we build the synthetic SSE stream using
:func:`._core.synthetic_sse_for_cache_hit` and wrap it in a custom
``ByteStream`` subclass that yields one chunk plus the ``[DONE]``
sentinel — the agent's ``response.iter_bytes()`` / ``aiter_bytes()``
loops terminate cleanly.

For non-streaming cache hits, the body is delivered as a single
buffered chunk via ``httpx.ByteStream(body)``.

## Inner-transport delegation

Users sometimes pass custom transports to ``httpx.Client`` for retry
policies, mocking, custom DNS, etc. We respect this by storing the
caller's transport as ``self._inner`` and delegating to it on cache
miss. If no transport was passed, ``self._inner`` is the default
``httpx.HTTPTransport()`` / ``httpx.AsyncHTTPTransport()``.
"""

from __future__ import annotations

import logging
from typing import Any

from rewind_agent.intercept._flow import (
    handle_intercepted_async,
    handle_intercepted_sync,
)
from rewind_agent.intercept._predicates import DefaultPredicates, Predicates
from rewind_agent.intercept._request import RewindRequest

logger = logging.getLogger(__name__)


# Conditional import — adapters are optional dependencies. If httpx
# isn't installed, the patch function below is a no-op so
# ``intercept.install()`` works in environments where the user only
# uses requests or aiohttp.
try:
    import httpx
    from httpx import (
        AsyncByteStream,
        AsyncHTTPTransport,
        ByteStream,
        HTTPTransport,
        Request,
        Response,
    )

    HTTPX_AVAILABLE = True
except ImportError:  # pragma: no cover — exercised in env-detection tests
    HTTPX_AVAILABLE = False


# Module-level patch state. Lets us idempotently install + correctly
# uninstall (test hygiene; production rarely uninstalls).
_ORIGINAL_CLIENT_INIT = None
_ORIGINAL_ASYNC_CLIENT_INIT = None
_PATCHED = False


# ── Sync transport ─────────────────────────────────────────────────


def _make_sync_transport_class(predicates: Predicates) -> Any:
    """Build a sync transport subclass bound to the given predicates.

    We close over ``predicates`` so the patched ``Client.__init__`` can
    swap in a fresh class per ``install()`` call without polluting a
    module-level singleton. This matters in tests that install with
    different predicates back-to-back.
    """

    class RewindHTTPTransport(HTTPTransport):  # type: ignore[misc, valid-type]
        """``httpx.HTTPTransport`` subclass that runs every request through
        :func:`._flow.handle_intercepted_sync`.

        On cache hit, returns a synthetic ``httpx.Response`` built from the
        cached body without ever calling the inner transport. On cache
        miss, delegates to ``super().handle_request`` (which is the same
        as a vanilla ``HTTPTransport``).
        """

        def __init__(self, *args: Any, _inner: Any = None, **kwargs: Any) -> None:
            # If the user passed their own transport via the patched
            # Client.__init__, it's threaded through ``_inner`` and we
            # delegate to it. Otherwise fall back to our own default
            # transport behavior (super().handle_request).
            self._inner = _inner
            super().__init__(*args, **kwargs)

        def handle_request(self, request: Request) -> Response:
            req = _build_rewind_request(request, sync=True)

            def live() -> Response:
                if self._inner is not None:
                    return self._inner.handle_request(request)
                return super(RewindHTTPTransport, self).handle_request(request)

            def synth_buffered(body: bytes, headers: dict[str, str]) -> Response:
                return Response(
                    status_code=200,
                    headers=headers,
                    stream=ByteStream(body),
                    request=request,
                )

            def synth_streaming(body: bytes, headers: dict[str, str]) -> Response:
                return Response(
                    status_code=200,
                    headers=headers,
                    stream=_SyntheticSSEByteStream(body),
                    request=request,
                )

            return handle_intercepted_sync(
                req,
                predicates=predicates,
                live=live,
                synth_buffered=synth_buffered,
                synth_streaming=synth_streaming,
                is_streaming=req.stream,
            )

    return RewindHTTPTransport


# ── Async transport ────────────────────────────────────────────────


def _make_async_transport_class(predicates: Predicates) -> Any:
    """Async counterpart of :func:`_make_sync_transport_class`."""

    class RewindAsyncHTTPTransport(AsyncHTTPTransport):  # type: ignore[misc, valid-type]
        """``httpx.AsyncHTTPTransport`` subclass for async clients."""

        def __init__(self, *args: Any, _inner: Any = None, **kwargs: Any) -> None:
            self._inner = _inner
            super().__init__(*args, **kwargs)

        async def handle_async_request(self, request: Request) -> Response:
            req = _build_rewind_request(request, sync=False)

            async def live() -> Response:
                if self._inner is not None:
                    return await self._inner.handle_async_request(request)
                return await super(
                    RewindAsyncHTTPTransport, self
                ).handle_async_request(request)

            def synth_buffered(body: bytes, headers: dict[str, str]) -> Response:
                return Response(
                    status_code=200,
                    headers=headers,
                    stream=ByteStream(body),
                    request=request,
                )

            def synth_streaming(body: bytes, headers: dict[str, str]) -> Response:
                return Response(
                    status_code=200,
                    headers=headers,
                    stream=_AsyncSyntheticSSEByteStream(body),
                    request=request,
                )

            return await handle_intercepted_async(
                req,
                predicates=predicates,
                live=live,
                synth_buffered=synth_buffered,
                synth_streaming=synth_streaming,
                is_streaming=req.stream,
            )

    return RewindAsyncHTTPTransport


# ── Synthetic SSE byte streams ─────────────────────────────────────
#
# ByteStream / AsyncByteStream protocols require __iter__ / __aiter__.
# We can't reuse iter_synthetic_sse_chunks directly because httpx
# expects a *class* implementing the protocol, and Generators can be
# consumed only once — leading to subtle bugs if httpx replays the
# stream (it doesn't today, but defensive). Wrap as a class.


class _SyntheticSSEByteStream(ByteStream):  # type: ignore[misc, valid-type]
    """Sync ByteStream that emits one SSE event + [DONE] sentinel.

    httpx calls ``__iter__`` once per response, but the wrapped data is
    immutable bytes so re-iteration would just emit the same chunks
    again — defensive, not relied upon.
    """

    def __init__(self, body: bytes) -> None:
        from rewind_agent.intercept._core import iter_synthetic_sse_chunks

        # Materialize chunks now; trivial in size and saves a re-import
        # per iteration.
        self._chunks = tuple(iter_synthetic_sse_chunks(body))
        # ByteStream expects a content arg; supply an empty bytes here
        # so the parent class is happy. Our overridden __iter__ ignores
        # the parent's stream state.
        super().__init__(b"")

    def __iter__(self):  # type: ignore[no-untyped-def]
        yield from self._chunks


class _AsyncSyntheticSSEByteStream(AsyncByteStream):  # type: ignore[misc, valid-type]
    """Async counterpart for httpx async clients.

    Unlike sync :class:`ByteStream` (which has an ``__init__(content: bytes)``),
    httpx's :class:`AsyncByteStream` is an abstract protocol with no
    constructor — calling ``super().__init__(b"")`` fails with ``object``'s
    "takes exactly one argument" error. We omit the super call.
    """

    def __init__(self, body: bytes) -> None:
        from rewind_agent.intercept._core import iter_synthetic_sse_chunks

        self._chunks = tuple(iter_synthetic_sse_chunks(body))

    async def __aiter__(self):  # type: ignore[no-untyped-def]
        for chunk in self._chunks:
            yield chunk

    async def aclose(self) -> None:
        # No resources to release; chunks are owned by self.
        return None


# ── Request normalization ──────────────────────────────────────────


def _build_rewind_request(request: Request, *, sync: bool) -> RewindRequest:
    """Convert an ``httpx.Request`` to a :class:`RewindRequest`.

    Headers are lowercased to match the predicate contract. Body is
    materialized to bytes here via the sync ``request.read()`` (works
    on both sync and async clients for typical JSON-bodied requests
    because httpx buffers the body at construction time).

    The ``sync`` argument is reserved for future use where async
    streaming uploads might require ``await request.aread()`` — for
    v1 we don't support those, see "Streaming uploads" below.

    ## Streaming uploads (NOT supported in v1)

    httpx's async clients support streaming request bodies (e.g. file
    uploads) where the body is an async iterator that's drained at
    send time. Reading that stream here would consume it, breaking
    the live path. v1 transparently passes through the live request
    in that case but the body bytes seen by the predicate / cache
    would be empty. Document this limitation; revisit if usage data
    shows agents actually doing this.

    ## Stream-flag detection

    Sourced from ``Accept: text/event-stream`` header alone. httpx
    doesn't expose ``stream=True`` argument at the transport layer,
    but the SDKs we care about (OpenAI, Anthropic) set the Accept
    header automatically. ``_core.detect_streaming`` provides a
    second-line check on the body's ``"stream": true`` field inside
    ``_flow``.
    """
    # ``request.headers.raw`` is a list of (bytes, bytes) tuples; the
    # ``.headers`` Mapping is case-insensitive but we want lowercase
    # for the predicate Protocol.
    headers = {k.decode().lower(): v.decode() for k, v in request.headers.raw}

    # ``request.read()`` works for both sync and async clients on
    # buffered bodies (the typical case). For streaming uploads it
    # would consume the iterator — see docstring caveat.
    try:
        body = request.read()
    except Exception:  # pragma: no cover — defensive against weird streams
        body = b""

    accept = headers.get("accept", "")
    stream = "text/event-stream" in accept.lower()

    return RewindRequest(
        url=str(request.url),
        method=request.method.upper(),
        headers=headers,
        body=body,
        stream=stream,
    )


# ── Public install / uninstall ─────────────────────────────────────


def patch_httpx_clients(predicates: Predicates | None = None) -> None:
    """Patch ``httpx.Client.__init__`` and ``httpx.AsyncClient.__init__``
    so subsequently-constructed clients use our transports.

    Idempotent — second call is a no-op. The ``predicates`` argument
    binds at patch time; re-installing with new predicates requires
    :func:`unpatch_httpx_clients` first.

    Safe to call when httpx isn't installed — the function checks
    :data:`HTTPX_AVAILABLE` and returns early. The caller
    (``intercept.install``) doesn't need to gate on availability.
    """
    global _ORIGINAL_CLIENT_INIT, _ORIGINAL_ASYNC_CLIENT_INIT, _PATCHED
    if not HTTPX_AVAILABLE:
        logger.debug("rewind: httpx not installed; skipping httpx patch")
        return
    if _PATCHED:
        return

    preds = predicates if predicates is not None else DefaultPredicates()
    rewind_sync_transport = _make_sync_transport_class(preds)
    rewind_async_transport = _make_async_transport_class(preds)

    _ORIGINAL_CLIENT_INIT = httpx.Client.__init__
    _ORIGINAL_ASYNC_CLIENT_INIT = httpx.AsyncClient.__init__

    original_client_init = _ORIGINAL_CLIENT_INIT
    original_async_client_init = _ORIGINAL_ASYNC_CLIENT_INIT

    def patched_client_init(self: Any, *args: Any, **kwargs: Any) -> None:
        # If user passed transport=…, wrap it so their custom retry /
        # mocking / DNS resolution still runs. Otherwise install ours
        # fresh. ``mounts=…`` (httpx's per-host transport routing) is
        # orthogonal: a request to a mounted host bypasses us. Operators
        # using mounts heavily should mount our transport explicitly.
        user_transport = kwargs.pop("transport", None)
        if user_transport is not None:
            kwargs["transport"] = rewind_sync_transport(_inner=user_transport)
        else:
            kwargs["transport"] = rewind_sync_transport()
        original_client_init(self, *args, **kwargs)

    def patched_async_client_init(self: Any, *args: Any, **kwargs: Any) -> None:
        # AsyncClient.__init__ is a SYNC method (it just stores config;
        # actual I/O happens later in async send). Symmetric to the
        # sync wrapper above.
        user_transport = kwargs.pop("transport", None)
        if user_transport is not None:
            kwargs["transport"] = rewind_async_transport(_inner=user_transport)
        else:
            kwargs["transport"] = rewind_async_transport()
        original_async_client_init(self, *args, **kwargs)

    httpx.Client.__init__ = patched_client_init  # type: ignore[method-assign]
    httpx.AsyncClient.__init__ = patched_async_client_init  # type: ignore[method-assign]
    _PATCHED = True
    logger.debug("rewind: patched httpx.Client + AsyncClient __init__")


def unpatch_httpx_clients() -> None:
    """Reverse :func:`patch_httpx_clients`. Mainly for tests; in
    production agents rarely uninstall the intercept layer.

    Restores the original ``__init__`` methods on both client types.
    Clients constructed during the patched window keep their
    ``RewindHTTPTransport`` reference (we don't try to mutate
    pre-existing instances — that would be too magical).
    """
    global _ORIGINAL_CLIENT_INIT, _ORIGINAL_ASYNC_CLIENT_INIT, _PATCHED
    if not HTTPX_AVAILABLE or not _PATCHED:
        return
    if _ORIGINAL_CLIENT_INIT is not None:
        httpx.Client.__init__ = _ORIGINAL_CLIENT_INIT  # type: ignore[method-assign]
    if _ORIGINAL_ASYNC_CLIENT_INIT is not None:
        httpx.AsyncClient.__init__ = _ORIGINAL_ASYNC_CLIENT_INIT  # type: ignore[method-assign]
    _ORIGINAL_CLIENT_INIT = None
    _ORIGINAL_ASYNC_CLIENT_INIT = None
    _PATCHED = False


def is_patched() -> bool:
    """Test introspection: did patch_httpx_clients run successfully?"""
    return _PATCHED
