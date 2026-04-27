# Phase 2 — `cached_llm_call` Decorator (Tier 2 of the Universal Replay Architecture)

**Status:** Planning. Branch `feat/phase-2-cached-llm-call-decorator` off master `2d2f995` (post-Phase 1 merge).

**Predecessor:** [PR #149 (Phase 1 — HTTP transport adapters)](https://github.com/agentoptics/rewind/pull/149) — merged. The `_flow.handle_intercepted_{sync,async}` orchestration, ExplicitClient cache APIs, and `RewindReplayDivergenceError` from Phase 1 are the building blocks here.

**One-line goal:** _Wrap a Python function once, and Rewind caches its result._

## Why this PR exists

Phase 1's `intercept.install()` patches the HTTP transport layer globally — every httpx/requests/aiohttp call from the process gets evaluated against predicates. That's powerful but blunt: you opt the WHOLE PROCESS in or out. Some operators want **per-call-site control**:

- Library code that's a thin wrapper around an LLM call ("here's the function you decorate")
- Functions that compose multiple LLM calls + tool calls; you want to cache the OUTER function not the inner HTTP requests
- Frameworks that don't go through plain HTTP (e.g. AWS Bedrock invokes via boto3 which wraps SigV4-signed requests in a non-recordable shape, or self-hosted models using gRPC)
- Tests that pin specific functions to known recordings

The decorator gives them a single primitive: `@cached_llm_call`. Wraps any function (sync or async); returns the cached result on hit or calls the function and records the live result on miss.

## Architecture

### Composition with Phase 0 + Phase 1

```
┌────────────────────────────────────────────────────────────────────┐
│ User code                                                          │
│                                                                    │
│   @cached_llm_call(extract_tokens=lambda r: (r.usage.x, r.usage.y))│
│   def chat(question: str) -> ChatCompletion:                       │
│       return openai_client.chat.completions.create(...)            │
└──────────────────────────────────────────────────────────────────┬─┘
                                                                   │
                                                                   ▼
┌────────────────────────────────────────────────────────────────────┐
│ Phase 2 — cached_llm_call decorator                                │
│                                                                    │
│   1. Hash function name + args                                     │
│   2. ExplicitClient.get_replayed_response (Phase 0)                │
│   3a. Hit  → return cached, increment savings (Phase 1)            │
│   3b. Miss → call original → ExplicitClient.record_llm_call        │
└────────────────────────────────────────────────────────────────────┘
                                                                   │
                                                                   ▼
                                  ┌──────────────────────────────────┐
                                  │  Phase 0 — Explicit Recording API│
                                  │  (cache content validation,      │
                                  │   strict-match 409, envelope     │
                                  │   format)                        │
                                  └──────────────────────────────────┘
```

### Key surface

```python
# rewind_agent/cached_call.py (new module)

def cached_llm_call(
    *,
    extract_model: Callable[[Any, Any], str] | None = None,
    extract_tokens: Callable[[Any, Any], tuple[int, int]] | None = None,
    cache_key: Callable[..., str] | None = None,
    name: str | None = None,
):
    """Decorator: wrap a function so its return value is cached
    by Rewind.

    Parameters
    ----------
    extract_model:
        ``(args, return_value) -> model_name``. Used by record_llm_call
        and the savings counter. Defaults to None — model recorded as
        empty string. Pass a lambda that pulls the model from the return
        type your function emits (e.g.
        ``lambda args, ret: ret.model``).
    extract_tokens:
        ``(args, return_value) -> (tokens_in, tokens_out)``. Same
        rationale; defaults to (0, 0). For OpenAI ChatCompletion-shaped
        returns, ``lambda args, ret: (ret.usage.prompt_tokens,
        ret.usage.completion_tokens)``.
    cache_key:
        ``(*args, **kwargs) -> str``. Override the default cache key
        derivation. Default: SHA-256 of ``f"{fn_name}|{json.dumps(args, kwargs)}"``.
        Useful when args contain non-serializable objects (clients,
        connections) and you want to key on a derived ID instead.
    name:
        Optional name for telemetry / logging. Defaults to the function's
        ``__qualname__``.

    Returns
    -------
    A decorator. Works on both sync and async functions; the wrapper
    is async-aware.
    """
```

### Why a decorator factory (`@cached_llm_call(...)`) rather than `@cached_llm_call`

The factory lets the user pass `extract_tokens` etc. as keyword args. The plain-decorator form would require positional unpacking and is less ergonomic. Both forms can be supported (factory + bare-decorator) via the standard `if not callable(args[0])` trick — see implementation below.

### Composition with `intercept.install()`

When the decorator runs and `intercept` is also active:

- Decorator's check happens FIRST (it wraps the user's function).
- If decorator caches a hit → returns cached, no HTTP call ever happens, so intercept never sees anything.
- If decorator misses → user function runs → makes an HTTP call → intercept records that HTTP call.
- Then the decorator records its OWN result via `record_llm_call`.

