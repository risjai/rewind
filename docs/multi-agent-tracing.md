# Multi-Agent Tracing

When your agent system has multiple agents — a planner that delegates to a researcher, a researcher that hands off to a writer — a flat list of LLM calls is useless. You need to see **which agent made which decision** and how they handed off control.

Rewind's multi-agent tracing gives you:

- **Span tree** — hierarchical grouping of LLM calls, tool invocations, and handoffs under their parent agent.
- **Threads** — multi-turn conversation tracking across sessions (e.g., a chatbot where each user message is a separate session).
- **Auto-capture** — native integrations with OpenAI Agents SDK and Pydantic AI detect agent boundaries automatically.

---

## Span tree

A span represents a logical unit of work — an agent execution, a tool call, or a handoff. Spans nest: a supervisor agent span contains child agent spans, which contain LLM calls and tool spans.

### Span types

| Type | Icon | Description |
|:-----|:-----|:------------|
| `agent` | 🤖 | An agent's execution boundary |
| `tool` | 🔧 | A tool invocation |
| `handoff` | 🔀 | Control transfer between agents |
| `custom` | 📦 | Any user-defined grouping |

### What the trace looks like

```
⏪ Rewind — Session Trace

  Session: customer-service   Steps: 12   Tokens: 3,450
  Agents: supervisor, researcher, writer

  ▼ ✓ 🤖 supervisor (agent)                          1.2s
    ├ ✓ 🧠  gpt-4o  "Route to researcher"           320ms  156↓ 28↑
    ▼ ✓ 🤖 researcher (agent)                        2.1s
    │ ├ ✓ 🧠  gpt-4o  "Search for information"      890ms  312↓ 35↑
    │ ├ ✓ 🔧  web_search("Tokyo population")          45ms
    │ └ ✓ 🧠  gpt-4o  "Synthesize results"          650ms  280↓ 95↑
    ├ ✓ 🔀 handoff: researcher → writer
    ▼ ✗ 🤖 writer (agent)                            1.8s
    │ ├ ✓ 🧠  gpt-4o  "Draft article"              1200ms  450↓ 180↑
    │ └ ✗ 🧠  gpt-4o  "Polish final draft"          600ms  320↓ 120↑
    │     ERROR: Hallucination — used stale data
    └ ✓ 🧠  gpt-4o  "Final review"                   400ms  200↓ 45↑
```

Without the span tree, this would be a flat list of 12 steps with no agent boundaries.

---

## Automatic tracing (zero config)

### OpenAI Agents SDK

