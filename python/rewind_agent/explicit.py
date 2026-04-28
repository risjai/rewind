"""
Explicit Recording API client — wire-format-agnostic recording, replay, and fork.

Works with any LLM provider (OpenAI, Anthropic, Salesforce LLM Gateway, Ollama, etc.)
without monkey-patching. The agent explicitly calls record/replay functions.

Thread-safe via contextvars. Error-resilient — recording failures never crash the agent.

Usage:
    import rewind_agent
    from rewind_agent.explicit import ExplicitClient

    client = ExplicitClient("http://127.0.0.1:4800")

    with client.session("my-agent"):
        # Before each LLM call:
        cached = client.get_replayed_response(request)
        if cached is not None:
            response = cached
        else:
            response = call_my_llm(request)
            client.record_llm_call(request, response, model="gpt-4o", duration_ms=500)

    # Or use the tool decorator:
    @client.cached_tool("get_pods")
    def get_pods(cluster: str) -> str:
        return k8s_api.list_pods(cluster)
"""

import asyncio
import contextvars
import functools
import inspect
import json
import logging
import time
import urllib.error
import urllib.request
from contextlib import asynccontextmanager, contextmanager
from typing import Any, Callable

logger = logging.getLogger("rewind.explicit")

_session_id: contextvars.ContextVar[str | None] = contextvars.ContextVar(
    "rewind_session_id", default=None
)
_timeline_id: contextvars.ContextVar[str | None] = contextvars.ContextVar(
    "rewind_timeline_id", default=None
)
_replay_context_id: contextvars.ContextVar[str | None] = contextvars.ContextVar(
    "rewind_replay_context_id", default=None
)

_TIMEOUT = 2.0


_SESSION_CACHE_TTL = 7200  # 2 hours


class RewindReplayDivergenceError(RuntimeError):
    """Phase 1 (Santa #4): strict-match replay lookup returned HTTP 409.

    Raised by :meth:`ExplicitClient.get_replayed_response` /
    ``_async`` when the server detects that the agent's request body
    hash diverges from the recording's stored ``request_hash`` at the
    next ordinal in the replay timeline AND the replay context was
    created with ``strict_match=True``.

    The replay cursor stays put on 409 (Phase 0 contract — see Santa
    review #2 in PR #148) so the caller can fix the request body and
    retry. Without this exception type the divergence would be
    swallowed by ``_post``'s generic ``except Exception`` and the
    adapters would silently treat it as a cache miss, defeating the
    whole purpose of strict mode.

    Attributes
    ----------
    message:
        Server-supplied error string. Includes truncated stored vs
        incoming hashes for visual diffing (Phase 0 server format).
    target_step:
        The recording step number where divergence occurred.
    """

    def __init__(
        self,
        message: str,
        *,
        target_step: int | None = None,
        stored_hash: str | None = None,
        incoming_hash: str | None = None,
    ) -> None:
        super().__init__(message)
        self.target_step = target_step
        self.stored_hash = stored_hash
        self.incoming_hash = incoming_hash


