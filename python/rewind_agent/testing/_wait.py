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
        for session in list(server.sessions.values()):
            if session.get("name") == name:
                return session
        time.sleep(poll_interval)
    raise TimeoutError(
        f"No session named {name!r} appeared within {timeout}s; "
        f"recorded sessions: {[s.get('name') for s in server.sessions.values()]}"
    )
