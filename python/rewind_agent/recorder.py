"""
Direct LLM call recorder — monkey-patches OpenAI/Anthropic SDK clients
to capture every call without a proxy.

Thread-safe. Error-resilient — recording failures never break LLM calls.
"""

import functools
import json
import logging
import threading
import time
import types

from .store import Store

logger = logging.getLogger("rewind")

# ── Response parsing helpers ──────────────────────────────────

def _serialize_response(response) -> dict:
    """Convert SDK response object to a JSON-serializable dict."""
    try:
        if hasattr(response, "model_dump"):
            return response.model_dump()
        return {"raw": str(response)}
    except Exception:
        return {"raw": str(response)}


_SENSITIVE_KEYS = frozenset({
    "api_key", "api_secret", "authorization", "x-api-key",
    "secret", "password", "token", "access_token", "refresh_token",
})


def _serialize_request(kwargs: dict) -> dict:
    """Convert create() kwargs to a JSON-serializable dict, stripping sensitive keys."""
    result = {}
    for k, v in kwargs.items():
        if k.lower() in _SENSITIVE_KEYS:
            continue
        try:
            json.dumps(v, default=str)
            result[k] = v
        except (TypeError, ValueError):
            result[k] = str(v)
    return result


def _extract_openai_usage(response_dict: dict) -> tuple:
    """Extract (tokens_in, tokens_out) from OpenAI response."""
    usage = response_dict.get("usage") or {}
    return (
        usage.get("prompt_tokens", 0) or 0,
        usage.get("completion_tokens", 0) or 0,
    )


def _extract_anthropic_usage(response_dict: dict) -> tuple:
    """Extract (tokens_in, tokens_out) from Anthropic response."""
    usage = response_dict.get("usage") or {}
    return (
        usage.get("input_tokens", 0) or 0,
        usage.get("output_tokens", 0) or 0,
    )


def _has_tool_calls_openai(response_dict: dict) -> bool:
    choices = response_dict.get("choices") or []
    if choices:
        msg = choices[0].get("message") or {}
        return bool(msg.get("tool_calls"))
    return False


def _has_tool_calls_anthropic(response_dict: dict) -> bool:
    content = response_dict.get("content") or []
    return any(block.get("type") == "tool_use" for block in content if isinstance(block, dict))


# ── Span context bridge ──────────────────────────────────────

_current_span_id_ref = None


def _get_current_span_id():
    """Resolve the enclosing span_id from hooks.py ContextVar (cached import)."""
    global _current_span_id_ref
    if _current_span_id_ref is None:
        from .hooks import _current_span_id
        _current_span_id_ref = _current_span_id
    return _current_span_id_ref.get()


# ── Streaming wrappers ────────────────────────────────────────

class _OpenAIStreamWrapper:
    """Wraps an OpenAI sync Stream to accumulate chunks and record on completion."""

    def __init__(self, stream, recorder, model, request_data, start_time):
        self._stream = stream
        self._recorder = recorder
        self._model = model
        self._request_data = request_data
        self._start_time = start_time
        self._content_parts = []
        self._tool_calls = []
        self._usage = {}
        self._finish_reason = None

    def __iter__(self):
        return self

    def __next__(self):
        try:
            chunk = next(self._stream)
            self._accumulate(chunk)
            return chunk
        except StopIteration:
            self._finalize()
            raise

    def __enter__(self):
        return self

    def __exit__(self, *args):
        if hasattr(self._stream, "__exit__"):
            self._stream.__exit__(*args)
        self._finalize()

    def _accumulate(self, chunk):
        try:
            chunk_dict = chunk.model_dump() if hasattr(chunk, "model_dump") else {}
            choices = chunk_dict.get("choices") or []
            if choices:
                delta = choices[0].get("delta") or {}
                content = delta.get("content")
                if content:
                    self._content_parts.append(content)
                tc = delta.get("tool_calls")
                if tc:
                    self._tool_calls.extend(tc)
                fr = choices[0].get("finish_reason")
                if fr:
                    self._finish_reason = fr
            usage = chunk_dict.get("usage")
            if usage:
                self._usage = usage
        except Exception:
            pass

    def _finalize(self):
        if hasattr(self, "_finalized"):
            return
        self._finalized = True
        duration_ms = int((time.perf_counter() - self._start_time) * 1000)
        content = "".join(self._content_parts)
        synthetic = {
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": content or None,
                },
                "finish_reason": self._finish_reason or "stop",
            }],
            "usage": self._usage,
            "model": self._model,
        }
        if self._tool_calls:
            synthetic["choices"][0]["message"]["tool_calls"] = self._tool_calls
        self._recorder._record_call(
            self._model, self._request_data, synthetic, duration_ms, provider="openai",
        )