class ExplicitClient:
    """Wire-format-agnostic recording client for the Rewind explicit API."""

    def __init__(self, base_url: str = "http://127.0.0.1:4800"):
        self.base_url = base_url.rstrip("/")
        self._enabled = True
        self._session_cache: dict[str, tuple[str, str, float]] = {}

    def _post(self, path: str, body: dict) -> dict | None:
        if not self._enabled:
            return None
        url = f"{self.base_url}/api{path}"
        data = json.dumps(body).encode("utf-8")
        req = urllib.request.Request(
            url, data=data, headers={"Content-Type": "application/json"}, method="POST"
        )
        try:
            with urllib.request.urlopen(req, timeout=_TIMEOUT) as resp:
                return json.loads(resp.read())
        except Exception as e:
            logger.debug("Rewind POST %s failed: %s", path, e)
            return None

    def _delete(self, path: str) -> dict | None:
        if not self._enabled:
            return None
        url = f"{self.base_url}/api{path}"
        req = urllib.request.Request(url, method="DELETE")
        try:
            with urllib.request.urlopen(req, timeout=_TIMEOUT) as resp:
                return json.loads(resp.read())
        except Exception as e:
            logger.debug("Rewind DELETE %s failed: %s", path, e)
            return None

    def _get(self, path: str) -> dict | list | None:
        if not self._enabled:
            return None
        url = f"{self.base_url}/api{path}"
        req = urllib.request.Request(url, method="GET")
        try:
            with urllib.request.urlopen(req, timeout=_TIMEOUT) as resp:
                return json.loads(resp.read())
        except Exception as e:
            logger.debug("Rewind GET %s failed: %s", path, e)
            return None

    # ── Session lifecycle ──────────────────────────────────────

    @contextmanager
    def session(self, name: str, *, thread_id: str | None = None,
                metadata: dict | None = None):
        """Context manager for recording sessions. Thread/async-safe via contextvars."""
        result = self._post("/sessions/start", {
            "name": name,
            **({"thread_id": thread_id} if thread_id else {}),
            **({"metadata": metadata} if metadata else {}),
        })
        if result is None:
            yield
            return

        sid = result["session_id"]
        tid = result["root_timeline_id"]
        tok_sid = _session_id.set(sid)
        tok_tid = _timeline_id.set(tid)
        try:
            yield
        except Exception:
            self._post(f"/sessions/{sid}/end", {"status": "errored"})
            raise
        else:
            self._post(f"/sessions/{sid}/end", {"status": "completed"})
        finally:
            _session_id.reset(tok_sid)
            _timeline_id.reset(tok_tid)

    @asynccontextmanager
    async def session_async(self, name: str, *, thread_id: str | None = None,
                            metadata: dict | None = None):
        """Async context manager for recording sessions."""
        loop = asyncio.get_running_loop()
        result = await loop.run_in_executor(
            None, lambda: self._post("/sessions/start", {
                "name": name,
                **({"thread_id": thread_id} if thread_id else {}),
                **({"metadata": metadata} if metadata else {}),
            })
        )
        if result is None:
            yield
            return

        sid = result["session_id"]
        tid = result["root_timeline_id"]
        tok_sid = _session_id.set(sid)
        tok_tid = _timeline_id.set(tid)
        try:
            yield
        except Exception:
            await loop.run_in_executor(
                None, lambda: self._post(f"/sessions/{sid}/end", {"status": "errored"})
            )
            raise
        else:
            await loop.run_in_executor(
                None, lambda: self._post(f"/sessions/{sid}/end", {"status": "completed"})
            )
        finally:
            _session_id.reset(tok_sid)
            _timeline_id.reset(tok_tid)

    # ── Long-lived sessions (one per conversation) ─────────────

    def ensure_session(self, conversation_id: str, *, name: str | None = None,
                       metadata: dict | None = None) -> None:
        """Create or reuse a Rewind session for this conversation.

        First call for a conversation_id creates a new session and caches the mapping.
        Subsequent calls reuse the cached session. Sets contextvars internally.
        Cache entries evict after 2 hours of inactivity.

        **Multi-replica safety (v0.15.1+):** the call passes
        ``conversation_id`` as the server-side ``client_session_key``
        so two ExplicitClient instances in different processes (Ray
        Serve replicas, autoscaling worker pools) that both miss
        their local cache for the same conversation collapse to a
        single Rewind session on the server. The server returns
        ``200 OK`` for the second-and-subsequent callers (instead of
        ``201 CREATED``) and hands back the existing session/root
        timeline ids — same shape as a fresh creation, so callers
        don't need to branch on the status code.
        """
        now = time.monotonic()
        self._evict_stale_sessions(now)

        if conversation_id in self._session_cache:
            sid, tid, _ = self._session_cache[conversation_id]
            self._session_cache[conversation_id] = (sid, tid, now)
            _session_id.set(sid)
            _timeline_id.set(tid)
            return

        session_name = name or f"session-{conversation_id[:8]}"
        result = self._post("/sessions/start", {
            "name": session_name,
            "client_session_key": conversation_id,
            **({"metadata": metadata} if metadata else {}),
        })
        if result is None:
            return

        sid = result["session_id"]
        tid = result["root_timeline_id"]
        self._session_cache[conversation_id] = (sid, tid, now)
        _session_id.set(sid)
        _timeline_id.set(tid)

    def clear_session(self) -> None:
        """Reset session contextvars. Call after processing a request if needed."""
        _session_id.set(None)
        _timeline_id.set(None)

    def _evict_stale_sessions(self, now: float) -> None:
        """Remove cache entries older than _SESSION_CACHE_TTL."""
        stale = [k for k, (_, _, ts) in self._session_cache.items()
                 if now - ts > _SESSION_CACHE_TTL]
        for k in stale:
            del self._session_cache[k]

    # ── LLM call recording ────────────────────────────────────

    def record_llm_call(
        self, request: Any, response: Any, *, model: str, duration_ms: int,
        tokens_in: int = 0, tokens_out: int = 0, client_step_id: str | None = None,
    ) -> int | None:
        """Record an LLM call. Returns the step number, or None if recording fails."""
        sid = _session_id.get()
        if sid is None:
            return None
        body: dict[str, Any] = {
            "request_body": request,
            "response_body": response,
            "model": model,
            "duration_ms": duration_ms,
        }
        if tokens_in:
            body["tokens_in"] = tokens_in
        if tokens_out:
            body["tokens_out"] = tokens_out
        if client_step_id:
            body["client_step_id"] = client_step_id
        tid = _timeline_id.get()
        if tid:
            body["timeline_id"] = tid
        result = self._post(f"/sessions/{sid}/llm-calls", body)
        return result["step_number"] if result else None

    async def record_llm_call_async(
        self, request: Any, response: Any, *, model: str, duration_ms: int,
        tokens_in: int = 0, tokens_out: int = 0, client_step_id: str | None = None,
    ) -> int | None:
        """Async variant — runs HTTP in a thread executor to avoid blocking the event loop."""
        sid = _session_id.get()
        if sid is None:
            return None
        body: dict[str, Any] = {
            "request_body": request,
            "response_body": response,
            "model": model,
            "duration_ms": duration_ms,
        }
        if tokens_in:
            body["tokens_in"] = tokens_in
        if tokens_out:
            body["tokens_out"] = tokens_out
        if client_step_id:
            body["client_step_id"] = client_step_id
        tid = _timeline_id.get()
        if tid:
            body["timeline_id"] = tid
        loop = asyncio.get_running_loop()
        result = await loop.run_in_executor(
            None, lambda: self._post(f"/sessions/{sid}/llm-calls", body)
        )
        return result["step_number"] if result else None

    # ── Tool call recording ───────────────────────────────────

    def record_tool_call(
        self, tool_name: str, request: Any, response: Any, *,
        duration_ms: int, error: str | None = None, client_step_id: str | None = None,
    ) -> int | None:
        """Record a tool call. Returns the step number, or None if recording fails."""
        sid = _session_id.get()
        if sid is None:
            return None
        body: dict[str, Any] = {
            "tool_name": tool_name,
            "request_body": request,
            "response_body": response,
            "duration_ms": duration_ms,
        }
        if error:
            body["error"] = error
        if client_step_id:
            body["client_step_id"] = client_step_id
        tid = _timeline_id.get()
        if tid:
            body["timeline_id"] = tid
        result = self._post(f"/sessions/{sid}/tool-calls", body)
        return result["step_number"] if result else None

    async def record_tool_call_async(
        self, tool_name: str, request: Any, response: Any, *,
        duration_ms: int, error: str | None = None, client_step_id: str | None = None,
    ) -> int | None:
        """Async variant."""
        sid = _session_id.get()
        if sid is None:
            return None
        body: dict[str, Any] = {
            "tool_name": tool_name,
            "request_body": request,
            "response_body": response,
            "duration_ms": duration_ms,
        }
        if error:
            body["error"] = error
        if client_step_id:
            body["client_step_id"] = client_step_id
        tid = _timeline_id.get()
        if tid:
            body["timeline_id"] = tid
        loop = asyncio.get_running_loop()
        result = await loop.run_in_executor(
            None, lambda: self._post(f"/sessions/{sid}/tool-calls", body)
        )
        return result["step_number"] if result else None

    # ── Replay ────────────────────────────────────────────────

    def _post_replay_lookup(
        self, sid: str, body: dict[str, Any]
    ) -> dict | None:
        """Post to ``/sessions/{sid}/llm-calls/replay-lookup`` with
        explicit HTTP 409 handling.

        Phase 1 (Santa #4): the generic ``_post`` swallows all
        exceptions to None — fine for record paths where errors
        should be best-effort, but wrong for strict-match replay
        where 409 is a meaningful signal that the caller diverged
        from the recording. This method re-raises 409 as
        :class:`RewindReplayDivergenceError` so adapters can surface
        it to user code (which is the whole point of opting into
        strict mode).

        Other errors (timeouts, server crashes, network glitches)
        still degrade to None (cache miss) so a transient Rewind
        outage doesn't break the agent's normal flow.
        """
        if not self._enabled:
            return None
        url = f"{self.base_url}/api/sessions/{sid}/llm-calls/replay-lookup"
        data = json.dumps(body).encode("utf-8")
        req = urllib.request.Request(
            url,
            data=data,
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        try:
            with urllib.request.urlopen(req, timeout=_TIMEOUT) as resp:
                return json.loads(resp.read())
        except urllib.error.HTTPError as e:
            if e.code == 409:
                # Strict-match divergence — read the body for the
                # server's diagnostic and convert to a typed exception.
                try:
                    error_body = e.read().decode("utf-8", errors="replace")
                except Exception:
                    error_body = f"HTTP 409 Conflict from {url}"
                raise RewindReplayDivergenceError(error_body) from e
            # Non-409 HTTP error — degrade to cache miss.
            logger.debug(
                "Rewind replay-lookup HTTP %d: %s", e.code, e.reason
            )
            return None
        except Exception as e:
            logger.debug("Rewind replay-lookup failed: %s", e)
            return None

    def get_replayed_response(self, request: Any = None) -> dict | None:
        """Check if a cached LLM response exists for replay. Returns response_body or None.

        Raises :class:`RewindReplayDivergenceError` when the server
        reports HTTP 409 strict-match divergence. Adapters propagate
        the exception as a library-native error so user code can
        choose to retry, log, or fail loudly.
        """
        sid = _session_id.get()
        ctx_id = _replay_context_id.get()
        if sid is None or ctx_id is None:
            return None
        body: dict[str, Any] = {"replay_context_id": ctx_id}
        if request is not None:
            body["request_body"] = request
        result = self._post_replay_lookup(sid, body)
        if result and result.get("hit"):
            return result.get("response_body")
        return None

    async def get_replayed_response_async(self, request: Any = None) -> dict | None:
        """Async variant. Same divergence semantics as the sync
        method — :class:`RewindReplayDivergenceError` propagates."""
        sid = _session_id.get()
        ctx_id = _replay_context_id.get()
        if sid is None or ctx_id is None:
            return None
        body: dict[str, Any] = {"replay_context_id": ctx_id}
        if request is not None:
            body["request_body"] = request
        loop = asyncio.get_running_loop()
        # run_in_executor doesn't propagate exceptions across the
        # await — they're embedded in the future and re-raised on await.
        result = await loop.run_in_executor(
            None, lambda: self._post_replay_lookup(sid, body)
        )
        if result and result.get("hit"):
            return result.get("response_body")
        return None

    def get_replayed_tool_response(
        self, tool_name: str, request: Any = None
    ) -> dict | None:
        """Check if a cached tool response exists for replay."""
        sid = _session_id.get()
        ctx_id = _replay_context_id.get()
        if sid is None or ctx_id is None:
            return None
        body: dict[str, Any] = {"replay_context_id": ctx_id}
        if tool_name:
            body["tool_name"] = tool_name
        if request is not None:
            body["request_body"] = request
        result = self._post(f"/sessions/{sid}/tool-calls/replay-lookup", body)
        if result and result.get("hit"):
            return result.get("response_body")
        return None

    async def get_replayed_tool_response_async(
        self, tool_name: str, request: Any = None
    ) -> dict | None:
        """Async variant."""
        sid = _session_id.get()
        ctx_id = _replay_context_id.get()
        if sid is None or ctx_id is None:
            return None
        body: dict[str, Any] = {"replay_context_id": ctx_id}
        if tool_name:
            body["tool_name"] = tool_name
        if request is not None:
            body["request_body"] = request
        loop = asyncio.get_running_loop()
        result = await loop.run_in_executor(
            None, lambda: self._post(f"/sessions/{sid}/tool-calls/replay-lookup", body)
        )
        if result and result.get("hit"):
            return result.get("response_body")
        return None

    # ── Replay context management ─────────────────────────────

    def start_replay(
        self, session_id: str, *, from_step: int = 0,
        timeline_id: str | None = None,
        strict_match: bool = False,
    ) -> str | None:
        """Create a replay context. Returns the replay_context_id.

        Parameters
        ----------
        session_id:
            Session whose recorded steps will be served from cache during
            replay.
        from_step:
            Step number where the replay forks. Cache lookups walk
            ``[from_step + 1, from_step + 2, ...]`` against the replayed
            timeline.
        timeline_id:
            Fork timeline to record new (live) steps into. Defaults to the
            session's root timeline (creates an in-place replay against
            ``main``).
        strict_match:
            **Step 0.1 cache content validation.** When ``True``, a
            divergence between the agent's request body and the
            recording's stored ``request_hash`` returns HTTP 409 instead
            of the cached response. The cursor stays put on 409 so the
            caller can fix and retry without consuming an ordinal slot.
            Default ``False`` (warn-on-divergence): cached step is
            returned with an ``X-Rewind-Cache-Divergent: true`` header
            and ``divergent: true`` in the JSON body.
        """
        if timeline_id is None:
            sess = self._get(f"/sessions/{session_id}")
            if sess and "timelines" in sess:
                root = next((t for t in sess["timelines"] if t.get("parent_timeline_id") is None), None)
                timeline_id = root["id"] if root else None
            if timeline_id is None:
                timelines = self._get(f"/sessions/{session_id}/timelines")
                if timelines:
                    root = next((t for t in timelines if t.get("parent_timeline_id") is None), None)
                    timeline_id = root["id"] if root else None
        if timeline_id is None:
            logger.warning("Could not resolve timeline for session %s", session_id)
            return None
        body: dict[str, Any] = {
            "session_id": session_id,
            "from_step": from_step,
            "fork_timeline_id": timeline_id,
        }
        if strict_match:
            # Only emit the field when set so older servers (pre-v0.13)
            # that don't know strict_match receive a request shape they
            # already accept (the field is unrecognized → ignored). Once
            # v0.13 is the floor we can pass it unconditionally.
            body["strict_match"] = True
        result = self._post("/replay-contexts", body)
        if result:
            ctx_id = result["replay_context_id"]
            _replay_context_id.set(ctx_id)
            _session_id.set(session_id)
            return ctx_id
        return None

    def stop_replay(self) -> None:
        """Release the current replay context."""
        ctx_id = _replay_context_id.get()
        if ctx_id:
            self._delete(f"/replay-contexts/{ctx_id}")
            _replay_context_id.set(None)

    def attach_replay_context(
        self,
        session_id: str,
        replay_context_id: str,
        timeline_id: str | None = None,
    ) -> None:
        """Attach to a *pre-existing* replay context.

        **Phase 3 commit 9 (resolves review HIGH #4 from the plan):**
        :meth:`start_replay` *creates* a fresh replay context; runners
        receive an *existing* one in their dispatch payload and must
        attach to it without creating a duplicate. This method sets
        the contextvars so subsequent recorder/intercept lookups
        target the supplied context.

        **Review #154 F2:** the dispatch payload now carries
        ``replay_context_timeline_id``. Pass it as ``timeline_id`` so
        ``_timeline_id`` is set; otherwise the first cache *miss*
        after the fork records its new live step into the root
        timeline instead of the fork. The dispatcher always supplies
        this field; callers using attach without going through a
        dispatch (e.g. tests) can omit it and accept that live misses
        won't have a defined recording target.

        Use this in runner code receiving a dispatch webhook:

        .. code-block:: python

            client = ExplicitClient(base_url=payload["base_url"])
            client.attach_replay_context(
                session_id=payload["session_id"],
                replay_context_id=payload["replay_context_id"],
                timeline_id=payload["replay_context_timeline_id"],
            )

        No server round-trip; sets contextvars only.
        """
        _session_id.set(session_id)
        _replay_context_id.set(replay_context_id)
        if timeline_id is not None:
            _timeline_id.set(timeline_id)

    def replay_from_iteration(
        self, session_id: str, iteration: int,
        *, timeline_id: str | None = None,
        strict_match: bool = False,
    ) -> str | None:
        """Start replay from the Nth LLM call (1-indexed).

        Iteration N = the Nth step where step_type == "llm_call".

        ``strict_match`` is forwarded to :meth:`start_replay` — see that
        method's docstring for cache validation semantics.
        """
        tid = timeline_id
        if tid is None:
            timelines = self._get(f"/sessions/{session_id}/timelines")
            if timelines:
                root = next((t for t in timelines if t.get("parent_timeline_id") is None), None)
                tid = root["id"] if root else None
        if tid is None:
            return None

        steps = self._get(f"/sessions/{session_id}/steps?timeline={tid}")
        if not steps:
            return None

        llm_steps = [s for s in steps if s.get("step_type") == "llm_call"]
        if iteration < 1 or iteration > len(llm_steps):
            logger.warning("Iteration %d out of range (1-%d)", iteration, len(llm_steps))
            return None

        from_step = llm_steps[iteration - 1]["step_number"] - 1
        return self.start_replay(
            session_id,
            from_step=from_step,
            timeline_id=tid,
            strict_match=strict_match,
        )

    # ── Fork ──────────────────────────────────────────────────

    def fork(self, session_id: str, *, at_step: int, label: str,
             timeline_id: str | None = None) -> str | None:
        """Fork a session at a specific step. Returns the fork_timeline_id."""
        body: dict[str, Any] = {"at_step": at_step, "label": label}
        if timeline_id:
            body["timeline_id"] = timeline_id
        result = self._post(f"/sessions/{session_id}/fork", body)
        return result["fork_timeline_id"] if result else None

    # ── Cached tool decorator ─────────────────────────────────

    def cached_tool(self, name: str | None = None):
        """Decorator that caches tool results during replay.

        During recording: executes the function and records input/output.
        During replay: returns cached output if available, otherwise executes live.

        Usage:
            @client.cached_tool("get_pods")
            def get_pods(cluster: str) -> str:
                return k8s_api.list_pods(cluster)

            @client.cached_tool("search")
            async def search(query: str) -> str:
                return await api.search(query)
        """
        def decorator(func: Callable) -> Callable:
            tool_name = name or func.__name__

            if inspect.iscoroutinefunction(func):
                @functools.wraps(func)
                async def async_wrapper(*args: Any, **kwargs: Any) -> Any:
                    request_data = _serialize_args(args, kwargs)
                    cached = await self.get_replayed_tool_response_async(
                        tool_name, request_data
                    )
                    if cached is not None:
                        return cached

                    start = time.monotonic()
                    error_msg = None
                    try:
                        result = await func(*args, **kwargs)
                    except Exception as e:
                        error_msg = str(e)
                        raise
                    finally:
                        elapsed_ms = int((time.monotonic() - start) * 1000)
                        response_data = (
                            {"error": error_msg} if error_msg
                            else _serialize_result(result)
                        )
                        await self.record_tool_call_async(
                            tool_name, request_data, response_data,
                            duration_ms=elapsed_ms,
                            error=error_msg,
                        )
                    return result
                return async_wrapper
            else:
                @functools.wraps(func)
                def sync_wrapper(*args: Any, **kwargs: Any) -> Any:
                    request_data = _serialize_args(args, kwargs)
                    cached = self.get_replayed_tool_response(tool_name, request_data)
                    if cached is not None:
                        return cached

                    start = time.monotonic()
                    error_msg = None
                    try:
                        result = func(*args, **kwargs)
                    except Exception as e:
                        error_msg = str(e)
                        raise
                    finally:
                        elapsed_ms = int((time.monotonic() - start) * 1000)
                        response_data = (
                            {"error": error_msg} if error_msg
                            else _serialize_result(result)
                        )
                        self.record_tool_call(
                            tool_name, request_data, response_data,
                            duration_ms=elapsed_ms,
                            error=error_msg,
                        )
                    return result
                return sync_wrapper

        return decorator


def _serialize_args(args: tuple, kwargs: dict) -> dict:
    """Convert function args to a JSON-serializable dict."""
    result: dict[str, Any] = {}
    if args:
        result["args"] = [_safe_json(a) for a in args]
    if kwargs:
        result["kwargs"] = {k: _safe_json(v) for k, v in kwargs.items()}
    return result


def _serialize_result(result: Any) -> Any:
    """Convert a function result to a JSON-serializable value."""
    return _safe_json(result)


def _safe_json(val: Any) -> Any:
    """Make a value JSON-serializable."""
    if val is None or isinstance(val, (bool, int, float, str)):
        return val
    if isinstance(val, (list, tuple)):
        return [_safe_json(v) for v in val]
    if isinstance(val, dict):
        return {str(k): _safe_json(v) for k, v in val.items()}
    try:
        json.dumps(val)
        return val
    except (TypeError, ValueError):
        return str(val)
