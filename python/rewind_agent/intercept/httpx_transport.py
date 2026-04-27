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
#
# IMPORTANT: this module must be IMPORTABLE without httpx installed.
# The Python SDK has no httpx dependency by default; users opt in by
# installing httpx separately. So everything httpx-typed (transport
# subclasses, ByteStream synthesizers) is lazily defined inside the
# patch function below, not at module level. Module-level classes
# that subclass ``httpx.ByteStream`` would raise NameError at import
# time when httpx is absent — exactly the failure mode CI hit on PR #149.
try:
    import httpx  # noqa: F401  — used via attribute access in patch fn

    HTTPX_AVAILABLE = True
except ImportError:  # pragma: no cover — environment-detection path
    HTTPX_AVAILABLE = False


# Module-level patch state. Lets us idempotently install + correctly
# uninstall (test hygiene; production rarely uninstalls).
_ORIGINAL_CLIENT_INIT = None
_ORIGINAL_ASYNC_CLIENT_INIT = None
_PATCHED = False


# ── Lazy class factory ─────────────────────────────────────────────
#
# Transport subclasses + ByteStream synthesizers are constructed inside
# this factory the first time ``patch_httpx_clients`` runs. They MUST
# NOT be defined at module level because their parent classes
# (``httpx.HTTPTransport``, ``httpx.ByteStream`` etc.) are conditional
# imports — defining a subclass at module level would raise NameError
# when httpx isn't installed, breaking ``import rewind_agent.intercept``
# in environments where the user only has requests or aiohttp.


