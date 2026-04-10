"""
Rewind Agent SDK — Chrome DevTools for AI agents.

Usage:
    import rewind_agent

    # Auto-patch OpenAI/Anthropic to route through the proxy
    rewind_agent.init()

    # Decorate agent steps for richer traces
    @rewind_agent.step("search")
    def search(query):
        return client.chat.completions.create(...)

    # Wrap LangGraph / CrewAI for automatic instrumentation
    graph = rewind_agent.wrap_langgraph(graph)
    crew = rewind_agent.wrap_crew(crew)
"""

from .patch import init, uninit, session, replay
from .hooks import (
    step,
    node,
    tool,
    trace,
    annotate,
    get_annotations,
    wrap_langgraph,
    wrap_crew,
)
from .assertions import Assertions, AssertionResult
from .openai_agents import openai_agents_hooks
from .pydantic_ai import pydantic_ai_hooks

__all__ = [
    "init",
    "uninit",
    "session",
    "replay",
    "step",
    "node",
    "tool",
    "trace",
    "annotate",
    "get_annotations",
    "wrap_langgraph",
    "wrap_crew",
    "openai_agents_hooks",
    "pydantic_ai_hooks",
    "Assertions",
    "AssertionResult",
]
__version__ = "0.5.5"
