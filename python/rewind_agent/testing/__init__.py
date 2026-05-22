"""Public testing utilities for downstream packages.

This module exposes the helpers downstream packages (e.g. the planned
``sf-rewind`` connector) use to write tests against Rewind's recording
and replay APIs without standing up a real server.

Stability: symbols listed in ``__all__`` follow normal SDK semver —
breaking changes require a minor bump in 0.x. Symbols under
``rewind_agent.testing._unstable`` are explicitly NOT covered by that
commitment and may break in any release.

Example
-------

>>> from rewind_agent import ExplicitClient
>>> from rewind_agent.testing import StubRewindServer
>>>
>>> with StubRewindServer() as server:
...     client = ExplicitClient(server.base_url)
...     with client.session("my-test"):
...         client.record_tool_call("ping", {}, "ok", duration_ms=1)
...     assert len(server.recorded_steps) == 1
"""

from rewind_agent.testing._stub_server import StubRewindServer
from rewind_agent.testing._dispatch import make_dispatch_payload
from rewind_agent.testing._wait import wait_for_session

__all__ = [
    "StubRewindServer",
    "make_dispatch_payload",
    "wait_for_session",
]
