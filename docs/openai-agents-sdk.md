# OpenAI Agents SDK Integration

[Rewind](https://github.com/agentoptics/rewind) is a time-travel debugger for AI agents. It records every LLM call, tool execution, and handoff — then lets you fork, replay from failure, and diff timelines.

This guide shows how to use Rewind with the [OpenAI Agents SDK](https://github.com/openai/openai-agents-python).

## Install

```bash
pip install rewind-agent[agents]
```

This installs `rewind-agent` and `openai-agents` together.

## Quick Start (Zero Config)

Add one line before your agent code — Rewind auto-detects the Agents SDK and registers a `TracingProcessor`:

```python
import rewind_agent
from agents import Agent, Runner, function_tool

rewind_agent.init()  # auto-registers tracing — that's it

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

Every LLM call, tool execution, and handoff is recorded to `~/.rewind/`.

## Inspect the Recording

```bash
# Quick trace view
rewind show latest

# Interactive TUI — navigate steps, see full context windows
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

Agent failed at step 5? Fix your code, then replay — steps 1-4 are served from cache (instant, free), only step 5 re-runs live:

```python
with rewind_agent.replay("latest", from_step=4):
    result = await Runner.run(agent, "What is the population of Tokyo?")
    # Steps 1-4: instant cached responses (0ms, 0 tokens)
    # Step 5+: live LLM calls, recorded to new forked timeline
```

Or from the CLI:

```bash
rewind replay latest --from 4
# Then point your agent at http://127.0.0.1:8443/v1
```

After replay, diff the original against the replayed timeline:

```bash
rewind diff <session> main replayed
```

## Regression Testing

Create a baseline from a known-good session, then check new sessions against it:

```bash
# Create baseline
rewind assert baseline latest --name "research-happy-path"

# After code changes, check for regressions
rewind assert check latest --against "research-happy-path"
```

Use in CI:

```python
from rewind_agent import Assertions

result = Assertions().check("research-happy-path", "latest")
assert result.passed, f"Regression: {result.failed_checks} checks failed"
```

## Explicit Hooks (Optional)

For additional lifecycle visibility, pass `RewindRunHooks` to `Runner.run()`:

```python
import rewind_agent

rewind_agent.init()
hooks = rewind_agent.openai_agents_hooks()

result = await Runner.run(agent, input, hooks=hooks)
```

This captures `on_llm_start`, `on_llm_end`, `on_tool_start`, `on_tool_end`, and `on_handoff` events.

## How It Works

Rewind registers a `TracingProcessor` with the Agents SDK's tracing system via `add_trace_processor()`. This processor receives span events and maps them to Rewind steps:

| Agents SDK Span | Rewind Step Type | Data Captured |
|:----------------|:-----------------|:--------------|
| `GenerationSpanData` | `llm_call` | Model, tokens, prompt, completion |
| `FunctionSpanData` | `tool_call` | Tool name, input, output |
| `HandoffSpanData` | `tool_call` | From/to agent names |

All data is stored locally in `~/.rewind/` (SQLite + content-addressed blobs). Nothing is sent to any cloud service.

## Multi-Agent Handoffs

Rewind automatically records agent-to-agent handoffs:

```python
triage = Agent(name="triage", instructions="...", handoffs=[researcher, writer])
researcher = Agent(name="researcher", instructions="...", tools=[web_search])
writer = Agent(name="writer", instructions="...")

result = await Runner.run(triage, "Write an article about Tokyo")
# Trace shows: triage → researcher (web_search) → writer
```

## MCP Server

Rewind ships an MCP server so AI assistants (Claude Code, Cursor) can query your recordings:

```
> "Why did my agent fail on the research task?"

The assistant calls show_session → reads the trace → identifies the failure.
```

## Links

- [GitHub](https://github.com/agentoptics/rewind)
- [PyPI](https://pypi.org/project/rewind-agent/)
- [Full Documentation](https://github.com/agentoptics/rewind#readme)