def _build_transport_classes(predicates: Predicates) -> tuple[Any, Any]:
    """Construct (sync_transport_class, async_transport_class) bound to
    the given predicates.

    Importing httpx at function entry rather than module level keeps
    this module importable without httpx. AssertionError if called when
    ``HTTPX_AVAILABLE`` is False (caller's responsibility to gate).
    """
    if not HTTPX_AVAILABLE:
        raise AssertionError(
            "_build_transport_classes called without httpx — caller must "
            "check HTTPX_AVAILABLE first"
        )
    # Re-import locally; ``import httpx`` at module level set the flag
    # but didn't expose the symbols we need here.
    import httpx as _httpx
    from rewind_agent.intercept._core import iter_synthetic_sse_chunks

    # ── ByteStream subclasses (sync + async) ─────────────────────
    #
    # httpx exposes two ByteStream protocols. Our subclasses emit one
    # SSE event + [DONE] sentinel from a buffered cached body.

    class _SyntheticSSEByteStream(_httpx.ByteStream):  # type: ignore[misc]
        """Sync ByteStream emitting `data: …\\n\\ndata: [DONE]\\n\\n` chunks."""

        def __init__(self, body: bytes) -> None:
            self._chunks = tuple(iter_synthetic_sse_chunks(body))
            # ByteStream.__init__ takes content bytes; we pass empty
            # because our overridden __iter__ owns the stream state.
            super().__init__(b"")

        def __iter__(self):  # type: ignore[no-untyped-def]
            yield from self._chunks

    class _AsyncSyntheticSSEByteStream(_httpx.AsyncByteStream):  # type: ignore[misc]
        """Async counterpart. AsyncByteStream is an abstract protocol with
        no constructor — calling super().__init__(b"") fails on
        object.__init__'s arg-count check, so we skip it.
        """

        def __init__(self, body: bytes) -> None:
            self._chunks = tuple(iter_synthetic_sse_chunks(body))

        async def __aiter__(self):  # type: ignore[no-untyped-def]
            for chunk in self._chunks:
                yield chunk

        async def aclose(self) -> None:
            return None

    # ── Sync transport ───────────────────────────────────────────

    class RewindHTTPTransport(_httpx.HTTPTransport):  # type: ignore[misc]
        """``httpx.HTTPTransport`` subclass that runs every request through
        :func:`._flow.handle_intercepted_sync`. Cache hit → synthetic
        Response built from cached body. Cache miss → delegate to
        ``super().handle_request`` (or to ``self._inner`` when the user
        supplied their own transport).
        """

        def __init__(self, *args: Any, _inner: Any = None, **kwargs: Any) -> None:
            self._inner = _inner
            super().__init__(*args, **kwargs)

        def close(self) -> None:
            """Phase 1 (Santa re-review #2): forward close to the wrapped
            transport. When the user's Client closes (explicitly or via
            __exit__), all wrapped transports — ours plus the configured
            httpx default that we wrapped post-init — must release their
            connection pools / cert verifiers / SSL contexts. Skipping
            close on _inner leaks those resources for the lifetime of
            the process.
            """
            if self._inner is not None:
                try:
                    self._inner.close()
                except Exception:
                    pass
            super().close()

        def handle_request(self, request: Any) -> Any:
            req = _build_rewind_request(request, sync=True)

            def live() -> Any:
                if self._inner is not None:
                    return self._inner.handle_request(request)
                return super(RewindHTTPTransport, self).handle_request(request)

            def synth_buffered(body: bytes, headers: dict[str, str]) -> Any:
                return _httpx.Response(
                    status_code=200,
                    headers=headers,
                    stream=_httpx.ByteStream(body),
                    request=request,
                )

            def synth_streaming(body: bytes, headers: dict[str, str]) -> Any:
                return _httpx.Response(
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

    # ── Async transport ──────────────────────────────────────────

    class RewindAsyncHTTPTransport(_httpx.AsyncHTTPTransport):  # type: ignore[misc]
        """``httpx.AsyncHTTPTransport`` subclass for async clients."""

        def __init__(self, *args: Any, _inner: Any = None, **kwargs: Any) -> None:
            self._inner = _inner
            super().__init__(*args, **kwargs)

        async def aclose(self) -> None:
            """Async counterpart of :meth:`RewindHTTPTransport.close`.
            Forwards to ``_inner.aclose()`` first (the configured
            default we wrapped post-init), then super to release our
            own resources. Without this, AsyncClient.aclose() leaks
            httpx's connection pool. See Santa re-review #2.
            """
            if self._inner is not None:
                try:
                    await self._inner.aclose()
                except Exception:
                    pass
            await super().aclose()

        async def handle_async_request(self, request: Any) -> Any:
            req = _build_rewind_request(request, sync=False)

            async def live() -> Any:
                if self._inner is not None:
                    return await self._inner.handle_async_request(request)
                return await super(
                    RewindAsyncHTTPTransport, self
                ).handle_async_request(request)

            def synth_buffered(body: bytes, headers: dict[str, str]) -> Any:
                return _httpx.Response(
                    status_code=200,
                    headers=headers,
                    stream=_httpx.ByteStream(body),
                    request=request,
                )

            def synth_streaming(body: bytes, headers: dict[str, str]) -> Any:
                return _httpx.Response(
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

    return RewindHTTPTransport, RewindAsyncHTTPTransport


# ── Request normalization ──────────────────────────────────────────


def _build_rewind_request(request: Any, *, sync: bool) -> RewindRequest:
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

    # Lazy import of httpx — module-level import succeeded (we got
    # past the HTTPX_AVAILABLE check), but importing again here makes
    # the local reference explicit for the patch logic below.
    import httpx as _httpx

    preds = predicates if predicates is not None else DefaultPredicates()
    rewind_sync_transport, rewind_async_transport = _build_transport_classes(preds)

    _ORIGINAL_CLIENT_INIT = _httpx.Client.__init__
    _ORIGINAL_ASYNC_CLIENT_INIT = _httpx.AsyncClient.__init__

    original_client_init = _ORIGINAL_CLIENT_INIT
    original_async_client_init = _ORIGINAL_ASYNC_CLIENT_INIT

    def patched_client_init(self: Any, *args: Any, **kwargs: Any) -> None:
        # Phase 1 (Santa #5): preserve httpx's configured default transport
        # options. The naive "swap in RewindHTTPTransport before calling
        # original_init" approach DROPS verify / cert / trust_env / http2 /
        # proxies / limits / local_address / retries / socket_options /
        # default_encoding / etc., because httpx only constructs its
        # configured default transport when transport=None reaches its
        # __init__. By replacing transport=None with our transport up front,
        # we short-circuit that path and the user loses every transport
        # config knob.
        #
        # Fix: TWO modes.
        #   (a) User explicitly passed transport= — wrap it. Their config
        #       lives on their transport already.
        #   (b) User passed only top-level config (verify=False, etc.) —
        #       let httpx build its configured default first, then wrap
        #       the post-init self._transport with ours.
        user_transport = kwargs.pop("transport", None)
        if user_transport is not None:
            # Mode (a): wrap explicitly-supplied transport.
            kwargs["transport"] = rewind_sync_transport(_inner=user_transport)
            original_client_init(self, *args, **kwargs)
        else:
            # Mode (b): let httpx do its standard transport
            # construction, then wrap. self._transport is httpx's
            # documented internal attribute (stable across 0.x).
            original_client_init(self, *args, **kwargs)
            configured = getattr(self, "_transport", None)
            if configured is not None:
                self._transport = rewind_sync_transport(_inner=configured)

    def patched_async_client_init(self: Any, *args: Any, **kwargs: Any) -> None:
        # AsyncClient.__init__ is a SYNC method (it just stores config;
        # actual I/O happens later in async send). Same two-mode fix as
        # the sync variant above — see Santa #5 docstring.
        user_transport = kwargs.pop("transport", None)
        if user_transport is not None:
            kwargs["transport"] = rewind_async_transport(_inner=user_transport)
            original_async_client_init(self, *args, **kwargs)
        else:
            original_async_client_init(self, *args, **kwargs)
            configured = getattr(self, "_transport", None)
            if configured is not None:
                self._transport = rewind_async_transport(_inner=configured)

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
