"""``requests`` transport adapter for rewind intercept.

Phase 1 of the Universal Replay Architecture. Subclass
``requests.adapters.HTTPAdapter`` and patch
``requests.Session.__init__`` so any session constructed after
:func:`rewind_agent.intercept.install` routes through cache-then-live.

## Why ``requests`` specifically

Older Anthropic SDK, LangChain, and most legacy / homegrown LLM
clients use ``requests``. Even when the modern provider SDKs have
moved to ``httpx``, library wrappers built on ``requests`` are still
deployed widely.

## Implementation notes

``requests`` is sync-only. The ``HTTPAdapter.send`` API gives us a
fully-prepared ``PreparedRequest`` with body buffered to bytes (or
str / generator for streaming uploads). We build a
:class:`RewindRequest` from it, drive through
:func:`._flow.handle_intercepted_sync`, and either return a synthetic
``requests.Response`` (cache hit) or delegate to ``super().send``
(cache miss).

## Cache-hit response synthesis

A ``requests.Response`` with ``_content`` set and ``_content_consumed=True``
makes the response body available via every consumer pattern:
``response.json()``, ``response.text``, ``response.iter_content()``,
``response.iter_lines()``. We set the body to either the cached JSON
bytes (buffered) or the synthetic SSE bytes (streaming), and the
``Content-Type`` header accordingly.

## Pre-existing sessions

Sessions constructed BEFORE :func:`patch_requests_sessions` runs keep
their default ``HTTPAdapter``. They can be migrated by an explicit
``session.mount("https://", RewindHTTPAdapter())`` call, but the
auto-injection only fires for new sessions. Document, don't try to
mutate live instances.
"""

from __future__ import annotations

import io
import logging
from typing import Any

from rewind_agent.intercept._flow import handle_intercepted_sync
from rewind_agent.intercept._predicates import DefaultPredicates, Predicates
from rewind_agent.intercept._request import RewindRequest

logger = logging.getLogger(__name__)


# Conditional import — see httpx_transport.py for the same pattern.
try:
    import requests
    from requests.adapters import HTTPAdapter
    from requests.models import PreparedRequest, Response

    REQUESTS_AVAILABLE = True
except ImportError:  # pragma: no cover — environment-detection paths
    REQUESTS_AVAILABLE = False


_ORIGINAL_SESSION_INIT = None
_PATCHED = False


def _make_adapter_class(predicates: Predicates) -> Any:
    """Build a HTTPAdapter subclass bound to the given predicates.

    Per-install factory so installing twice with different predicates
    produces distinct classes (matters for tests; production rarely
    re-installs).
    """

    class RewindHTTPAdapter(HTTPAdapter):  # type: ignore[misc, valid-type]
        """Drop-in replacement for the default ``requests.adapters.HTTPAdapter``.

        On cache hit, bypasses the network entirely by constructing a
        synthetic ``requests.Response``. On cache miss, delegates to
        the parent class's ``send`` (which uses urllib3 under the hood).
        """

        def send(  # type: ignore[override]
            self,
            request: PreparedRequest,
            stream: bool = False,
            timeout: Any = None,
            verify: bool = True,
            cert: Any = None,
            proxies: dict[str, str] | None = None,
        ) -> Response:
            req = _build_rewind_request(request, stream_arg=stream)

            def live() -> Response:
                # Delegate to the parent HTTPAdapter — which is the
                # actual urllib3-driven HTTP transport.
                return super(RewindHTTPAdapter, self).send(
                    request,
                    stream=stream,
                    timeout=timeout,
                    verify=verify,
                    cert=cert,
                    proxies=proxies,
                )

            def synth_buffered(body: bytes, headers: dict[str, str]) -> Response:
                return _build_synthetic_response(
                    body=body, headers=headers, request=request
                )

            def synth_streaming(body: bytes, headers: dict[str, str]) -> Response:
                # Streaming cache hits need an SSE-formatted body, not
                # the raw JSON — the agent's iter_content / iter_lines
                # loop expects ``data: {…}\n\ndata: [DONE]\n\n``. Phase 0's
                # synthetic_sse_for_cache_hit packages the bytes; we
                # then deliver them through the same buffered Response
                # mechanism (requests doesn't distinguish iter_content
                # source — _content + _content_consumed=True covers
                # both buffered .text/.json AND streaming iter_content).
                from rewind_agent.intercept._core import (
                    synthetic_sse_for_cache_hit,
                )

                sse_body = synthetic_sse_for_cache_hit(body)
                return _build_synthetic_response(
                    body=sse_body, headers=headers, request=request
                )

            return handle_intercepted_sync(
                req,
                predicates=predicates,
                live=live,
                synth_buffered=synth_buffered,
                synth_streaming=synth_streaming,
                is_streaming=req.stream,
            )

    return RewindHTTPAdapter


