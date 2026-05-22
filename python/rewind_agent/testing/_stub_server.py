"""In-process stub of the Rewind HTTP API.

Public API: :class:`StubRewindServer`. Imported via
``from rewind_agent.testing import StubRewindServer``.

This stub serves the subset of routes that recording + replay handlers
hit during unit tests:

* ``POST /api/sessions/start`` — creates a session, returns
  ``{session_id, root_timeline_id}``.
* ``POST /api/sessions/{id}/end`` — finalizes a session.
* ``POST /api/sessions/{id}/llm-calls`` — records an LLM call.
* ``POST /api/sessions/{id}/tool-calls`` — records a tool call.
* ``GET /api/sessions`` — lists all started sessions.
* ``GET /api/sessions/{id}/timelines`` — returns a single root timeline.
* ``GET /api/sessions/{id}/steps`` — returns recorded steps.

Anything beyond this minimal surface belongs in
:mod:`rewind_agent.testing._unstable`.
"""

from __future__ import annotations

import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from typing import Any


class _StubHandler(BaseHTTPRequestHandler):
    server: "_StubServer"  # type: ignore[assignment]

    def do_POST(self) -> None:  # noqa: N802
        length = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(length)) if length else {}
        owner = self.server  # type: ignore[attr-defined]

        if self.path == "/api/sessions/start":
            # Decide the response inside the lock, then write the socket
            # outside it. Both branches now follow the same pattern, so a
            # slow socket write never blocks other handlers waiting on
            # the lock.
            status = 201
            with owner.lock:
                key = body.get("client_session_key")
                if key and key in owner.sessions_by_client_key:
                    sid, tid = owner.sessions_by_client_key[key]
                    status = 200
                else:
                    idx = len(owner.sessions) + 1
                    sid = f"s-{idx}"
                    tid = f"tl-{idx}"
                    owner.sessions[sid] = {
                        "session_id": sid,
                        "name": body.get("name", ""),
                        "root_timeline_id": tid,
                        "metadata": body.get("metadata", {}),
                        "thread_id": body.get("thread_id"),
                        "ended": False,
                    }
                    if key:
                        owner.sessions_by_client_key[key] = (sid, tid)
            self._json(status, {"session_id": sid, "root_timeline_id": tid})
            return

        if self.path.endswith("/end"):
            sid = self.path.split("/")[3]
            with owner.lock:
                session = owner.sessions.get(sid)
                if session is not None:
                    session["ended"] = True
            self._json(200, {"session_id": sid})
            return

        if self.path.endswith("/llm-calls") and "replay-lookup" not in self.path:
            sid = self.path.split("/")[3]
            with owner.lock:
                # Match real-server semantics: step_number is per-session,
                # not global. Tests authored against this stub can rely on
                # `step_number == 1` being the first call within a session
                # regardless of how many other sessions have recorded.
                step_number = sum(
                    1 for s in owner.recorded_steps
                    if s.get("session_id") == sid
                ) + 1
                owner.recorded_steps.append({
                    "session_id": sid,
                    "step_number": step_number,
                    "step_type": "llm_call",
                    "model": body.get("model"),
                    "request_body": body.get("request_body"),
                    "response_body": body.get("response_body"),
                    "tool_name": None,
                })
            self._json(201, {"step_number": step_number})
            return

        if self.path.endswith("/tool-calls") and "replay-lookup" not in self.path:
            sid = self.path.split("/")[3]
            with owner.lock:
                step_number = sum(
                    1 for s in owner.recorded_steps
                    if s.get("session_id") == sid
                ) + 1
                owner.recorded_steps.append({
                    "session_id": sid,
                    "step_number": step_number,
                    "step_type": "tool_call",
                    "tool_name": body.get("tool_name"),
                    "request_body": body.get("request_body"),
                    "response_body": body.get("response_body"),
                    "model": None,
                })
            self._json(201, {"step_number": step_number})
            return

        if "replay-lookup" in self.path:
            self._json(200, {"hit": False})
            return

        self._json(404, {"error": f"unhandled POST {self.path}"})

    def do_GET(self) -> None:  # noqa: N802
        owner = self.server  # type: ignore[attr-defined]

        if self.path == "/api/sessions":
            with owner.lock:
                listing = list(owner.sessions.values())
            self._json(200, listing)
            return

        if "/timelines" in self.path:
            sid = self.path.split("/")[3]
            with owner.lock:
                session = owner.sessions.get(sid)
            if session is None:
                self._json(404, {"error": "session not found"})
                return
            self._json(200, [
                {
                    "id": session["root_timeline_id"],
                    "session_id": sid,
                    "parent_timeline_id": None,
                }
            ])
            return

        if "/steps" in self.path:
            sid = self.path.split("/")[3]
            with owner.lock:
                steps = [
                    s for s in owner.recorded_steps
                    if s.get("session_id") == sid
                ]
            self._json(200, steps)
            return

        self._json(404, {"error": f"unhandled GET {self.path}"})

    def _json(self, status: int, payload: Any) -> None:
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args, **_kwargs) -> None:  # silence
        pass


class _StubServer(HTTPServer):
    """:class:`HTTPServer` subclass that carries the recording state."""

    def __init__(self, addr) -> None:
        super().__init__(addr, _StubHandler)
        self.lock = threading.Lock()
        self.sessions: dict[str, dict[str, Any]] = {}
        self.sessions_by_client_key: dict[str, tuple[str, str]] = {}
        self.recorded_steps: list[dict[str, Any]] = []


class StubRewindServer:
    """In-process stub of the Rewind HTTP API for unit tests.

    Bind on a random localhost port. Use as a context manager:

        with StubRewindServer() as server:
            client = ExplicitClient(server.base_url)
            ...

    Inspectable state:
        * ``server.base_url`` — pass to :class:`ExplicitClient`.
        * ``server.recorded_steps`` — list of step dicts as recorded.
        * ``server.sessions`` — mapping of ``session_id`` to session dict.
    """

    def __init__(self) -> None:
        self._server = _StubServer(("127.0.0.1", 0))
        self.base_url = f"http://127.0.0.1:{self._server.server_address[1]}"
        self._thread = threading.Thread(
            target=self._server.serve_forever,
            daemon=True,
        )

    def __enter__(self) -> "StubRewindServer":
        self._thread.start()
        return self

    def __exit__(self, exc_type, exc, tb) -> None:
        self._server.shutdown()
        self._server.server_close()
        # Join the worker thread so a test asserting "port is free /
        # serve_forever has returned" right after exit doesn't race.
        # serve_forever has already returned by the time shutdown() is
        # done, so the join is bounded; the timeout is defensive.
        self._thread.join(timeout=2.0)

    @property
    def recorded_steps(self) -> list[dict[str, Any]]:
        return self._server.recorded_steps

    @property
    def sessions(self) -> dict[str, dict[str, Any]]:
        return self._server.sessions
