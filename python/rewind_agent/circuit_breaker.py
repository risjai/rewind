"""
Mid-session circuit breaker for proxy mode.

Detects proxy failures from LLM SDK connection errors and transparently
falls through to direct recording via a throwaway client. No per-request
health checks. No added latency on the happy path.

State machine: CLOSED → OPEN → HALF_OPEN → CLOSED
"""

from __future__ import annotations

import functools
import logging
import threading
import time

logger = logging.getLogger("rewind")

# Default upstream URLs when the user had no env var set before proxy init
_DEFAULT_OPENAI_URL = "https://api.openai.com/v1"
_DEFAULT_ANTHROPIC_URL = "https://api.anthropic.com"


def _is_connection_error(error: Exception) -> bool:
    """Check if an exception indicates the proxy is unreachable.

    Detects by class name to avoid importing openai/anthropic at module level
    (preserves the zero-required-dependency guarantee).
    """
    error_type = type(error).__name__

    # Direct match on SDK connection error type.
    # APITimeoutError is excluded: timeouts may originate from the upstream
    # provider (slow LLM response), not the proxy. Including it would cause
    # false trips when the proxy is healthy but upstream is slow.
    if error_type == "APIConnectionError":
        return True

    # stdlib connection errors
    if isinstance(error, (ConnectionRefusedError, ConnectionError, ConnectionResetError)):
        return True

    # Check the exception chain for wrapped transport errors
    cause = error.__cause__ or error.__context__
    if cause:
        cause_type = type(cause).__name__
        if cause_type in ("ConnectError", "ConnectionRefusedError",
                          "ConnectionError", "ConnectionResetError"):
            return True

    return False


