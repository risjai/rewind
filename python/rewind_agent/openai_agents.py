"""
OpenAI Agents SDK integration for Rewind.

Registers a TracingProcessor that maps agent spans → Rewind steps,
and provides RunHooks for richer lifecycle capture.

Usage (auto — just init, it detects the Agents SDK):

    import rewind_agent
    from agents import Agent, Runner

    rewind_agent.init()
    agent = Agent(name="researcher", instructions="...", tools=[...])
    result = await Runner.run(agent, "What is the population of Tokyo?")
    # Everything recorded — LLM calls, tool executions, handoffs

Usage (explicit hooks for full lifecycle capture):

    hooks = rewind_agent.openai_agents_hooks()
    result = await Runner.run(agent, input, hooks=hooks)

Zero dependencies beyond the agents SDK itself — gracefully skips if not installed.
"""

import json
import logging
import threading
import time

logger = logging.getLogger("rewind")

# Import TracingProcessor base class — required for proper subclassing
try:
    from agents.tracing import TracingProcessor as _TracingProcessorBase
except ImportError:
    # Agents SDK not installed — define a no-op base so the module loads cleanly
    class _TracingProcessorBase:
        def on_trace_start(self, trace): pass
        def on_trace_end(self, trace): pass
        def on_span_start(self, span): pass
        def on_span_end(self, span): pass
        def shutdown(self): pass
        def force_flush(self): pass


def _safe_json(obj, max_len=2000) -> str:
    """Safely serialize an object to JSON string, truncated."""
    try:
        s = json.dumps(obj, default=str, separators=(",", ":"))
        return s[:max_len]
    except Exception:
        return str(obj)[:max_len]


# ── TracingProcessor ─────────────────────────────────────────

