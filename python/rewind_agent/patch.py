"""
Initialization layer for Rewind recording.

Two modes:
  - "direct" (default): Records LLM calls in-process by monkey-patching
    OpenAI/Anthropic SDK clients. No proxy needed.
  - "proxy": Redirects LLM traffic through the Rewind proxy server.
    Requires `rewind record` running in another terminal.
"""

import atexit
import contextlib
import os

_original_base_url = None
_original_anthropic_base_url = None
_initialized = False
_mode = None
_recorder = None
_store = None
_session_id = None
_thread_id = None
_thread_ordinal = 0
_circuit_breaker = None

REWIND_PROXY_URL = "http://127.0.0.1:8443"


def init(mode: str = "direct", proxy_url: str = None, session_name: str = "default",
         auto_patch: bool = True):
    """
    Initialize Rewind recording.

    Args:
        mode: "direct" (default) records in-process, no proxy needed.
              "proxy" redirects traffic through the Rewind proxy.
        proxy_url: Override proxy URL (proxy mode only).
        session_name: Name for this recording session.
        auto_patch: If True, monkey-patch OpenAI/Anthropic clients.
    """
    global _original_base_url, _initialized, _mode, _recorder, _store, _session_id

    if _initialized:
        return

    _mode = mode

    if mode == "direct":
        _init_direct(session_name, auto_patch)
    else:
        fell_through = _init_proxy(proxy_url, auto_patch, session_name)
        if fell_through:
            _mode = "direct"

    _initialized = True
    atexit.register(_atexit_cleanup)


def uninit():
    """Stop recording and clean up."""
    global _original_base_url, _original_anthropic_base_url, _initialized, _mode, _recorder, _store, _session_id, _circuit_breaker

    if not _initialized:
        return

    if _mode == "direct":
        if _recorder:
            _recorder.unpatch_all()
        # Unpatch Pydantic AI if patched
        for key, (cls, method_name, original) in _pydantic_ai_originals.items():
            setattr(cls, method_name, original)
        _pydantic_ai_originals.clear()
        if _store and _session_id:
            try:
                _store.update_session_status(_session_id, "completed")
            except Exception:
                pass
            _store.close()
        _recorder = None
        _store = None
        _session_id = None
    else:
        # Proxy mode cleanup — teardown circuit breaker, restore base URLs
        if _circuit_breaker:
            _circuit_breaker.teardown()
            _circuit_breaker = None
        if _original_base_url is not None:
            os.environ["OPENAI_BASE_URL"] = _original_base_url
        else:
            os.environ.pop("OPENAI_BASE_URL", None)
        if _original_anthropic_base_url is not None:
            os.environ["ANTHROPIC_BASE_URL"] = _original_anthropic_base_url
        else:
            os.environ.pop("ANTHROPIC_BASE_URL", None)

    _initialized = False
    _mode = None


@contextlib.contextmanager
def session(name: str = "default", mode: str = "direct", proxy_url: str = None):
    """
    Context manager for a Rewind recording session.

    Usage:
        with rewind_agent.session("my-agent"):
            client = openai.OpenAI()
            client.chat.completions.create(...)
    """
    init(mode=mode, proxy_url=proxy_url, session_name=name)
    try:
        yield
    finally:
        uninit()


@contextlib.contextmanager
def replay(session_ref: str = "latest", from_step: int = None):
    """
    Context manager for fork-and-execute replay.

    Steps 1 through from_step are served from cache (0ms, 0 tokens).
    Steps after from_step call the real LLM and are recorded into a new forked timeline.

    Usage:
        with rewind_agent.replay("latest", from_step=4):
            result = my_agent.run("Research Tokyo population")
            # Steps 1-4: instant cached responses
            # Step 5+: live LLM calls
    """
    from .store import Store
    from .recorder import Recorder

    store = Store()

    # Resolve session
    sess = store.get_session(session_ref)
    if sess is None:
        raise ValueError(f"Session not found: {session_ref}")

    # Get root timeline and its steps
    root_tl = store.get_root_timeline(sess["id"])
    if root_tl is None:
        raise ValueError(f"No timeline found for session {sess['id']}")

    parent_steps = store.get_full_timeline_steps(root_tl["id"], sess["id"])
    if not parent_steps:
        raise ValueError("Session has no steps to replay")

    # Determine fork point
    if from_step is None:
        from_step = len(parent_steps)
    if from_step < 1 or from_step > len(parent_steps):
        raise ValueError(
            f"Invalid from_step={from_step}. Session has {len(parent_steps)} steps (use 1-{len(parent_steps)})."
        )

    # Create forked timeline
    fork_tl_id = store.create_fork_timeline(
        sess["id"], root_tl["id"], from_step, "replayed"
    )

    # Create recorder in replay mode
    recorder = Recorder(
        store, sess["id"], fork_tl_id,
        replay_steps=parent_steps,
        fork_at_step=from_step,
    )
    recorder.patch_all()

    _print_replay_banner(sess["name"], from_step, len(parent_steps))

    try:
        yield
    finally:
        recorder.unpatch_all()
        try:
            store.update_session_status(sess["id"], "completed")
        except Exception:
            pass
        store.close()


