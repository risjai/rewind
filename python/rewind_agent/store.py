"""
Pure Python store — format-compatible with the Rust rewind-store crate.

Writes to the same SQLite database (~/.rewind/rewind.db) and content-addressed
blob store (~/.rewind/objects/) that the Rust CLI, TUI, and MCP server read.

Zero external dependencies — uses only Python stdlib (sqlite3, hashlib, json).
"""

import hashlib
import json
import os
import sqlite3
import threading
import uuid
from datetime import datetime, timezone


# ── Blob Store ────────────────────────────────────────────────

class BlobStore:
    """Content-addressed blob store (like git objects). SHA-256 hashing."""

    def __init__(self, root: str):
        self._root = root
        os.makedirs(root, exist_ok=True)

    def put(self, data: bytes) -> str:
        """Store data and return its SHA-256 hex hash."""
        h = hashlib.sha256(data).hexdigest()
        path = self._blob_path(h)
        if not os.path.exists(path):
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "wb") as f:
                f.write(data)
        return h

    def get(self, h: str) -> bytes:
        """Retrieve data by hash."""
        with open(self._blob_path(h), "rb") as f:
            return f.read()

    def put_json(self, obj) -> str:
        """Store a JSON-serializable object. Uses compact format to match Rust's serde_json::to_vec."""
        data = json.dumps(obj, separators=(",", ":"), default=str).encode("utf-8")
        return self.put(data)

    def _blob_path(self, h: str) -> str:
        """Path: {root}/{first 2 hex chars}/{remaining hex chars}"""
        if len(h) < 3:
            return os.path.join(self._root, "_invalid", h)
        return os.path.join(self._root, h[:2], h[2:])


# ── Store ─────────────────────────────────────────────────────

_SCHEMA = """
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'recording',
    total_steps INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    metadata TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS timelines (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    parent_timeline_id TEXT REFERENCES timelines(id),
    fork_at_step INTEGER,
    created_at TEXT NOT NULL,
    label TEXT NOT NULL DEFAULT 'main'
);

CREATE TABLE IF NOT EXISTS steps (
    id TEXT PRIMARY KEY,
    timeline_id TEXT NOT NULL REFERENCES timelines(id),
    session_id TEXT NOT NULL REFERENCES sessions(id),
    step_number INTEGER NOT NULL,
    step_type TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    created_at TEXT NOT NULL,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    tokens_in INTEGER NOT NULL DEFAULT 0,
    tokens_out INTEGER NOT NULL DEFAULT 0,
    model TEXT NOT NULL DEFAULT '',
    request_blob TEXT NOT NULL DEFAULT '',
    response_blob TEXT NOT NULL DEFAULT '',
    error TEXT
);

CREATE INDEX IF NOT EXISTS idx_steps_timeline ON steps(timeline_id, step_number);
CREATE INDEX IF NOT EXISTS idx_steps_session ON steps(session_id);
CREATE INDEX IF NOT EXISTS idx_timelines_session ON timelines(session_id);

CREATE TABLE IF NOT EXISTS replay_cache (
    request_hash TEXT PRIMARY KEY,
    response_blob TEXT NOT NULL,
    model TEXT NOT NULL DEFAULT '',
    tokens_in INTEGER NOT NULL DEFAULT 0,
    tokens_out INTEGER NOT NULL DEFAULT 0,
    hit_count INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    last_hit_at TEXT
);

CREATE TABLE IF NOT EXISTS snapshots (
    id TEXT PRIMARY KEY,
    session_id TEXT,
    label TEXT NOT NULL,
    directory TEXT NOT NULL,
    blob_hash TEXT NOT NULL,
    file_count INTEGER NOT NULL DEFAULT 0,
    size_bytes INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS baselines (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    source_session_id TEXT NOT NULL REFERENCES sessions(id),
    source_timeline_id TEXT NOT NULL REFERENCES timelines(id),
    created_at TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    step_count INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    metadata TEXT NOT NULL DEFAULT '{}'
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_baselines_name ON baselines(name);

CREATE TABLE IF NOT EXISTS baseline_steps (
    id TEXT PRIMARY KEY,
    baseline_id TEXT NOT NULL REFERENCES baselines(id) ON DELETE CASCADE,
    step_number INTEGER NOT NULL,
    step_type TEXT NOT NULL,
    expected_status TEXT NOT NULL,
    expected_model TEXT NOT NULL DEFAULT '',
    tokens_in INTEGER NOT NULL DEFAULT 0,
    tokens_out INTEGER NOT NULL DEFAULT 0,
    tool_name TEXT,
    response_blob TEXT NOT NULL DEFAULT '',
    request_blob TEXT NOT NULL DEFAULT '',
    has_error INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_baseline_steps_baseline
    ON baseline_steps(baseline_id, step_number);
"""


