# OpenAI Agents SDK Integration

[Rewind](https://github.com/agentoptics/rewind) is a time-travel debugger for AI agents. It provides a [`TracingProcessor`](https://github.com/agentoptics/rewind/blob/master/python/rewind_agent/openai_agents.py) that captures all agent spans — LLM generations, tool executions, and handoffs — then lets you fork timelines, replay from failure, and diff the results.

## Install

```bash
pip install rewind-agent[agents]
```

## TracingProcessor Integration

Rewind implements [`RewindTracingProcessor`](https://github.com/agentoptics/rewind/blob/master/python/rewind_agent/openai_agents.py), a subclass of the Agents SDK's `TracingProcessor`. It receives all span events and records them as Rewind steps.

### Automatic Registration (Recommended)

`rewind_agent.init()` auto-detects the Agents SDK and calls `add_trace_processor()`:

```python
import rewind_agent
from agents import Agent, Runner, function_tool

rewind_agent.init()  # registers RewindTracingProcessor via add_trace_processor()

@function_tool
def web_search(query: str) -> str:
    """Search the web."""
    return f"Results for: {query}"

agent = Agent(
    name="researcher",
    instructions="You are a research assistant. Use web_search to find information.",
    tools=[web_search],
)

result = await Runner.run(agent, "What is the population of Tokyo?")
print(result.final_output)
```

### Manual Registration

You can also register the processor directly:

```python
from agents.tracing import add_trace_processor
from rewind_agent.openai_agents import RewindTracingProcessor
from rewind_agent.store import Store

store = Store()
session_id, timeline_id = store.create_session("my-agent")

processor = RewindTracingProcessor(store, session_id, timeline_id)
add_trace_processor(processor)

# Now run your agent — all spans flow to Rewind
result = await Runner.run(agent, "What is the population of Tokyo?")
```

## Span Mapping

The processor maps Agents SDK spans to Rewind steps:

| Agents SDK Span | Rewind Step Type | Data Captured |
|:----------------|:-----------------|:--------------|
| `GenerationSpanData` | `llm_call` | Model, tokens (from `usage`), input, output |
| `FunctionSpanData` | `tool_call` | Tool name, input, output |
| `HandoffSpanData` | `tool_call` | From/to agent names |
| `AgentSpanData` | *(metadata)* | Agent name, tools, handoffs |

## Inspect the Recording

```bash
# Quick trace view
rewind show latest

# Interactive TUI
rewind inspect latest
```

```
⏪ Rewind — Session Trace

  Session: default
  Steps: 5    Tokens: 1,231

  ┌ ✓ 🧠  Step 1  gpt-4o       320ms   156↓  28↑
  │   → tool_calls: web_search
  ├ ✓ 🔧  Step 2  tool:web_search  45ms
  ├ ✓ 🧠  Step 3  gpt-4o       890ms   312↓  35↑
  │   → tool_calls: web_search
  ├ ✓ 🔧  Step 4  tool:web_search  38ms
  └ ✓ 🧠  Step 5  gpt-4o      1450ms   520↓ 180↑
```

## Replay from Failure

Agent failed at step 5? Fix your code, replay — steps 1-4 served from cache (0ms, 0 tokens), only step 5 re-runs live:

```python
with rewind_agent.replay("latest", from_step=4):
    result = await Runner.run(agent, "What is the population of Tokyo?")
    # Steps 1-4: instant cached responses
    # Step 5+: live LLM calls, recorded to new forked timeline
```

Or from the CLI:

```bash
rewind replay latest --from 4
```

Then diff: `rewind diff <session> main replayed`

## Regression Testing

```bash
# Create baseline from a known-good session
rewind assert baseline latest --name "research-happy-path"

# After code changes, check for regressions
rewind assert check latest --against "research-happy-path"
```

## Multi-Agent Handoffs

Handoffs between agents are automatically recorded:

```python
triage = Agent(name="triage", instructions="...", handoffs=[researcher, writer])
researcher = Agent(name="researcher", instructions="...", tools=[web_search])
writer = Agent(name="writer", instructions="...")

result = await Runner.run(triage, "Write an article about Tokyo")
# Trace shows: triage → researcher (web_search) → writer
```

## Links

- [GitHub](https://github.com/agentoptics/rewind)
- [PyPI](https://pypi.org/project/rewind-agent/)
- [Source: RewindTracingProcessor](https://github.com/agentoptics/rewind/blob/master/python/rewind_agent/openai_agents.py)