@contextlib.contextmanager
def thread(thread_id: str):
    """
    Context manager for grouping sessions into a conversation thread.

    Usage:
        with rewind_agent.thread("conversation-123"):
            with rewind_agent.session("turn-1"):
                agent.run("Hello")
            with rewind_agent.session("turn-2"):
                agent.run("Follow up")
    """
    global _thread_id, _thread_ordinal
    _thread_id = thread_id
    _thread_ordinal = 0
    try:
        yield
    finally:
        _thread_id = None
        _thread_ordinal = 0


def _print_replay_banner(session_name: str, from_step: int, total_steps: int):
    _print_logo()
    print("  \033[36m\033[1mFork & Execute Replay\033[0m")
    print()
    print(f"  \033[90m  Session:\033[0m  {session_name}")
    print(f"  \033[90m  Cached:\033[0m   \033[32mSteps 1-{from_step} (0ms, 0 tokens)\033[0m")
    print(f"  \033[90m  Live:\033[0m     \033[36mSteps {from_step + 1}+ (real LLM calls)\033[0m")
    print()


def _init_direct(session_name: str, auto_patch: bool):
    """Initialize direct recording mode."""
    global _recorder, _store, _session_id, _thread_ordinal

    from .store import Store
    from .recorder import Recorder

    _store = Store()
    sid, tid = _store.create_session(session_name)
    _session_id = sid

    if _thread_id:
        _store.set_session_thread(sid, _thread_id, _thread_ordinal)
        _thread_ordinal += 1

    _recorder = Recorder(_store, sid, tid)
    if auto_patch:
        _recorder.patch_all()

    # Auto-register OpenAI Agents SDK tracing if available
    _try_register_openai_agents(tid)

    # Auto-patch Pydantic AI Agent to inject hooks if available
    _try_patch_pydantic_ai(tid)

    _print_direct_banner(session_name)


def _try_register_openai_agents(timeline_id: str):
    """Register Rewind tracing with the OpenAI Agents SDK if it's installed.
    The TracingProcessor creates spans for agent structure; the monkey-patches
    remain active to record all LLM call steps (including raw SDK calls)."""
    try:
        from .openai_agents import register_tracing_processor
        register_tracing_processor(_store, _session_id, timeline_id, _recorder)
    except Exception:
        pass  # agents SDK not installed or other import issue — skip silently


_pydantic_ai_originals = {}


def _try_patch_pydantic_ai(timeline_id: str):
    """Monkey-patch Pydantic AI Agent.__init__ to auto-inject Rewind hooks.
    This means every Agent created after init() automatically gets recording."""
    try:
        from pydantic_ai import Agent as PydanticAgent
        from .pydantic_ai import create_rewind_hooks
    except ImportError:
        return

    hooks = create_rewind_hooks(_store, _session_id, timeline_id)
    if hooks is None:
        return

    import functools
    original_init = PydanticAgent.__init__
    _pydantic_ai_originals["init"] = (PydanticAgent, "__init__", original_init)

    @functools.wraps(original_init)
    def patched_init(self, *args, **kwargs):
        # Inject Rewind hooks into capabilities
        capabilities = list(kwargs.get("capabilities", None) or [])
        capabilities.append(hooks)
        kwargs["capabilities"] = capabilities
        return original_init(self, *args, **kwargs)

    PydanticAgent.__init__ = patched_init


