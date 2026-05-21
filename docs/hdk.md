# Integrate any framework with Rewind

You're building an agent — or shipping a framework that agents use — and you want recording, fork, and replay without writing a custom HTTP emitter for every app. This page is the decision tree.

The big idea: **Rewind already ships the parts.** You don't need a framework adapter, a Protocol, or a plugin. Pick the tier that matches your transport and call site, write 5–35 lines, done.

## Pick a tier

```
Are your LLM calls HTTP-shaped? (httpx / requests / aiohttp)
├── Yes → Tier 1: intercept.install()              ~5 LOC
└── No  → Tier 2: ExplicitClient                   ~30 LOC

Need to bake in defaults for your org / framework?
        → Tier 3: ship a small connector package   ~50 LOC
```

| Tier | Use when | Code size | API |
|:---|:---|:---|:---|
| 1 | LLM calls go over httpx / requests / aiohttp (most modern Python LLM clients) | ~5 LOC | [`intercept.install()`](intercept-quickstart.md) |
| 2 | Custom transport: gRPC, in-process LLM, mTLS-tunneled, message bus | ~30 LOC | [`ExplicitClient`](recording-api.md) |
| 3 | You're shipping a connector for an org or framework family (one package consumed by every internal agent) | ~50 LOC | Tier 1 or 2 + a thin wrapper |

Most integrators stop at Tier 1.

## Tier 1 — `connector.setup()`

For any agent that talks to LLMs over httpx, requests, or aiohttp. The connector wraps a session, installs HTTP intercept, and tears both down on exit — one call, one `with` block, no order to remember.

```python
import rewind_agent

with rewind_agent.connector.setup(name="my-agent"):
    run_agent_loop()
```

For a custom LLM gateway hostname, pass `llm_hosts` (or set `REWIND_LLM_HOSTS=a.example,b.example` in the env):

```python
with rewind_agent.connector.setup(
    name="my-agent",
    llm_hosts=("llm-gateway.example",),
):
    run_agent_loop()
```

The yielded value is an `ExplicitClient` — useful for non-HTTP record paths (gRPC, in-process LLMs) inside the same block:

```python
with rewind_agent.connector.setup(name="my-agent") as client:
    response = my_grpc_client.chat(request)
    client.record_llm_call(
        request=request, response=response.dict(),
        model="my-private-llama", duration_ms=duration_ms,
    )
```

**Configuration knobs** (env > default; kwargs override env):

- `REWIND_ENABLED=0` — kill switch with zero overhead in prod when off.
- `REWIND_URL` — Rewind server URL (default `http://127.0.0.1:4800`).
- `REWIND_LLM_HOSTS` — comma-separated hostnames to treat as LLM gateways in addition to the strict-by-default provider list.

**Replay-aware**: when `REWIND_SESSION_ID` and `REWIND_REPLAY_CONTEXT_ID` are set (the runner-subprocess pattern from [runners.md](runners.md)), `setup()` skips creating a fresh session and lets `intercept.install()` attach to the existing replay context. Drop the connector into runner-driven replay handlers without phantom sessions.

For per-library examples, custom predicates, streaming behavior, the savings counter, and troubleshooting, see [intercept-quickstart.md](intercept-quickstart.md).

### When you need finer control

If you need to install intercept and start sessions independently — for example, a long-running process that opens many short sessions back-to-back — use the underlying primitives directly:

```python
from rewind_agent.explicit import ExplicitClient
from rewind_agent.intercept import DefaultPredicates, install, uninstall

class MyPredicates(DefaultPredicates):
    def is_llm_call(self, req):
        return "llm-gateway.example" in req.url_parts.netloc or super().is_llm_call(req)

client = ExplicitClient()
install(predicates=MyPredicates())  # once at process startup
try:
    for task in tasks:
        with client.session(name=task.name):  # short session per task
            run_task(task)
finally:
    uninstall()
```

The order matters: the session sets `_session_id` / `_timeline_id` contextvars; the intercept layer reads those vars on every recorded call. Without an active session, intercepted calls silently no-op. **This is the most common Tier-1 mistake** — `connector.setup()` exists specifically to remove this footgun.

## Tier 2 — `ExplicitClient`

For agents that don't go over HTTP — gRPC, in-process LLMs, message-bus dispatch, anything `intercept` can't see. You wrap your existing call sites with explicit `record_*` calls.

```python
import time
from rewind_agent.explicit import ExplicitClient

client = ExplicitClient()
with client.session("my-agent"):
    t0 = time.time()
    # your custom LLM call
    request = {"messages": [{"role": "user", "content": "hi"}]}
    response = my_grpc_client.chat(request)
    duration_ms = int((time.time() - t0) * 1000)

    client.record_llm_call(
        request=request,
        response=response.to_dict(),
        model="my-private-llama-70b",
        duration_ms=duration_ms,
        tokens_in=response.usage.prompt_tokens,
        tokens_out=response.usage.completion_tokens,
    )
```

