"""``wait_for_session`` — poll a :class:`StubRewindServer` for a session
matching a name, used by integration-style tests.

Public API: imported via ``from rewind_agent.testing import wait_for_session``.
"""

from __future__ import annotations

import time
from typing import Any

from rewind_agent.testing._stub_server import StubRewindServer


def wait_for_session(
    server: StubRewindServer,
    *,
    name: str,
    timeout: float = 5.0,
    poll_interval: float = 0.02,
) -> dict[str, Any]:
    """Poll ``server`` until a session with ``name`` appears, or raise.

    Returns the session dict (the same shape :class:`StubRewindServer` records
    in ``server.sessions``).

    Raises:
        TimeoutError: when no matching session appears within ``timeout``
            seconds. Caller is responsible for naming sessions distinctly.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        # Snapshot under the stub's lock for symmetry with the rest of
        # the stub. CPython's GIL would make the bare iteration safe in
        # practice, but explicit locking keeps the test infrastructure
        # honest about its concurrency contract.
        with server._server.lock:  # type: ignore[attr-defined]
            snapshot = list(server.sessions.values())
        for session in snapshot:
            if session.get("name") == name:
                return session
        time.sleep(poll_interval)
    raise TimeoutError(
        f"No session named {name!r} appeared within {timeout}s; "
        f"recorded sessions: {[s.get('name') for s in server.sessions.values()]}"
    )