def _proxy_is_healthy(url: str, timeout: float = 0.5) -> bool:
    """
    Quick health check — returns True if the Rewind proxy is alive.

    Uses urllib.request with a 0.5s timeout. On localhost this adds negligible
    latency when the proxy is up. When genuinely down, blocks for up to 0.5s
    at init time — acceptable tradeoff for preventing broken LLM calls.
    """
    import json
    import urllib.request
    try:
        req = urllib.request.Request(f"{url}/_rewind/health", method="GET")
        resp = urllib.request.urlopen(req, timeout=timeout)
        if resp.status != 200:
            return False
        body = json.loads(resp.read())
        return body.get("status") == "ok"
    except Exception:
        return False


def _init_proxy(proxy_url: str, auto_patch: bool, session_name: str = "default") -> bool:
    """Initialize proxy recording mode with health-check fallthrough.

    If the proxy is unreachable, falls back to direct mode with a warning
    instead of silently breaking all LLM calls.

    Returns True if fell through to direct mode, False if proxy mode succeeded.
    """
    global _original_base_url, _original_anthropic_base_url

    url = proxy_url or REWIND_PROXY_URL

    if not _proxy_is_healthy(url):
        import logging
        logging.getLogger("rewind").warning(
            "Rewind proxy not reachable at %s. "
            "Falling back to direct recording mode. "
            "Start the proxy with: rewind record",
            url,
        )
        _init_direct(session_name, auto_patch)
        return True

    # Store originals for both providers so uninit() can restore them
    _original_base_url = os.environ.get("OPENAI_BASE_URL")
    _original_anthropic_base_url = os.environ.get("ANTHROPIC_BASE_URL")

    os.environ["OPENAI_BASE_URL"] = f"{url}/v1"
    os.environ["ANTHROPIC_BASE_URL"] = f"{url}/anthropic"

    if auto_patch:
        _patch_existing_clients(url)

    # Install circuit breaker for mid-session proxy failure detection
    global _circuit_breaker
    from .circuit_breaker import ProxyCircuitBreaker
    _circuit_breaker = ProxyCircuitBreaker(
        proxy_url=url,
        original_openai_url=_original_base_url,
        original_anthropic_url=_original_anthropic_base_url,
        session_name=session_name,
    )
    _circuit_breaker.install_patches()

    _print_proxy_banner(url)
    return False


def _atexit_cleanup():
    """Best-effort cleanup on interpreter exit."""
    try:
        uninit()
    except Exception:
        pass


def _patch_existing_clients(proxy_url: str):
    """Patch already-instantiated OpenAI clients if the module is loaded."""
    try:
        import openai
        if hasattr(openai, "_client"):
            openai._client.base_url = f"{proxy_url}/v1"
    except ImportError:
        pass


def _print_direct_banner(session_name: str):
    _print_logo()
    print("  \033[36m\033[1mRecording active\033[0m \033[90m(direct)\033[0m")
    print()
    print(f"  \033[90m  Session:\033[0m  {session_name}")
    print("  \033[90m  Store:\033[0m    ~/.rewind/")
    print("  \033[90m  Debug:\033[0m    \033[32mrewind show latest\033[0m")
    print()
    print("  \033[33m  ● Recording all LLM calls\033[0m")
    print()


def _print_proxy_banner(proxy_url: str):
    _print_logo()
    print("  \033[36m\033[1mRecording active\033[0m \033[90m(proxy)\033[0m")
    print()
    print(f"  \033[90m  Proxy:\033[0m    {proxy_url}")
    print(f"  \033[90m  OpenAI:\033[0m   {proxy_url}/v1")
    print("  \033[90m  Debug:\033[0m    \033[32mrewind show latest\033[0m")
    print()
    print("  \033[33m  ● Recording all LLM calls\033[0m")
    print()


def _print_logo():
    C = "\033[36m"
    B = "\033[1m"
    D = "\033[2m"
    X = "\033[0m"
    print()
    print(f"  {C}{B}  ⏪  r e w i n d{X}")
    print(f"  {D}  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{X}")
    print(f"  {D}  The time-travel debugger for AI agents{X}")
    print()