def _build_rewind_request(
    request: PreparedRequest, *, stream_arg: bool
) -> RewindRequest:
    """Convert a ``PreparedRequest`` to a :class:`RewindRequest`.

    The ``stream`` flag is set from ANY of:

    - The ``stream_arg`` parameter passed to ``HTTPAdapter.send`` (which
      ``Session.send`` forwards from the user's ``session.send(stream=True)``
      or ``session.post(stream=True)`` call).
    - ``Accept: text/event-stream`` header (set by streaming SDKs).
    - ``"stream": true`` in the JSON body (caught later by
      ``_core.detect_streaming`` inside ``_flow``).

    Body normalization: ``PreparedRequest.body`` can be bytes, str, or
    a generator (streaming upload). We convert bytes/str to bytes;
    generators are NOT supported in v1 (consuming the generator here
    would break the live path).
    """
    headers = {k.lower(): v for k, v in request.headers.items()}

    body_attr = request.body
    if isinstance(body_attr, bytes):
        body = body_attr
    elif isinstance(body_attr, str):
        body = body_attr.encode("utf-8")
    else:
        # Generator / file-like / None — fall back to empty bytes.
        # Streaming uploads land here; documented limitation.
        body = b""

    accept = headers.get("accept", "")
    is_stream = stream_arg or "text/event-stream" in accept.lower()

    return RewindRequest(
        url=request.url or "",
        method=(request.method or "").upper(),
        headers=headers,
        body=body,
        stream=is_stream,
    )


def _build_synthetic_response(
    *, body: bytes, headers: dict[str, str], request: PreparedRequest
) -> Response:
    """Build a ``requests.Response`` from cached bytes.

    Sets the body via the ``_content`` + ``_content_consumed=True``
    pattern so every consumer (``response.json``, ``response.text``,
    ``response.iter_content``, ``response.iter_lines``) sees the
    body. ``raw`` is set to a BytesIO so anything that reaches into
    ``response.raw`` (rare, but ``response.raw.read()`` is documented)
    also works.
    """
    resp = Response()
    resp.status_code = 200
    resp._content = body  # type: ignore[attr-defined]
    resp._content_consumed = True  # type: ignore[attr-defined]
    resp.encoding = "utf-8"
    resp.headers.update(headers)
    resp.headers.setdefault("content-length", str(len(body)))
    resp.url = request.url or ""
    # Some callers reach into response.raw for low-level streaming;
    # urllib3.response.HTTPResponse is the "real" type. A BytesIO
    # works for the typical iter_content / read patterns.
    resp.raw = io.BytesIO(body)
    # request reference so response.request.url etc work.
    resp.request = request
    return resp


def patch_requests_sessions(predicates: Predicates | None = None) -> None:
    """Patch ``requests.Session.__init__`` to mount our adapter.

    Idempotent. The patched ``__init__`` runs the original first
    (which mounts the default ``HTTPAdapter`` on ``http://`` and
    ``https://``), then re-mounts our adapter on top of those. Mount
    routing in requests is last-mounted-wins for prefix matches, so
    our adapter takes precedence.

    Safe when ``requests`` isn't installed; returns silently.
    """
    global _ORIGINAL_SESSION_INIT, _PATCHED
    if not REQUESTS_AVAILABLE:
        logger.debug("rewind: requests not installed; skipping requests patch")
        return
    if _PATCHED:
        return

    preds = predicates if predicates is not None else DefaultPredicates()
    rewind_adapter_class = _make_adapter_class(preds)
    _ORIGINAL_SESSION_INIT = requests.Session.__init__
    original_init = _ORIGINAL_SESSION_INIT

    def patched_init(self: Any, *args: Any, **kwargs: Any) -> None:
        original_init(self, *args, **kwargs)
        # Re-mount with our adapter. Unmount-then-mount is the
        # safest sequence: requests uses an OrderedDict for adapters
        # and longest-prefix-match wins, so we want ours to be the
        # most-specific mount for both protocols.
        self.mount("https://", rewind_adapter_class())
        self.mount("http://", rewind_adapter_class())

    requests.Session.__init__ = patched_init  # type: ignore[method-assign]
    _PATCHED = True
    logger.debug("rewind: patched requests.Session.__init__")


def unpatch_requests_sessions() -> None:
    """Reverse :func:`patch_requests_sessions`. Test hygiene."""
    global _ORIGINAL_SESSION_INIT, _PATCHED
    if not REQUESTS_AVAILABLE or not _PATCHED:
        return
    if _ORIGINAL_SESSION_INIT is not None:
        requests.Session.__init__ = _ORIGINAL_SESSION_INIT  # type: ignore[method-assign]
    _ORIGINAL_SESSION_INIT = None
    _PATCHED = False


def is_patched() -> bool:
    """Test introspection: did patch_requests_sessions run successfully?"""
    return _PATCHED
