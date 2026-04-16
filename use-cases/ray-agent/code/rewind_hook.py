"""Fire-and-forget event emitter for Rewind time-travel debugger.

Drop this file into ray-agent at: src/observability/rewind_hook.py

All public methods schedule background tasks via asyncio.create_task().
They return immediately and never block the caller. When
REWIND_ENABLED != "true", all methods are silent no-ops.

Envelope format matches crates/rewind-web/src/hooks.rs:
  HookEventEnvelope { source, event_type, timestamp, payload }
  HookPayload { session_id, tool_name, tool_input,
                          tool_response, tool_use_id, cwd }
"""

import asyncio
import json
import logging
import os
from datetime import datetime, timezone
from typing import Any

import httpx

logger = logging.getLogger(__name__)

REWIND_URL = os.getenv("REWIND_URL", "http://127.0.0.1:4800")
REWIND_ENABLED = os.getenv("REWIND_ENABLED", "false").lower() == "true"
REWIND_FULL_CAPTURE = os.getenv("REWIND_FULL_CAPTURE", "false").lower() == "true"


class RewindHook:
    """Async, fire-and-forget event emitter for Rewind.

    Usage in AgentIngress.__init__:
        self.rewind_hook = RewindHook()
        self.react_agent = ReactAgent(
            self.llm_client, self.tool_executor,
            rewind_hook=self.rewind_hook,
        )
    """

    def __init__(self, source: str = "ray-agent"):
        self.source = source
        self.enabled = REWIND_ENABLED
        self._client: httpx.AsyncClient | None = None
        self._url = f"{REWIND_URL}/api/hooks/event"

        if self.enabled:
            self._client = httpx.AsyncClient(timeout=2.0)
            logger.info(f"Rewind hook enabled: {self._url}")
        else:
            logger.info("Rewind hook disabled (set REWIND_ENABLED=true to enable)")

    async def _emit(self, event_type: str, payload: dict[str, Any]) -> None:
        """POST an event envelope to Rewind. Never raises."""
        if not self.enabled or not self._client:
            return
        envelope = {
            "source": self.source,
            "event_type": event_type,
            "timestamp": datetime.now(timezone.utc).isoformat(),
            "payload": payload,
        }
        try:
            resp = await self._client.post(
                self._url, json=envelope, timeout=2.0
            )
            if resp.status_code >= 400:
                logger.debug(f"Rewind POST {event_type}: {resp.status_code}")
        except Exception as e:
            logger.debug(f"Rewind POST {event_type} failed: {e}")

    def _fire(self, event_type: str, payload: dict[str, Any]) -> None:
        """Schedule _emit as a background task. Never blocks the caller."""
        if not self.enabled:
            return
        try:
            loop = asyncio.get_event_loop()
            if loop.is_running():
                loop.create_task(self._emit(event_type, payload))
            else:
                loop.run_until_complete(self._emit(event_type, payload))
        except RuntimeError:
            try:
                asyncio.run(self._emit(event_type, payload))
            except Exception:
                pass

    # ── Session lifecycle ──────────────────────────────────────

    def session_start(self, session_id: str, question: str,
                      cluster: str | None = None) -> None:
        """Call at the start of ReactAgent.run()."""
        self._fire("SessionStart", {
            "session_id": session_id,
            "tool_name": "session_start",
            "cwd": f"/ray-agent/{cluster or 'all'}",
            "tool_input": {
                "question": question,
                "cluster": cluster,
            },
        })

    def session_end(self, session_id: str, iterations: int,
                    tools_called: list[str],
                    error: str | None = None) -> None:
        """Call in the finally block of ReactAgent.run()."""
        self._fire("SessionEnd", {
            "session_id": session_id,
            "tool_name": "session_end",
            "tool_response": json.dumps({
                "iterations": iterations,
                "tools_called": tools_called,
                "error": error,
            }),
        })

    # ── LLM calls ──────────────────────────────────────────────

    def pre_llm_call(self, session_id: str, tool_use_id: str,
                     messages: list[dict], iteration: int) -> None:
        """Call before each LLM Gateway request."""
        tool_input: dict[str, Any] = {
            "iteration": iteration,
            "message_count": len(messages),
            "last_user_msg": _last_content(messages, "user"),
        }
        if REWIND_FULL_CAPTURE:
            tool_input["messages"] = messages
        self._fire("PreToolUse", {
            "session_id": session_id,
            "tool_use_id": tool_use_id,
            "tool_name": "__llm_call__",
            "tool_input": tool_input,
        })

    def post_llm_call(self, session_id: str, tool_use_id: str,
                      content: str | None, tool_calls: list | None,
                      elapsed_s: float) -> None:
        """Call after each LLM Gateway response."""
        tool_names = _extract_tool_names(tool_calls)
        response_data: dict[str, Any] = {
            "elapsed_s": round(elapsed_s, 3),
            "tool_calls": tool_names,
        }
        if REWIND_FULL_CAPTURE:
            response_data["content"] = content
            response_data["raw_tool_calls"] = tool_calls
        else:
            response_data["content_preview"] = (content or "")[:500]
        self._fire("PostToolUse", {
            "session_id": session_id,
            "tool_use_id": tool_use_id,
            "tool_name": "__llm_call__",
            "tool_response": json.dumps(response_data),
        })

    # ── Tool executions ────────────────────────────────────────

    def pre_tool(self, session_id: str, tool_use_id: str,
                 tool_name: str, arguments: dict) -> None:
        """Call before each tool execution."""
        self._fire("PreToolUse", {
            "session_id": session_id,
            "tool_use_id": tool_use_id,
            "tool_name": tool_name,
            "tool_input": arguments,
        })

    def post_tool(self, session_id: str, tool_use_id: str,
                  tool_name: str, result: str,
                  elapsed_s: float, error: str | None = None) -> None:
        """Call after each tool execution completes."""
        event = "PostToolUse" if not error else "PostToolUseFailure"
        self._fire(event, {
            "session_id": session_id,
            "tool_use_id": tool_use_id,
            "tool_name": tool_name,
            "tool_response": result[:4000],
            "tool_input": {"elapsed_s": round(elapsed_s, 3)},
        })

    # ── Alert responder ────────────────────────────────────────

    def alert_triage_start(self, session_id: str,
                           alert_type: str, service: str) -> None:
        """Call at the start of AlertResponder._triage_alert()."""
        self._fire("SessionStart", {
            "session_id": session_id,
            "tool_name": "alert_triage",
            "cwd": f"/ray-agent/alerts/{service}",
            "tool_input": {
                "alert_type": alert_type,
                "service": service,
            },
        })

    async def close(self) -> None:
        """Shut down the HTTP client. Call on application shutdown."""
        if self._client:
            await self._client.aclose()


def _last_content(messages: list[dict], role: str) -> str:
    """Extract the last message content for a given role."""
    for m in reversed(messages):
        if m.get("role") == role:
            return (m.get("content") or "")[:300]
    return ""


def _extract_tool_names(tool_calls: list | None) -> list[str]:
    """Extract tool names from either native or prompt-mode format.

    Native mode: tc["function"]["name"]
    Prompt mode: tc["name"]
    """
    if not tool_calls:
        return []
    names = []
    for tc in tool_calls:
        name = (
            tc.get("function", {}).get("name")
            or tc.get("name")
            or "?"
        )
        names.append(name)
    return names