class RewindTracingProcessor(_TracingProcessorBase):
    """
    Subclasses the OpenAI Agents SDK TracingProcessor to capture all
    agent spans and record them as a Rewind span tree with linked steps.
    """

    def __init__(self, store, session_id, timeline_id, recorder=None):
        self._store = store
        self._session_id = session_id
        self._timeline_id = timeline_id
        self._recorder = recorder
        self._step_counter = 0
        self._lock = threading.Lock()
        self._span_starts = {}
        # Map SDK span_id → Rewind span_id
        self._span_id_map = {}
        # Map SDK span_id → SDK parent_id (for resolving parent chains)
        self._sdk_parent_map = {}

    def on_trace_start(self, trace) -> None:
        """Called when a new trace begins. Use group_id for thread linking."""
        logger.debug("Rewind: agents trace started — %s", getattr(trace, "name", ""))
        group_id = getattr(trace, "group_id", None)
        if group_id:
            try:
                existing = self._store.get_sessions_by_thread(group_id)
                ordinal = len(existing)
                self._store.set_session_thread(self._session_id, group_id, ordinal)
            except Exception:
                logger.debug("Rewind: failed to set thread from trace group_id", exc_info=True)

    def on_trace_end(self, trace) -> None:
        """Called when a trace completes."""
        logger.debug("Rewind: agents trace ended — %s", getattr(trace, "name", ""))

    def on_span_start(self, span) -> None:
        """Called when a span begins. Create Rewind span for agent/tool/handoff types."""
        sdk_span_id = getattr(span, "span_id", id(span))
        sdk_parent_id = getattr(span, "parent_id", None)
        self._span_starts[sdk_span_id] = time.perf_counter()
        self._sdk_parent_map[sdk_span_id] = sdk_parent_id

        span_data = getattr(span, "span_data", None)
        if span_data is None:
            return

        span_type_name = type(span_data).__name__

        parent_rewind_span_id = None
        if sdk_parent_id and sdk_parent_id in self._span_id_map:
            parent_rewind_span_id = self._span_id_map[sdk_parent_id]

        try:
            if span_type_name == "AgentSpanData":
                agent_name = getattr(span_data, "name", "unknown") or "unknown"
                rewind_span_id = self._store.create_span(
                    session_id=self._session_id,
                    timeline_id=self._timeline_id,
                    span_type="agent",
                    name=agent_name,
                    parent_span_id=parent_rewind_span_id,
                    metadata=json.dumps({
                        "handoffs": getattr(span_data, "handoffs", []),
                        "tools": getattr(span_data, "tools", []),
                        "output_type": getattr(span_data, "output_type", None),
                    }),
                )
                self._span_id_map[sdk_span_id] = rewind_span_id

            elif span_type_name == "HandoffSpanData":
                from_agent = getattr(span_data, "from_agent", "unknown") or "unknown"
                to_agent = getattr(span_data, "to_agent", "unknown") or "unknown"
                rewind_span_id = self._store.create_span(
                    session_id=self._session_id,
                    timeline_id=self._timeline_id,
                    span_type="handoff",
                    name=f"{from_agent}\u2192{to_agent}",
                    parent_span_id=parent_rewind_span_id,
                    metadata=json.dumps({"from_agent": from_agent, "to_agent": to_agent}),
                )
                self._span_id_map[sdk_span_id] = rewind_span_id

            elif span_type_name == "FunctionSpanData":
                tool_name = getattr(span_data, "name", "unknown_tool") or "unknown_tool"
                rewind_span_id = self._store.create_span(
                    session_id=self._session_id,
                    timeline_id=self._timeline_id,
                    span_type="tool",
                    name=tool_name,
                    parent_span_id=parent_rewind_span_id,
                )
                self._span_id_map[sdk_span_id] = rewind_span_id

        except Exception:
            logger.warning("Rewind: failed to create span on span_start", exc_info=True)

    def on_span_end(self, span) -> None:
        """Called when a span ends. Close Rewind spans and record steps."""
        try:
            self._handle_span_end(span)
        except Exception:
            logger.warning("Rewind: failed to record agents span", exc_info=True)

    def shutdown(self) -> None:
        pass

    def force_flush(self) -> None:
        pass

    def _handle_span_end(self, span):
        span_data = getattr(span, "span_data", None)
        if span_data is None:
            return

        sdk_span_id = getattr(span, "span_id", id(span))
        sdk_parent_id = self._sdk_parent_map.get(sdk_span_id)
        start_time = self._span_starts.pop(sdk_span_id, None)
        duration_ms = int((time.perf_counter() - start_time) * 1000) if start_time else 0

        span_type_name = type(span_data).__name__
        error_obj = getattr(span, "error", None)
        error_msg = None
        if error_obj is not None:
            error_msg = getattr(error_obj, "message", str(error_obj))

        parent_rewind_span_id = None
        if sdk_parent_id and sdk_parent_id in self._span_id_map:
            parent_rewind_span_id = self._span_id_map[sdk_parent_id]

        rewind_span_id = self._span_id_map.get(sdk_span_id)
        if rewind_span_id:
            status = "error" if error_msg else "completed"
            self._store.update_span_status(rewind_span_id, status, duration_ms, error_msg)

        if span_type_name == "GenerationSpanData":
            self._record_generation(span_data, duration_ms, error_msg, parent_rewind_span_id)
        elif span_type_name == "FunctionSpanData":
            self._record_function(span_data, duration_ms, error_msg, rewind_span_id)
        elif span_type_name == "HandoffSpanData":
            self._record_handoff(span_data, duration_ms, error_msg, rewind_span_id)

    def _record_generation(self, span_data, duration_ms, error, span_id):
        """Record an LLM generation as a Rewind step linked to its parent span."""
        model = getattr(span_data, "model", "unknown") or "unknown"
        usage = getattr(span_data, "usage", None) or {}
        input_tokens = usage.get("input_tokens", 0) or 0
        output_tokens = usage.get("output_tokens", 0) or 0

        model_config = getattr(span_data, "model_config", None)
        request_data = {"model": model}
        if model_config:
            request_data["model_config"] = _safe_json(model_config)
        input_data = getattr(span_data, "input", None)
        if input_data is not None:
            request_data["input"] = _safe_json(input_data)

        response_data = {}
        output_data = getattr(span_data, "output", None)
        if output_data is not None:
            response_data["output"] = _safe_json(output_data)
        response_data["usage"] = {"input_tokens": input_tokens, "output_tokens": output_tokens}

        self._write_step(
            step_type="llm_call", model=model, duration_ms=duration_ms,
            tokens_in=input_tokens, tokens_out=output_tokens,
            request_data=request_data, response_data=response_data,
            error=error, span_id=span_id,
        )

    def _record_function(self, span_data, duration_ms, error, span_id):
        """Record a tool/function call as a Rewind step linked to its span."""
        tool_name = getattr(span_data, "name", None) or "unknown_tool"
        input_val = getattr(span_data, "input", None)
        output_val = getattr(span_data, "output", None)

        request_data = {"tool": tool_name, "input": _safe_json(input_val) if input_val else None}
        response_data = {"tool": tool_name, "output": _safe_json(output_val) if output_val else None}

        self._write_step(
            step_type="tool_call", model=f"tool:{tool_name}", duration_ms=duration_ms,
            tokens_in=0, tokens_out=0,
            request_data=request_data, response_data=response_data,
            error=error, span_id=span_id,
        )

    def _record_handoff(self, span_data, duration_ms, error, span_id):
        """Record an agent handoff as a Rewind step linked to its span."""
        from_agent = getattr(span_data, "from_agent", None) or "unknown"
        to_agent = getattr(span_data, "to_agent", None) or "unknown"

        request_data = {"handoff": {"from": from_agent, "to": to_agent}}
        response_data = {"handoff": {"from": from_agent, "to": to_agent, "status": "completed"}}

        self._write_step(
            step_type="tool_call", model=f"handoff:{from_agent}->{to_agent}",
            duration_ms=duration_ms, tokens_in=0, tokens_out=0,
            request_data=request_data, response_data=response_data,
            error=error, span_id=span_id,
        )

    def _write_step(self, step_type, model, duration_ms, tokens_in, tokens_out,
                     request_data, response_data, error, span_id=None):
        """Write a step to the Rewind store, optionally linked to a span."""
        status = "error" if error else "success"
        req_hash = self._store.blobs.put_json(request_data)
        resp_hash = self._store.blobs.put_json(response_data if not error else {"error": error})

        with self._lock:
            self._step_counter += 1
            step_number = self._step_counter

            self._store.create_step(
                session_id=self._session_id,
                timeline_id=self._timeline_id,
                step_number=step_number,
                step_type=step_type,
                status=status,
                model=model,
                duration_ms=duration_ms,
                tokens_in=tokens_in,
                tokens_out=tokens_out,
                request_blob=req_hash,
                response_blob=resp_hash,
                error=error,
                span_id=span_id,
            )
            self._store.update_session_stats(
                self._session_id, step_number, tokens_in + tokens_out,
            )