That's a double-record on miss. We need to either:
1. Suppress the inner HTTP recording when the decorator wraps it.
2. Accept the double-record as informational (different granularity — one captures the HTTP, one captures the function-level semantics).
3. Use a contextvar flag set by the decorator that intercept checks.

**Plan: option 3.** A contextvar `_cached_llm_call_active` set to True during the decorator's call. Intercept's `_flow.handle_intercepted_{sync,async}` checks it: if set, skips recording (the decorator will record at a higher level). This gives us clean composition without double-records.

### Cache key derivation

Default: serialize args + kwargs as JSON, append function qualname, SHA-256 hash. Specifically:

```python
def _default_cache_key(fn_name: str, args: tuple, kwargs: dict) -> str:
    # Best-effort serialization; non-JSON-able args (e.g. open files,
    # client objects) get repr()'d. The point is "stable hash for
    # equivalent inputs" not "fully reversible representation".
    payload = {
        "fn": fn_name,
        "args": [_safe_repr(a) for a in args],
        "kwargs": {k: _safe_repr(v) for k, v in sorted(kwargs.items())},
    }
    serialized = json.dumps(payload, sort_keys=True, default=_safe_repr)
    return hashlib.sha256(serialized.encode("utf-8")).hexdigest()
```

`_safe_repr` falls back to `repr(obj)` for non-JSON types. The cache key is opaque to the user — they don't care about its shape, only that "same inputs → same key".

### What gets sent to the server

The decorator builds a synthetic "request body" for `ExplicitClient.record_llm_call` and `get_replayed_response`. Shape:

```json
{
  "_rewind_decorator": "cached_llm_call",
  "fn_name": "my_module.chat",
  "cache_key": "<sha256-hex>",
  "args_repr": ["question text"],
  "kwargs_repr": {}
}
```

The cache_key field is what the server's content-hash validation matches on (Phase 0 #4 cache validation). The fn_name + args_repr fields are for human-readable display in the dashboard; they don't affect cache lookup.

**Cache hit semantics:** server returns the previously-recorded `response_value`. The decorator returns it as the function's return. Type fidelity is preserved if the original return was JSON-serializable (dicts, lists, primitives). For complex types (Pydantic models, dataclasses, OpenAI's ChatCompletion), the user is responsible for ensuring round-trip compatibility — typically via the `response_model.parse_obj(cached)` pattern after the call.

We document this clearly: **the decorator caches JSON-serializable return values**. For richer types, wrap the return in `dict(model_dump=True)` or use the SDK's own `from_dict` constructor.

## What ships in this PR

### New files

- `python/rewind_agent/cached_call.py` — the decorator + helpers (~250 LOC)
- `python/tests/test_cached_call.py` — sync + async, hit / miss / divergence, custom keys, custom extractors, composition with `intercept.install()` (~300 LOC)

### Updates