class _AsyncOpenAIStreamWrapper:
    """Wraps an OpenAI async Stream."""

    def __init__(self, stream, recorder, model, request_data, start_time):
        self._stream = stream
        self._recorder = recorder
        self._model = model
        self._request_data = request_data
        self._start_time = start_time
        self._content_parts = []
        self._tool_calls = []
        self._usage = {}
        self._finish_reason = None

    def __aiter__(self):
        return self

    async def __anext__(self):
        try:
            chunk = await self._stream.__anext__()
            self._accumulate(chunk)
            return chunk
        except StopAsyncIteration:
            self._finalize()
            raise

    async def __aenter__(self):
        return self

    async def __aexit__(self, *args):
        if hasattr(self._stream, "__aexit__"):
            await self._stream.__aexit__(*args)
        self._finalize()

    def _accumulate(self, chunk):
        _OpenAIStreamWrapper._accumulate(self, chunk)

    def _finalize(self):
        _OpenAIStreamWrapper._finalize(self)


class _AnthropicStreamWrapper:
    """Wraps an Anthropic sync MessageStream."""

    def __init__(self, stream, recorder, model, request_data, start_time):
        self._stream = stream
        self._recorder = recorder
        self._model = model
        self._request_data = request_data
        self._start_time = start_time
        self._text_parts = []
        self._content_blocks = []
        self._usage = {}

    def __iter__(self):
        return self

    def __next__(self):
        try:
            event = next(self._stream)
            self._accumulate(event)
            return event
        except StopIteration:
            self._finalize()
            raise

    def __enter__(self):
        return self

    def __exit__(self, *args):
        if hasattr(self._stream, "__exit__"):
            self._stream.__exit__(*args)
        self._finalize()

    def _accumulate(self, event):
        try:
            ev = event.model_dump() if hasattr(event, "model_dump") else {}
            ev_type = ev.get("type", "")
            if ev_type == "content_block_delta":
                delta = ev.get("delta") or {}
                if delta.get("type") == "text_delta":
                    self._text_parts.append(delta.get("text", ""))
            elif ev_type == "content_block_start":
                block = ev.get("content_block") or {}
                if block.get("type") == "tool_use":
                    self._content_blocks.append(block)
            elif ev_type == "message_start":
                msg = ev.get("message") or {}
                usage = msg.get("usage") or {}
                self._usage["input_tokens"] = usage.get("input_tokens", 0)
            elif ev_type == "message_delta":
                delta_usage = ev.get("usage") or {}
                self._usage["output_tokens"] = delta_usage.get("output_tokens", 0)
        except Exception:
            pass

    def _finalize(self):
        if hasattr(self, "_finalized"):
            return
        self._finalized = True
        duration_ms = int((time.perf_counter() - self._start_time) * 1000)
        text = "".join(self._text_parts)
        content = [{"type": "text", "text": text}] if text else []
        content.extend(self._content_blocks)
        synthetic = {
            "content": content,
            "usage": self._usage,
            "model": self._model,
            "role": "assistant",
            "stop_reason": "end_turn",
        }
        self._recorder._record_call(
            self._model, self._request_data, synthetic, duration_ms, provider="anthropic",
        )


class _AsyncAnthropicStreamWrapper:
    """Wraps an Anthropic async MessageStream."""

    def __init__(self, stream, recorder, model, request_data, start_time):
        self._stream = stream
        self._recorder = recorder
        self._model = model
        self._request_data = request_data
        self._start_time = start_time
        self._text_parts = []
        self._content_blocks = []
        self._usage = {}

    def __aiter__(self):
        return self

    async def __anext__(self):
        try:
            event = await self._stream.__anext__()
            self._accumulate(event)
            return event
        except StopAsyncIteration:
            self._finalize()
            raise

    async def __aenter__(self):
        return self

    async def __aexit__(self, *args):
        if hasattr(self._stream, "__aexit__"):
            await self._stream.__aexit__(*args)
        self._finalize()

    def _accumulate(self, event):
        _AnthropicStreamWrapper._accumulate(self, event)

    def _finalize(self):
        _AnthropicStreamWrapper._finalize(self)


