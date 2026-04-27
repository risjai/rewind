# HTTP Intercept Quickstart

Wrap your Python HTTP client once and Rewind records every LLM call automatically. No per-call-site instrumentation, no SDK monkey-patches, no proxy.

## When to use this vs other paths

| Your situation | Use this |
| :--- | :--- |
| Python agent using `httpx` (modern OpenAI SDK, Anthropic SDK, etc.) | **HTTP intercept** (`intercept.install()`) |
| Python agent using `requests` (legacy clients, LangChain) | **HTTP intercept** (`intercept.install()`) |
| Python agent using `aiohttp` (pure-async stacks) | **HTTP intercept** (`intercept.install()`) |
| Python agent using OpenAI / Anthropic SDK directly, zero config | `rewind_agent.init()` (auto-patches the SDK) |
| Any language, OpenAI/Anthropic wire format | `rewind record -- <agent>` (proxy) |
| Custom LLM client, non-HTTP transport, or non-Python | [Explicit Recording API](recording-api.md) |

If you're between `init()` and `intercept.install()`, the rule of thumb is: **`init()` for zero-config OpenAI/Anthropic; `intercept.install()` for everything else HTTP**. Custom gateways, mTLS proxies, or LLM frameworks that wrap the SDK all work cleanly under `intercept`.

## 60-second quickstart

```bash
pip install rewind-agent
```

```python
from rewind_agent import intercept

intercept.install()  # patches httpx, requests, aiohttp if installed

# Now ANY HTTP call from this process to a known LLM provider gets
# recorded. The agent code below is unchanged — no per-call-site hooks.

import openai
client = openai.OpenAI()
response = client.chat.completions.create(
    model="gpt-4o-mini",
    messages=[{"role": "user", "content": "What is 2+2?"}],
)
```

That's it. Run `rewind tui` or open `http://127.0.0.1:4800` to see the recorded session, fork from any step, and replay. On replay, cached steps return instantly without hitting the upstream provider.

## What gets recorded by default

The default predicate matches **only the LLM provider hosts we've explicitly tested**:

- `api.openai.com`
- `api.anthropic.com`
- `generativelanguage.googleapis.com` (Google Gemini)
- `api.cohere.ai`
- `api.together.xyz`
- `api.groq.com`
- `api.deepseek.com`
- `api.mistral.ai`

Calls to any other host pass through untouched. This is **strict-by-default** on purpose — silent recording of unrelated endpoints is worse than the brief "why isn't anything recording?" debugging trip when your provider isn't on the list. The cure is custom predicates (see below).

## Per-library examples

### httpx (sync + async)

`intercept.install()` patches `httpx.Client.__init__` and `httpx.AsyncClient.__init__`. Any client constructed after install routes through Rewind's transport. The OpenAI SDK uses `httpx` under the hood, so this covers most modern Python LLM clients.

```python
from rewind_agent import intercept
import httpx

intercept.install()

# Sync — no code change at the call site
with httpx.Client() as client:
    r = client.post(
        "https://api.openai.com/v1/chat/completions",
        json={"model": "gpt-4o-mini", "messages": [{"role": "user", "content": "hi"}]},
        headers={"Authorization": f"Bearer {api_key}"},
    )
```

```python
# Async — same story
import asyncio

async def main():
    async with httpx.AsyncClient() as client:
        r = await client.post(
            "https://api.anthropic.com/v1/messages",
            json={"model": "claude-3-5-sonnet-20241022", "messages": []},
            headers={"x-api-key": api_key},
        )

asyncio.run(main())
```

**httpx config preservation:** if you construct `httpx.Client(verify=False, http2=True, ...)` without an explicit `transport=`, Rewind wraps httpx's configured default transport rather than replacing it. Your verify / cert / http2 / proxies / limits / trust_env / retries / SSL context all survive.

### requests

`intercept.install()` patches `requests.Session.__init__` to mount Rewind's HTTPAdapter on `http://` and `https://`.

```python
from rewind_agent import intercept
import requests

intercept.install()

# New session — patched at construction
session = requests.Session()
r = session.post(
    "https://api.cohere.ai/v2/chat",
    json={"model": "command-r-plus", "messages": [{"role": "user", "content": "hi"}]},
    headers={"Authorization": f"Bearer {api_key}"},
)
```

**Caveat:** `requests.Session` instances constructed _before_ `intercept.install()` keep their original adapter — we don't mutate live instances. Migrate by calling `install()` early in startup, or explicitly `session.mount("https://", RewindHTTPAdapter())` for pre-existing sessions.

### aiohttp

