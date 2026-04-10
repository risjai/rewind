# Recording

**Rewind** is a time-travel debugger for AI agents. It records every LLM call your agent makes — the full request, response, context window, token counts, and timing — so you can inspect, fork, replay, and diff later.

This page covers the two recording modes, agent hooks for enriching traces, and streaming behavior.

---

## Two ways to record

Choose the approach that fits your stack:

| | **Direct mode** (Python) | **Proxy mode** (any language) |
|:---|:---|:---|
| **Setup** | `rewind_agent.init()` — one line | `rewind record` in a second terminal |
| **Languages** | Python (OpenAI + Anthropic SDKs) | Any language that makes HTTP calls |
| **How it works** | Monkey-patches SDK clients in-process | HTTP proxy intercepts LLM traffic |
| **Streaming** | Captured via stream wrappers | SSE pass-through, zero added latency |
| **Best for** | Quick iteration, Python agents | Polyglot teams, non-Python agents |

---

## Direct mode

No proxy, no second terminal, no environment variables. Add one line and every LLM call is recorded:

```python
import rewind_agent

rewind_agent.init()  # patches OpenAI + Anthropic automatically

# Or as a scoped session:
with rewind_agent.session("my-agent"):
    client = openai.OpenAI()
    client.chat.completions.create(model="gpt-4o", messages=[...])
```

Under the hood, `init()` monkey-patches the OpenAI and Anthropic Python SDK clients so that every call is captured and written to `~/.rewind/`. No configuration beyond that single line.

```python
import rewind_agent
import openai

rewind_agent.init()  # that's it — all LLM calls are now recorded

client = openai.OpenAI()
client.chat.completions.create(model="gpt-4o", messages=[...])
# Recorded to ~/.rewind/ — inspect with: rewind show latest
```

---

## Proxy mode

Works with any language or framework. Start the proxy, point your agent at it:

```bash
rewind record --name "my-agent" --upstream https://api.openai.com
# In another terminal:
export OPENAI_BASE_URL=http://127.0.0.1:8443/v1
python3 my_agent.py   # or node, go, rust — anything that calls the LLM
```

The proxy intercepts every HTTP request to the LLM API, records it, and forwards it upstream. Streaming (SSE) responses are passed through in real-time — the agent sees zero added latency while Rewind accumulates the full response for storage.

---

## Agent hooks — enrich recordings with semantic labels

Without hooks, the recording shows "LLM Call 1", "LLM Call 2". With hooks, steps show up as "search", "plan", "execute" — much more useful when debugging.

```python
import rewind_agent

@rewind_agent.step("search")
def search_web(query: str) -> str:
    return client.chat.completions.create(...)

@rewind_agent.tool("calculator")
def calculate(a: float, b: float) -> float:
    return a + b

@rewind_agent.node("planner")       # LangGraph-style node
def plan(state: dict) -> dict:
    return {"steps": ["research", "write", "review"]}

with rewind_agent.trace("analysis_phase"):
    rewind_agent.annotate("confidence", 0.92)
    result = search_web("Tokyo population")
```

### Hook reference

| Decorator / Function | Purpose |
|:---|:---|
| `@step("name")` | Label a function as a named step in the trace |
| `@tool("name")` | Label a function as a tool invocation |
| `@node("name")` | Label a function as a graph node (LangGraph-style) |
| `trace("name")` | Context manager that groups nested calls under a named span |
| `annotate(key, value)` | Attach arbitrary metadata to the current step |

---

## Streaming

Both recording modes handle streaming transparently:

- **Proxy mode**: SSE streams are forwarded to the agent in real-time while being accumulated for recording. The agent sees zero added latency.
- **Direct mode**: Stream wrappers capture chunks as they arrive, then write the full assembled response to storage after the stream completes.

---

## Examples

See these example scripts for working code:

- [`examples/01_basic_recording.py`](../examples/01_basic_recording.py) — Minimal proxy-mode recording
- [`examples/03_python_hooks.py`](../examples/03_python_hooks.py) — `@step`, `@tool`, `@node`, `trace()`, and `annotate()`
- [`examples/05_direct_mode.py`](../examples/05_direct_mode.py) — Direct mode with `init()` and `session()`
