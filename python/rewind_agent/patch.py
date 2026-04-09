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
_initialized = False
_mode = None
_recorder = None
_store = None
_session_id = None

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
        _init_proxy(proxy_url, auto_patch)

    _initialized = True
    atexit.register(_atexit_cleanup)


def uninit():
    """Stop recording and clean up."""
    global _original_base_url, _initialized, _mode, _recorder, _store, _session_id

    if not _initialized:
        return

    if _mode == "direct":
        if _recorder:
            _recorder.unpatch_all()
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
        # Proxy mode cleanup
        if _original_base_url is not None:
            os.environ["OPENAI_BASE_URL"] = _original_base_url
        else:
            os.environ.pop("OPENAI_BASE_URL", None)
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


def _print_replay_banner(session_name: str, from_step: int, total_steps: int):
    _print_logo()
    print(f"  \033[36m\033[1mFork & Execute Replay\033[0m")
    print()
    print(f"  \033[90m  Session:\033[0m  {session_name}")
    print(f"  \033[90m  Cached:\033[0m   \033[32mSteps 1-{from_step} (0ms, 0 tokens)\033[0m")
    print(f"  \033[90m  Live:\033[0m     \033[36mSteps {from_step + 1}+ (real LLM calls)\033[0m")
    print()


def _init_direct(session_name: str, auto_patch: bool):
    """Initialize direct recording mode."""
    global _recorder, _store, _session_id

    from .store import Store
    from .recorder import Recorder

    _store = Store()
    sid, tid = _store.create_session(session_name)
    _session_id = sid

    _recorder = Recorder(_store, sid, tid)
    if auto_patch:
        _recorder.patch_all()

    _print_direct_banner(session_name)


def _init_proxy(proxy_url: str, auto_patch: bool):
    """Initialize proxy recording mode (existing behavior)."""
    global _original_base_url

    url = proxy_url or REWIND_PROXY_URL
    _original_base_url = os.environ.get("OPENAI_BASE_URL")
    os.environ["OPENAI_BASE_URL"] = f"{url}/v1"
    os.environ["ANTHROPIC_BASE_URL"] = f"{url}/anthropic"

    if auto_patch:
        _patch_existing_clients(url)

    _print_proxy_banner(url)


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
    print(f"  \033[36m\033[1mRecording active\033[0m \033[90m(direct)\033[0m")
    print()
    print(f"  \033[90m  Session:\033[0m  {session_name}")
    print(f"  \033[90m  Store:\033[0m    ~/.rewind/")
    print(f"  \033[90m  Debug:\033[0m    \033[32mrewind show latest\033[0m")
    print()
    print(f"  \033[33m  ● Recording all LLM calls\033[0m")
    print()


def _print_proxy_banner(proxy_url: str):
    _print_logo()
    print(f"  \033[36m\033[1mRecording active\033[0m \033[90m(proxy)\033[0m")
    print()
    print(f"  \033[90m  Proxy:\033[0m    {proxy_url}")
    print(f"  \033[90m  OpenAI:\033[0m   {proxy_url}/v1")
    print(f"  \033[90m  Debug:\033[0m    \033[32mrewind show latest\033[0m")
    print()
    print(f"  \033[33m  ● Recording all LLM calls\033[0m")
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
