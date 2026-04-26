"""Normalized request shape passed to predicates across all intercept adapters.

All three transport adapters (httpx / requests / aiohttp) convert their
library-native request objects into this shape before calling the operator's
``is_llm_call`` / ``is_tool_call`` predicates. A single signature means
predicates copy-paste cleanly between agents using different HTTP clients.

The shape is deliberately small — just the fields a predicate would
realistically branch on (URL, method, content-type, body-sniff bytes).
Library-specific extensions (httpx ``Request`` extensions, aiohttp
``trace_request_ctx``, etc.) are not exposed; the abstraction is the point.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Mapping
from urllib.parse import urlparse, ParseResult


@dataclass(frozen=True)
class RewindRequest:
    """Normalized HTTP request as seen by a predicate.

    Attributes
    ----------
    url:
        Full request URL (including scheme, host, path, query). Use
        :attr:`url_parts` for component-wise access; predicates can do
        ``req.url_parts.netloc.endswith("…")`` without re-parsing.
    method:
        Uppercase HTTP method (``"POST"``, ``"GET"`` …).
    headers:
        Lower-case header name → value mapping. Adapters normalize header
        keys to lowercase before constructing ``RewindRequest`` so predicates
        don't have to handle ``Content-Type`` vs ``content-type``.
    body:
        Request body bytes (``b""`` for GET / no-body requests). For
        body-typed clients that haven't yet materialized the body
        (httpx streaming uploads, aiohttp file uploads), the adapter MUST
        buffer the body to bytes before constructing ``RewindRequest`` and
        re-emit an equivalent stream for the live path. See ``_core.py``
        for the streaming buffer policy.
    stream:
        True when the caller explicitly asked for a streaming response
        (``stream=True`` argument or ``Accept: text/event-stream`` header).
        Adapters set this from library-specific signals so predicates
        don't have to inspect the body.
    """

    url: str
    method: str
    headers: Mapping[str, str] = field(default_factory=dict)
    body: bytes = b""
    stream: bool = False

    @property
    def url_parts(self) -> ParseResult:
        """Lazy URL component split. Cheap (urlparse is pure stdlib)."""
        return urlparse(self.url)

    def header(self, name: str) -> str | None:
        """Case-insensitive header lookup convenience."""
        return self.headers.get(name.lower())

    def content_type(self) -> str | None:
        """The bare content-type without parameters (``"application/json"``
        from ``"application/json; charset=utf-8"``)."""
        ct = self.header("content-type")
        if ct is None:
            return None
        return ct.split(";", 1)[0].strip().lower()
