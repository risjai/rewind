# Phase 1 — HTTP Transport Adapters (Tier 1 of the Universal Replay Architecture)

**Status:** Planning. Branch `feat/phase-1-http-adapters` off master `cd1cad4` (post-Phase 0 merge).

**Predecessor:** [`feat/step-0-prereqs`](https://github.com/agentoptics/rewind/pull/148) — merged Phase 0 prerequisites (cache content validation, response envelope, intercept core primitives).

**One-line goal:** _Wrap your Python HTTP client once, and replay just works._

## Why this PR

After Phase 0, the replay machinery is correct end-to-end but only reachable from two places:

1. The CLI proxy (`rewind run -- <agent>`) — works, but assumes OpenAI-compatible SDK traffic.
2. `ExplicitClient.record_llm_call()` calls scattered through agent code — reliable but requires per-call-site changes.

Neither covers the agents we actually care about: ones built on bespoke HTTP gateways (`ray-agent`'s mTLS flow), agents using older SDKs, or third-party libraries that bypass the OpenAI SDK entirely. Phase 2 closes the gap by intercepting at the **transport layer** of the three Python HTTP clients that virtually every LLM library is built on:

- **`httpx`** — OpenAI ≥ 1.0, Anthropic SDK, most modern Python LLM clients.
- **`requests`** — older Anthropic SDK, LangChain, plenty of homegrown clients.
- **`aiohttp`** — pure-async stacks, some agentic frameworks, web-scale services.

A single `intercept.install()` call patches all three, and any HTTP-based LLM call from anywhere in the process gets the cache-then-live treatment automatically.

## What ships in this PR

### New files

- `python/rewind_agent/intercept/_install.py` — orchestrator: idempotent `install(predicates=…)` + `uninstall()` for cleanup in tests.
- `python/rewind_agent/intercept/_predicates.py` — default `is_llm_call` / `is_tool_call` predicates that fire on the URLs of the three big providers (OpenAI / Anthropic / common gateway prefixes). Operators can pass custom predicates to `install(predicates=…)`.
- `python/rewind_agent/intercept/httpx_transport.py` — `RewindHTTPTransport` and `RewindAsyncHTTPTransport` subclasses of `httpx.HTTPTransport` / `httpx.AsyncHTTPTransport`. `_install` patches `httpx.Client.__init__` / `httpx.AsyncClient.__init__` so even late-bound clients get intercepted.
- `python/rewind_agent/intercept/requests_adapter.py` — `RewindHTTPAdapter` subclass of `requests.adapters.HTTPAdapter`. `_install` patches `requests.Session.__init__` so any session created post-install gets the adapter.
- `python/rewind_agent/intercept/aiohttp_middleware.py` — `RewindTraceConfig` built on `aiohttp.TraceConfig` events (`on_request_start`, `on_request_chunk_sent`, `on_response_chunk_received`). `_install` patches `aiohttp.ClientSession.__init__` to inject the trace config.
- `python/rewind_agent/intercept/_flow.py` — shared cache-then-live decision logic. Library-specific adapters call into this so the orchestration is consistent.

### Updates to public API

- `python/rewind_agent/intercept/__init__.py` — re-exports `install`, `uninstall`, `default_is_llm_call`, `default_is_tool_call`.
- `python/rewind_agent/__init__.py` — top-level `from rewind_agent import intercept` works (already does, just confirming).

### Tests

- `python/tests/test_intercept_httpx.py` — sync + async, GET/POST, streaming + buffered, cache hit + miss + divergence, predicate routing.
- `python/tests/test_intercept_requests.py` — same matrix, sync only.
- `python/tests/test_intercept_aiohttp.py` — same matrix, async only.
- `python/tests/test_intercept_install.py` — install/uninstall idempotency, multiple installs, install-after-client-creation, install with custom predicates.
- `python/tests/test_intercept_flow.py` — cache-then-live decision logic in isolation (no real HTTP).

Estimated test count: ~80 (matches the per-adapter combinatorics × happy/sad paths).

### Validation target

- `use-cases/ray-agent/code/rewind_hook.py` migrated from manual `RewindHook` + `ExplicitClient` to a single `intercept.install()` call. Net diff: ~120 LOC removed, ~3 LOC added in `serve/main.py`. Ships in this PR's commit history but as a separate commit so the diff is reviewable in isolation.

## Architecture

### Composition with Phase 0 primitives

```
┌─────────────────────────────────────────────────────────────────┐
│ Agent code (any HTTP-based LLM client, any framework)          │
└──────────────────────────────────┬──────────────────────────────┘
                                   │
                  ┌────────────────┼────────────────┐
                  │                │                │
                  ▼                ▼                ▼
            ┌─────────┐      ┌──────────┐     ┌─────────┐
            │  httpx  │      │ requests │     │ aiohttp │
            │ Client  │      │ Session  │     │ Client  │
            └────┬────┘      └────┬─────┘     └────┬────┘
                 │                │                 │
                 ▼                ▼                 ▼
       ╔════════════════════════════════════════════════════╗
       ║  rewind_agent.intercept transport adapters         ║
       ║  ─────────────────────────────────────────────     ║
       ║  1. Build RewindRequest(url, method, headers, body)║
       ║  2. predicates(req) → is_llm? skip / is_tool? skip ║
       ║  3. flow.handle(req) → live or cached              ║
       ╚════════════════════════════════════════════════════╝
                                   │
            ┌──────────────────────┼──────────────────────┐
            │                      │                      │
            ▼                      ▼                      ▼
   ┌──────────────────┐  ┌─────────────────┐   ┌──────────────────┐
   │ Phase 0          │  │ ExplicitClient  │   │ Phase 0          │
   │ detect_streaming │  │ ─────────       │   │ ResponseEnvelope │
   │ + synthetic_sse  │  │ get_replayed_   │   │ + format=0/1     │
   │ + RewindRequest  │  │ response()      │   │ unwrap on read   │
   │ (already merged) │  │ record_llm_call │   │ (already merged) │
   └──────────────────┘  └─────────────────┘   └──────────────────┘
```

Phase 0 left exactly the right primitives in place. Phase 2 is glue: build a `RewindRequest` from the library-native request, run the predicate, decide live-vs-cached, and on cache hit re-emit a wire-faithful response (synthetic SSE for streaming, buffered JSON for non-streaming).

### Cache-then-live decision flow (`_flow.py`)

```python
def handle_intercepted(
    req: RewindRequest,
    *,
    predicates: Predicates,
    live: Callable[[], LibResponse],
) -> LibResponse:
    if not predicates.is_llm_call(req):
        return live()  # not our concern, pass through

    # Pull request body bytes (already buffered by adapter)
    request_value = parse_request_body(req)  # JSON-decoded for cache key match

    # Phase 0 cache lookup with content validation
    cached = explicit.get_replayed_response(request_value)
    if cached is not None:
        # Cache hit — synthesize a library-native response from the cached body.
        if detect_streaming(req):
            return adapter_stream_response(cached)  # synthetic SSE, [DONE] sentinel
        return adapter_buffered_response(cached)    # JSON content-type, body=cached

    # Cache miss — go live, record on the way back.
    started = monotonic()
    resp = live()
    duration_ms = int((monotonic() - started) * 1000)
    response_value, tokens_in, tokens_out, model = parse_response(req, resp)
    explicit.record_llm_call(
        request=request_value, response=response_value,
        model=model, duration_ms=duration_ms,
        tokens_in=tokens_in, tokens_out=tokens_out,
    )
    return resp
```

`adapter_stream_response` / `adapter_buffered_response` are per-adapter — they construct an `httpx.Response`, a `requests.Response`, or an `aiohttp.ClientResponse` from the bytes. Each transport adapter implements them; `_flow.handle_intercepted` doesn't know which library it's serving.

### Streaming buffer policy

For sync clients (`requests`), the body is already buffered by the time the adapter sees it, so this is trivial. For async clients (`httpx.AsyncClient`, `aiohttp.ClientSession`), a streaming **request** body (file upload, chunked POST) needs to be drained into bytes before predicate evaluation, then re-emitted for the live path. `_flow` exposes a helper `buffer_and_replay(stream)` that handles the rewind correctly.

For streaming **responses**:
- Cache hit: synthesize via `synthetic_sse_for_cache_hit(cached_body)` from Phase 0. Single chunk + `[DONE]` sentinel. v1 fidelity.
- Cache miss: pass through live, accumulate bytes into a buffer, record the assembled final body when the stream closes. The proxy already uses this exact pattern in `handle_streaming_response` (`crates/rewind-proxy/src/lib.rs:735`); the Python adapter is structurally similar.

### Predicate contract

```python
@runtime_checkable
class Predicates(Protocol):
    def is_llm_call(self, req: RewindRequest) -> bool: ...
    def is_tool_call(self, req: RewindRequest) -> bool: ...

# Default predicates fire on URL prefix:
DEFAULT_LLM_HOSTS = (
    "api.openai.com",
    "api.anthropic.com",
    "generativelanguage.googleapis.com",   # Gemini
    "api.cohere.ai",
    "api.together.xyz",
    "api.groq.com",
    "api.deepseek.com",
)
```

Operators with custom gateways (ray-agent's mTLS proxy, Azure OpenAI deployments) pass `install(predicates=MyPredicates())`. The signature is small enough that predicates copy-paste cleanly between agents — see `RewindRequest` in Phase 0 for the rationale.

## Per-adapter implementation notes

### httpx (`httpx_transport.py`)

- Subclass `httpx.HTTPTransport.handle_request` / `httpx.AsyncHTTPTransport.handle_async_request`.
- The `Request` object has `request.url`, `request.method`, `request.headers` (case-insensitive `Headers`), and `request.read()` / `await request.aread()` for body bytes.
- For streaming response on cache hit, return an `httpx.Response(200, headers=…, stream=httpx.ByteStream(synthetic_sse_for_cache_hit(body)))`.
- `httpx.AsyncByteStream` for the async path.
- Patch `httpx.Client.__init__` / `httpx.AsyncClient.__init__` to inject our transport when the user didn't pass one. If they passed their own transport, **wrap** it (preserving any custom retry/timeout config they wanted).

### requests (`requests_adapter.py`)

- Subclass `requests.adapters.HTTPAdapter.send`.
- The `PreparedRequest` has `prepared.url`, `prepared.method`, `prepared.headers`, `prepared.body`. Body might be bytes, str, or a generator (for streaming uploads).
- For cache hit, build a `requests.Response` with `_content` set to the cached body (or to the SSE-formatted bytes for streaming consumers).
- For streaming consumers using `response.iter_content()`, the `iter_synthetic_sse_chunks` (sync) generator from Phase 0 works directly.
- Patch `requests.Session.__init__` to mount our adapter on `https://` and `http://` schemes. Pre-existing sessions need an explicit `session.mount(...)` call — document this caveat.

### aiohttp (`aiohttp_middleware.py`)

- Use `aiohttp.TraceConfig` with `on_request_start` / `on_request_chunk_sent` / `on_response_chunk_received`.
- Trace configs are the official extension point; subclassing `ClientSession` is fragile across versions.
- For cache hit, we can't return a synthetic response from a trace config event — instead, raise `aiohttp.ClientError` to short-circuit the actual request, then provide the cached response via a small `RewindReplayConnector` that intercepts before the transport.
- This is the trickiest of the three. May need a follow-up if the trace-config approach doesn't compose cleanly. Fallback: monkey-patch `ClientSession._request` directly (less elegant but works).

### `install()` orchestrator (`_install.py`)

```python
_INSTALLED = False
_ORIGINAL_HTTPX_INIT = None
_ORIGINAL_HTTPX_ASYNC_INIT = None
_ORIGINAL_REQUESTS_INIT = None
_ORIGINAL_AIOHTTP_INIT = None

def install(predicates: Predicates = DEFAULT_PREDICATES) -> None:
    """Idempotent — calling twice is a no-op. Patches all three libraries
    if they're importable; missing libraries are silently skipped."""
    global _INSTALLED
    if _INSTALLED:
        return
    _patch_httpx_if_available(predicates)
    _patch_requests_if_available(predicates)
    _patch_aiohttp_if_available(predicates)
    _INSTALLED = True

def uninstall() -> None:
    """Reverse install(). Mainly for tests; production rarely uninstalls."""
    ...
```

`install()` is the **only** public entry point users need. They don't touch transports, adapters, or middleware directly. Custom predicates flow through this single seam.

## Test plan

### Unit tests (per adapter)

For each of httpx (sync+async), requests, aiohttp:

- ☐ Buffered POST cache miss → live request hits upstream, response recorded
- ☐ Buffered POST cache hit → cached body returned, no upstream request
- ☐ Streaming POST cache miss → live SSE proxied through, accumulated body recorded as envelope
- ☐ Streaming POST cache hit → synthetic SSE returned, `[DONE]` sentinel terminates the stream
- ☐ Cache hit with `strict_match=True` and a divergent body → HTTP 409 surfaces as a library-native exception
- ☐ Predicate returns False → request passes through untouched, no recording
- ☐ Tool call URL → recorded via `record_tool_call`, not `record_llm_call`
- ☐ Custom predicates → URL routing logic exercised
- ☐ Bypassed transport (user passed their own) → wrapped, not replaced

### Integration tests

- ☐ httpx → real OpenAI client construction (the user's actual entry point)
- ☐ requests → real Anthropic SDK construction
- ☐ Replay across adapters: record with httpx, replay with requests in a different process, verify cache hit on the same request hash
- ☐ ray-agent integration test: `intercept.install()` + a single fake agent loop → all calls recorded → replay produces identical agent behavior

### Cross-cutting

- ☐ install/uninstall idempotency
- ☐ install before any client creation, after, and mid-lifetime — all three should work
- ☐ install in one async event loop and use across threads (httpx async + asyncio.run)
- ☐ Replay context propagation through context vars (already a Phase 0 primitive — just verify it survives across the new code paths)

## Migration: ray-agent port

`use-cases/ray-agent/code/rewind_hook.py` today is ~280 LOC of:

1. `RewindHook` class wrapping the agent's outbound HTTP gateway.
2. `_send_to_rewind` posting structured envelope events to `POST /api/hooks/event`.
3. Per-call-site instrumentation in `serve/main.py` to invoke the hook.

After Phase 2, this collapses to:

```python
# serve/main.py
from rewind_agent import intercept

def setup_observability():
    if os.getenv("REWIND_ENABLED", "false") == "true":
        intercept.install()  # ← that's it
```

The `RewindHook` file is deleted (or kept as a thin compatibility shim that just calls `intercept.install()` for one release). All HTTP traffic from the agent — through its mTLS gateway — gets recorded automatically because the gateway uses `httpx`.

This migration is the validation criterion for the architecture: if `intercept.install()` can replace 280 LOC of bespoke wiring with 3 lines, the abstraction worked.

## Decisions on previously-open questions

1. **Default predicates: STRICT.** `DEFAULT_LLM_HOSTS` only contains the providers we've actually tested (OpenAI, Anthropic, Google Gemini, Cohere, Together, Groq, DeepSeek). Operators with custom gateways pass `install(predicates=MyPredicates())`. Rationale: surprise recording is worse than `pip install`-time invisibility — users with non-listed providers will hit "no recording happens" within seconds of testing and reach for `install(predicates=…)`, which is one Google search away.

2. **aiohttp strategy: TraceConfig first, monkey-patch fallback opt-in.** Try `aiohttp.TraceConfig` for the happy path. If short-circuiting the request from a trace-config event proves untenable, ship a separate `intercept.install_aiohttp_monkeypatch()` opt-in alongside the default `install()`. Clear log message on the limitation. Don't gate the entire PR on aiohttp implementation hardness.

3. **Tool call routing: LLM-host predicate only by default.** `default_is_llm_call(req)` fires on the standard LLM provider hosts. `default_is_tool_call(req)` returns `False`. Operators that route HTTP-based tool calls through their own gateway pass custom predicates. Rationale: HTTP-based tools are rare in practice (most tools are in-process function calls); per-deployment, not generic.

4. **Streaming chunk fidelity: defer.** Single-chunk synthetic SSE from Phase 0 is sufficient. Real chunk-level replay is a follow-up PR — separate scope, separate review.

5. **Cost-saved math: include a basic counter.** Expose `intercept.savings() -> SavingsSnapshot` returning `{cache_hits, tokens_saved_in, tokens_saved_out, cost_saved_usd_estimate}` aggregated across the install lifetime. Cost estimation uses the existing `crates/rewind-store/src/pricing.rs` table (Phase 0 already has it). Dashboard's "X tokens saved" stat lights up immediately, no Tier 2 dependency.

## Decision on post-merge releases

Cut `v0.13.0` GitHub Release **before** Phase 1 lands. Otherwise Phase 1 either rides 0.13.0 (which means 0.13.0's release artifacts include Phase 1 code that wasn't in the original PR) or skips to 0.14.0 ahead of an unreleased 0.13.0 (history-confusing). Releases are user-initiated per `CLAUDE.md` so I'll flag this in the next reply but not auto-trigger.

## Estimated scope

- **New code:** ~1500 LOC across 6 new files in `python/rewind_agent/intercept/`.
- **Tests:** ~2000 LOC across 5 new test files. ~80 test cases total.
- **Migration:** -270 LOC + 3 LOC in `use-cases/ray-agent/code/`.
- **Docs:** `docs/intercept-quickstart.md` — operator-facing how-to with httpx, requests, aiohttp examples. ~200 lines markdown.
- **Versions:** Python SDK minor bump `0.15.0 → 0.16.0` (new public package surface) — assuming v0.13.0 / 0.15.0 release happens first. No Rust changes expected, BUT: the small `intercept.savings()` aggregator may need a tiny `Store::cache_hit_savings()` method on the Rust side to surface accumulated tokens. If so, Rust patch bump `0.13.0 → 0.13.1`.

## Sequencing within the PR

To keep the diff reviewable, commits land in this order:

1. `feat(python): intercept._predicates + _flow shared logic`
2. `feat(python): httpx transport adapters (sync + async)`
3. `feat(python): requests transport adapter`
4. `feat(python): aiohttp middleware`
5. `feat(python): intercept.install() orchestrator`
6. `test(python): per-adapter unit + integration tests`
7. `feat(use-cases): port ray-agent from RewindHook to intercept.install()`
8. `docs: intercept quickstart + decision matrix update`
9. `chore: bump Python SDK to 0.16.0`

Each commit ships green tests + clippy/lint clean. Review-friendly.

## Acceptance criteria

- [ ] `intercept.install()` works with httpx (sync + async), requests, aiohttp
- [ ] Cache hit path returns library-native responses (not raw bytes)
- [ ] Streaming cache hit synthesizes SSE that closes cleanly
- [ ] Cache miss path records via `ExplicitClient.record_llm_call` with correct token counts
- [ ] Custom predicates work
- [ ] Default predicates fire on the standard LLM provider hosts
- [ ] ray-agent integration: 280 LOC → 3 LOC, agent behavior unchanged
- [ ] All tests green, `cargo clippy -- -D warnings` clean, no Python lint regressions
- [ ] Pre-existing v0.13 features (Phase 0) still work — no regressions in `cargo test --workspace`
- [ ] CI green on the PR branch
- [ ] Santa-style review pass: two independent reviewers concur

## NOT in this PR (deferred)

- **Tier 2 decorator** (`cached_llm_call` on ExplicitClient) — separate PR.
- **Tier 3 runner registry + dashboard "Run replay" button** — separate PR after Tier 2.
- **Real chunk-level streaming replay** — open question #4 above.
- **Synchronous-async bridge** — agents that mix sync requests with async httpx in the same process need careful context-var handling. Documented but not implemented in v1.
- **Doc rewrites for `docs/replay-and-forking.md` / `docs/recording-api.md`** — held until Tier 1 GA.
