"""
Rewind Hooks — Framework-agnostic decorators for enriching agent recordings.

These decorators work with ANY Python agent framework. They annotate
the LLM calls captured by the Rewind proxy with semantic metadata:
node names, agent state, step descriptions.

Works with LangGraph, CrewAI, OpenAI Agents SDK, or plain functions.
"""

import contextvars
import functools
import json
import logging
import os
import time
import urllib.request
from contextlib import contextmanager
from typing import Any, Callable

REWIND_PROXY = os.environ.get("REWIND_PROXY", "http://127.0.0.1:8443")

_step_counter = 0
_annotations: list[dict] = []


def step(name: str | None = None, metadata: dict | None = None):
    """
    Decorator that records a function as a named agent step.

    Usage:
        @rewind.step("search")
        def search_web(query: str) -> str:
            return client.chat.completions.create(...)

        @rewind.step("plan", metadata={"agent": "planner"})
        def plan_task(goal: str) -> dict:
            ...
    """
    def decorator(func: Callable) -> Callable:
        step_name = name or func.__name__

        @functools.wraps(func)
        def wrapper(*args, **kwargs):
            global _step_counter
            _step_counter += 1
            step_id = _step_counter

            start = time.perf_counter()
            error = None
            result = None

            _annotate("step_start", {
                "step_id": step_id,
                "step_name": step_name,
                "metadata": metadata or {},
                "args_preview": _safe_preview(args, kwargs),
            })

            try:
                result = func(*args, **kwargs)
                return result
            except Exception as e:
                error = str(e)
                raise
            finally:
                elapsed_ms = (time.perf_counter() - start) * 1000
                _annotate("step_end", {
                    "step_id": step_id,
                    "step_name": step_name,
                    "duration_ms": round(elapsed_ms, 2),
                    "error": error,
                    "result_preview": _safe_str(result)[:200] if result else None,
                })

        return wrapper
    return decorator


def node(name: str):
    """
    Decorator for LangGraph-style graph nodes.

    Usage:
        @rewind.node("researcher")
        def researcher(state: dict) -> dict:
            ...
    """
    return step(name=name, metadata={"type": "graph_node"})


def tool(name: str | None = None):
    """
    Decorator for tool/function calls.

    Usage:
        @rewind.tool("web_search")
        def search(query: str) -> str:
            ...
    """
    def decorator(func: Callable) -> Callable:
        tool_name = name or func.__name__
        return step(name=tool_name, metadata={"type": "tool"})(func)
    return decorator


@contextmanager
def trace(name: str, metadata: dict | None = None):
    """
    Context manager for tracing a block of agent execution.

    Usage:
        with rewind.trace("research_phase"):
            result1 = search(query)
            result2 = analyze(result1)
    """
    global _step_counter
    _step_counter += 1
    step_id = _step_counter
    start = time.perf_counter()

    _annotate("trace_start", {
        "step_id": step_id,
        "trace_name": name,
        "metadata": metadata or {},
    })

    error = None
    try:
        yield
    except Exception as e:
        error = str(e)
        raise
    finally:
        elapsed_ms = (time.perf_counter() - start) * 1000
        _annotate("trace_end", {
            "step_id": step_id,
            "trace_name": name,
            "duration_ms": round(elapsed_ms, 2),
            "error": error,
        })


# ── Manual Span Creation ──────────────────────────────────────

_current_span_id: contextvars.ContextVar[str | None] = contextvars.ContextVar("_current_span_id", default=None)


def span(name: str, span_type: str = "custom"):
    """
    Decorator and context manager for creating Rewind spans.
    Groups all LLM calls and tool calls within this scope under a named span.

    Usage as decorator:
        @rewind_agent.span("planning-phase")
        def plan(state):
            ...

    Usage as context manager:
        with rewind_agent.span("retry-loop"):
            for attempt in range(3):
                result = agent.run(query)
    """
    return _SpanContext(name, span_type)


class _SpanContext:
    """Dual-use decorator/context manager for span creation."""

    def __init__(self, name: str, span_type: str = "custom"):
        self._name = name
        self._span_type = span_type
        self._span_id = None
        self._start = None
        self._token = None

    def __call__(self, func):
        """Use as decorator."""
        @functools.wraps(func)
        def wrapper(*args, **kwargs):
            with self:
                return func(*args, **kwargs)
        return wrapper

    def __enter__(self):
        from . import patch as _patch
        self._start = time.perf_counter()

        store = getattr(_patch, "_store", None)
        session_id = getattr(_patch, "_session_id", None)
        recorder = getattr(_patch, "_recorder", None)
        timeline_id = recorder._timeline_id if recorder else None

        if store and session_id and timeline_id:
            parent_span_id = _current_span_id.get()
            try:
                self._span_id = store.create_span(
                    session_id=session_id,
                    timeline_id=timeline_id,
                    span_type=self._span_type,
                    name=self._name,
                    parent_span_id=parent_span_id,
                )
                self._token = _current_span_id.set(self._span_id)
            except Exception:
                logging.getLogger("rewind").debug(
                    "Rewind: failed to create manual span", exc_info=True
                )
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        from . import patch as _patch

        elapsed_ms = int((time.perf_counter() - self._start) * 1000) if self._start else 0
        error_msg = str(exc_val) if exc_val else None
        status = "error" if exc_val else "completed"

        if self._span_id:
            store = getattr(_patch, "_store", None)
            if store:
                try:
                    store.update_span_status(self._span_id, status, elapsed_ms, error_msg)
                except Exception:
                    pass

        if self._token is not None:
            _current_span_id.reset(self._token)

        return False

    @property
    def span_id(self):
        return self._span_id