def _now_rfc3339() -> str:
    return datetime.now(timezone.utc).isoformat()


def _new_id() -> str:
    return str(uuid.uuid4())


class Store:
    """
    Pure Python store that writes to the same format as the Rust rewind-store crate.
    Thread-safe — all writes are serialized via a lock.
    """

    def __init__(self, root: str = None):
        if root is None:
            root = os.path.join(os.path.expanduser("~"), ".rewind")
        os.makedirs(root, exist_ok=True)

        db_path = os.path.join(root, "rewind.db")
        self._conn = sqlite3.connect(db_path, check_same_thread=False)
        self._conn.execute("PRAGMA journal_mode=WAL")
        self._conn.execute("PRAGMA foreign_keys=ON")
        self._conn.execute("PRAGMA busy_timeout=5000")
        self._conn.executescript(_SCHEMA)

        self.blobs = BlobStore(os.path.join(root, "objects"))
        self._lock = threading.Lock()

    def create_session(self, name: str = "default") -> tuple:
        """
        Create a new session and its root timeline atomically.
        Returns (session_id, timeline_id).
        """
        session_id = _new_id()
        timeline_id = _new_id()
        now = _now_rfc3339()

        with self._lock:
            self._conn.execute(
                "INSERT INTO sessions (id, name, created_at, updated_at, status, total_steps, total_tokens, metadata) "
                "VALUES (?, ?, ?, ?, 'recording', 0, 0, '{}')",
                (session_id, name, now, now),
            )
            self._conn.execute(
                "INSERT INTO timelines (id, session_id, parent_timeline_id, fork_at_step, created_at, label) "
                "VALUES (?, ?, NULL, NULL, ?, 'main')",
                (timeline_id, session_id, now),
            )
            self._conn.commit()

        return session_id, timeline_id

    def create_step(
        self,
        session_id: str,
        timeline_id: str,
        step_number: int,
        step_type: str,
        status: str,
        model: str,
        duration_ms: int,
        tokens_in: int,
        tokens_out: int,
        request_blob: str,
        response_blob: str,
        error: str = None,
    ) -> str:
        """Insert a step record. Returns the step ID."""
        step_id = _new_id()
        now = _now_rfc3339()

        # Lock is expected to be held by the caller (Recorder._record_call)
        self._conn.execute(
            "INSERT INTO steps (id, timeline_id, session_id, step_number, step_type, status, "
            "created_at, duration_ms, tokens_in, tokens_out, model, request_blob, response_blob, error) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (step_id, timeline_id, session_id, step_number, step_type, status,
             now, duration_ms, tokens_in, tokens_out, model,
             request_blob, response_blob, error),
        )
        self._conn.commit()
        return step_id

    def update_session_stats(self, session_id: str, total_steps: int, tokens: int):
        """Update session aggregate stats."""
        now = _now_rfc3339()
        self._conn.execute(
            "UPDATE sessions SET total_steps = ?, "
            "total_tokens = total_tokens + ?, updated_at = ? WHERE id = ?",
            (total_steps, tokens, now, session_id),
        )
        self._conn.commit()

    def update_session_status(self, session_id: str, status: str):
        """Update session status ('recording', 'completed', 'failed')."""
        now = _now_rfc3339()
        self._conn.execute(
            "UPDATE sessions SET status = ?, updated_at = ? WHERE id = ?",
            (status, now, session_id),
        )
        self._conn.commit()

    # ── Query methods for replay ────────────────────────────────

    def get_latest_session(self) -> dict | None:
        """Return the most recent session as a dict, or None."""
        row = self._conn.execute(
            "SELECT id, name, status, total_steps, total_tokens "
            "FROM sessions ORDER BY created_at DESC LIMIT 1"
        ).fetchone()
        if not row:
            return None
        return {"id": row[0], "name": row[1], "status": row[2],
                "total_steps": row[3], "total_tokens": row[4]}

    def get_session(self, session_ref: str) -> dict | None:
        """Resolve a session by ID, prefix, or 'latest'."""
        if session_ref == "latest":
            return self.get_latest_session()
        row = self._conn.execute(
            "SELECT id, name, status, total_steps, total_tokens "
            "FROM sessions WHERE id = ?", (session_ref,)
        ).fetchone()
        if row:
            return {"id": row[0], "name": row[1], "status": row[2],
                    "total_steps": row[3], "total_tokens": row[4]}
        # prefix match
        row = self._conn.execute(
            "SELECT id, name, status, total_steps, total_tokens "
            "FROM sessions WHERE id LIKE ? ORDER BY created_at DESC LIMIT 1",
            (session_ref + "%",)
        ).fetchone()
        if row:
            return {"id": row[0], "name": row[1], "status": row[2],
                    "total_steps": row[3], "total_tokens": row[4]}
        return None

    def get_root_timeline(self, session_id: str) -> dict | None:
        """Return the root timeline for a session."""
        row = self._conn.execute(
            "SELECT id, session_id, parent_timeline_id, fork_at_step, label "
            "FROM timelines WHERE session_id = ? AND parent_timeline_id IS NULL "
            "ORDER BY created_at LIMIT 1", (session_id,)
        ).fetchone()
        if not row:
            return None
        return {"id": row[0], "session_id": row[1],
                "parent_timeline_id": row[2], "fork_at_step": row[3], "label": row[4]}

    def get_steps(self, timeline_id: str) -> list[dict]:
        """Return all steps for a timeline, ordered by step_number."""
        rows = self._conn.execute(
            "SELECT id, timeline_id, session_id, step_number, step_type, status, "
            "duration_ms, tokens_in, tokens_out, model, request_blob, response_blob, error "
            "FROM steps WHERE timeline_id = ? ORDER BY step_number", (timeline_id,)
        ).fetchall()
        return [
            {"id": r[0], "timeline_id": r[1], "session_id": r[2],
             "step_number": r[3], "step_type": r[4], "status": r[5],
             "duration_ms": r[6], "tokens_in": r[7], "tokens_out": r[8],
             "model": r[9], "request_blob": r[10], "response_blob": r[11],
             "error": r[12]}
            for r in rows
        ]

    def get_full_timeline_steps(self, timeline_id: str, session_id: str) -> list[dict]:
        """Get all steps for a timeline, including inherited parent steps for forks."""
        row = self._conn.execute(
            "SELECT parent_timeline_id, fork_at_step FROM timelines WHERE id = ?",
            (timeline_id,)
        ).fetchone()
        if row and row[0] is not None and row[1] is not None:
            parent_id, fork_at = row[0], row[1]
            parent_steps = [s for s in self.get_steps(parent_id) if s["step_number"] <= fork_at]
            own_steps = self.get_steps(timeline_id)
            combined = parent_steps + own_steps
            combined.sort(key=lambda s: s["step_number"])
            return combined
        return self.get_steps(timeline_id)

    def create_fork_timeline(self, session_id: str, parent_timeline_id: str,
                             fork_at_step: int, label: str = "replayed") -> str:
        """Create a forked timeline. Returns the new timeline ID."""
        timeline_id = _new_id()
        now = _now_rfc3339()
        with self._lock:
            self._conn.execute(
                "INSERT INTO timelines (id, session_id, parent_timeline_id, fork_at_step, created_at, label) "
                "VALUES (?, ?, ?, ?, ?, ?)",
                (timeline_id, session_id, parent_timeline_id, fork_at_step, now, label),
            )
            self._conn.commit()
        return timeline_id

    def close(self):
        """Close the database connection."""
        try:
            self._conn.close()
        except Exception:
            pass
