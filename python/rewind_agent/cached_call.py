"""``cached_llm_call`` decorator (Tier 2 of the Universal Replay Architecture).

Phase 2 building on Phase 0 (ExplicitClient cache APIs) + Phase 1
(intercept layer). Where Phase 1's ``intercept.install()`` patches
the HTTP transport globally, this module provides per-function
control: decorate a function, get caching behavior on its return
value.

## When to use this vs Phase 1's intercept

- **You want to cache the OUTER function** that composes multiple
  inner LLM/tool calls — decorator caches the composite result.
- **Your LLM call doesn't go through plain HTTP** (boto3 SigV4 to
  Bedrock, gRPC to a self-hosted model, etc.) — decorator caches at
  the function-return level, agnostic of transport.
- **You want explicit, line-by-line control** over what gets cached
  vs what hits live every time. Ergonomic for tests pinning specific
  functions.

## Composition with ``intercept.install()``

Both can be active in the same process. The decorator's cache check
happens FIRST (it wraps the user's function). On hit, no HTTP call
ever fires, so intercept never sees anything. On miss, the user's
function runs (inner HTTP calls might be intercepted), but the
decorator sets a contextvar (:data:`_cached_llm_call_active`) that
intercept's ``_flow`` checks: when set, intercept skips recording
to avoid double-recording the same logical event at two different
granularities.
"""

from __future__ import annotations

import contextvars
import functools
import hashlib
import inspect
import json
import logging
import time
from typing import Any, Callable

from rewind_agent.explicit import ExplicitClient

logger = logging.getLogger(__name__)


# Phase 2: contextvar set during a cached_llm_call invocation. Phase 1's
# _flow checks this and skips recording when set so we don't double-
# record under intercept.install(). Default False; only flipped during
# the decorator's body.
_cached_llm_call_active: contextvars.ContextVar[bool] = contextvars.ContextVar(
    "_cached_llm_call_active", default=False
)


def is_cached_llm_call_active() -> bool:
    """True when a ``cached_llm_call``-wrapped function is currently
    executing on this task. Used by :mod:`.intercept._flow` to
    suppress double-recording of inner HTTP calls."""
    return _cached_llm_call_active.get()


# Public types that callers will plug into the decorator.
ExtractModelFn = Callable[[Any, Any], str]
ExtractTokensFn = Callable[[Any, Any], "tuple[int, int]"]
CacheKeyFn = Callable[..., str]


