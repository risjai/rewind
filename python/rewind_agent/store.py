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
    total_cost_usd REAL NOT NULL DEFAULT 0.0,
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
    cost_usd REAL NOT NULL DEFAULT 0.0,
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
    original_cost_usd REAL NOT NULL DEFAULT 0.0,
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
                "INSERT INTO sessions (id, name, created_at, updated_at, status, total_steps, total_cost_usd, total_tokens, metadata) "
                "VALUES (?, ?, ?, ?, 'recording', 0, 0.0, 0, '{}')",
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
        cost_usd: float,
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
            "created_at, duration_ms, tokens_in, tokens_out, cost_usd, model, request_blob, response_blob, error) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (step_id, timeline_id, session_id, step_number, step_type, status,
             now, duration_ms, tokens_in, tokens_out, cost_usd, model,
             request_blob, response_blob, error),
        )
        self._conn.commit()
        return step_id

    def update_session_stats(self, session_id: str, total_steps: int, cost: float, tokens: int):
        """Update session aggregate stats."""
        now = _now_rfc3339()
        self._conn.execute(
            "UPDATE sessions SET total_steps = ?, total_cost_usd = total_cost_usd + ?, "
            "total_tokens = total_tokens + ?, updated_at = ? WHERE id = ?",
            (total_steps, cost, tokens, now, session_id),
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

    def close(self):
        """Close the database connection."""
        try:
            self._conn.close()
        except Exception:
            pass