Patches `aiohttp.ClientSession._request`. `base_url` + relative paths resolve correctly through `yarl`:

```python
from rewind_agent import intercept
import aiohttp

intercept.install()

async with aiohttp.ClientSession(base_url="https://api.openai.com") as session:
    async with session.post(
        "/v1/chat/completions",  # relative path — resolved against base_url
        json={"model": "gpt-4o-mini", "messages": []},
    ) as resp:
        data = await resp.json()
```

Both `await session.post(absolute_url, ...)` and `await session.post(relative_path)` are intercepted; host predicates see the absolute URL after resolution.

## Custom predicates — corporate gateways, custom LLM proxies

If your agent talks to a custom LLM gateway hostname, subclass `DefaultPredicates`:

```python
from rewind_agent.intercept import DefaultPredicates, install

class CorpPredicates(DefaultPredicates):
    def is_llm_call(self, req):
        host = req.url_parts.netloc.lower()
        # Match anything ending in our internal LLM gateway domain
        if host.endswith(".llm-gateway.corp.example"):
            return True
        # Fall through to the default (api.openai.com etc.)
        return super().is_llm_call(req)

install(predicates=CorpPredicates())
```

Predicates receive a `RewindRequest`:

```python
@dataclass(frozen=True)
class RewindRequest:
    url: str          # full URL with scheme
    method: str       # uppercase, "POST" / "GET" / etc.
    headers: dict[str, str]  # lowercase keys
    body: bytes       # request body, b"" for no-body
    stream: bool      # True if Accept: text/event-stream
```

Convenience helpers on the request:

- `req.url_parts` — parsed URL (`urllib.parse.ParseResult`)
- `req.header("name")` — case-insensitive header lookup
- `req.content_type()` — bare content-type without parameters

## Streaming behavior

### Cache hit on a streaming request

Replays a synthesized SSE stream — one `data: { ... }` event followed by `data: [DONE]\n\n`. The agent's streaming loop terminates cleanly:

```python
# This works on cache hit even though the original streamed token-by-token
async with httpx.AsyncClient() as client:
    async with client.stream(
        "POST",
        "https://api.openai.com/v1/chat/completions",
        json={"model": "gpt-4o-mini", "stream": True, "messages": [...]},
        headers={"accept": "text/event-stream"},
    ) as r:
        async for chunk in r.aiter_bytes():
            ...  # receives single synthetic chunk + [DONE] sentinel
```

### Cache miss on a streaming request

Live response passes through immediately — your `async for chunk` loop sees the real upstream stream. Recording happens with placeholder zero-tokens metadata; full token capture from streaming responses is a v1.1 follow-up. **Cache hits work correctly** because cached steps are recorded from buffered (non-streaming) responses on prior runs OR via the proxy CLI.

### Streaming detection

Rewind treats a request as streaming if **any** of:

1. `RewindRequest.stream` is True (set by adapter from `stream=True` kwargs / library-specific signals).
2. `Accept: text/event-stream` header is present.
3. Request body contains `"stream": true` as a JSON field (Phase 0 body-aware detection — handles SDKs that stream without setting an Accept header).

All three are OR-combined; any one fires the streaming path.

## Strict-match mode (catching divergent replays)

By default, when the agent's request body diverges from the recording's stored hash at the next ordinal, Rewind serves the cached response with an `X-Rewind-Cache-Divergent: true` header. That's the right default for "best-effort replay" but wrong for "I want to ASSERT that replay matches the recording exactly".

Strict mode raises a typed exception on divergence:

```python
from rewind_agent import (
    ExplicitClient,
    RewindReplayDivergenceError,
    intercept,
)

intercept.install()
client = ExplicitClient()

# Strict replay — diverging requests raise instead of serving cached
client.start_replay(session_id, strict_match=True)

try:
    response = openai_client.chat.completions.create(...)
except RewindReplayDivergenceError as e:
    print(f"Replay diverged at step {e.target_step}")
    print(f"Stored hash:   {e.stored_hash}")
    print(f"Incoming hash: {e.incoming_hash}")
    # Fix the test, the prompt, or the agent code; retry — the
    # replay cursor stays put on 409 so you don't lose your slot.
```

Other 4xx / 5xx errors from the Rewind server still degrade to "cache miss" so a transient outage doesn't break the agent. Only HTTP 409 divergence raises.

## Savings counter

Process-lifetime cache-hit metrics — useful for the agent's own telemetry / logging:

```python
from rewind_agent import intercept

intercept.install()

# … agent runs, hits some cached responses, hits some live …

snap = intercept.savings()
print(f"Saved {snap.cache_hits} cache hits "
      f"= {snap.tokens_saved_in + snap.tokens_saved_out} tokens "
      f"≈ ${snap.cost_saved_usd_estimate:.4f}")
```

The cost estimate uses an in-process pricing table covering the most common models (GPT-4o, Claude 3.5, Gemini 1.5, Llama 3.x, Mistral, etc.). Unknown models contribute zero USD but still count toward cache hits and token totals. Override per-call:

```python
from rewind_agent.intercept._savings import record_cache_hit

# Custom pricing for self-hosted models
record_cache_hit(
    model="my-private-llama",
    tokens_in=1000,
    tokens_out=500,
    cost_table={"my-private-llama": (0.10, 0.20)},  # ($/1M in, $/1M out)
)
```

## Install / uninstall lifecycle

```python
from rewind_agent.intercept import install, uninstall, is_installed

install()                  # idempotent — second call is a no-op
print(is_installed())      # → True

uninstall()                # mainly for tests; restores original __init__s
```

`install()` patches every importable HTTP library. Missing libraries silently skip — `pip install rewind-agent` doesn't drag in `httpx`, `requests`, or `aiohttp`. Install only the library your agent uses.

When debugging, you can check what got patched:

```python
from rewind_agent.intercept import (
    httpx_transport,
    requests_adapter,
    aiohttp_middleware,
)

print(httpx_transport.HTTPX_AVAILABLE, httpx_transport.is_patched())
print(requests_adapter.REQUESTS_AVAILABLE, requests_adapter.is_patched())
print(aiohttp_middleware.AIOHTTP_AVAILABLE, aiohttp_middleware.is_patched())
```

## What's NOT supported (today)

- **Streaming-miss recording fidelity.** Live streams pass through correctly, but the recorded response on a streaming miss has placeholder zero-tokens metadata — full SSE tee + chunk capture is on the roadmap as v1.1.
- **Streaming uploads** (request body as an async iterator / file generator). Adapters fall back to empty body for predicate matching; the live request still goes through. Buffered request bodies are the typical case for LLM calls and work fine.
- **httpx mounts.** Per-host transport routing via `httpx.Client(mounts={...})` bypasses Rewind for mounted hosts. Operators using mounts heavily should mount our transport explicitly.
- **WebSocket upgrades on aiohttp.** `ClientSession.ws_connect()` uses a different code path; we don't intercept it. WebSocket support will likely come with the streaming-tee work.
- **`raise_for_status` on aiohttp cache hits.** The synthetic ClientResponse always returns status 200 on cache hit. Recordings of non-2xx responses lose that signal in replay; rare in practice but documented.

## Troubleshooting

### "I called `intercept.install()` but nothing is being recorded"

1. Confirm `REWIND_ENABLED=true` (or your project's equivalent) and the Rewind server is running on `http://127.0.0.1:4800`.
2. Check the host of the URL you're hitting against `DEFAULT_LLM_HOSTS` — exact match, no subdomains. Use a custom predicate if needed.
3. Confirm at least one supported HTTP library is installed: `pip install httpx` (most common) or `requests` or `aiohttp`. `intercept.install()` is missing-library-tolerant by design.
4. The order matters: `install()` only patches clients constructed _after_ it runs. Pre-existing sessions / clients keep their original transport. Move `install()` earlier in startup.

### "Rewind broke my agent — got `httpx.ResponseNotRead`"

Should not happen on v0.15.0+ — Phase 1's `_flow` materializes response bodies via `read()` / `aread()` before parsing. If you see this, file a bug with your httpx version and a minimal repro.

### "Recording works locally but not in CI / production"

1. Check that `httpx`/`requests`/`aiohttp` are actually installed in the target environment. They're optional deps of `rewind-agent`.
2. Verify `REWIND_URL` points to a reachable Rewind server.
3. If you're using `aiohttp.ClientSession(base_url=...)` with relative paths, you need v0.15.0+ — earlier versions had a bug where relative URLs bypassed the host predicate (see PR #149 re-review #3).

### "I want to record some hosts but not others"

Custom predicate (see above). The `Predicates` Protocol is small enough to copy-paste between projects.

## See also

- [Recording API](recording-api.md) — the underlying HTTP surface that intercept builds on
- [Framework Integrations](framework-integrations.md) — when to use intercept vs `init()` vs proxy mode
- [Recording](recording.md) — overview of all recording paths
- [`rewind_agent.intercept` API reference](https://github.com/agentoptics/rewind/blob/master/python/rewind_agent/intercept/__init__.py) — full module docstring