`record_tool_call` records tool / function-call steps. `get_replayed_response` checks the replay cache during fork+replay; on a hit, return the cached response instead of calling the live model. The full HTTP wire format and method reference live in [recording-api.md](recording-api.md).

Async variants (`record_llm_call_async`, `record_tool_call_async`, `session_async`, `get_replayed_response_async`) are drop-in for asyncio code paths and run HTTP in a thread executor so the event loop never blocks.

## Tier 3 — ship a connector

If you're integrating a framework family — every internal agent in your org, every team using a private LLM gateway, every agent in a monorepo — package the boilerplate as a small connector. ~50 LOC, one place to update defaults.

The pattern:

```python
# my_org_rewind/__init__.py
import os
from contextlib import contextmanager
from rewind_agent.explicit import ExplicitClient
from rewind_agent.intercept import DefaultPredicates, install, uninstall

_DEFAULT_LLM_HOSTS = ("llm-gateway.your-org.example",)

class _OrgPredicates(DefaultPredicates):
    def __init__(self, hosts):
        super().__init__()
        self._hosts = tuple(h.strip() for h in hosts if h and h.strip())

    def is_llm_call(self, req):
        if any(h in req.url_parts.netloc for h in self._hosts):
            return True
        return super().is_llm_call(req)

@contextmanager
def setup(name, *, base_url=None, llm_hosts=None, enabled=None):
    """Connect a my-org-hosted agent to Rewind.

    Override defaults via env (REWIND_ENABLED, REWIND_URL, REWIND_LLM_HOSTS)
    or per-call kwargs. Yields the ExplicitClient for non-HTTP record_* calls.
    """
    if enabled is None:
        enabled = os.environ.get("REWIND_ENABLED", "1") != "0"
    if not enabled:
        yield None
        return

    hosts = llm_hosts
    if hosts is None:
        env = os.environ.get("REWIND_LLM_HOSTS", "")
        hosts = tuple(env.split(",")) if env else _DEFAULT_LLM_HOSTS

    url = base_url or os.environ.get("REWIND_URL", "http://127.0.0.1:4800")
    client = ExplicitClient(base_url=url)
    with client.session(name):
        install(predicates=_OrgPredicates(hosts))
        try:
            yield client
        finally:
            uninstall()
```

Every agent in your org becomes a 3-line integration:

```python
import my_org_rewind
with my_org_rewind.setup("alerts-triage"):
    asyncio.run(main())
```

Configurable knobs every connector should expose:

- `REWIND_ENABLED=0` — kill switch with zero overhead in prod when off.
- `REWIND_URL` — Rewind server URL (default `http://127.0.0.1:4800`).
- `REWIND_LLM_HOSTS` — comma-separated hostnames to treat as LLM gateways.
- Per-call kwargs override env values, useful for tests and multi-tenant cases.

## Common pitfalls

**Recording silently no-ops.** `record_llm_call` returns `None` when no session is active. Always wrap your agent loop in `with client.session(...)` before calling `install(...)` or `record_*`. The intercept layer reuses the session's contextvars; without them, intercepted calls succeed (HTTP-wise) but record nothing.

**Don't combine `init()` with `intercept.install()`.** `rewind_agent.init()` patches the OpenAI / Anthropic Python SDKs at the SDK layer; `intercept.install()` patches httpx / requests / aiohttp at the transport layer. Running both in the same process double-records every call. Pick one path per process.

**`connector.setup()` is sync, even in async code.** It's a `@contextmanager`, not an `@asynccontextmanager`. Async callers should use plain `with` — not `async with` — even inside an `async def`:

```python
async def main():
    with rewind_agent.connector.setup(name="my-agent"):  # NOT `async with`
        await run_agent_loop()
```

Only the setup and teardown are synchronous (HTTP POST `/api/sessions/start` and `/api/sessions/{id}/end`); the body of the block can be fully async.

**`requests.Session()` constructed before `install()` keeps its old adapter.** The intercept layer patches `Session.__init__`; live instances aren't mutated. Move `install()` earlier in startup, or call `session.mount(...)` explicitly for pre-existing sessions. Same caveat applies to `httpx.Client` constructed pre-install.

**Replay needs a replay context.** Fork-and-replay against a recorded session works automatically when the dispatch flow runs through `runner.py` (see [runners.md](runners.md)). For custom replay drivers, call `client.attach_replay_context(session_id, replay_context_id)` before `install()`; intercept will then serve cached responses on each step.

## Where to go next

- [intercept-quickstart.md](intercept-quickstart.md) — full Tier 1 reference: per-library examples, custom predicates, streaming behavior, savings counter, troubleshooting.
- [recording-api.md](recording-api.md) — HTTP wire format and `ExplicitClient` method reference for Tier 2.
- [framework-integrations.md](framework-integrations.md) — first-class adapters for OpenAI Agents SDK, Pydantic AI, LangGraph, CrewAI.
- [runners.md](runners.md) — wiring fork+replay into your runtime.