def cached_llm_call(
    *,
    extract_model: ExtractModelFn | None = None,
    extract_tokens: ExtractTokensFn | None = None,
    cache_key: CacheKeyFn | None = None,
    name: str | None = None,
) -> Callable[[Callable[..., Any]], Callable[..., Any]]:
    """Decorator: wrap a function so its return value is cached by
    Rewind.

    Sync and async functions both supported (detected via
    :func:`inspect.iscoroutinefunction`). Generator and async-
    generator functions raise :class:`TypeError` at decoration time —
    streaming-style returns aren't representable in a single cached
    value.

    Parameters
    ----------
    extract_model:
        ``(call_args, return_value) -> model_name`` — used by
        ``record_llm_call`` and the savings counter. ``call_args`` is
        ``{"args": (...), "kwargs": {...}}``; ``return_value`` is
        whatever the function returned (or returned-from-cache on a
        hit). Default: empty string. For OpenAI-shaped returns, pass
        ``lambda call_args, ret: ret.model``.
    extract_tokens:
        ``(call_args, return_value) -> (tokens_in, tokens_out)``.
        Same call shape as ``extract_model``. Default: ``(0, 0)`` —
        the savings counter still ticks the cache_hit count but USD
        estimate / token totals stay zero. For OpenAI ChatCompletion-
        shaped returns, pass
        ``lambda call_args, ret: (ret.usage.prompt_tokens,
        ret.usage.completion_tokens)``.
    cache_key:
        ``(*args, **kwargs) -> str`` — override the default cache key
        derivation. Useful when args contain non-serializable objects
        (clients, connections) and you want to key on a derived ID
        instead. Default: SHA-256 of ``f"{fn_qualname}|{json(args, kwargs)}"``
        with ``_safe_repr`` fallback for non-JSON-able args.
    name:
        Optional name for telemetry / logging. Defaults to the
        wrapped function's ``__qualname__``.

    Returns
    -------
    A decorator. Apply via ``@cached_llm_call(...)`` (the parens are
    required even with no arguments — keyword-only signature).

    Examples
    --------

    Basic usage:

    >>> from rewind_agent import cached_llm_call
    >>> @cached_llm_call()
    ... def chat(question: str) -> dict:
    ...     return openai_client.chat.completions.create(
    ...         model="gpt-4o-mini",
    ...         messages=[{"role": "user", "content": question}],
    ...     ).model_dump()

    With custom token extraction:

    >>> @cached_llm_call(
    ...     extract_model=lambda call, ret: ret.model,
    ...     extract_tokens=lambda call, ret: (
    ...         ret.usage.prompt_tokens,
    ...         ret.usage.completion_tokens,
    ...     ),
    ... )
    ... def chat(question):
    ...     return openai_client.chat.completions.create(...)

    Async:

    >>> @cached_llm_call()
    ... async def chat(question: str) -> dict:
    ...     resp = await async_client.chat.completions.create(...)
    ...     return resp.model_dump()

    Custom cache key (skip un-hashable client arg):

    >>> @cached_llm_call(cache_key=lambda client, q, **_: q)
    ... def chat(client, question: str) -> dict:
    ...     return client.chat.completions.create(...)
    """

    def decorator(fn: Callable[..., Any]) -> Callable[..., Any]:
        if inspect.isgeneratorfunction(fn) or inspect.isasyncgenfunction(fn):
            raise TypeError(
                "cached_llm_call doesn't support generator / async-generator "
                "functions — they yield rather than return a single value, "
                "and Rewind's cache stores a single return per cache key. "
                "Wrap the generator's consumer in a regular function and "
                "decorate that instead."
            )

        fn_name = name or getattr(fn, "__qualname__", None) or getattr(
            fn, "__name__", "<anonymous>"
        )

        if inspect.iscoroutinefunction(fn):
            return _build_async_wrapper(
                fn,
                fn_name=fn_name,
                extract_model=extract_model,
                extract_tokens=extract_tokens,
                cache_key=cache_key,
            )

        return _build_sync_wrapper(
            fn,
            fn_name=fn_name,
            extract_model=extract_model,
            extract_tokens=extract_tokens,
            cache_key=cache_key,
        )

    return decorator


# ── Wrapper builders ───────────────────────────────────────────────


def _build_sync_wrapper(
    fn: Callable[..., Any],
    *,
    fn_name: str,
    extract_model: ExtractModelFn | None,
    extract_tokens: ExtractTokensFn | None,
    cache_key: CacheKeyFn | None,
) -> Callable[..., Any]:
    """Build a sync wrapper around ``fn``."""

    @functools.wraps(fn)
    def wrapper(*args: Any, **kwargs: Any) -> Any:
        return _invoke_sync(
            fn=fn,
            fn_name=fn_name,
            args=args,
            kwargs=kwargs,
            extract_model=extract_model,
            extract_tokens=extract_tokens,
            cache_key=cache_key,
        )

    return wrapper


def _build_async_wrapper(
    fn: Callable[..., Any],
    *,
    fn_name: str,
    extract_model: ExtractModelFn | None,
    extract_tokens: ExtractTokensFn | None,
    cache_key: CacheKeyFn | None,
) -> Callable[..., Any]:
    """Build an async wrapper around ``fn``."""

    @functools.wraps(fn)
    async def wrapper(*args: Any, **kwargs: Any) -> Any:
        return await _invoke_async(
            fn=fn,
            fn_name=fn_name,
            args=args,
            kwargs=kwargs,
            extract_model=extract_model,
            extract_tokens=extract_tokens,
            cache_key=cache_key,
        )

    return wrapper


# ── Sync invocation (cache-then-live decision flow) ────────────────