- `python/rewind_agent/__init__.py` — re-export `cached_llm_call` from the top level
- `python/rewind_agent/intercept/_flow.py` — check the new contextvar `_cached_llm_call_active` and skip recording when set (prevents double-record under intercept)
- `docs/cached-llm-call.md` (new) — operator-facing docs in the same style as `intercept-quickstart.md` (~200 lines markdown)
- `docs/recording.md` — extend the decision matrix from three ways to four. The decorator becomes "Decorator mode (per-function)".
- `docs/getting-started.md` — add a brief mention pointing at the decorator for users who want function-level control.

### Tests

~25 cases covering:

- Basic sync function — cache hit returns cached, miss records and returns live
- Basic async function — same matrix
- Custom `extract_tokens` / `extract_model` — values reach `record_llm_call` correctly
- Custom `cache_key` — overrides default derivation
- Argless function — no kwargs/args still hashes to a stable key
- Non-JSON args (callable, file handle) — `_safe_repr` fallback works
- Strict-match divergence on cache hit — `RewindReplayDivergenceError` propagates from the underlying lookup
- Composition with `intercept.install()` — decorator runs first, no double-record on miss (verified via contextvar check)
- Decorator on a method — `self` is part of the cache key only via `_safe_repr`; same instance produces same key
- Decorator under `replay_context` (set by `ExplicitClient.start_replay`) — uses the right replay context for lookup

## Open questions

1. **Sync vs async detection.** Use `inspect.iscoroutinefunction(fn)` at decoration time; build sync wrapper or async wrapper accordingly. Edge case: generator functions (`yield`) and async generators — punt to v2.1; raise an error if a generator function is decorated.

2. **Class methods + classmethods.** `self` in args means each instance produces a different cache key. Document this; usually correct (different instance → different LLM context). For class methods (cls is first arg), same. Static methods: just like plain functions.

3. **Return value JSON-serializability.** What happens if the function returns an `openai.ChatCompletion` object that's not JSON-serializable directly?
   - On miss: we record the response. If it's not JSON-able, recording fails and we log a warning, but still return the live result.
   - On hit: we return the JSON dict from the cache. The user gets a dict instead of their custom type. Document this clearly.
   - Mitigation: encourage users to convert in the function (`return response.model_dump()`) for cacheable types.

4. **`cache_llm_call` vs `cached_llm_call`.** The plan called it `cached_llm_call` so that's what we ship. Bikeshed-able; see if user feedback prompts a rename.

## Sequencing within the PR

7 commits ordered for review-friendliness:

1. `plan: phase 2 cached_llm_call decorator` (this doc as anchor)
2. `feat(python): cached_call module + sync + async wrappers`
3. `feat(python): cached_call cache key + safe_repr serialization`
4. `feat(python): _flow contextvar to skip double-record under intercept`
5. `test(python): cached_call tests`
6. `docs: cached-llm-call.md operator how-to + decision matrix update`
7. `chore: bump python sdk to 0.16.0` (or stays at 0.15.0 if 0.15.0 still unpublished — check at PR time)

## Acceptance criteria

- [ ] `@cached_llm_call(...)` works on sync and async functions
- [ ] Cache hit returns the recorded value; miss calls live and records
- [ ] Custom `extract_model` / `extract_tokens` / `cache_key` work
- [ ] Strict-match divergence propagates `RewindReplayDivergenceError`
- [ ] No double-record when used under `intercept.install()`
- [ ] All Phase 1 tests still pass; no regressions
- [ ] `scripts/pre-push-check.sh` all 5 stages green
- [ ] Documentation in matching style of existing docs

## NOT in this PR (deferred)

- **Tier 3 — runner registry + dashboard "Run replay" button** — separate PR
- **Generator / async-generator decorator support** — punt; raise on decoration if needed
- **Auto-detection of return-type → token extraction** — manual `extract_tokens` only; auto-detect could be v2.1 if user feedback warrants