class _AnthropicStreamManagerWrapper:
    """Wraps a MessageStreamManager returned by client.messages.stream().
    Records the LLM call when the context manager exits."""

    def __init__(self, manager, recorder, model, request_data, start_time):
        self._manager = manager
        self._recorder = recorder
        self._model = model
        self._request_data = request_data
        self._start_time = start_time
        self._stream = None

    def __enter__(self):
        self._stream = self._manager.__enter__()
        return self._stream

    def __exit__(self, exc_type, exc_val, exc_tb):
        result = self._manager.__exit__(exc_type, exc_val, exc_tb)
        duration_ms = int((time.perf_counter() - self._start_time) * 1000)

        if exc_val:
            self._recorder._record_call(
                self._model, self._request_data, None,
                duration_ms, error=str(exc_val), provider="anthropic",
            )
        elif self._stream:
            try:
                final = getattr(self._stream, "get_final_message", None)
                if final:
                    msg = final()
                    resp_dict = msg.model_dump() if hasattr(msg, "model_dump") else {}
                else:
                    resp_dict = {}
            except Exception:
                resp_dict = {}
            self._recorder._record_call(
                self._model, self._request_data, resp_dict,
                duration_ms, provider="anthropic",
            )
        return result


class _AsyncAnthropicStreamManagerWrapper:
    """Wraps an AsyncMessageStreamManager returned by client.messages.stream().
    Records the LLM call when the async context manager exits."""

    def __init__(self, manager, recorder, model, request_data, start_time):
        self._manager = manager
        self._recorder = recorder
        self._model = model
        self._request_data = request_data
        self._start_time = start_time
        self._stream = None

    async def __aenter__(self):
        self._stream = await self._manager.__aenter__()
        return self._stream

    async def __aexit__(self, exc_type, exc_val, exc_tb):
        result = await self._manager.__aexit__(exc_type, exc_val, exc_tb)
        duration_ms = int((time.perf_counter() - self._start_time) * 1000)

        if exc_val:
            self._recorder._record_call(
                self._model, self._request_data, None,
                duration_ms, error=str(exc_val), provider="anthropic",
            )
        elif self._stream:
            try:
                final = getattr(self._stream, "get_final_message", None)
                if final:
                    msg = final()
                    resp_dict = msg.model_dump() if hasattr(msg, "model_dump") else {}
                else:
                    resp_dict = {}
            except Exception:
                resp_dict = {}
            self._recorder._record_call(
                self._model, self._request_data, resp_dict,
                duration_ms, provider="anthropic",
            )
        return result


# ── Recorder ──────────────────────────────────────────────────