def _invoke_sync(
    *,
    fn: Callable[..., Any],
    fn_name: str,
    args: tuple,
    kwargs: dict,
    extract_model: ExtractModelFn | None,
    extract_tokens: ExtractTokensFn | None,
    cache_key: CacheKeyFn | None,
) -> Any:
    request_value = _build_request_payload(fn_name, args, kwargs, cache_key)
    client = ExplicitClient()

    # Cache lookup. RewindReplayDivergenceError propagates up to the
    # caller — strict-mode divergence MUST surface, not be turned into
    # a cache miss (Santa #4 contract from PR #149).
    cached = client.get_replayed_response(request_value)
    if cached is not None:
        # Hit — return the cached value. Decorator's contextvar is
        # NOT set here because we never invoke the user's function on
        # a hit (no inner HTTP calls happen, so no risk of intercept
        # double-recording).
        _record_cache_hit_savings(args=args, kwargs=kwargs, response=cached,
                                   extract_model=extract_model,
                                   extract_tokens=extract_tokens)
        return cached

    # Miss — call the user's function under the contextvar so any
    # inner HTTP calls (under intercept.install()) skip their own
    # recording and let us record at the function-level granularity.
    token = _cached_llm_call_active.set(True)
    started = time.monotonic()
    try:
        return_value = fn(*args, **kwargs)
    finally:
        _cached_llm_call_active.reset(token)
    duration_ms = int((time.monotonic() - started) * 1000)

    _record_live(
        client=client,
        request_value=request_value,
        return_value=return_value,
        args=args,
        kwargs=kwargs,
        duration_ms=duration_ms,
        extract_model=extract_model,
        extract_tokens=extract_tokens,
    )

    return return_value


# ── Async invocation (mirrors sync) ────────────────────────────────


async def _invoke_async(
    *,
    fn: Callable[..., Any],
    fn_name: str,
    args: tuple,
    kwargs: dict,
    extract_model: ExtractModelFn | None,
    extract_tokens: ExtractTokensFn | None,
    cache_key: CacheKeyFn | None,
) -> Any:
    request_value = _build_request_payload(fn_name, args, kwargs, cache_key)
    client = ExplicitClient()

    cached = await client.get_replayed_response_async(request_value)
    if cached is not None:
        _record_cache_hit_savings(args=args, kwargs=kwargs, response=cached,
                                   extract_model=extract_model,
                                   extract_tokens=extract_tokens)
        return cached

    token = _cached_llm_call_active.set(True)
    started = time.monotonic()
    try:
        return_value = await fn(*args, **kwargs)
    finally:
        _cached_llm_call_active.reset(token)
    duration_ms = int((time.monotonic() - started) * 1000)

    await _record_live_async(
        client=client,
        request_value=request_value,
        return_value=return_value,
        args=args,
        kwargs=kwargs,
        duration_ms=duration_ms,
        extract_model=extract_model,
        extract_tokens=extract_tokens,
    )

    return return_value


# ── Recording helpers ──────────────────────────────────────────────


def _record_live(
    *,
    client: ExplicitClient,
    request_value: dict[str, Any],
    return_value: Any,
    args: tuple,
    kwargs: dict,
    duration_ms: int,
    extract_model: ExtractModelFn | None,
    extract_tokens: ExtractTokensFn | None,
) -> None:
    response_value = _to_json_serializable(return_value)
    call_args = {"args": args, "kwargs": kwargs}
    model = _safe_extract_model(extract_model, call_args, return_value)
    tokens_in, tokens_out = _safe_extract_tokens(
        extract_tokens, call_args, return_value
    )
    try:
        client.record_llm_call(
            request=request_value,
            response=response_value,
            model=model,
            duration_ms=duration_ms,
            tokens_in=tokens_in,
            tokens_out=tokens_out,
        )
    except Exception as exc:  # pragma: no cover — defensive
        logger.warning("rewind: cached_llm_call record failed: %s", exc)