class ProxyCircuitBreaker:
    """Mid-session circuit breaker for proxy mode.

    Detects proxy failure from SDK connection errors and transparently
    falls through to direct recording. Thread-safe.
    """

    CLOSED = "closed"
    OPEN = "open"
    HALF_OPEN = "half_open"

    def __init__(
        self,
        proxy_url: str,
        original_openai_url: str | None,
        original_anthropic_url: str | None,
        session_name: str = "default",
        failure_threshold: int = 2,
        recovery_timeout: float = 30.0,
    ):
        self.proxy_url = proxy_url
        self.original_openai_url = original_openai_url or _DEFAULT_OPENAI_URL
        self.original_anthropic_url = original_anthropic_url or _DEFAULT_ANTHROPIC_URL
        self.session_name = session_name
        self.failure_threshold = failure_threshold
        self.recovery_timeout = recovery_timeout

        self.state = self.CLOSED
        self.failure_count = 0
        self.last_failure_time: float = 0

        self._lock = threading.Lock()
        self._direct_store = None
        self._direct_recorder = None
        self._direct_session_id = None
        self._originals: dict = {}

    def should_try_proxy(self) -> bool:
        """Return True if the next call should attempt the proxy."""
        with self._lock:
            if self.state == self.CLOSED:
                return True
            if self.state == self.HALF_OPEN:
                return True
            # OPEN — check if recovery timeout has elapsed
            if self.state == self.OPEN:
                elapsed = time.monotonic() - self.last_failure_time
                if elapsed >= self.recovery_timeout:
                    self.state = self.HALF_OPEN
                    logger.info(
                        "Rewind circuit breaker: probing proxy (%.0fs elapsed)",
                        elapsed,
                    )
                    return True
            return False

    def record_success(self) -> None:
        """Called after a successful LLM call through the proxy."""
        with self._lock:
            self.failure_count = 0
            if self.state == self.HALF_OPEN:
                logger.info("Rewind circuit breaker: proxy recovered. Resuming proxy mode.")
                self._close_circuit()

    def record_failure(self, error: Exception) -> bool:
        """Called on a connection error. Returns True if circuit just tripped to OPEN."""
        with self._lock:
            self.failure_count += 1
            self.last_failure_time = time.monotonic()

            if self.state == self.HALF_OPEN:
                # Probe failed — reopen
                self.state = self.OPEN
                logger.warning("Rewind circuit breaker: probe failed, staying OPEN.")
                return False

            if self.state == self.CLOSED and self.failure_count >= self.failure_threshold:
                self._trip_to_open()
                return True

            return False

    def _trip_to_open(self) -> None:
        """Transition to OPEN state. Create direct-mode Store + Recorder."""
        from .store import Store
        from .recorder import Recorder

        self.state = self.OPEN
        fallback_name = f"{self.session_name} (proxy-fallback)"

        self._direct_store = Store()
        sid, tid = self._direct_store.create_session(fallback_name)
        self._direct_session_id = sid
        self._direct_recorder = Recorder(self._direct_store, sid, tid)

        logger.warning(
            "Rewind circuit breaker: proxy unreachable after %d failures. "
            "Tripped to OPEN. Recording in direct mode (session: %s).",
            self.failure_count, fallback_name,
        )

    def _close_circuit(self) -> None:
        """Transition to CLOSED. Tear down direct-mode resources."""
        self.state = self.CLOSED
        self.failure_count = 0

        if self._direct_store and self._direct_session_id:
            try:
                self._direct_store.update_session_status(
                    self._direct_session_id, "completed"
                )
            except Exception:
                pass
            self._direct_store.close()

        self._direct_store = None
        self._direct_recorder = None
        self._direct_session_id = None

    def teardown(self) -> None:
        """Clean up all resources. Called from uninit()."""
        with self._lock:
            if self._direct_store:
                self._close_circuit()
        self.uninstall_patches()

    # ── Patch installation ──────────────────────────────────────

    def install_patches(self) -> None:
        """Install circuit-breaker-aware patches on SDK classes."""
        self._patch_openai_sync()
        self._patch_openai_async()
        self._patch_anthropic_sync()
        self._patch_anthropic_async()
        self._patch_anthropic_stream_sync()
        self._patch_anthropic_stream_async()

    def uninstall_patches(self) -> None:
        """Restore original SDK methods."""
        for key, (cls, method_name, original) in self._originals.items():
            setattr(cls, method_name, original)
        self._originals.clear()

    # ── Direct-call helpers (throwaway client) ────────────────

    def _call_direct_openai_sync(self, completions_self, args, kwargs):
        """Retry an OpenAI call via direct upstream using a throwaway client."""
        from .recorder import _serialize_request, _serialize_response, _OpenAIStreamWrapper
        import openai

        existing_client = completions_self._client
        direct_client = openai.OpenAI(
            base_url=self.original_openai_url,
            api_key=existing_client.api_key,
            organization=getattr(existing_client, "organization", None),
        )
        request_data = _serialize_request(kwargs)
        model = kwargs.get("model", "unknown")
        is_streaming = kwargs.get("stream", False)
        start = time.perf_counter()

        try:
            result = direct_client.chat.completions.create(*args, **kwargs)
            if is_streaming and self._direct_recorder:
                return _OpenAIStreamWrapper(result, self._direct_recorder, model, request_data, start)
            if self._direct_recorder:
                duration_ms = int((time.perf_counter() - start) * 1000)
                resp_dict = _serialize_response(result)
                self._direct_recorder._record_call(model, request_data, resp_dict, duration_ms, provider="openai")
            return result
        except Exception as e:
            if self._direct_recorder:
                duration_ms = int((time.perf_counter() - start) * 1000)
                self._direct_recorder._record_call(model, request_data, None, duration_ms, error=str(e), provider="openai")
            raise

    async def _call_direct_openai_async(self, completions_self, args, kwargs):
        """Retry an async OpenAI call via direct upstream."""
        from .recorder import _serialize_request, _serialize_response, _AsyncOpenAIStreamWrapper
        import openai

        existing_client = completions_self._client
        direct_client = openai.AsyncOpenAI(
            base_url=self.original_openai_url,
            api_key=existing_client.api_key,
            organization=getattr(existing_client, "organization", None),
        )
        request_data = _serialize_request(kwargs)
        model = kwargs.get("model", "unknown")
        is_streaming = kwargs.get("stream", False)
        start = time.perf_counter()

        try:
            result = await direct_client.chat.completions.create(*args, **kwargs)
            if is_streaming and self._direct_recorder:
                return _AsyncOpenAIStreamWrapper(result, self._direct_recorder, model, request_data, start)
            if self._direct_recorder:
                duration_ms = int((time.perf_counter() - start) * 1000)
                resp_dict = _serialize_response(result)
                self._direct_recorder._record_call(model, request_data, resp_dict, duration_ms, provider="openai")
            return result
        except Exception as e:
            if self._direct_recorder:
                duration_ms = int((time.perf_counter() - start) * 1000)
                self._direct_recorder._record_call(model, request_data, None, duration_ms, error=str(e), provider="openai")
            raise

    def _call_direct_anthropic_sync(self, messages_self, args, kwargs):
        """Retry an Anthropic call via direct upstream using a throwaway client."""
        from .recorder import _serialize_request, _serialize_response, _AnthropicStreamWrapper
        import anthropic

        existing_client = messages_self._client
        direct_client = anthropic.Anthropic(
            base_url=self.original_anthropic_url,
            api_key=existing_client.api_key,
        )
        request_data = _serialize_request(kwargs)
        model = kwargs.get("model", "unknown")
        is_streaming = kwargs.get("stream", False)
        start = time.perf_counter()

        try:
            result = direct_client.messages.create(*args, **kwargs)
            if is_streaming and self._direct_recorder:
                return _AnthropicStreamWrapper(result, self._direct_recorder, model, request_data, start)
            if self._direct_recorder:
                duration_ms = int((time.perf_counter() - start) * 1000)
                resp_dict = _serialize_response(result)
                self._direct_recorder._record_call(model, request_data, resp_dict, duration_ms, provider="anthropic")
            return result
        except Exception as e:
            if self._direct_recorder:
                duration_ms = int((time.perf_counter() - start) * 1000)
                self._direct_recorder._record_call(model, request_data, None, duration_ms, error=str(e), provider="anthropic")
            raise

    async def _call_direct_anthropic_async(self, messages_self, args, kwargs):
        """Retry an async Anthropic call via direct upstream."""
        from .recorder import _serialize_request, _serialize_response, _AsyncAnthropicStreamWrapper
        import anthropic

        existing_client = messages_self._client
        direct_client = anthropic.AsyncAnthropic(
            base_url=self.original_anthropic_url,
            api_key=existing_client.api_key,
        )
        request_data = _serialize_request(kwargs)
        model = kwargs.get("model", "unknown")
        is_streaming = kwargs.get("stream", False)
        start = time.perf_counter()

        try:
            result = await direct_client.messages.create(*args, **kwargs)
            if is_streaming and self._direct_recorder:
                return _AsyncAnthropicStreamWrapper(result, self._direct_recorder, model, request_data, start)
            if self._direct_recorder:
                duration_ms = int((time.perf_counter() - start) * 1000)
                resp_dict = _serialize_response(result)
                self._direct_recorder._record_call(model, request_data, resp_dict, duration_ms, provider="anthropic")
            return result
        except Exception as e:
            if self._direct_recorder:
                duration_ms = int((time.perf_counter() - start) * 1000)
                self._direct_recorder._record_call(model, request_data, None, duration_ms, error=str(e), provider="anthropic")
            raise

    # ── Patch methods ─────────────────────────────────────────

    def _patch_openai_sync(self):
        try:
            from openai.resources.chat.completions import Completions
        except ImportError:
            return

        original = Completions.create
        cb = self

        @functools.wraps(original)
        def patched(completions_self, *args, **kwargs):
            if cb.should_try_proxy():
                try:
                    result = original(completions_self, *args, **kwargs)
                    # Only record success for non-streaming calls. For streaming,
                    # original() returns a stream object — no data has flowed yet.
                    # The proxy could die mid-stream after we've reset failure_count.
                    if not kwargs.get("stream", False):
                        cb.record_success()
                    return result
                except Exception as e:
                    if _is_connection_error(e):
                        cb.record_failure(e)
                    else:
                        raise
            return cb._call_direct_openai_sync(completions_self, args, kwargs)

        Completions.create = patched
        self._originals["cb_openai_sync"] = (Completions, "create", original)

    def _patch_openai_async(self):
        try:
            from openai.resources.chat.completions import AsyncCompletions
        except ImportError:
            return

        original = AsyncCompletions.create
        cb = self

        @functools.wraps(original)
        async def patched(completions_self, *args, **kwargs):
            if cb.should_try_proxy():
                try:
                    result = await original(completions_self, *args, **kwargs)
                    if not kwargs.get("stream", False):
                        cb.record_success()
                    return result
                except Exception as e:
                    if _is_connection_error(e):
                        cb.record_failure(e)
                    else:
                        raise
            return await cb._call_direct_openai_async(completions_self, args, kwargs)

        AsyncCompletions.create = patched
        self._originals["cb_openai_async"] = (AsyncCompletions, "create", original)

    def _patch_anthropic_sync(self):
        try:
            from anthropic.resources.messages import Messages
        except ImportError:
            return

        original = Messages.create
        cb = self

        @functools.wraps(original)
        def patched(messages_self, *args, **kwargs):
            if cb.should_try_proxy():
                try:
                    result = original(messages_self, *args, **kwargs)
                    if not kwargs.get("stream", False):
                        cb.record_success()
                    return result
                except Exception as e:
                    if _is_connection_error(e):
                        cb.record_failure(e)
                    else:
                        raise
            return cb._call_direct_anthropic_sync(messages_self, args, kwargs)

        Messages.create = patched
        self._originals["cb_anthropic_sync"] = (Messages, "create", original)

    def _patch_anthropic_async(self):
        try:
            from anthropic.resources.messages import AsyncMessages
        except ImportError:
            return

        original = AsyncMessages.create
        cb = self

        @functools.wraps(original)
        async def patched(messages_self, *args, **kwargs):
            if cb.should_try_proxy():
                try:
                    result = await original(messages_self, *args, **kwargs)
                    if not kwargs.get("stream", False):
                        cb.record_success()
                    return result
                except Exception as e:
                    if _is_connection_error(e):
                        cb.record_failure(e)
                    else:
                        raise
            return await cb._call_direct_anthropic_async(messages_self, args, kwargs)

        AsyncMessages.create = patched
        self._originals["cb_anthropic_async"] = (AsyncMessages, "create", original)

    def _patch_anthropic_stream_sync(self):
        try:
            from anthropic.resources.messages import Messages
        except ImportError:
            return
        if not hasattr(Messages, "stream"):
            return

        original = Messages.stream
        cb = self

        @functools.wraps(original)
        def patched(messages_self, *args, **kwargs):
            if cb.should_try_proxy():
                try:
                    result = original(messages_self, *args, **kwargs)
                    # Don't record_success() — stream() returns a manager,
                    # no data has flowed. Proxy could die mid-stream.
                    return result
                except Exception as e:
                    if _is_connection_error(e):
                        cb.record_failure(e)
                    else:
                        raise
            # OPEN: create throwaway client for stream
            from .recorder import _serialize_request, _AnthropicStreamManagerWrapper
            import anthropic
            existing_client = messages_self._client
            direct_client = anthropic.Anthropic(
                base_url=cb.original_anthropic_url,
                api_key=existing_client.api_key,
            )
            request_data = _serialize_request(kwargs)
            model = kwargs.get("model", "unknown")
            start = time.perf_counter()
            try:
                manager = direct_client.messages.stream(*args, **kwargs)
                if cb._direct_recorder:
                    return _AnthropicStreamManagerWrapper(
                        manager, cb._direct_recorder, model, request_data, start,
                    )
                return manager
            except Exception as e:
                if cb._direct_recorder:
                    duration_ms = int((time.perf_counter() - start) * 1000)
                    cb._direct_recorder._record_call(model, request_data, None, duration_ms, error=str(e), provider="anthropic")
                raise

        Messages.stream = patched
        self._originals["cb_anthropic_stream_sync"] = (Messages, "stream", original)

    def _patch_anthropic_stream_async(self):
        try:
            from anthropic.resources.messages import AsyncMessages
        except ImportError:
            return
        if not hasattr(AsyncMessages, "stream"):
            return

        original = AsyncMessages.stream
        cb = self

        @functools.wraps(original)
        def patched(messages_self, *args, **kwargs):
            if cb.should_try_proxy():
                try:
                    result = original(messages_self, *args, **kwargs)
                    # Don't record_success() — stream manager, no data yet.
                    return result
                except Exception as e:
                    if _is_connection_error(e):
                        cb.record_failure(e)
                    else:
                        raise
            # OPEN: create throwaway client for async stream
            from .recorder import _serialize_request, _AsyncAnthropicStreamManagerWrapper
            import anthropic
            existing_client = messages_self._client
            direct_client = anthropic.AsyncAnthropic(
                base_url=cb.original_anthropic_url,
                api_key=existing_client.api_key,
            )
            request_data = _serialize_request(kwargs)
            model = kwargs.get("model", "unknown")
            start = time.perf_counter()
            try:
                manager = direct_client.messages.stream(*args, **kwargs)
                if cb._direct_recorder:
                    return _AsyncAnthropicStreamManagerWrapper(
                        manager, cb._direct_recorder, model, request_data, start,
                    )
                return manager
            except Exception as e:
                if cb._direct_recorder:
                    duration_ms = int((time.perf_counter() - start) * 1000)
                    cb._direct_recorder._record_call(model, request_data, None, duration_ms, error=str(e), provider="anthropic")
                raise

        AsyncMessages.stream = patched
        self._originals["cb_anthropic_stream_async"] = (AsyncMessages, "stream", original)
