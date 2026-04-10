# rewind-agent

**Python SDK for [Rewind](https://github.com/agentoptics/rewind) — the time-travel debugger for AI agents.**

Record every LLM call. See the exact context window. Fork, fix, replay from failure — without re-running.

## Install

```bash
pip install rewind-agent
```

This installs both the Python SDK **and** the `rewind` CLI. The native binary is auto-downloaded on first use.

## Quick Start

One line to start recording — no proxy, no setup:

```python
import rewind_agent
import openai

rewind_agent.init()  # patches OpenAI + Anthropic automatically

client = openai.OpenAI()
client.chat.completions.create(model="gpt-4o", messages=[...])
# Recorded to ~/.rewind/ — inspect with: rewind show latest
```

Or as a scoped session:

```python
with rewind_agent.session("my-agent"):
    client = openai.OpenAI()
    client.chat.completions.create(model="gpt-4o", messages=[...])
```

## Two Recording Modes

| | **Direct mode** (default) | **Proxy mode** |
|:---|:---|:---|
| **Setup** | `rewind_agent.init()` | `rewind record` in a second terminal |
| **How** | Monkey-patches SDK clients in-process | HTTP proxy intercepts LLM traffic |
| **Best for** | Python agents, quick iteration | Any language, polyglot teams |

```python
# Direct mode (default — no proxy needed)
rewind_agent.init(mode="direct")

# Proxy mode (requires `rewind record` running)
rewind_agent.init(mode="proxy", proxy_url="http://127.0.0.1:8443")
```

## Replay from Failure

Agent failed at step 5? Fix your code, then replay — steps 1-4 are cached (instant, free), step 5 re-runs live:

```python
with rewind_agent.replay("latest", from_step=4):
    result = my_agent.run("Research Tokyo population")
    # Steps 1-4: instant cached responses (0ms, 0 tokens)
    # Step 5+: live LLM calls, recorded to a new forked timeline
```

After the replay, diff the timelines: `rewind diff <session> main replayed`

## Regression Testing

Turn any session into a baseline. After code changes, check for regressions:

```python
from rewind_agent import Assertions

# Check the latest session against a known-good baseline
result = Assertions().check("booking-happy-path", "latest")
assert result.passed, f"Regression: {result.failed_checks} checks failed"
```

Checks step types, models, tool calls, error status, and token usage. Supports configurable tolerance:

```python
result = Assertions().check("my-baseline", "latest", token_tolerance=0.15)
print(f"Passed: {result.passed_checks}/{result.total_checks}")
```

## Agent Hooks

Enrich recordings with semantic labels:

```python
@rewind_agent.step("search")
def search(query: str) -> str:
    return client.chat.completions.create(...)

@rewind_agent.tool("calculator")
def calculate(a: float, b: float) -> float:
    return a + b

with rewind_agent.trace("analysis"):
    rewind_agent.annotate("confidence", 0.92)
    result = search("Tokyo population")
```

## Framework Adapters

```python
# LangGraph
graph = rewind_agent.wrap_langgraph(compiled_graph)

# CrewAI
crew = rewind_agent.wrap_crew(crew)
```

## Learn More

- [GitHub](https://github.com/agentoptics/rewind)
- [Changelog](https://github.com/agentoptics/rewind/blob/master/CHANGELOG.md)