class Recorder:
    """
    Monkey-patches OpenAI/Anthropic SDK clients to record every LLM call
    directly to the Rewind store. Thread-safe.

    In replay mode (replay_steps + fork_at_step set), steps <= fork_at_step
    return cached responses from the parent timeline without calling the LLM.
    """

    def __init__(self, store: Store, session_id: str, timeline_id: str,
                 replay_steps: list = None, fork_at_step: int = None):
        self._store = store
        self._session_id = session_id
        self._timeline_id = timeline_id
        self._step_counter = 0
        self._lock = threading.Lock()
        self._originals = {}
        self._replay_steps = replay_steps  # list of parent step dicts
        self._fork_at_step = fork_at_step  # step cutoff for cached replay

    def patch_all(self):
        """Patch all supported SDK clients."""
        self._patch_openai_sync()
        self._patch_openai_async()
        self._patch_anthropic_sync()
        self._patch_anthropic_async()
        self._patch_anthropic_stream_sync()
        self._patch_anthropic_stream_async()

    def next_step_number(self) -> int:
        """Atomically increment and return the next step number."""
        with self._lock:
            self._step_counter += 1
            return self._step_counter

    def unpatch_all(self):
        """Restore all original methods."""
        for key, (cls, method_name, original) in self._originals.items():
            setattr(cls, method_name, original)
        self._originals.clear()

    # ── Replay helpers ─────────────────────────────────────────

    def _try_replay_cached(self, provider: str):
        """
        Check if the next step should be served from cache (fork-and-execute mode).
        Returns the cached response dict if within replay range, or None for live calls.

        Does NOT create step records — the forked timeline inherits parent steps
        via get_full_timeline_steps(). Only the step counter is advanced so that
        live calls after the fork point get correct step numbers.
        """
        if self._replay_steps is None or self._fork_at_step is None:
            return None

        with self._lock:
            next_step = self._step_counter + 1
            if next_step > self._fork_at_step:
                return None

            parent = None
            for s in self._replay_steps:
                if s["step_number"] == next_step:
                    parent = s
                    break
            if parent is None:
                return None

            resp_bytes = self._store.blobs.get(parent["response_blob"])
            resp_data = json.loads(resp_bytes)

            self._step_counter += 1

            logger.info(
                "Rewind: fork replay — served cached step %d/%d (0ms, 0 tokens)",
                self._step_counter, self._fork_at_step,
            )

            return resp_data

    @staticmethod
    def _build_openai_response(resp_data: dict):
        """Build a fake OpenAI ChatCompletion from stored JSON."""
        try:
            from openai.types.chat import ChatCompletion
            return ChatCompletion.model_validate(resp_data)
        except Exception:
            return types.SimpleNamespace(**resp_data)

    @staticmethod
    def _build_anthropic_response(resp_data: dict):
        """Build a fake Anthropic Message from stored JSON."""
        try:
            from anthropic.types import Message
            return Message.model_validate(resp_data)
        except Exception:
            return types.SimpleNamespace(**resp_data)

    # ── OpenAI sync ───────────────────────────────────────────

    def _patch_openai_sync(self):
        try:
            from openai.resources.chat.completions import Completions
        except ImportError:
            return

        original = Completions.create
        recorder = self

        @functools.wraps(original)
        def patched(completions_self, *args, **kwargs):
            # Fork-and-execute: serve cached response if within replay range
            cached = recorder._try_replay_cached("openai")
            if cached is not None:
                return recorder._build_openai_response(cached)

            request_data = _serialize_request(kwargs)
            model = kwargs.get("model", "unknown")
            is_streaming = kwargs.get("stream", False)
            start = time.perf_counter()

            try:
                result = original(completions_self, *args, **kwargs)
                if is_streaming:
                    return _OpenAIStreamWrapper(result, recorder, model, request_data, start)
                duration_ms = int((time.perf_counter() - start) * 1000)
                resp_dict = _serialize_response(result)
                recorder._record_call(model, request_data, resp_dict, duration_ms, provider="openai")
                return result
            except Exception as e:
                duration_ms = int((time.perf_counter() - start) * 1000)
                recorder._record_call(model, request_data, None, duration_ms, error=str(e), provider="openai")
                raise

        Completions.create = patched
        self._originals["openai_sync"] = (Completions, "create", original)

    # ── OpenAI async ──────────────────────────────────────────

    def _patch_openai_async(self):
        try:
            from openai.resources.chat.completions import AsyncCompletions
        except ImportError:
            return

        original = AsyncCompletions.create
        recorder = self

        @functools.wraps(original)
        async def patched(completions_self, *args, **kwargs):
            cached = recorder._try_replay_cached("openai")
            if cached is not None:
                return recorder._build_openai_response(cached)

            request_data = _serialize_request(kwargs)
            model = kwargs.get("model", "unknown")
            is_streaming = kwargs.get("stream", False)
            start = time.perf_counter()

            try:
                result = await original(completions_self, *args, **kwargs)
                if is_streaming:
                    return _AsyncOpenAIStreamWrapper(result, recorder, model, request_data, start)
                duration_ms = int((time.perf_counter() - start) * 1000)
                resp_dict = _serialize_response(result)
                recorder._record_call(model, request_data, resp_dict, duration_ms, provider="openai")
                return result
            except Exception as e:
                duration_ms = int((time.perf_counter() - start) * 1000)
                recorder._record_call(model, request_data, None, duration_ms, error=str(e), provider="openai")
                raise

        AsyncCompletions.create = patched
        self._originals["openai_async"] = (AsyncCompletions, "create", original)

    # ── Anthropic sync ────────────────────────────────────────

    def _patch_anthropic_sync(self):
        try:
            from anthropic.resources.messages import Messages
        except ImportError:
            return

        original = Messages.create
        recorder = self

        @functools.wraps(original)
        def patched(messages_self, *args, **kwargs):
            cached = recorder._try_replay_cached("anthropic")
            if cached is not None:
                return recorder._build_anthropic_response(cached)

            request_data = _serialize_request(kwargs)
            model = kwargs.get("model", "unknown")
            is_streaming = kwargs.get("stream", False)
            start = time.perf_counter()

            try:
                result = original(messages_self, *args, **kwargs)
                if is_streaming:
                    return _AnthropicStreamWrapper(result, recorder, model, request_data, start)
                duration_ms = int((time.perf_counter() - start) * 1000)
                resp_dict = _serialize_response(result)
                recorder._record_call(model, request_data, resp_dict, duration_ms, provider="anthropic")
                return result
            except Exception as e:
                duration_ms = int((time.perf_counter() - start) * 1000)
                recorder._record_call(model, request_data, None, duration_ms, error=str(e), provider="anthropic")
                raise

        Messages.create = patched
        self._originals["anthropic_sync"] = (Messages, "create", original)

    # ── Anthropic async ───────────────────────────────────────

    def _patch_anthropic_async(self):
        try:
            from anthropic.resources.messages import AsyncMessages
        except ImportError:
            return

        original = AsyncMessages.create
        recorder = self

        @functools.wraps(original)
        async def patched(messages_self, *args, **kwargs):
            cached = recorder._try_replay_cached("anthropic")
            if cached is not None:
                return recorder._build_anthropic_response(cached)

            request_data = _serialize_request(kwargs)
            model = kwargs.get("model", "unknown")
            is_streaming = kwargs.get("stream", False)
            start = time.perf_counter()

            try:
                result = await original(messages_self, *args, **kwargs)
                if is_streaming:
                    return _AsyncAnthropicStreamWrapper(result, recorder, model, request_data, start)
                duration_ms = int((time.perf_counter() - start) * 1000)
                resp_dict = _serialize_response(result)
                recorder._record_call(model, request_data, resp_dict, duration_ms, provider="anthropic")
                return result
            except Exception as e:
                duration_ms = int((time.perf_counter() - start) * 1000)
                recorder._record_call(model, request_data, None, duration_ms, error=str(e), provider="anthropic")
                raise

        AsyncMessages.create = patched
        self._originals["anthropic_async"] = (AsyncMessages, "create", original)

    # ── Anthropic .stream() — separate from .create(stream=True) ──

    def _patch_anthropic_stream_sync(self):
        try:
            from anthropic.resources.messages import Messages
        except ImportError:
            return

        if not hasattr(Messages, "stream"):
            return

        original = Messages.stream
        recorder = self

        @functools.wraps(original)
        def patched(messages_self, *args, **kwargs):
            request_data = _serialize_request(kwargs)
            model = kwargs.get("model", "unknown")
            start = time.perf_counter()

            try:
                manager = original(messages_self, *args, **kwargs)
                return _AnthropicStreamManagerWrapper(
                    manager, recorder, model, request_data, start,
                )
            except Exception as e:
                duration_ms = int((time.perf_counter() - start) * 1000)
                recorder._record_call(model, request_data, None, duration_ms, error=str(e), provider="anthropic")
                raise

        Messages.stream = patched
        self._originals["anthropic_stream_sync"] = (Messages, "stream", original)

    def _patch_anthropic_stream_async(self):
        try:
            from anthropic.resources.messages import AsyncMessages
        except ImportError:
            return

        if not hasattr(AsyncMessages, "stream"):
            return

        original = AsyncMessages.stream
        recorder = self

        @functools.wraps(original)
        def patched(messages_self, *args, **kwargs):
            request_data = _serialize_request(kwargs)
            model = kwargs.get("model", "unknown")
            start = time.perf_counter()

            try:
                manager = original(messages_self, *args, **kwargs)
                return _AsyncAnthropicStreamManagerWrapper(
                    manager, recorder, model, request_data, start,
                )
            except Exception as e:
                duration_ms = int((time.perf_counter() - start) * 1000)
                recorder._record_call(model, request_data, None, duration_ms, error=str(e), provider="anthropic")
                raise

        AsyncMessages.stream = patched
        self._originals["anthropic_stream_async"] = (AsyncMessages, "stream", original)

    # ── Core recording ────────────────────────────────────────

    def _record_call(
        self,
        model: str,
        request_data: dict,
        response_data: dict = None,
        duration_ms: int = 0,
        error: str = None,
        provider: str = "openai",
    ):
        """Record an LLM call as a step. Thread-safe. Never raises."""
        try:
            status = "error" if error else "success"
            step_type = "llm_call"

            tokens_in, tokens_out = 0, 0
            if response_data:
                if provider == "anthropic":
                    tokens_in, tokens_out = _extract_anthropic_usage(response_data)
                else:
                    tokens_in, tokens_out = _extract_openai_usage(response_data)

            req_hash = self._store.blobs.put_json(request_data)
            resp_hash = self._store.blobs.put_json(response_data or {"error": error or "unknown"})

            span_id = None
            try:
                span_id = _get_current_span_id()
            except Exception:
                logger.debug("Rewind: could not resolve span_id", exc_info=True)

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

        except Exception:
            logger.warning("Rewind: failed to record LLM call", exc_info=True)