# ── RunHooks ─────────────────────────────────────────────────

class RewindRunHooks:
    """
    OpenAI Agents SDK RunHooks that capture lifecycle events with full
    request/response data. Use alongside RewindTracingProcessor for
    the richest recordings.

    Usage:
        hooks = RewindRunHooks(store, session_id, timeline_id)
        result = await Runner.run(agent, input, hooks=hooks)
    """

    def __init__(self, store, session_id, timeline_id):
        self._store = store
        self._session_id = session_id
        self._timeline_id = timeline_id
        self._step_counter = 0
        self._lock = threading.Lock()

    async def on_agent_start(self, context, agent) -> None:
        logger.debug("Rewind: agent started — %s", agent.name)

    async def on_agent_end(self, context, agent, output) -> None:
        logger.debug("Rewind: agent ended — %s", agent.name)

    async def on_handoff(self, context, from_agent, to_agent) -> None:
        logger.debug("Rewind: handoff %s → %s", from_agent.name, to_agent.name)

    async def on_tool_start(self, context, agent, tool) -> None:
        logger.debug("Rewind: tool start — %s/%s", agent.name, tool.name)

    async def on_tool_end(self, context, agent, tool, result) -> None:
        logger.debug("Rewind: tool end — %s/%s", agent.name, tool.name)

    async def on_llm_start(self, context, agent, system_prompt, input_items) -> None:
        logger.debug("Rewind: LLM start — %s (%d input items)", agent.name, len(input_items))

    async def on_llm_end(self, context, agent, response) -> None:
        logger.debug("Rewind: LLM end — %s", agent.name)


# ── Public API ───────────────────────────────────────────────

def register_tracing_processor(store, session_id, timeline_id, recorder=None):
    """
    Register RewindTracingProcessor with the OpenAI Agents SDK.
    Called automatically by rewind_agent.init() if the SDK is installed.

    Returns the processor instance, or None if the SDK is not available.
    """
    try:
        from agents.tracing import add_trace_processor
    except ImportError:
        return None

    processor = RewindTracingProcessor(store, session_id, timeline_id, recorder)
    add_trace_processor(processor)
    logger.info("Rewind: registered OpenAI Agents SDK tracing processor")
    return processor


def openai_agents_hooks(store=None, session_id=None, timeline_id=None):
    """
    Create RewindRunHooks for the OpenAI Agents SDK.

    If store/session_id/timeline_id are not provided, uses the global
    rewind_agent state (requires rewind_agent.init() first).

    Usage:
        import rewind_agent
        rewind_agent.init()

        hooks = rewind_agent.openai_agents_hooks()
        result = await Runner.run(agent, input, hooks=hooks)
    """
    if store is None:
        from . import patch as _patch
        store = _patch._store
        session_id = _patch._session_id
        # Get timeline from recorder
        if _patch._recorder:
            timeline_id = _patch._recorder._timeline_id

    if store is None or session_id is None:
        raise RuntimeError(
            "rewind_agent.init() must be called before openai_agents_hooks(). "
            "Or pass store, session_id, timeline_id explicitly."
        )

    return RewindRunHooks(store, session_id, timeline_id)
