# rewind-agent

**Python SDK for [Rewind](https://github.com/agentoptics/rewind) — the time-travel debugger for AI agents.**

Record every LLM call. See the exact context window. Fork, fix, replay — without re-running.

## Install

```bash
pip install rewind-agent
```

Requires the Rewind CLI for recording. Install with:

```bash
curl -fsSL https://raw.githubusercontent.com/agentoptics/rewind/main/install.sh | sh
```

## Quick Start

```python
import rewind_agent

# Auto-patches OpenAI/Anthropic clients to route through the Rewind proxy
rewind_agent.init()

# Your existing agent code runs unchanged — all LLM calls are recorded
client = openai.OpenAI()
client.chat.completions.create(model="gpt-4o", messages=[...])
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
- [Changelog](https://github.com/agentoptics/rewind/blob/main/CHANGELOG.md)
- [Examples](https://github.com/agentoptics/rewind/tree/main/examples)