async def _record_live_async(
    *,
    client: ExplicitClient,
    request_value: dict[str, Any],
    return_value: Any,
    args: tuple,
    kwargs: dict,
    duration_ms: int,
    extract_model: ExtractModelFn | None,
    extract_tokens: ExtractTokensFn | None,
) -> None:
    response_value = _to_json_serializable(return_value)
    call_args = {"args": args, "kwargs": kwargs}
    model = _safe_extract_model(extract_model, call_args, return_value)
    tokens_in, tokens_out = _safe_extract_tokens(
        extract_tokens, call_args, return_value
    )
    try:
        await client.record_llm_call_async(
            request=request_value,
            response=response_value,
            model=model,
            duration_ms=duration_ms,
            tokens_in=tokens_in,
            tokens_out=tokens_out,
        )
    except Exception as exc:  # pragma: no cover — defensive
        logger.warning("rewind: cached_llm_call_async record failed: %s", exc)


def _record_cache_hit_savings(
    *,
    args: tuple,
    kwargs: dict,
    response: Any,
    extract_model: ExtractModelFn | None,
    extract_tokens: ExtractTokensFn | None,
) -> None:
    """Update the Phase 1 savings counter with the cache-hit's
    estimated tokens-saved. Best-effort; failures don't propagate."""
    try:
        from rewind_agent.intercept._savings import record_cache_hit
    except ImportError:  # pragma: no cover — intercept package present in same SDK
        return
    call_args = {"args": args, "kwargs": kwargs}
    model = _safe_extract_model(extract_model, call_args, response)
    tokens_in, tokens_out = _safe_extract_tokens(
        extract_tokens, call_args, response
    )
    try:
        record_cache_hit(model=model, tokens_in=tokens_in, tokens_out=tokens_out)
    except Exception as exc:  # pragma: no cover — defensive
        logger.debug("rewind: savings counter update failed: %s", exc)


def _safe_extract_model(
    fn: ExtractModelFn | None, call_args: dict, return_value: Any
) -> str:
    if fn is None:
        return ""
    try:
        result = fn(call_args, return_value)
        return result if isinstance(result, str) else ""
    except Exception as exc:
        logger.debug("rewind: extract_model failed: %s", exc)
        return ""


def _safe_extract_tokens(
    fn: ExtractTokensFn | None, call_args: dict, return_value: Any
) -> tuple[int, int]:
    if fn is None:
        return (0, 0)
    try:
        result = fn(call_args, return_value)
        if isinstance(result, tuple) and len(result) == 2:
            tin, tout = result
            return (int(tin) if isinstance(tin, int) else 0,
                    int(tout) if isinstance(tout, int) else 0)
    except Exception as exc:
        logger.debug("rewind: extract_tokens failed: %s", exc)
    return (0, 0)


# ── Cache-key + serialization helpers ──────────────────────────────


def _build_request_payload(
    fn_name: str,
    args: tuple,
    kwargs: dict,
    cache_key: CacheKeyFn | None,
) -> dict[str, Any]:
    """Build the synthetic 'request body' that gets sent to
    ExplicitClient.{get_replayed_response, record_llm_call}.

    Shape (stable; keep this stable for cache-hit consistency across
    SDK versions):

    ::

        {
          "_rewind_decorator": "cached_llm_call",
          "fn_name": "<qualname>",
          "cache_key": "<sha256-hex>"
        }

    **Identity-only payload (Review #2 fix on PR #151).** The server's
    content-hash validation (Phase 0) hashes the WHOLE request body to
    derive the request_hash for cache lookup. If we included
    ``args_repr`` / ``kwargs_repr`` here, two calls with the same custom
    ``cache_key`` but different non-stable args (e.g. an OpenAI client
    object whose repr embeds a memory address) would produce different
    request_hashes and miss the cache.

    The fix: only stable identity fields go in the payload. The
    ``cache_key`` (default-derived from args via ``_default_cache_key``,
    or user-supplied via ``cache_key=`` parameter) IS the identity.
    Args / kwargs are NOT in the payload at all — when the user passes
    a custom ``cache_key=lambda client, q, **_: q``, the client object's
    unstable repr is correctly invisible to the cache.

    For dashboard display, the ``fn_name + cache_key`` pair is enough
    to identify the call. Power users wanting full args in the dashboard
    can encode them inside their custom ``cache_key`` (a plain string
    like ``f"chat:{q}"`` instead of a hash) so the key itself is
    human-readable.
    """
    if cache_key is not None:
        try:
            key = cache_key(*args, **kwargs)
            if not isinstance(key, str):
                # Defensive: coerce to string. Custom cache_key
                # functions returning bytes / ints / etc. shouldn't
                # crash; they should still produce a usable key.
                key = str(key)
        except Exception as exc:
            logger.warning(
                "rewind: custom cache_key raised; falling back to default. err=%s", exc
            )
            key = _default_cache_key(fn_name, args, kwargs)
    else:
        key = _default_cache_key(fn_name, args, kwargs)

    return {
        "_rewind_decorator": "cached_llm_call",
        "fn_name": fn_name,
        "cache_key": key,
    }


