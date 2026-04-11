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

    def get_json(self, h: str):
        """Retrieve a JSON object by hash. Returns None if hash is empty or file missing."""
        if not h:
            return None
        try:
            data = self.get(h)
            return json.loads(data)
        except (FileNotFoundError, json.JSONDecodeError):
            return None

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

CREATE TABLE IF NOT EXISTS datasets (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 1,
    example_count INTEGER NOT NULL DEFAULT 0,
    metadata TEXT NOT NULL DEFAULT '{}'
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_datasets_name_version
    ON datasets(name, version);

CREATE TABLE IF NOT EXISTS dataset_examples (
    id TEXT PRIMARY KEY,
    dataset_id TEXT NOT NULL REFERENCES datasets(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    input_blob TEXT NOT NULL,
    expected_blob TEXT NOT NULL DEFAULT '',
    metadata TEXT NOT NULL DEFAULT '{}',
    source_session_id TEXT,
    source_step_id TEXT,
    created_at TEXT NOT NULL,
    UNIQUE(dataset_id, ordinal)
);
CREATE INDEX IF NOT EXISTS idx_dataset_examples_dataset
    ON dataset_examples(dataset_id, ordinal);

CREATE TABLE IF NOT EXISTS evaluators (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    evaluator_type TEXT NOT NULL,
    config_blob TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT ''
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_evaluators_name ON evaluators(name);

CREATE TABLE IF NOT EXISTS experiments (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    dataset_version INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    created_at TEXT NOT NULL,
    completed_at TEXT,
    total_examples INTEGER NOT NULL DEFAULT 0,
    completed_examples INTEGER NOT NULL DEFAULT 0,
    avg_score REAL,
    min_score REAL,
    max_score REAL,
    pass_rate REAL,
    total_duration_ms INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    config_blob TEXT NOT NULL DEFAULT '',
    metadata TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_experiments_dataset ON experiments(dataset_id);
CREATE INDEX IF NOT EXISTS idx_experiments_name ON experiments(name);

CREATE TABLE IF NOT EXISTS experiment_results (
    id TEXT PRIMARY KEY,
    experiment_id TEXT NOT NULL REFERENCES experiments(id) ON DELETE CASCADE,
    example_id TEXT NOT NULL REFERENCES dataset_examples(id),
    ordinal INTEGER NOT NULL,
    output_blob TEXT NOT NULL DEFAULT '',
    trace_session_id TEXT,
    trace_timeline_id TEXT,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    tokens_in INTEGER NOT NULL DEFAULT 0,
    tokens_out INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'pending',
    error TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_experiment_results_experiment
    ON experiment_results(experiment_id, ordinal);

CREATE TABLE IF NOT EXISTS experiment_scores (
    id TEXT PRIMARY KEY,
    result_id TEXT NOT NULL REFERENCES experiment_results(id) ON DELETE CASCADE,
    evaluator_id TEXT NOT NULL REFERENCES evaluators(id),
    score REAL NOT NULL,
    passed INTEGER NOT NULL,
    reasoning TEXT NOT NULL DEFAULT '',
    metadata TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_experiment_scores_result
    ON experiment_scores(result_id);
CREATE INDEX IF NOT EXISTS idx_experiment_scores_evaluator
    ON experiment_scores(evaluator_id);

CREATE TABLE IF NOT EXISTS spans (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    timeline_id TEXT NOT NULL REFERENCES timelines(id),
    parent_span_id TEXT REFERENCES spans(id),
    span_type TEXT NOT NULL,
    name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'running',
    started_at TEXT NOT NULL,
    ended_at TEXT,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    metadata TEXT NOT NULL DEFAULT '{}',
    error TEXT
);

CREATE INDEX IF NOT EXISTS idx_spans_session ON spans(session_id);
CREATE INDEX IF NOT EXISTS idx_spans_timeline ON spans(timeline_id);
CREATE INDEX IF NOT EXISTS idx_spans_parent ON spans(parent_span_id);
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
            root = os.environ.get("REWIND_DATA") or os.path.join(os.path.expanduser("~"), ".rewind")
        os.makedirs(root, exist_ok=True)

        db_path = os.path.join(root, "rewind.db")
        self._conn = sqlite3.connect(db_path, check_same_thread=False)
        self._conn.execute("PRAGMA journal_mode=WAL")
        self._conn.execute("PRAGMA foreign_keys=ON")
        self._conn.execute("PRAGMA busy_timeout=5000")
        self._conn.executescript(_SCHEMA)

        # v0.5 migrations: multi-agent tracing columns
        try:
            self._conn.execute("ALTER TABLE steps ADD COLUMN span_id TEXT")
        except sqlite3.OperationalError:
            pass  # column already exists
        try:
            self._conn.execute("ALTER TABLE sessions ADD COLUMN thread_id TEXT")
        except sqlite3.OperationalError:
            pass
        try:
            self._conn.execute("ALTER TABLE sessions ADD COLUMN thread_ordinal INTEGER")
        except sqlite3.OperationalError:
            pass
        try:
            self._conn.execute("CREATE INDEX IF NOT EXISTS idx_sessions_thread ON sessions(thread_id, thread_ordinal)")
        except sqlite3.OperationalError:
            pass

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
        span_id: str = None,
    ) -> str:
        """Insert a step record. Returns the step ID."""
        step_id = _new_id()
        now = _now_rfc3339()

        self._conn.execute(
            "INSERT INTO steps (id, timeline_id, session_id, step_number, step_type, status, "
            "created_at, duration_ms, tokens_in, tokens_out, model, request_blob, response_blob, error, span_id) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (step_id, timeline_id, session_id, step_number, step_type, status,
             now, duration_ms, tokens_in, tokens_out, model,
             request_blob, response_blob, error, span_id),
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
            "duration_ms, tokens_in, tokens_out, model, request_blob, response_blob, error, span_id "
            "FROM steps WHERE timeline_id = ? ORDER BY step_number", (timeline_id,)
        ).fetchall()
        return [
            {"id": r[0], "timeline_id": r[1], "session_id": r[2],
             "step_number": r[3], "step_type": r[4], "status": r[5],
             "duration_ms": r[6], "tokens_in": r[7], "tokens_out": r[8],
             "model": r[9], "request_blob": r[10], "response_blob": r[11],
             "error": r[12], "span_id": r[13]}
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

    # ── Evaluation: Datasets ─────────────────────────────────

    def get_dataset_by_name(self, name: str) -> dict | None:
        """Return the latest version of a dataset by name, or None."""
        row = self._conn.execute(
            "SELECT id, name, description, created_at, updated_at, version, example_count, metadata "
            "FROM datasets WHERE name = ? ORDER BY version DESC LIMIT 1",
            (name,),
        ).fetchone()
        if not row:
            return None
        return {
            "id": row[0], "name": row[1], "description": row[2],
            "created_at": row[3], "updated_at": row[4], "version": row[5],
            "example_count": row[6], "metadata": row[7],
        }

    def get_dataset_examples(self, dataset_id: str) -> list:
        """Return all examples for a dataset, ordered by ordinal."""
        rows = self._conn.execute(
            "SELECT id, dataset_id, ordinal, input_blob, expected_blob, metadata, "
            "source_session_id, source_step_id, created_at "
            "FROM dataset_examples WHERE dataset_id = ? ORDER BY ordinal",
            (dataset_id,),
        ).fetchall()
        return [
            {
                "id": r[0], "dataset_id": r[1], "ordinal": r[2],
                "input_blob": r[3], "expected_blob": r[4], "metadata": r[5],
                "source_session_id": r[6], "source_step_id": r[7],
                "created_at": r[8],
            }
            for r in rows
        ]

    # ── Evaluation: Evaluators ─────────────────────────────────

    def create_evaluator(self, evaluator_id: str, name: str, evaluator_type: str,
                         config_blob: str = "", description: str = ""):
        """Register an evaluator in the database."""
        now = _now_rfc3339()
        with self._lock:
            self._conn.execute(
                "INSERT OR IGNORE INTO evaluators "
                "(id, name, evaluator_type, config_blob, created_at, description) "
                "VALUES (?, ?, ?, ?, ?, ?)",
                (evaluator_id, name, evaluator_type, config_blob, now, description),
            )
            self._conn.commit()

    def get_evaluator_by_name(self, name: str) -> dict | None:
        """Return an evaluator by name, or None."""
        row = self._conn.execute(
            "SELECT id, name, evaluator_type, config_blob, created_at, description "
            "FROM evaluators WHERE name = ?",
            (name,),
        ).fetchone()
        if not row:
            return None
        return {
            "id": row[0], "name": row[1], "evaluator_type": row[2],
            "config_blob": row[3], "created_at": row[4], "description": row[5],
        }

    # ── Evaluation: Experiments ─────────────────────────────────

    def create_experiment(self, experiment_id: str, name: str, dataset_id: str,
                          dataset_version: int, total_examples: int,
                          config_blob: str = "", metadata: str = "{}"):
        """Create a new experiment record."""
        now = _now_rfc3339()
        with self._lock:
            self._conn.execute(
                "INSERT INTO experiments "
                "(id, name, dataset_id, dataset_version, status, created_at, "
                "total_examples, completed_examples, config_blob, metadata) "
                "VALUES (?, ?, ?, ?, 'running', ?, ?, 0, ?, ?)",
                (experiment_id, name, dataset_id, dataset_version,
                 now, total_examples, config_blob, metadata),
            )
            self._conn.commit()

    def update_experiment_status(self, experiment_id: str, status: str):
        """Update experiment status ('running', 'completed', 'failed')."""
        now = _now_rfc3339()
        completed_at = now if status in ("completed", "failed") else None
        with self._lock:
            self._conn.execute(
                "UPDATE experiments SET status = ?, completed_at = ? WHERE id = ?",
                (status, completed_at, experiment_id),
            )
            self._conn.commit()

    def update_experiment_progress(self, experiment_id: str, completed_examples: int):
        """Update the number of completed examples."""
        with self._lock:
            self._conn.execute(
                "UPDATE experiments SET completed_examples = ? WHERE id = ?",
                (completed_examples, experiment_id),
            )
            self._conn.commit()

    def update_experiment_aggregates(self, experiment_id: str, avg_score: float,
                                     min_score: float, max_score: float,
                                     pass_rate: float, total_duration_ms: int,
                                     total_tokens: int = 0):
        """Update experiment aggregate statistics."""
        with self._lock:
            self._conn.execute(
                "UPDATE experiments SET avg_score = ?, min_score = ?, max_score = ?, "
                "pass_rate = ?, total_duration_ms = ?, total_tokens = ? WHERE id = ?",
                (avg_score, min_score, max_score, pass_rate,
                 total_duration_ms, total_tokens, experiment_id),
            )
            self._conn.commit()

    def get_experiment(self, experiment_id: str) -> dict | None:
        """Return an experiment by ID, or None."""
        row = self._conn.execute(
            "SELECT id, name, dataset_id, dataset_version, status, created_at, "
            "completed_at, total_examples, completed_examples, avg_score, "
            "min_score, max_score, pass_rate, total_duration_ms, total_tokens, "
            "config_blob, metadata "
            "FROM experiments WHERE id = ?",
            (experiment_id,),
        ).fetchone()
        if not row:
            return None
        return {
            "id": row[0], "name": row[1], "dataset_id": row[2],
            "dataset_version": row[3], "status": row[4],
            "created_at": row[5], "completed_at": row[6],
            "total_examples": row[7], "completed_examples": row[8],
            "avg_score": row[9], "min_score": row[10], "max_score": row[11],
            "pass_rate": row[12], "total_duration_ms": row[13],
            "total_tokens": row[14], "config_blob": row[15],
            "metadata": row[16],
        }

    def get_experiment_by_name(self, name: str) -> dict | None:
        """Return the latest experiment with the given name, or None."""
        row = self._conn.execute(
            "SELECT id, name, dataset_id, dataset_version, status, created_at, "
            "completed_at, total_examples, completed_examples, avg_score, "
            "min_score, max_score, pass_rate, total_duration_ms, total_tokens, "
            "config_blob, metadata "
            "FROM experiments WHERE name = ? ORDER BY created_at DESC LIMIT 1",
            (name,),
        ).fetchone()
        if not row:
            return None
        return {
            "id": row[0], "name": row[1], "dataset_id": row[2],
            "dataset_version": row[3], "status": row[4],
            "created_at": row[5], "completed_at": row[6],
            "total_examples": row[7], "completed_examples": row[8],
            "avg_score": row[9], "min_score": row[10], "max_score": row[11],
            "pass_rate": row[12], "total_duration_ms": row[13],
            "total_tokens": row[14], "config_blob": row[15],
            "metadata": row[16],
        }

    # ── Evaluation: Experiment Results ──────────────────────────

    def create_experiment_result(self, result_id: str, experiment_id: str,
                                 example_id: str, ordinal: int,
                                 output_blob: str = "", duration_ms: int = 0,
                                 tokens_in: int = 0, tokens_out: int = 0,
                                 status: str = "success", error: str = None):
        """Create an experiment result record for a single example."""
        now = _now_rfc3339()
        with self._lock:
            self._conn.execute(
                "INSERT INTO experiment_results "
                "(id, experiment_id, example_id, ordinal, output_blob, "
                "duration_ms, tokens_in, tokens_out, status, error, created_at) "
                "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                (result_id, experiment_id, example_id, ordinal, output_blob,
                 duration_ms, tokens_in, tokens_out, status, error, now),
            )
            self._conn.commit()

    def get_experiment_results(self, experiment_id: str) -> list:
        """Return all results for an experiment, ordered by ordinal."""
        rows = self._conn.execute(
            "SELECT id, experiment_id, example_id, ordinal, output_blob, "
            "trace_session_id, trace_timeline_id, duration_ms, tokens_in, "
            "tokens_out, status, error, created_at "
            "FROM experiment_results WHERE experiment_id = ? ORDER BY ordinal",
            (experiment_id,),
        ).fetchall()
        return [
            {
                "id": r[0], "experiment_id": r[1], "example_id": r[2],
                "ordinal": r[3], "output_blob": r[4],
                "trace_session_id": r[5], "trace_timeline_id": r[6],
                "duration_ms": r[7], "tokens_in": r[8], "tokens_out": r[9],
                "status": r[10], "error": r[11], "created_at": r[12],
            }
            for r in rows
        ]

    # ── Evaluation: Experiment Scores ──────────────────────────

    def create_experiment_score(self, score_id: str, result_id: str,
                                evaluator_id: str, score: float, passed: bool,
                                reasoning: str = "", metadata: str = "{}"):
        """Create a score record for an evaluator on an experiment result."""
        now = _now_rfc3339()
        with self._lock:
            self._conn.execute(
                "INSERT INTO experiment_scores "
                "(id, result_id, evaluator_id, score, passed, reasoning, metadata, created_at) "
                "VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                (score_id, result_id, evaluator_id, score,
                 1 if passed else 0, reasoning, metadata, now),
            )
            self._conn.commit()

    def get_experiment_scores(self, result_id: str) -> list:
        """Return all scores for an experiment result."""
        rows = self._conn.execute(
            "SELECT id, result_id, evaluator_id, score, passed, reasoning, "
            "metadata, created_at "
            "FROM experiment_scores WHERE result_id = ?",
            (result_id,),
        ).fetchall()
        return [
            {
                "id": r[0], "result_id": r[1], "evaluator_id": r[2],
                "score": r[3], "passed": bool(r[4]), "reasoning": r[5],
                "metadata": r[6], "created_at": r[7],
            }
            for r in rows
        ]

    # ── Spans ─────────────────────────────────────────────────

    def create_span(
        self,
        session_id: str,
        timeline_id: str,
        span_type: str,
        name: str,
        parent_span_id: str = None,
        metadata: str = "{}",
    ) -> str:
        """Create a span record. Returns the span ID."""
        span_id = _new_id()
        now = _now_rfc3339()
        with self._lock:
            self._conn.execute(
                "INSERT INTO spans (id, session_id, timeline_id, parent_span_id, span_type, "
                "name, status, started_at, ended_at, duration_ms, metadata, error) "
                "VALUES (?, ?, ?, ?, ?, ?, 'running', ?, NULL, 0, ?, NULL)",
                (span_id, session_id, timeline_id, parent_span_id, span_type,
                 name, now, metadata),
            )
            self._conn.commit()
        return span_id

    def update_span_status(
        self,
        span_id: str,
        status: str,
        duration_ms: int = 0,
        error: str = None,
    ):
        """Update span status, duration, and optional error."""
        now = _now_rfc3339()
        ended_at = now if status in ("completed", "error") else None
        with self._lock:
            self._conn.execute(
                "UPDATE spans SET status = ?, ended_at = ?, duration_ms = ?, error = ? WHERE id = ?",
                (status, ended_at, duration_ms, error, span_id),
            )
            self._conn.commit()

    def get_span(self, span_id: str) -> dict | None:
        """Return a span by ID, or None."""
        row = self._conn.execute(
            "SELECT id, session_id, timeline_id, parent_span_id, span_type, name, "
            "status, started_at, ended_at, duration_ms, metadata, error "
            "FROM spans WHERE id = ?",
            (span_id,),
        ).fetchone()
        if not row:
            return None
        return self._row_to_span(row)

    def get_spans_by_session(self, session_id: str) -> list[dict]:
        """Return all spans for a session, ordered by start time."""
        rows = self._conn.execute(
            "SELECT id, session_id, timeline_id, parent_span_id, span_type, name, "
            "status, started_at, ended_at, duration_ms, metadata, error "
            "FROM spans WHERE session_id = ? ORDER BY started_at",
            (session_id,),
        ).fetchall()
        return [self._row_to_span(r) for r in rows]

    def get_spans_by_timeline(self, timeline_id: str) -> list[dict]:
        """Return all spans for a timeline, ordered by start time."""
        rows = self._conn.execute(
            "SELECT id, session_id, timeline_id, parent_span_id, span_type, name, "
            "status, started_at, ended_at, duration_ms, metadata, error "
            "FROM spans WHERE timeline_id = ? ORDER BY started_at",
            (timeline_id,),
        ).fetchall()
        return [self._row_to_span(r) for r in rows]

    def get_child_spans(self, parent_span_id: str) -> list[dict]:
        """Return child spans of a given span."""
        rows = self._conn.execute(
            "SELECT id, session_id, timeline_id, parent_span_id, span_type, name, "
            "status, started_at, ended_at, duration_ms, metadata, error "
            "FROM spans WHERE parent_span_id = ? ORDER BY started_at",
            (parent_span_id,),
        ).fetchall()
        return [self._row_to_span(r) for r in rows]

    def get_steps_by_span(self, span_id: str) -> list[dict]:
        """Return all steps linked to a specific span."""
        rows = self._conn.execute(
            "SELECT id, timeline_id, session_id, step_number, step_type, status, "
            "duration_ms, tokens_in, tokens_out, model, request_blob, response_blob, error, span_id "
            "FROM steps WHERE span_id = ? ORDER BY step_number",
            (span_id,),
        ).fetchall()
        return [
            {"id": r[0], "timeline_id": r[1], "session_id": r[2],
             "step_number": r[3], "step_type": r[4], "status": r[5],
             "duration_ms": r[6], "tokens_in": r[7], "tokens_out": r[8],
             "model": r[9], "request_blob": r[10], "response_blob": r[11],
             "error": r[12], "span_id": r[13]}
            for r in rows
        ]

    def update_step_span_id(self, step_id: str, span_id: str):
        """Link a step to a span."""
        with self._lock:
            self._conn.execute(
                "UPDATE steps SET span_id = ? WHERE id = ?",
                (span_id, step_id),
            )
            self._conn.commit()

    # ── Threads (via session columns) ─────────────────────────

    def get_sessions_by_thread(self, thread_id: str) -> list[dict]:
        """Return all sessions in a thread, ordered by ordinal."""
        rows = self._conn.execute(
            "SELECT id, name, status, total_steps, total_tokens, thread_id, thread_ordinal "
            "FROM sessions WHERE thread_id = ? ORDER BY thread_ordinal, created_at",
            (thread_id,),
        ).fetchall()
        return [
            {"id": r[0], "name": r[1], "status": r[2],
             "total_steps": r[3], "total_tokens": r[4],
             "thread_id": r[5], "thread_ordinal": r[6]}
            for r in rows
        ]

    def list_thread_ids(self) -> list[str]:
        """Return all distinct thread IDs."""
        rows = self._conn.execute(
            "SELECT DISTINCT thread_id FROM sessions WHERE thread_id IS NOT NULL ORDER BY thread_id"
        ).fetchall()
        return [r[0] for r in rows]

    def set_session_thread(self, session_id: str, thread_id: str, ordinal: int):
        """Set the thread_id and thread_ordinal for a session."""
        with self._lock:
            self._conn.execute(
                "UPDATE sessions SET thread_id = ?, thread_ordinal = ? WHERE id = ?",
                (thread_id, ordinal, session_id),
            )
            self._conn.commit()

    def _row_to_span(self, r) -> dict:
        """Convert a span row tuple to a dict."""
        return {
            "id": r[0], "session_id": r[1], "timeline_id": r[2],
            "parent_span_id": r[3], "span_type": r[4], "name": r[5],
            "status": r[6], "started_at": r[7], "ended_at": r[8],
            "duration_ms": r[9], "metadata": r[10], "error": r[11],
        }

    def close(self):
        """Close the database connection."""
        try:
            self._conn.close()
        except Exception:
            pass
