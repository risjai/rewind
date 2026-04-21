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

from .patch import init, uninit, session, replay, thread
from .hooks import (
    step,
    node,
    tool,
    trace,
    span,
    annotate,
    get_annotations,
    wrap_langgraph,
    wrap_crew,
)
from .explicit import ExplicitClient
from .assertions import Assertions, AssertionResult
from .openai_agents import openai_agents_hooks
from .pydantic_ai import pydantic_ai_hooks
from .evaluation import (
    Dataset,
    EvalScore,
    EvalFailedError,
    ExperimentResult,
    ExampleResult,
    ComparisonResult,
    evaluate,
    compare,
    evaluator,
    exact_match,
    contains_match,
    regex_match,
    tool_use_match,
    llm_judge_evaluator,
)

__all__ = [
    "init",
    "uninit",
    "session",
    "replay",
    "thread",
    "step",
    "node",
    "tool",
    "trace",
    "span",
    "annotate",
    "get_annotations",
    "wrap_langgraph",
    "wrap_crew",
    "openai_agents_hooks",
    "pydantic_ai_hooks",
    "Assertions",
    "AssertionResult",
    # Evaluation
    "Dataset",
    "EvalScore",
    "EvalFailedError",
    "ExperimentResult",
    "ExampleResult",
    "ComparisonResult",
    "evaluate",
    "compare",
    "evaluator",
    "exact_match",
    "contains_match",
    "regex_match",
    "tool_use_match",
    "llm_judge_evaluator",
    # OTel export
    "export_otel",
    # Explicit Recording API
    "ExplicitClient",
]


def export_otel(session_id: str, **kwargs) -> int:
    """Export a recorded session as OTel traces. Requires: pip install rewind-agent[otel]"""
    from .otel_export import export_session
    return export_session(session_id, **kwargs)


def import_otel(**kwargs) -> str:
    """Import an OTel trace into Rewind. See otel_import.import_otel for args."""
    from .otel_import import import_otel as _import_otel
    return _import_otel(**kwargs)


def import_from_langfuse(trace_id: str, **kwargs) -> str:
    """Import a Langfuse trace into Rewind. See langfuse_import.import_from_langfuse for args."""
    from .langfuse_import import import_from_langfuse as _import
    return _import(trace_id, **kwargs)


__version__ = "0.14.8"
