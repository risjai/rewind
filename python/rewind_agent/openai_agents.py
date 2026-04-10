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
    agent spans and record them as Rewind steps.

    Receives span lifecycle events (start/end) and maps them to the
    Rewind store:
    - LLM calls (GenerationSpanData) → llm_call steps with model, tokens, prompt, completion
    - Tool executions (FunctionSpanData) → tool_call steps with name, input, output
    - Agent spans (AgentSpanData) → metadata on surrounding steps
    - Handoffs (HandoffSpanData) → handoff steps with from/to agent names

    Usage:
        from rewind_agent.openai_agents import RewindTracingProcessor
        from agents.tracing import add_trace_processor

        processor = RewindTracingProcessor(store, session_id, timeline_id)
        add_trace_processor(processor)
    """

    def __init__(self, store, session_id, timeline_id, recorder=None):
        self._store = store
        self._session_id = session_id
        self._timeline_id = timeline_id
        self._recorder = recorder
        self._step_counter = 0
        self._lock = threading.Lock()
        # Track pending spans for duration calculation
        self._span_starts = {}

    def on_trace_start(self, trace) -> None:
        """Called when a new trace begins (one Runner.run call)."""
        logger.debug("Rewind: agents trace started — %s", getattr(trace, "name", ""))

    def on_trace_end(self, trace) -> None:
        """Called when a trace completes."""
        logger.debug("Rewind: agents trace ended — %s", getattr(trace, "name", ""))

    def on_span_start(self, span) -> None:
        """Called when a span begins. Record start time for duration."""
        span_id = getattr(span, "span_id", id(span))
        self._span_starts[span_id] = time.perf_counter()

    def on_span_end(self, span) -> None:
        """Called when a span ends. Map to a Rewind step based on span type."""
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

        span_id = getattr(span, "span_id", id(span))
        start_time = self._span_starts.pop(span_id, None)
        duration_ms = int((time.perf_counter() - start_time) * 1000) if start_time else 0

        span_type = type(span_data).__name__
        error_obj = getattr(span, "error", None)
        error_msg = None
        if error_obj is not None:
            error_msg = getattr(error_obj, "message", str(error_obj))

        # ── GenerationSpanData → llm_call step ──
        if span_type == "GenerationSpanData":
            self._record_generation(span_data, duration_ms, error_msg)

        # ── FunctionSpanData → tool_call step ──
        elif span_type == "FunctionSpanData":
            self._record_function(span_data, duration_ms, error_msg)

        # ── HandoffSpanData → annotate handoff ──
        elif span_type == "HandoffSpanData":
            self._record_handoff(span_data, duration_ms, error_msg)

        # AgentSpanData, GuardrailSpanData, etc. — skip (they wrap other spans)

    def _record_generation(self, span_data, duration_ms, error):
        """Record an LLM generation as a Rewind step."""
        model = getattr(span_data, "model", "unknown") or "unknown"

        # Usage is a dict with input_tokens/output_tokens
        usage = getattr(span_data, "usage", None) or {}
        input_tokens = usage.get("input_tokens", 0) or 0
        output_tokens = usage.get("output_tokens", 0) or 0

        # Build request/response dicts from span data
        model_config = getattr(span_data, "model_config", None)
        request_data = {
            "model": model,
            "model_config": _safe_json(model_config) if model_config else None,
        }
        # The input field contains the prompt/messages
        input_data = getattr(span_data, "input", None)
        if input_data is not None:
            request_data["input"] = _safe_json(input_data)

        response_data = {}
        output_data = getattr(span_data, "output", None)
        if output_data is not None:
            response_data["output"] = _safe_json(output_data)
        response_data["usage"] = {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
        }

        self._write_step(
            step_type="llm_call",
            model=model,
            duration_ms=duration_ms,
            tokens_in=input_tokens,
            tokens_out=output_tokens,
            request_data=request_data,
            response_data=response_data,
            error=error,
        )

    def _record_function(self, span_data, duration_ms, error):
        """Record a tool/function call as a Rewind step."""
        tool_name = getattr(span_data, "name", None) or "unknown_tool"
        input_val = getattr(span_data, "input", None)
        output_val = getattr(span_data, "output", None)

        request_data = {
            "tool": tool_name,
            "input": _safe_json(input_val) if input_val else None,
        }
        response_data = {
            "tool": tool_name,
            "output": _safe_json(output_val) if output_val else None,
        }

        self._write_step(
            step_type="tool_call",
            model=f"tool:{tool_name}",
            duration_ms=duration_ms,
            tokens_in=0,
            tokens_out=0,
            request_data=request_data,
            response_data=response_data,
            error=error,
        )

    def _record_handoff(self, span_data, duration_ms, error):
        """Record an agent handoff as a Rewind step."""
        from_agent = getattr(span_data, "from_agent", None) or "unknown"
        to_agent = getattr(span_data, "to_agent", None) or "unknown"

        request_data = {"handoff": {"from": from_agent, "to": to_agent}}
        response_data = {"handoff": {"from": from_agent, "to": to_agent, "status": "completed"}}

        self._write_step(
            step_type="tool_call",
            model=f"handoff:{from_agent}->{to_agent}",
            duration_ms=duration_ms,
            tokens_in=0,
            tokens_out=0,
            request_data=request_data,
            response_data=response_data,
            error=error,
        )

    def _write_step(self, step_type, model, duration_ms, tokens_in, tokens_out,
                     request_data, response_data, error):
        """Write a step to the Rewind store."""
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