If the [Agents SDK](https://github.com/openai/openai-agents-python) is installed, `init()` auto-registers a `RewindTracingProcessor` that captures all agent spans, handoffs, and tool executions:

```bash
pip install rewind-agent[agents]
```

```python
import rewind_agent
from agents import Agent, Runner, function_tool

rewind_agent.init()  # auto-detects Agents SDK, registers tracing

@function_tool
def web_search(query: str) -> str:
    """Search the web."""
    return f"Results for: {query}"

triage = Agent(name="triage", instructions="Route queries.", handoffs=[researcher])
researcher = Agent(name="researcher", instructions="Research topics.", tools=[web_search])

result = await Runner.run(triage, "What is the population of Tokyo?")
# rewind show latest → span tree with agent boundaries
```

**What gets captured automatically:**

| Agents SDK Span | Rewind Span Type | Data |
|:----------------|:-----------------|:-----|
| `AgentSpanData` | `agent` | Agent name, tools, handoffs, output type |
| `HandoffSpanData` | `handoff` | From agent → to agent |
| `FunctionSpanData` | `tool` | Tool name, input, output |
| `GenerationSpanData` | *(via monkey-patch)* | Model, tokens, messages |

See [openai-agents-sdk.md](openai-agents-sdk.md) for the full integration guide.

### Pydantic AI

Auto-detected the same way:

```bash
pip install rewind-agent[pydantic]
```

```python
import rewind_agent
from pydantic_ai import Agent

rewind_agent.init()  # auto-detects Pydantic AI

agent = Agent("openai:gpt-4o", system_prompt="You are a helpful assistant.")
result = agent.run_sync("What is the capital of France?")
```

See [framework-integrations.md](framework-integrations.md) for LangGraph, CrewAI, and other frameworks.

---

## Manual spans — `@span()` decorator

For custom agent architectures or any code you want to group in the span tree, use the `@span()` decorator or context manager:

### As a decorator

```python
import rewind_agent

rewind_agent.init()

@rewind_agent.span("planning-phase")
def plan(task: str) -> list[str]:
    return client.chat.completions.create(
        model="gpt-4o",
        messages=[{"role": "user", "content": f"Break down: {task}"}],
    )

@rewind_agent.span("execution-phase")
def execute(steps: list[str]) -> str:
    for step in steps:
        run_step(step)  # LLM calls inside are nested under this span
    return "done"
```

### As a context manager

```python
with rewind_agent.span("retry-loop"):
    for attempt in range(3):
        result = call_llm(...)
        if result.success:
            break
```

### Nesting

Spans nest automatically via context variables. Inner spans become children of the outer span:

```python
@rewind_agent.span("supervisor", span_type="agent")
def supervise(query: str):
    plan = plan_task(query)

    @rewind_agent.span("researcher", span_type="agent")
    def research():
        return search(plan)

    @rewind_agent.span("writer", span_type="agent")
    def write():
        return draft(research())

    return write()
```

The `span_type` parameter defaults to `"custom"` but can be set to `"agent"`, `"tool"`, or `"handoff"` for proper iconography in the tree.

---

## Threads — multi-turn conversations

A thread groups multiple sessions into a single conversation. Each session is one "turn" — the user says something, the agent responds.

```python
import rewind_agent

rewind_agent.init()

with rewind_agent.thread("conversation-123"):
    with rewind_agent.session("turn-1"):
        result = agent.run("What is the capital of France?")

    with rewind_agent.session("turn-2"):
        result = agent.run("And what about Germany?")

    with rewind_agent.session("turn-3"):
        result = agent.run("Compare the two cities.")
```

All three sessions are linked by the thread ID `conversation-123` and ordered by their ordinal (1, 2, 3).

### OpenAI Agents SDK threads

When using the Agents SDK, Rewind auto-extracts the `group_id` from traces as the thread ID. Multi-turn conversations are threaded automatically.

---

## CLI commands

### View span tree

```bash
# span tree (default when spans exist)
rewind show latest
# flat step list (ignore spans)
rewind show latest --flat
```

### List threads

```bash
rewind threads
```

```
⏪ Rewind — Threads

  Thread ID                Sessions  Steps  Tokens
  conversation-123              3      15    4,200
  support-ticket-456            5      32    8,100
```

### View a thread

```bash
rewind thread conversation-123
```

```
⏪ Rewind — Thread: conversation-123

  Turn 1: turn-1              5 steps   1,200 tokens
  Turn 2: turn-2              4 steps     980 tokens
  Turn 3: turn-3              6 steps   2,020 tokens
```

---

## Web dashboard

The web UI at `http://127.0.0.1:4800` renders span trees interactively:

- **Session view** — automatically shows the span tree when spans are present, with expand/collapse controls, type icons, durations, and token counts per span.
- **Thread view** — lists all threads, click into a thread to see each turn as a card with its own span tree.

```bash
rewind web --port 4800
# Open http://127.0.0.1:4800
```

---

## Web API

| Method | Path | Description |
|:-------|:-----|:------------|
| `GET` | `/api/sessions/{id}/spans?timeline={tid}` | Span tree for a session |
| `GET` | `/api/threads` | List all threads |
| `GET` | `/api/threads/{id}` | Thread detail with sessions |

---

## MCP server

AI assistants (Claude Code, Cursor, Windsurf) can query span trees via the MCP server:

| Tool | Parameters | Description |
|:-----|:-----------|:------------|
| `get_span_tree` | `session`, `timeline?` | Full hierarchical span tree |
| `list_threads` | — | List all conversation threads |
| `show_thread` | `thread_id` | Thread detail with sessions |
| `get_thread_summary` | `thread_id` | Condensed thread view |

---

## Related guides

- [OpenAI Agents SDK Integration](openai-agents-sdk.md) — TracingProcessor details, span mapping, handoff capture
- [Framework Integrations](framework-integrations.md) — Pydantic AI, LangGraph, CrewAI
- [Recording](recording.md) — `@step`, `@tool`, `@node`, `trace()`, `annotate()` hooks
