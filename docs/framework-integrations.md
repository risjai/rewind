# Framework Integrations

Rewind is a time-travel debugger for AI agents that records every LLM call for inspection, forking, replay, and diffing. It integrates natively with popular agent frameworks so recordings capture rich semantic information (agent names, tool calls, handoffs) with minimal setup.

## OpenAI Agents SDK -- zero-config, auto-detected

If you're using the [OpenAI Agents SDK](https://github.com/openai/openai-agents-python), Rewind auto-detects it. Just `init()` and every agent run is recorded -- LLM calls, tool executions, and handoffs between agents:

```python
import rewind_agent
from agents import Agent, Runner

rewind_agent.init()  # auto-detects Agents SDK, registers tracing

agent = Agent(name="researcher", instructions="You are a research assistant.", tools=[web_search])
result = await Runner.run(agent, "What is the population of Tokyo?")

# rewind show latest → trace with agent names, tool calls, handoffs
```

```
⏪ Rewind — Session Trace

  ┌ ✓ 🧠  Step 1  gpt-4o       researcher     320ms   156↓  28↑
  │   → tool_calls: web_search
  ├ ✓ 🔧  Step 2  tool:web_search               45ms
  ├ ✓ 🧠  Step 3  gpt-4o       researcher     890ms   312↓  35↑
  │   → final answer
```

For explicit control over lifecycle hooks:

```python
hooks = rewind_agent.openai_agents_hooks()
result = await Runner.run(agent, input, hooks=hooks)
```

Install with: `pip install rewind-agent[agents]`

For a deep dive on the OpenAI Agents SDK integration, see [openai-agents-sdk.md](openai-agents-sdk.md).

## Pydantic AI -- auto-injected via Hooks

If [Pydantic AI](https://ai.pydantic.dev/) is installed, `init()` auto-patches `Agent.__init__` to inject Rewind's Hooks capability. Every agent gets recording for free:

```python
import rewind_agent
from pydantic_ai import Agent

rewind_agent.init()  # auto-patches Pydantic AI Agent

agent = Agent('openai:gpt-4o', system_prompt='You are a research assistant.')
result = agent.run_sync('What is the population of Tokyo?')
# rewind show latest → full trace with model, tokens, tool calls
```

Or pass hooks explicitly as a capability:

```python
hooks = rewind_agent.pydantic_ai_hooks()
agent = Agent('openai:gpt-4o', capabilities=[hooks])
```

Install with: `pip install rewind-agent[pydantic]`

## LangGraph

Wrap a compiled LangGraph graph to automatically record all graph nodes:

```python
# LangGraph — wraps all graph nodes automatically
graph = rewind_agent.wrap_langgraph(compiled_graph)
result = graph.invoke({"input": "..."})
```

See the full example: [`examples/04_langgraph.py`](../examples/04_langgraph.py)

## CrewAI

Hook into CrewAI step and task callbacks:

```python
# CrewAI — hooks into step and task callbacks
crew = rewind_agent.wrap_crew(crew)
result = crew.kickoff()
```

## Examples

- [`examples/04_langgraph.py`](../examples/04_langgraph.py) -- LangGraph integration
- [`examples/09_pydantic_ai.py`](../examples/09_pydantic_ai.py) -- Pydantic AI integration
- [`examples/10_openai_agents_sdk.py`](../examples/10_openai_agents_sdk.py) -- OpenAI Agents SDK integration