def annotate(key: str, value: Any):
    """
    Add a custom annotation to the current recording.

    Usage:
        rewind.annotate("confidence", 0.85)
        rewind.annotate("decision", "retry with different prompt")
    """
    _annotate("custom", {"key": key, "value": _safe_str(value)})


def get_annotations() -> list[dict]:
    """Return all annotations recorded in this session."""
    return list(_annotations)


# ── LangGraph adapter ──────────────────────────────────────────

def wrap_langgraph(graph, recorder_name: str = "langgraph"):
    """
    Wrap a LangGraph compiled graph to record all node executions.

    Usage:
        from langgraph.graph import StateGraph
        graph = builder.compile()
        graph = rewind.wrap_langgraph(graph)
        result = graph.invoke({"input": "..."})

    This wraps each node's function with @rewind.node() automatically.
    """
    if graph is None:
        raise TypeError("wrap_langgraph() requires a compiled LangGraph, got None")
    try:
        nodes = graph.nodes
    except AttributeError:
        return graph  # not a LangGraph, return unchanged

    for node_name, node_fn in list(nodes.items()):
        if node_name in ("__start__", "__end__"):
            continue
        wrapped = step(name=f"{recorder_name}/{node_name}", metadata={"type": "graph_node", "graph": recorder_name})(node_fn)
        nodes[node_name] = wrapped

    return graph


# ── CrewAI adapter ─────────────────────────────────────────────

def wrap_crew(crew, recorder_name: str = "crewai"):
    """
    Instrument a CrewAI Crew to record task and step execution.

    Usage:
        from crewai import Crew
        crew = Crew(agents=[...], tasks=[...])
        crew = rewind.wrap_crew(crew)
        result = crew.kickoff()

    Hooks into step_callback and task_callback if available.
    """
    # Install step callback
    original_step_cb = getattr(crew, 'step_callback', None)

    def _step_callback(output):
        _annotate("crew_step", {
            "recorder": recorder_name,
            "output_preview": _safe_str(output)[:300],
        })
        if original_step_cb:
            original_step_cb(output)

    # Install task callback
    original_task_cb = getattr(crew, 'task_callback', None)

    def _task_callback(output):
        _annotate("crew_task_complete", {
            "recorder": recorder_name,
            "output_preview": _safe_str(output)[:300],
        })
        if original_task_cb:
            original_task_cb(output)

    try:
        crew.step_callback = _step_callback
        crew.task_callback = _task_callback
    except (AttributeError, TypeError):
        pass  # older CrewAI versions may not support this

    return crew


# ── Internal ───────────────────────────────────────────────────

_direct_mode_warned = False


def _annotate(event_type: str, data: dict):
    """Record an annotation locally and optionally POST to the proxy."""
    global _direct_mode_warned
    entry = {
        "type": event_type,
        "timestamp": time.time(),
        "data": data,
    }
    _annotations.append(entry)

    # Check if we're in direct mode (no proxy to POST to)
    from . import patch as _patch
    if getattr(_patch, "_mode", None) == "direct":
        if not _direct_mode_warned:
            _direct_mode_warned = True
            import logging
            logging.getLogger("rewind").info(
                "Annotations (@step, @tool, trace) are not yet persisted in direct recording mode. "
                "LLM calls are still fully recorded. Use mode='proxy' for annotation support."
            )
        return

    # Best-effort POST to proxy side-channel (non-blocking)
    try:
        payload = json.dumps(entry).encode()
        req = urllib.request.Request(
            f"{REWIND_PROXY}/_rewind/annotate",
            data=payload,
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        urllib.request.urlopen(req, timeout=0.5)
    except Exception:
        pass  # proxy may not be running, that's fine


def _safe_preview(args, kwargs) -> str:
    """Safely preview function arguments."""
    parts = []
    for a in args[:3]:
        parts.append(_safe_str(a)[:100])
    for k, v in list(kwargs.items())[:3]:
        parts.append(f"{k}={_safe_str(v)[:100]}")
    return ", ".join(parts)


def _safe_str(obj) -> str:
    """Safely convert any object to string."""
    try:
        if isinstance(obj, (dict, list)):
            return json.dumps(obj, default=str)[:500]
        return str(obj)[:500]
    except Exception:
        return "<unserializable>"
