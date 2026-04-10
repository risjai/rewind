"""
Pydantic AI integration for Rewind.

Uses the Pydantic AI Hooks capability to capture LLM requests/responses,
tool executions, and agent run lifecycle — recording everything as Rewind steps.

Usage (auto — just init, it detects Pydantic AI):

    import rewind_agent
    from pydantic_ai import Agent

    rewind_agent.init()
    agent = Agent('openai:gpt-4o', system_prompt='You are helpful.')
    result = agent.run_sync('What is the population of Tokyo?')
    # rewind show latest → full trace with model, tokens, tools

Usage (explicit — pass hooks to Agent):

    hooks = rewind_agent.pydantic_ai_hooks()
    agent = Agent('openai:gpt-4o', capabilities=[hooks])

Zero dependencies beyond pydantic-ai itself — gracefully skips if not installed.
"""

import json
import logging
import threading
import time

logger = logging.getLogger("rewind")


def _safe_json(obj, max_len=2000) -> str:
    try:
        s = json.dumps(obj, default=str, separators=(",", ":"))
        return s[:max_len]
    except Exception:
        return str(obj)[:max_len]


def _serialize(obj) -> dict:
    """Serialize a pydantic model or arbitrary object to dict."""
    if hasattr(obj, "model_dump"):
        try:
            return obj.model_dump()
        except Exception:
            pass
    if hasattr(obj, "__dict__"):
        return {k: str(v)[:500] for k, v in obj.__dict__.items() if not k.startswith("_")}
    return {"raw": str(obj)[:1000]}


def create_rewind_hooks(store, session_id, timeline_id):
    """
    Create a Pydantic AI Hooks capability that records all LLM calls
    and tool executions as Rewind steps.

    Returns a Hooks instance ready to be passed as a capability to an Agent.
    """
    try:
        from pydantic_ai.capabilities import Hooks
    except ImportError:
        return None

    hooks = Hooks()
    step_counter = [0]
    lock = threading.Lock()
    request_starts = {}

    @hooks.on.before_model_request
    def _before_model(ctx, request_context):
        request_starts[id(request_context)] = time.perf_counter()
        return request_context

    @hooks.on.after_model_request
    def _after_model(ctx, *, request_context, response):
        try:
            start = request_starts.pop(id(request_context), None)
            duration_ms = int((time.perf_counter() - start) * 1000) if start else 0

            # Extract model name
            model_name = "unknown"
            if hasattr(ctx, "model") and ctx.model is not None:
                model_name = getattr(ctx.model, "model_name", None) or str(ctx.model)

            # Extract tokens from response
            resp_dict = _serialize(response)
            usage = resp_dict.get("usage", {}) or {}
            tokens_in = usage.get("request_tokens", 0) or usage.get("input_tokens", 0) or 0
            tokens_out = usage.get("response_tokens", 0) or usage.get("output_tokens", 0) or 0

            # Build request data
            agent_name = ""
            if hasattr(ctx, "agent") and ctx.agent is not None:
                agent_name = getattr(ctx.agent, "name", "") or ""

            req_data = {
                "model": model_name,
                "agent": agent_name,
            }
            req_context_dict = _serialize(request_context)
            if req_context_dict:
                req_data["request_context"] = _safe_json(req_context_dict)

            resp_data = resp_dict

            # Write step
            req_hash = store.blobs.put_json(req_data)
            resp_hash = store.blobs.put_json(resp_data)

            with lock:
                step_counter[0] += 1
                step_number = step_counter[0]

                store.create_step(
                    session_id=session_id,
                    timeline_id=timeline_id,
                    step_number=step_number,
                    step_type="llm_call",
                    status="success",
                    model=model_name,
                    duration_ms=duration_ms,
                    tokens_in=tokens_in,
                    tokens_out=tokens_out,
                    request_blob=req_hash,
                    response_blob=resp_hash,
                    error=None,
                )
                store.update_session_stats(session_id, step_number, tokens_in + tokens_out)

        except Exception:
            logger.warning("Rewind: failed to record pydantic-ai model request", exc_info=True)

        return response

    tool_starts = {}

    @hooks.on.before_tool_execute
    def _before_tool(ctx, *, call, tool_def, args):
        tool_starts[call.tool_call_id] = time.perf_counter()
        return args

    @hooks.on.after_tool_execute
    def _after_tool(ctx, *, call, tool_def, args, result):
        try:
            start = tool_starts.pop(call.tool_call_id, None)
            duration_ms = int((time.perf_counter() - start) * 1000) if start else 0

            tool_name = tool_def.name if hasattr(tool_def, "name") else str(call.tool_name)

            req_data = {
                "tool": tool_name,
                "args": _safe_json(_serialize(args)),
            }
            resp_data = {
                "tool": tool_name,
                "result": _safe_json(result),
            }

            req_hash = store.blobs.put_json(req_data)
            resp_hash = store.blobs.put_json(resp_data)

            with lock:
                step_counter[0] += 1
                step_number = step_counter[0]

                store.create_step(
                    session_id=session_id,
                    timeline_id=timeline_id,
                    step_number=step_number,
                    step_type="tool_call",
                    status="success",
                    model=f"tool:{tool_name}",
                    duration_ms=duration_ms,
                    tokens_in=0,
                    tokens_out=0,
                    request_blob=req_hash,
                    response_blob=resp_hash,
                    error=None,
                )
                store.update_session_stats(session_id, step_number, 0)

        except Exception:
            logger.warning("Rewind: failed to record pydantic-ai tool execution", exc_info=True)

        return result

    logger.info("Rewind: created Pydantic AI hooks capability")
    return hooks


def pydantic_ai_hooks(store=None, session_id=None, timeline_id=None):
    """
    Create a Pydantic AI Hooks capability for Rewind recording.

    If store/session_id/timeline_id are not provided, uses the global
    rewind_agent state (requires rewind_agent.init() first).

    Usage:
        import rewind_agent
        from pydantic_ai import Agent

        rewind_agent.init()
        hooks = rewind_agent.pydantic_ai_hooks()
        agent = Agent('openai:gpt-4o', capabilities=[hooks])
        result = agent.run_sync('Hello')
    """
    if store is None:
        from . import patch as _patch
        store = _patch._store
        session_id = _patch._session_id
        if _patch._recorder:
            timeline_id = _patch._recorder._timeline_id

    if store is None or session_id is None:
        raise RuntimeError(
            "rewind_agent.init() must be called before pydantic_ai_hooks(). "
            "Or pass store, session_id, timeline_id explicitly."
        )

    hooks = create_rewind_hooks(store, session_id, timeline_id)
    if hooks is None:
        raise ImportError("pydantic-ai is not installed. Install with: pip install pydantic-ai")
    return hooks
