"""Predicates that route HTTP requests to record/replay or pass-through.

Phase 1 of the Universal Replay Architecture. Each transport adapter
(:mod:`.httpx_transport`, :mod:`.requests_adapter`,
:mod:`.aiohttp_middleware`) builds a :class:`RewindRequest` from its
library-native request, then asks two questions in order:

1. ``is_llm_call(req)`` — should this go through the LLM record/replay
   pipeline (cache lookup, ResponseEnvelope record on miss, synthetic
   SSE on streaming cache hit)?
2. ``is_tool_call(req)`` — should this go through the tool-call record
   pipeline (no cache lookup; tool calls aren't replayed today)?

Either-or-neither: if both return False, the adapter passes the request
through untouched and never touches the recording surface. If both
return True, ``is_llm_call`` wins (LLM call routing supersedes tool
call routing — the typical case is an LLM endpoint that also looks
like a tool because the operator's gateway routes both through the
same prefix).

## Default predicate strictness

The default predicates only fire on hosts we've actually tested
(see :data:`DEFAULT_LLM_HOSTS`). Operators with custom gateways pass
``intercept.install(predicates=…)`` with a custom matcher. Rationale:
silent recording of the wrong endpoints is worse than a quick
"why isn't anything being recorded?" debugging trip — operators with
non-listed providers will hit the latter within seconds and reach for
the documented escape hatch.
"""

from __future__ import annotations

from typing import Protocol, runtime_checkable

from rewind_agent.intercept._request import RewindRequest


# LLM provider hostnames we've tested as record+replay targets. Strict
# by design — see module docstring.
#
# Ordering doesn't matter (frozenset), but we keep the literal in
# alphabetical order for diff stability when adding providers.
DEFAULT_LLM_HOSTS: frozenset[str] = frozenset(
    {
        "api.anthropic.com",
        "api.cohere.ai",
        "api.deepseek.com",
        "api.groq.com",
        "api.mistral.ai",
        "api.openai.com",
        "api.together.xyz",
        "generativelanguage.googleapis.com",  # Google Gemini
    }
)


def default_is_llm_call(req: RewindRequest) -> bool:
    """True iff the request hits one of :data:`DEFAULT_LLM_HOSTS`.

    Host comparison is exact + case-insensitive. Subdomains do NOT
    match — ``api.openai.com`` is recorded, ``proxy.openai.com`` is not.
    Operators routing through their own subdomain pass a custom
    predicate.

    Why exact match? Two reasons:

    1. **Predictability.** A user looking at "what got recorded?" can
       grep for the listed hosts and trust nothing else snuck in.
    2. **Gateway opacity.** Many enterprise deployments terminate TLS
       at a corporate gateway (``llm-gateway.corp.example``) that
       routes to multiple upstream providers. Adding a substring or
       suffix match would surface those gateways even when the
       operator hasn't opted in. Custom predicate is the right tool
       there.
    """
    host = req.url_parts.netloc.lower()
    # Strip any user:pass@ prefix and :port suffix; compare just the
    # hostname so a `https://user:pw@api.openai.com:443/...` URL still
    # matches. urllib's urlparse leaves the host bare in `netloc` only
    # when there's no userinfo — defensive split here is cheap.
    if "@" in host:
        host = host.rsplit("@", 1)[1]
    if ":" in host:
        host = host.rsplit(":", 1)[0]
    return host in DEFAULT_LLM_HOSTS


def default_is_tool_call(_req: RewindRequest) -> bool:
    """Default: never. HTTP-based tool calls are deployment-specific
    (each operator's gateway has different routing). Custom predicate
    is the right place. Keeping the default off avoids surprise
    recording of internal HTTP endpoints that happen to flow through
    the same Python process as the LLM client.
    """
    return False


@runtime_checkable
class Predicates(Protocol):
    """Routing decisions for the intercept layer.

    The two methods are queried per-request. Implementations can be
    stateful (e.g. a predicate that watches a feature flag) but should
    be cheap — they run on every HTTP request the process makes,
    including non-LLM ones.
    """

    def is_llm_call(self, req: RewindRequest) -> bool:
        ...

    def is_tool_call(self, req: RewindRequest) -> bool:
        ...


class DefaultPredicates:
    """Concrete strict-by-default predicate set.

    Equivalent to passing nothing to ``install()``. Held as a class
    rather than a singleton so users who want to compose with the
    default ("listed hosts plus my private gateway") can subclass:

    >>> class MyPredicates(DefaultPredicates):
    ...     def is_llm_call(self, req):
    ...         if req.url_parts.netloc.endswith(".my-gateway.example"):
    ...             return True
    ...         return super().is_llm_call(req)
    >>> intercept.install(predicates=MyPredicates())
    """

    def is_llm_call(self, req: RewindRequest) -> bool:
        return default_is_llm_call(req)

    def is_tool_call(self, req: RewindRequest) -> bool:
        return default_is_tool_call(req)