def _default_cache_key(fn_name: str, args: tuple, kwargs: dict) -> str:
    """Stable cache key from fn_name + args + kwargs.

    SHA-256 of a JSON serialization with ``_safe_repr`` fallback for
    non-JSON-able values. Deterministic for equivalent inputs:
    ``sorted(kwargs.items())`` for ordering invariance,
    ``_safe_repr`` for non-serializable args.
    """
    payload = {
        "fn": fn_name,
        "args": [_safe_repr(a) for a in args],
        "kwargs": {k: _safe_repr(v) for k, v in sorted(kwargs.items())},
    }
    serialized = json.dumps(payload, sort_keys=True, default=_safe_repr)
    return hashlib.sha256(serialized.encode("utf-8")).hexdigest()


def _safe_repr(obj: Any) -> Any:
    """JSON-default callback that returns a stable string for
    non-serializable values.

    The point is "stable hash for equivalent inputs", not "fully
    reversible representation". We use ``repr()`` which is stable
    for primitives + most types; pathological cases (objects with
    address-based reprs that change between runs) would defeat
    caching, but the user can provide a custom ``cache_key``
    function for those.
    """
    if isinstance(obj, (str, int, float, bool, type(None))):
        return obj
    if isinstance(obj, (list, tuple)):
        return [_safe_repr(x) for x in obj]
    if isinstance(obj, dict):
        return {str(k): _safe_repr(v) for k, v in obj.items()}
    return repr(obj)


def _to_json_serializable(value: Any) -> Any:
    """Convert an arbitrary return value to a JSON-serializable shape.

    Handles the common cases:

    - Already JSON-able (dict / list / primitives) — returned as-is.
    - Has ``model_dump()`` (Pydantic, OpenAI SDK return types) —
      called to get a dict.
    - Has ``__dict__`` — extracted as a dict.
    - Otherwise — ``repr()`` fallback.

    The stored value on cache hit will be the JSON-deserialized form,
    NOT the original Python type. Operators wanting type fidelity
    should ``return response.model_dump()`` in their decorated
    function and reconstruct on the call site (or use a custom
    response-type wrapper).
    """
    # Fast path: direct JSON serialization. Catches dict, list,
    # primitives without any inspection.
    try:
        json.dumps(value)
        return value
    except (TypeError, ValueError):
        pass

    # Pydantic v2 / OpenAI SDK return types.
    model_dump = getattr(value, "model_dump", None)
    if callable(model_dump):
        try:
            dumped = model_dump()
            json.dumps(dumped)
            return dumped
        except Exception:
            pass

    # Pydantic v1 fallback (if the user is on the older API).
    dict_method = getattr(value, "dict", None)
    if callable(dict_method):
        try:
            dumped = dict_method()
            json.dumps(dumped)
            return dumped
        except Exception:
            pass

    # Last resort: extract __dict__.
    inst_dict = getattr(value, "__dict__", None)
    if isinstance(inst_dict, dict):
        try:
            cleaned = {k: _to_json_serializable(v) for k, v in inst_dict.items()}
            json.dumps(cleaned)
            return cleaned
        except Exception:
            pass

    # Pathological case — log and store a repr. Future cache hits
    # will get the repr string back as the "response", which the
    # user's code probably can't reconstruct. Documented in the
    # decorator's docstring.
    logger.warning(
        "rewind: cached_llm_call return value isn't JSON-serializable "
        "(%s); storing repr. Consider returning a dict (e.g. via "
        "response.model_dump()) for type-fidelity on cache hits.",
        type(value).__name__,
    )
    return repr(value)
