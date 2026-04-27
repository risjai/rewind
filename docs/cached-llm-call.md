# `cached_llm_call` Decorator

Wrap a Python function once and Rewind caches its return value. The decorator gives you per-function control as an alternative (or complement) to the global [HTTP intercept](intercept-quickstart.md).

## When to use this vs `intercept.install()`

| Your situation | Use this |
| :--- | :--- |
| You want to cache the OUTER function that composes multiple LLM/tool calls | **Decorator** — caches the composite return value |
| Your LLM call doesn't go through plain HTTP (Bedrock via boto3, gRPC, etc.) | **Decorator** — caches at function-return level, transport-agnostic |
| You want explicit per-call-site control over what's cached | **Decorator** — line-by-line opt-in |
| You want every HTTP call to OpenAI / Anthropic / etc. cached automatically | [HTTP intercept](intercept-quickstart.md) — global, hands-off |
| Tests pinning specific functions to known recordings | **Decorator** — function-level granularity |

You can use both at once. The decorator's check fires first (it wraps the user's function), and on miss the inner HTTP calls under intercept are NOT double-recorded — see [Composition](#composition-with-httpinterceptinstall) below.

## Session requirement

The decorator records via `ExplicitClient.record_llm_call`, which is a no-op when no Rewind session is active on the `ExplicitClient` contextvar. **You must enter a session through `ExplicitClient` before calling decorated functions** — otherwise the function runs normally and returns the live result, but nothing gets recorded (silent no-op consistent with the rest of the SDK).

Three valid patterns:

```python
from rewind_agent import ExplicitClient

client = ExplicitClient()

# 1. Scoped context manager (preferred for tests / one-off scripts)
with client.session("my-experiment"):
    result = chat("What is 2+2?")  # records under "my-experiment"

# 2. Long-lived session (one per conversation; auto-cached, evicts after 2h idle)
client.ensure_session(conversation_id="user-123")
result1 = chat(...)
result2 = chat(...)  # both record under the same session

# 3. Replay against an existing session
client.start_replay(session_id, strict_match=False)
result = chat(...)  # cache hit if args match the recording
```

### `init()` does NOT enable the decorator

A common gotcha: `rewind_agent.init()` opens a session for the **direct-mode SDK monkey-patches** (`patch.py`), but the decorator records through `ExplicitClient`, which uses a separate contextvar in `explicit.py`. The two are NOT the same.

If you call `init()` and decorate a function, the inner OpenAI/Anthropic SDK call gets recorded (via `init()`'s patches), but the decorator's outer `record_llm_call` is still a silent no-op. To get function-level recording, you need one of the three `ExplicitClient` patterns above.

You can use them together:

```python
import rewind_agent
from rewind_agent import ExplicitClient, cached_llm_call

rewind_agent.init()                 # records OpenAI/Anthropic SDK calls
client = ExplicitClient()

@cached_llm_call()
def chat(q): ...

with client.session("composed"):
    result = chat("...")  # NOW the decorator records too
    # The contextvar suppresses init()'s inner record on miss to
    # avoid double-recording — see "Composition" section below.
```

Without `ExplicitClient.session()`/`ensure_session()`/`start_replay()`, the decorator runs and returns correctly but doesn't record. This is by design: the decorator should never crash a production agent because Rewind isn't configured.

## 60-second quickstart

```python
from rewind_agent import ExplicitClient, cached_llm_call

@cached_llm_call()
def chat(question: str) -> dict:
    response = openai_client.chat.completions.create(
        model="gpt-4o-mini",
        messages=[{"role": "user", "content": question}],
    )
    return response.model_dump()  # JSON-serializable for the cache

client = ExplicitClient()
with client.session("my-quickstart"):
    # First call: hits OpenAI, records the return value
    result1 = chat("What is 2+2?")

    # Second call (with same args): served from cache, OpenAI NOT hit
    result2 = chat("What is 2+2?")
    assert result1 == result2  # True; cached round-trip
```

To replay a previously-recorded session deterministically (instead of recording fresh):

```python
client.start_replay(session_id, strict_match=False)
# Now decorated calls hit cache on matching args; live calls happen
# only on cache miss or args that diverge from the recording.
result = chat("What is 2+2?")  # cache hit (zero-cost)
```

## Async functions

```python
@cached_llm_call()
async def chat(question: str) -> dict:
    response = await async_client.chat.completions.create(...)
    return response.model_dump()
```

The decorator detects async functions via `inspect.iscoroutinefunction` and emits an async wrapper. Generator and async-generator functions raise `TypeError` at decoration time — the cache stores a single return per key, not a stream of yields. Wrap a generator's consumer in a regular function and decorate that.

## Custom token + model extraction

By default, recorded steps for decorated functions have `model=""` and `tokens_in/out=0` (the savings counter still ticks `cache_hits` but USD estimate / token totals stay zero). Pass extractor lambdas:

```python
@cached_llm_call(
    extract_model=lambda call_args, ret: ret["model"],
    extract_tokens=lambda call_args, ret: (
        ret["usage"]["prompt_tokens"],
        ret["usage"]["completion_tokens"],
    ),
)
def chat(question: str) -> dict:
    return openai_client.chat.completions.create(...).model_dump()
```

`call_args` is `{"args": (...positional...), "kwargs": {...keyword...}}` — useful when you need to derive things from the inputs as well as the return. `ret` is the return value (or the cached value on a hit, so the savings counter ticks correctly on hits too).

If your extractor raises, the call doesn't break — we log a warning and record with zeros.

## Custom cache keys

Cache keys default to a SHA-256 of `f"{fn_qualname}|{json(args, kwargs)}"` with `_safe_repr` fallback for non-JSON-able args. Most cases need nothing more.

If your function takes objects that don't have a stable `repr()` (e.g. an OpenAI client object whose `repr` includes a memory address), the default key changes between runs and you'll never hit cache. Override:

```python
@cached_llm_call(
    cache_key=lambda client, question, **kw: question,  # ignore the client arg
)
def chat(client, question: str) -> dict:
    return client.chat.completions.create(...)
```

Or for more complex derivation:

```python
@cached_llm_call(
    cache_key=lambda *args, **kwargs: hashlib.sha256(
        json.dumps({"q": kwargs["question"], "model": kwargs["model"]}).encode()
    ).hexdigest(),
)
def chat(*, question: str, model: str = "gpt-4o-mini") -> dict:
    ...
```

The custom function receives the same `(*args, **kwargs)` your decorated function gets. If it raises, we fall back to the default derivation (and log a warning).

### What gets sent to the server (identity-only payload)

The decorator sends a minimal payload to the Rewind server for cache lookup and recording:

```json
{
  "_rewind_decorator": "cached_llm_call",
  "fn_name": "module.chat",
  "cache_key": "<sha256-hex-or-user-supplied-string>"
}
```

**Args and kwargs are deliberately NOT in the payload.** The server hashes the whole request body to derive the lookup key (Phase 0 content validation), so including args would make unstable arg reprs (memory addresses, file handles) defeat custom `cache_key` lambdas that try to ignore those args. The cache key IS the identity; everything else is derivation noise.

If you want richer dashboard display (the request payload is what shows in the dashboard's "request" view), encode the human-readable identity directly in the cache key:

```python
@cached_llm_call(
    # Plain string keys are fine; they show up in the dashboard verbatim
    cache_key=lambda *, question, model: f"chat:{model}:{question[:50]}",
)
def chat(*, question: str, model: str) -> dict:
    ...
```

## Return type round-trip

The decorator stores **JSON-serializable values** in the cache. On a cache hit, you get the JSON-deserialized form back, NOT the original Python type. This matters for SDK return types like `openai.ChatCompletion`:

```python
@cached_llm_call()
def chat(q: str) -> openai.ChatCompletion:
    return openai_client.chat.completions.create(...)  # NOT model_dump()'d

result1 = chat("hi")            # First call: real ChatCompletion object
result2 = chat("hi")            # Cache hit: JSON dict, NOT ChatCompletion
assert result1.choices[0]...    # Works
assert result2.choices[0]...    # AttributeError on dict access pattern
```

**Best practice:** return a dict from the decorated function (`return response.model_dump()`) and reconstruct on the call site if you need the SDK type. The cache will round-trip cleanly.

The decorator handles common conversions automatically:

- Already JSON-able (`dict`, `list`, primitives) → stored as-is
- Has `model_dump()` (Pydantic v2, OpenAI SDK) → called and the result stored
- Has `dict()` (Pydantic v1) → called as fallback
- Has `__dict__` → extracted
- Pathological case → `repr()` stored, warning logged. Future cache hits get the repr string back as the "response", which the user's code probably can't reconstruct.

## Strict-match divergence

If the agent calls a decorated function with arguments whose hash diverges from the recording's stored hash at the next ordinal AND the replay context was created with `strict_match=True`, the decorator surfaces a typed exception:

```python
from rewind_agent import (
    ExplicitClient,
    RewindReplayDivergenceError,
    cached_llm_call,
)

client = ExplicitClient()
client.start_replay(session_id, strict_match=True)

@cached_llm_call()
def chat(question: str) -> dict:
    return openai_client.chat.completions.create(...).model_dump()

try:
    result = chat("This question wasn't in the recording")
except RewindReplayDivergenceError as e:
    print(f"Replay diverged at step {e.target_step}")
    # Fix the test, the args, or the prompt; retry — the replay
    # cursor stays put on 409 so you don't lose your slot.
```

## Composition with `intercept.install()`

Both decorators and the global intercept can be active in the same process. Their interaction:

- **Cache hit on the decorated function:** the decorator returns the cached value WITHOUT calling the user's function. No HTTP calls happen, so intercept never sees anything.
- **Cache miss:** the decorator sets a contextvar (`_cached_llm_call_active`), calls the user's function, and records the return value at function-level granularity. Intercept's `_flow` checks the contextvar — when set, it skips its own recording so we don't double-record the same logical event at two granularities.

Net effect: ONE recording per cache miss, at the granularity (function-level) the decorator chose. The contextvar is properly reset even if the user's function raises, so an exception in the function body doesn't leak the suppression to subsequent code.

## Inspecting decorator state

```python
from rewind_agent.cached_call import is_cached_llm_call_active

# Returns True only inside a @cached_llm_call-wrapped function's body.
# Mostly useful for testing; production code rarely needs it.
print(is_cached_llm_call_active())
```

## What's NOT supported

- **Generator / async-generator functions** — raise `TypeError` at decoration. Cache stores a single return per key, not a stream.
- **Memoization across processes** — each `ExplicitClient` instance talks to the local Rewind server; cache is scoped to a single Rewind installation, not Redis or a distributed cache.
- **Auto-detection of return-type → token extraction** — pass `extract_tokens` explicitly. Auto-detection is a v2.1 candidate if user feedback warrants.
- **TTL / cache invalidation** — caching is keyed on input hash; "invalidate" means changing the function or the args. The Rewind server has its own retention policy for recorded sessions.

## See also

- [HTTP Intercept Quickstart](intercept-quickstart.md) — the global transport-layer alternative
- [Recording](recording.md) — overview of all recording paths
- [Recording API](recording-api.md) — the underlying HTTP surface this decorator builds on
- [`rewind_agent.cached_call`](https://github.com/agentoptics/rewind/blob/master/python/rewind_agent/cached_call.py) — full module docstring + source
