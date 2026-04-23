use anyhow::Result;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};

use crate::blobs::BlobStore;
use crate::models::*;

pub struct Store {
    conn: Connection,
    pub blobs: BlobStore,
    _root: PathBuf,
}

impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)?;

        // Harden directory and file permissions. See docs/security-audit.md §HIGH-03.
        // ~/.rewind/ contains full LLM conversations that may include API keys, PII,
        // and proprietary code — restrict to owner-only access.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_perms = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(root, dir_perms)?;

            // Validate directory ownership: refuse to use a data dir owned by
            // another user (guards against REWIND_DATA hijack — see LOW-07).
            validate_owner(root)?;
        }

        let db_path = root.join("rewind.db");
        let blobs_path = root.join("objects");

        let conn = Connection::open(&db_path).map_err(|e| {
            anyhow::anyhow!(
                "Failed to open database at {path}: {e}.\n\
                 If this persists, back up and delete {path} and retry.",
                path = db_path.display(), e = e
            )
        })?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| {
                anyhow::anyhow!(
                    "Database at {path} appears corrupted: {e}.\n\
                     Back up and delete {path} to start fresh.",
                    path = db_path.display(), e = e
                )
            })?;

        // Tighten DB file to 0600 (owner read/write only).
        // WAL/SHM files (-wal, -shm) are created by SQLite at its default umask;
        // the parent dir's 0700 is the intended defense for those.
        #[cfg(unix)]
        if db_path.exists() {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o600))?;
        }

        let blobs = BlobStore::new(&blobs_path)?;

        let store = Store {
            conn,
            blobs,
            _root: root.to_path_buf(),
        };
        store.migrate()?;
        Ok(store)
    }

    /// Open with default path (~/.rewind/)
    pub fn open_default() -> Result<Self> {
        let home = dirs_path();
        Self::open(&home)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "
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

            -- Instant Replay: cache successful LLM responses by request hash
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

            -- Snapshots: workspace state captures
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

            -- Assertion baselines: extracted from known-good sessions
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

            -- Baseline steps: expected step signatures for regression checks
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

            -- Evaluation: datasets (versioned test-case collections)
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

            -- Evaluation: dataset examples (input/expected pairs in blob store)
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

            -- Evaluation: evaluators (scoring function definitions)
            CREATE TABLE IF NOT EXISTS evaluators (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                evaluator_type TEXT NOT NULL,
                config_blob TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT ''
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_evaluators_name ON evaluators(name);

            -- Evaluation: experiments (application runs against datasets)
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

            -- Evaluation: per-example results
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

            -- Evaluation: per-evaluator scores
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

            -- Timeline scores (LLM-as-Judge)
            CREATE TABLE IF NOT EXISTS timeline_scores (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                timeline_id TEXT NOT NULL REFERENCES timelines(id),
                evaluator_id TEXT NOT NULL REFERENCES evaluators(id),
                score REAL NOT NULL,
                passed INTEGER NOT NULL,
                reasoning TEXT NOT NULL DEFAULT '',
                input_blob TEXT NOT NULL DEFAULT '',
                output_blob TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL,
                UNIQUE(timeline_id, evaluator_id)
            );
            CREATE INDEX IF NOT EXISTS idx_timeline_scores_session
                ON timeline_scores(session_id);

            -- Multi-agent tracing: span tree
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
            ",
        )?;

        // v0.5 migrations: add columns for multi-agent tracing
        // These are idempotent — silently ignored if columns already exist
        let _ = self.conn.execute("ALTER TABLE steps ADD COLUMN span_id TEXT", []);
        let _ = self.conn.execute("ALTER TABLE sessions ADD COLUMN thread_id TEXT", []);
        let _ = self.conn.execute("ALTER TABLE sessions ADD COLUMN thread_ordinal INTEGER", []);
        let _ = self.conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_sessions_thread ON sessions(thread_id, thread_ordinal)");

        // v0.6 migrations: hooks integration — session source and step tool_name
        let _ = self.conn.execute("ALTER TABLE sessions ADD COLUMN source TEXT NOT NULL DEFAULT 'proxy'", []);
        let _ = self.conn.execute("ALTER TABLE steps ADD COLUMN tool_name TEXT", []);
        let _ = self.conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_steps_session_tool ON steps(session_id, tool_name)");

        // v0.12 migrations: explicit recording API — replay contexts + step counters
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS replay_contexts (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                timeline_id TEXT NOT NULL REFERENCES timelines(id),
                from_step INTEGER NOT NULL,
                current_step INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                last_accessed_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_replay_contexts_session ON replay_contexts(session_id);

            CREATE TABLE IF NOT EXISTS step_counters (
                session_id TEXT NOT NULL,
                timeline_id TEXT NOT NULL,
                counter INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (session_id, timeline_id)
            );
            ",
        )?;

        Ok(())
    }

    // ── Sessions ──────────────────────────────────────────────

    pub fn create_session(&self, session: &Session) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sessions (id, name, created_at, updated_at, status, source, total_steps, total_tokens, metadata, thread_id, thread_ordinal)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                session.id,
                session.name,
                session.created_at.to_rfc3339(),
                session.updated_at.to_rfc3339(),
                session.status.as_str(),
                session.source.as_str(),
                session.total_steps,
                session.total_tokens,
                session.metadata.to_string(),
                session.thread_id,
                session.thread_ordinal,
            ],
        )?;
        Ok(())
    }

    pub fn update_session_stats(&self, session_id: &str, steps: u32, tokens: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET total_steps = ?1, total_tokens = total_tokens + ?2, updated_at = ?3 WHERE id = ?4",
            params![steps, tokens, chrono::Utc::now().to_rfc3339(), session_id],
        )?;
        Ok(())
    }

    /// Set total_tokens to an absolute value (not additive).
    /// Used by transcript sync which computes the full total from the JSONL file.
    pub fn update_session_tokens(&self, session_id: &str, total_tokens: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET total_tokens = ?1, updated_at = ?2 WHERE id = ?3",
            params![total_tokens, chrono::Utc::now().to_rfc3339(), session_id],
        )?;
        Ok(())
    }

    /// Update session metadata JSON.
    pub fn update_session_metadata(&self, session_id: &str, metadata: &serde_json::Value) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET metadata = ?1, updated_at = ?2 WHERE id = ?3",
            params![metadata.to_string(), chrono::Utc::now().to_rfc3339(), session_id],
        )?;
        Ok(())
    }

    pub fn update_session_status(&self, session_id: &str, status: SessionStatus) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status.as_str(), chrono::Utc::now().to_rfc3339(), session_id],
        )?;
        Ok(())
    }

    /// Mark recording sessions as completed if no activity for `stale_after` duration.
    /// Returns the IDs of sessions that were completed.
    pub fn complete_stale_sessions(&self, stale_after: chrono::Duration) -> Result<Vec<String>> {
        let cutoff = (chrono::Utc::now() - stale_after).to_rfc3339();
        let now = chrono::Utc::now().to_rfc3339();

        let mut stmt = self.conn.prepare(
            "SELECT id FROM sessions WHERE status = 'recording' AND updated_at < ?1",
        )?;
        let ids: Vec<String> = stmt
            .query_map(params![cutoff], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;

        if !ids.is_empty() {
            self.conn.execute(
                "UPDATE sessions SET status = 'completed', updated_at = ?1
                 WHERE status = 'recording' AND updated_at < ?2",
                params![now, cutoff],
            )?;
        }
        Ok(ids)
    }

    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, created_at, updated_at, status, source, total_steps, total_tokens, metadata, thread_id, thread_ordinal
             FROM sessions ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], Self::row_to_session)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_session(&self, session_id: &str) -> Result<Option<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, created_at, updated_at, status, source, total_steps, total_tokens, metadata, thread_id, thread_ordinal
             FROM sessions WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![session_id], Self::row_to_session)?;
        Ok(rows.next().transpose()?)
    }

    pub fn get_latest_session(&self) -> Result<Option<Session>> {
        let sessions = self.list_sessions()?;
        Ok(sessions.into_iter().next())
    }

    // ── Timelines ─────────────────────────────────────────────

    pub fn create_timeline(&self, timeline: &Timeline) -> Result<()> {
        self.conn.execute(
            "INSERT INTO timelines (id, session_id, parent_timeline_id, fork_at_step, created_at, label)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                timeline.id,
                timeline.session_id,
                timeline.parent_timeline_id,
                timeline.fork_at_step,
                timeline.created_at.to_rfc3339(),
                timeline.label,
            ],
        )?;
        Ok(())
    }

    pub fn get_timelines(&self, session_id: &str) -> Result<Vec<Timeline>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, parent_timeline_id, fork_at_step, created_at, label
             FROM timelines WHERE session_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(Timeline {
                id: row.get(0)?,
                session_id: row.get(1)?,
                parent_timeline_id: row.get(2)?,
                fork_at_step: row.get(3)?,
                created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                    .unwrap()
                    .with_timezone(&chrono::Utc),
                label: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_root_timeline(&self, session_id: &str) -> Result<Option<Timeline>> {
        let timelines = self.get_timelines(session_id)?;
        Ok(timelines.into_iter().find(|t| t.parent_timeline_id.is_none()))
    }

    // ── Steps ─────────────────────────────────────────────────

    pub fn create_step(&self, step: &Step) -> Result<()> {
        self.conn.execute(
            "INSERT INTO steps (id, timeline_id, session_id, step_number, step_type, status, created_at, duration_ms, tokens_in, tokens_out, model, request_blob, response_blob, error, span_id, tool_name)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                step.id,
                step.timeline_id,
                step.session_id,
                step.step_number,
                step.step_type.as_str(),
                step.status.as_str(),
                step.created_at.to_rfc3339(),
                step.duration_ms,
                step.tokens_in,
                step.tokens_out,
                step.model,
                step.request_blob,
                step.response_blob,
                step.error,
                step.span_id,
                step.tool_name,
            ],
        )?;
        Ok(())
    }

    pub fn get_steps(&self, timeline_id: &str) -> Result<Vec<Step>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, timeline_id, session_id, step_number, step_type, status, created_at, duration_ms, tokens_in, tokens_out, model, request_blob, response_blob, error, span_id, tool_name
             FROM steps WHERE timeline_id = ?1 ORDER BY step_number",
        )?;
        let rows = stmt.query_map(params![timeline_id], Self::row_to_step)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_step(&self, step_id: &str) -> Result<Option<Step>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, timeline_id, session_id, step_number, step_type, status, created_at, duration_ms, tokens_in, tokens_out, model, request_blob, response_blob, error, span_id, tool_name
             FROM steps WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![step_id], Self::row_to_step)?;
        Ok(rows.next().transpose()?)
    }

    pub fn get_step_by_number(&self, timeline_id: &str, step_number: u32) -> Result<Option<Step>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, timeline_id, session_id, step_number, step_type, status, created_at, duration_ms, tokens_in, tokens_out, model, request_blob, response_blob, error, span_id, tool_name
             FROM steps WHERE timeline_id = ?1 AND step_number = ?2",
        )?;
        let mut rows = stmt.query_map(params![timeline_id, step_number], Self::row_to_step)?;
        Ok(rows.next().transpose()?)
    }

    // ── Step Counters (Explicit API) ──────────────────────────

    // SAFETY: next_step_number uses separate INSERT + SELECT statements.
    // This is safe because Store is always accessed through Arc<Mutex<Store>>,
    // and callers hold the MutexGuard for the entire handler scope.
    // If locking changes to RwLock, use INSERT ... RETURNING counter instead.
    /// Atomically allocate the next step number for a session+timeline pair.
    pub fn next_step_number(&self, session_id: &str, timeline_id: &str) -> Result<u32> {
        self.conn.execute(
            "INSERT INTO step_counters (session_id, timeline_id, counter) VALUES (?1, ?2, 1)
             ON CONFLICT(session_id, timeline_id) DO UPDATE SET counter = counter + 1",
            params![session_id, timeline_id],
        )?;
        let counter: u32 = self.conn.query_row(
            "SELECT counter FROM step_counters WHERE session_id = ?1 AND timeline_id = ?2",
            params![session_id, timeline_id],
            |row| row.get(0),
        )?;
        Ok(counter)
    }

    /// Sync the step counter to a specific value (used when rehydrating from hooks).
    pub fn sync_step_counter(&self, session_id: &str, timeline_id: &str, value: u32) -> Result<()> {
        self.conn.execute(
            "INSERT INTO step_counters (session_id, timeline_id, counter) VALUES (?1, ?2, ?3)
             ON CONFLICT(session_id, timeline_id) DO UPDATE SET counter = MAX(counter, ?3)",
            params![session_id, timeline_id, value],
        )?;
        Ok(())
    }

    // ── Replay Contexts (Explicit API) ──────────────────────

    pub fn create_replay_context(
        &self, id: &str, session_id: &str, timeline_id: &str, from_step: u32,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO replay_contexts (id, session_id, timeline_id, from_step, current_step, created_at, last_accessed_at)
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?5)",
            params![id, session_id, timeline_id, from_step, now],
        )?;
        Ok(())
    }

    /// Advance the replay context cursor and return the current step number.
    pub fn advance_replay_context(&self, context_id: &str) -> Result<u32> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE replay_contexts SET current_step = current_step + 1, last_accessed_at = ?1 WHERE id = ?2",
            params![now, context_id],
        )?;
        let step: u32 = self.conn.query_row(
            "SELECT current_step FROM replay_contexts WHERE id = ?1",
            params![context_id],
            |row| row.get(0),
        )?;
        Ok(step)
    }

    pub fn get_replay_context(&self, context_id: &str) -> Result<Option<(String, String, u32, u32)>> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, timeline_id, from_step, current_step FROM replay_contexts WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![context_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, u32>(2)?, row.get::<_, u32>(3)?))
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn delete_replay_context(&self, context_id: &str) -> Result<bool> {
        let affected = self.conn.execute(
            "DELETE FROM replay_contexts WHERE id = ?1",
            params![context_id],
        )?;
        Ok(affected > 0)
    }

    pub fn cleanup_expired_replay_contexts(&self, max_age_secs: i64) -> Result<usize> {
        let cutoff = (chrono::Utc::now() - chrono::Duration::seconds(max_age_secs)).to_rfc3339();
        let affected = self.conn.execute(
            "DELETE FROM replay_contexts WHERE last_accessed_at < ?1",
            params![cutoff],
        )?;
        Ok(affected)
    }

    pub fn count_replay_contexts(&self) -> Result<u64> {
        let count: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM replay_contexts", [], |row| row.get(0),
        )?;
        Ok(count)
    }

    // ── Instant Replay Cache ──────────────────────────────────

    pub fn cache_put(&self, request_hash: &str, response_blob: &str, model: &str, tokens_in: u64, tokens_out: u64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO replay_cache (request_hash, response_blob, model, tokens_in, tokens_out, hit_count, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)",
            params![request_hash, response_blob, model, tokens_in, tokens_out, chrono::Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn cache_get(&self, request_hash: &str) -> Result<Option<CacheEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT request_hash, response_blob, model, tokens_in, tokens_out, hit_count
             FROM replay_cache WHERE request_hash = ?1",
        )?;
        let mut rows = stmt.query_map(params![request_hash], |row| {
            Ok(CacheEntry {
                request_hash: row.get(0)?,
                response_blob: row.get(1)?,
                model: row.get(2)?,
                tokens_in: row.get(3)?,
                tokens_out: row.get(4)?,
                hit_count: row.get(5)?,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn cache_hit(&self, request_hash: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE replay_cache SET hit_count = hit_count + 1, last_hit_at = ?1 WHERE request_hash = ?2",
            params![chrono::Utc::now().to_rfc3339(), request_hash],
        )?;
        Ok(())
    }

    pub fn cache_stats(&self) -> Result<CacheStats> {
        let mut stmt = self.conn.prepare(
            "SELECT COUNT(*), COALESCE(SUM(hit_count), 0), COALESCE(SUM(hit_count * (tokens_in + tokens_out)), 0) FROM replay_cache",
        )?;
        let stats = stmt.query_row([], |row| {
            Ok(CacheStats {
                entries: row.get(0)?,
                total_hits: row.get(1)?,
                total_tokens_saved: row.get(2)?,
            })
        })?;
        Ok(stats)
    }

    // ── Snapshots ─────────────────────────────────────────────

    pub fn create_snapshot(&self, snapshot: &Snapshot) -> Result<()> {
        self.conn.execute(
            "INSERT INTO snapshots (id, session_id, label, directory, blob_hash, file_count, size_bytes, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                snapshot.id,
                snapshot.session_id,
                snapshot.label,
                snapshot.directory,
                snapshot.blob_hash,
                snapshot.file_count,
                snapshot.size_bytes,
                snapshot.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn list_snapshots(&self) -> Result<Vec<Snapshot>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, label, directory, blob_hash, file_count, size_bytes, created_at
             FROM snapshots ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Snapshot {
                id: row.get(0)?,
                session_id: row.get(1)?,
                label: row.get(2)?,
                directory: row.get(3)?,
                blob_hash: row.get(4)?,
                file_count: row.get(5)?,
                size_bytes: row.get(6)?,
                created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(7)?)
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_snapshot(&self, snapshot_ref: &str) -> Result<Option<Snapshot>> {
        if snapshot_ref == "latest" {
            let snapshots = self.list_snapshots()?;
            return Ok(snapshots.into_iter().last());
        }
        // Try exact ID, then prefix, then label
        let snapshots = self.list_snapshots()?;
        if let Some(s) = snapshots.iter().find(|s| s.id == snapshot_ref) {
            return Ok(Some(s.clone()));
        }
        if let Some(s) = snapshots.iter().find(|s| s.id.starts_with(snapshot_ref)) {
            return Ok(Some(s.clone()));
        }
        if let Some(s) = snapshots.iter().find(|s| s.label == snapshot_ref) {
            return Ok(Some(s.clone()));
        }
        Ok(None)
    }

    // ── Baselines ─────────────────────────────────────────────

    pub fn create_baseline(&self, baseline: &Baseline) -> Result<()> {
        self.conn.execute(
            "INSERT INTO baselines (id, name, source_session_id, source_timeline_id, created_at, description, step_count, total_tokens, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                baseline.id,
                baseline.name,
                baseline.source_session_id,
                baseline.source_timeline_id,
                baseline.created_at.to_rfc3339(),
                baseline.description,
                baseline.step_count,
                baseline.total_tokens,
                baseline.metadata.to_string(),
            ],
        )?;
        Ok(())
    }

    pub fn list_baselines(&self) -> Result<Vec<Baseline>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, source_session_id, source_timeline_id, created_at, description, step_count, total_tokens, metadata
             FROM baselines ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Baseline {
                id: row.get(0)?,
                name: row.get(1)?,
                source_session_id: row.get(2)?,
                source_timeline_id: row.get(3)?,
                created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                    .unwrap()
                    .with_timezone(&chrono::Utc),
                description: row.get(5)?,
                step_count: row.get(6)?,
                total_tokens: row.get(7)?,
                metadata: serde_json::from_str(&row.get::<_, String>(8)?).unwrap_or_default(),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_baseline_by_name(&self, name: &str) -> Result<Option<Baseline>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, source_session_id, source_timeline_id, created_at, description, step_count, total_tokens, metadata
             FROM baselines WHERE name = ?1",
        )?;
        let mut rows = stmt.query_map(params![name], |row| {
            Ok(Baseline {
                id: row.get(0)?,
                name: row.get(1)?,
                source_session_id: row.get(2)?,
                source_timeline_id: row.get(3)?,
                created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                    .unwrap()
                    .with_timezone(&chrono::Utc),
                description: row.get(5)?,
                step_count: row.get(6)?,
                total_tokens: row.get(7)?,
                metadata: serde_json::from_str(&row.get::<_, String>(8)?).unwrap_or_default(),
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn get_baseline(&self, id: &str) -> Result<Option<Baseline>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, source_session_id, source_timeline_id, created_at, description, step_count, total_tokens, metadata
             FROM baselines WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], |row| {
            Ok(Baseline {
                id: row.get(0)?,
                name: row.get(1)?,
                source_session_id: row.get(2)?,
                source_timeline_id: row.get(3)?,
                created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                    .unwrap()
                    .with_timezone(&chrono::Utc),
                description: row.get(5)?,
                step_count: row.get(6)?,
                total_tokens: row.get(7)?,
                metadata: serde_json::from_str(&row.get::<_, String>(8)?).unwrap_or_default(),
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn delete_baseline(&self, id: &str) -> Result<()> {
        // baseline_steps cascade on delete
        self.conn.execute("DELETE FROM baselines WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn create_baseline_step(&self, step: &BaselineStep) -> Result<()> {
        self.conn.execute(
            "INSERT INTO baseline_steps (id, baseline_id, step_number, step_type, expected_status, expected_model, tokens_in, tokens_out, tool_name, response_blob, request_blob, has_error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                step.id,
                step.baseline_id,
                step.step_number,
                step.step_type,
                step.expected_status,
                step.expected_model,
                step.tokens_in,
                step.tokens_out,
                step.tool_name,
                step.response_blob,
                step.request_blob,
                step.has_error as i32,
            ],
        )?;
        Ok(())
    }

    pub fn get_baseline_steps(&self, baseline_id: &str) -> Result<Vec<BaselineStep>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, baseline_id, step_number, step_type, expected_status, expected_model, tokens_in, tokens_out, tool_name, response_blob, request_blob, has_error
             FROM baseline_steps WHERE baseline_id = ?1 ORDER BY step_number",
        )?;
        let rows = stmt.query_map(params![baseline_id], |row| {
            Ok(BaselineStep {
                id: row.get(0)?,
                baseline_id: row.get(1)?,
                step_number: row.get(2)?,
                step_type: row.get(3)?,
                expected_status: row.get(4)?,
                expected_model: row.get(5)?,
                tokens_in: row.get(6)?,
                tokens_out: row.get(7)?,
                tool_name: row.get(8)?,
                response_blob: row.get(9)?,
                request_blob: row.get(10)?,
                has_error: row.get::<_, i32>(11)? != 0,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // ── Datasets ───────────────────────────────────────────────

    pub fn create_dataset(&self, dataset: &Dataset) -> Result<()> {
        self.conn.execute(
            "INSERT INTO datasets (id, name, description, created_at, updated_at, version, example_count, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                dataset.id,
                dataset.name,
                dataset.description,
                dataset.created_at.to_rfc3339(),
                dataset.updated_at.to_rfc3339(),
                dataset.version,
                dataset.example_count,
                dataset.metadata.to_string(),
            ],
        )?;
        Ok(())
    }

    /// List datasets, returning only the latest version of each name.
    pub fn list_datasets(&self) -> Result<Vec<Dataset>> {
        let mut stmt = self.conn.prepare(
            "SELECT d.id, d.name, d.description, d.created_at, d.updated_at, d.version, d.example_count, d.metadata
             FROM datasets d
             INNER JOIN (SELECT name, MAX(version) as max_ver FROM datasets GROUP BY name) latest
             ON d.name = latest.name AND d.version = latest.max_ver
             ORDER BY d.updated_at DESC",
        )?;
        let rows = stmt.query_map([], Self::row_to_dataset)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Get the latest version of a dataset by name.
    pub fn get_dataset_by_name(&self, name: &str) -> Result<Option<Dataset>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, created_at, updated_at, version, example_count, metadata
             FROM datasets WHERE name = ?1 ORDER BY version DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![name], Self::row_to_dataset)?;
        Ok(rows.next().transpose()?)
    }

    /// Get a specific version of a dataset.
    pub fn get_dataset_by_name_version(&self, name: &str, version: u32) -> Result<Option<Dataset>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, created_at, updated_at, version, example_count, metadata
             FROM datasets WHERE name = ?1 AND version = ?2",
        )?;
        let mut rows = stmt.query_map(params![name, version], Self::row_to_dataset)?;
        Ok(rows.next().transpose()?)
    }

    pub fn get_dataset(&self, id: &str) -> Result<Option<Dataset>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, created_at, updated_at, version, example_count, metadata
             FROM datasets WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], Self::row_to_dataset)?;
        Ok(rows.next().transpose()?)
    }

    pub fn delete_dataset_by_name(&self, name: &str) -> Result<()> {
        // Cascades to dataset_examples
        self.conn.execute("DELETE FROM datasets WHERE name = ?1", params![name])?;
        Ok(())
    }

    pub fn update_dataset_example_count(&self, dataset_id: &str, count: u32) -> Result<()> {
        self.conn.execute(
            "UPDATE datasets SET example_count = ?1, updated_at = ?2 WHERE id = ?3",
            params![count, chrono::Utc::now().to_rfc3339(), dataset_id],
        )?;
        Ok(())
    }

    fn row_to_step(row: &rusqlite::Row) -> rusqlite::Result<Step> {
        Ok(Step {
            id: row.get(0)?,
            timeline_id: row.get(1)?,
            session_id: row.get(2)?,
            step_number: row.get(3)?,
            step_type: StepType::parse(&row.get::<_, String>(4)?),
            status: StepStatus::parse(&row.get::<_, String>(5)?),
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(6)?)
                .unwrap()
                .with_timezone(&chrono::Utc),
            duration_ms: row.get(7)?,
            tokens_in: row.get(8)?,
            tokens_out: row.get(9)?,
            model: row.get(10)?,
            request_blob: row.get(11)?,
            response_blob: row.get(12)?,
            error: row.get(13)?,
            span_id: row.get(14)?,
            tool_name: row.get(15)?,
        })
    }

    fn row_to_session(row: &rusqlite::Row) -> rusqlite::Result<Session> {
        Ok(Session {
            id: row.get(0)?,
            name: row.get(1)?,
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(2)?)
                .unwrap()
                .with_timezone(&chrono::Utc),
            updated_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(3)?)
                .unwrap()
                .with_timezone(&chrono::Utc),
            status: SessionStatus::parse(&row.get::<_, String>(4)?),
            source: SessionSource::parse(&row.get::<_, String>(5)?),
            total_steps: row.get(6)?,
            total_tokens: row.get(7)?,
            metadata: serde_json::from_str(&row.get::<_, String>(8)?).unwrap_or_default(),
            thread_id: row.get(9)?,
            thread_ordinal: row.get(10)?,
        })
    }

    fn row_to_span(row: &rusqlite::Row) -> rusqlite::Result<Span> {
        Ok(Span {
            id: row.get(0)?,
            session_id: row.get(1)?,
            timeline_id: row.get(2)?,
            parent_span_id: row.get(3)?,
            span_type: SpanType::parse(&row.get::<_, String>(4)?),
            name: row.get(5)?,
            status: row.get(6)?,
            started_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(7)?)
                .unwrap()
                .with_timezone(&chrono::Utc),
            ended_at: row.get::<_, Option<String>>(8)?
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc)),
            duration_ms: row.get(9)?,
            metadata: serde_json::from_str(&row.get::<_, String>(10)?).unwrap_or_default(),
            error: row.get(11)?,
        })
    }

    fn row_to_dataset(row: &rusqlite::Row) -> rusqlite::Result<Dataset> {
        Ok(Dataset {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(3)?)
                .unwrap()
                .with_timezone(&chrono::Utc),
            updated_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                .unwrap()
                .with_timezone(&chrono::Utc),
            version: row.get(5)?,
            example_count: row.get(6)?,
            metadata: serde_json::from_str(&row.get::<_, String>(7)?).unwrap_or_default(),
        })
    }

    // ── Dataset Examples ──────────────────────────────────────

    pub fn create_dataset_example(&self, example: &DatasetExample) -> Result<()> {
        self.conn.execute(
            "INSERT INTO dataset_examples (id, dataset_id, ordinal, input_blob, expected_blob, metadata, source_session_id, source_step_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                example.id,
                example.dataset_id,
                example.ordinal,
                example.input_blob,
                example.expected_blob,
                example.metadata.to_string(),
                example.source_session_id,
                example.source_step_id,
                example.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn get_dataset_examples(&self, dataset_id: &str) -> Result<Vec<DatasetExample>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, dataset_id, ordinal, input_blob, expected_blob, metadata, source_session_id, source_step_id, created_at
             FROM dataset_examples WHERE dataset_id = ?1 ORDER BY ordinal",
        )?;
        let rows = stmt.query_map(params![dataset_id], |row| {
            Ok(DatasetExample {
                id: row.get(0)?,
                dataset_id: row.get(1)?,
                ordinal: row.get(2)?,
                input_blob: row.get(3)?,
                expected_blob: row.get(4)?,
                metadata: serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or_default(),
                source_session_id: row.get(6)?,
                source_step_id: row.get(7)?,
                created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(8)?)
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Copy all examples from one dataset to another (for versioning).
    pub fn copy_dataset_examples(&self, from_dataset_id: &str, to_dataset_id: &str) -> Result<u32> {
        let examples = self.get_dataset_examples(from_dataset_id)?;
        for ex in &examples {
            let new_ex = DatasetExample {
                id: uuid::Uuid::new_v4().to_string(),
                dataset_id: to_dataset_id.to_string(),
                ordinal: ex.ordinal,
                input_blob: ex.input_blob.clone(),
                expected_blob: ex.expected_blob.clone(),
                metadata: ex.metadata.clone(),
                source_session_id: ex.source_session_id.clone(),
                source_step_id: ex.source_step_id.clone(),
                created_at: ex.created_at,
            };
            self.create_dataset_example(&new_ex)?;
        }
        Ok(examples.len() as u32)
    }

    // ── Evaluators ────────────────────────────────────────────

    pub fn create_evaluator(&self, evaluator: &Evaluator) -> Result<()> {
        self.conn.execute(
            "INSERT INTO evaluators (id, name, evaluator_type, config_blob, created_at, description)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                evaluator.id,
                evaluator.name,
                evaluator.evaluator_type,
                evaluator.config_blob,
                evaluator.created_at.to_rfc3339(),
                evaluator.description,
            ],
        )?;
        Ok(())
    }

    pub fn list_evaluators(&self) -> Result<Vec<Evaluator>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, evaluator_type, config_blob, created_at, description
             FROM evaluators ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Evaluator {
                id: row.get(0)?,
                name: row.get(1)?,
                evaluator_type: row.get(2)?,
                config_blob: row.get(3)?,
                created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                    .unwrap()
                    .with_timezone(&chrono::Utc),
                description: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_evaluator_by_name(&self, name: &str) -> Result<Option<Evaluator>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, evaluator_type, config_blob, created_at, description
             FROM evaluators WHERE name = ?1",
        )?;
        let mut rows = stmt.query_map(params![name], |row| {
            Ok(Evaluator {
                id: row.get(0)?,
                name: row.get(1)?,
                evaluator_type: row.get(2)?,
                config_blob: row.get(3)?,
                created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                    .unwrap()
                    .with_timezone(&chrono::Utc),
                description: row.get(5)?,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn delete_evaluator(&self, name: &str) -> Result<()> {
        self.conn.execute("DELETE FROM evaluators WHERE name = ?1", params![name])?;
        Ok(())
    }

    // ── Experiments ───────────────────────────────────────────

    pub fn create_experiment(&self, exp: &Experiment) -> Result<()> {
        self.conn.execute(
            "INSERT INTO experiments (id, name, dataset_id, dataset_version, status, created_at, completed_at, total_examples, completed_examples, avg_score, min_score, max_score, pass_rate, total_duration_ms, total_tokens, config_blob, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                exp.id,
                exp.name,
                exp.dataset_id,
                exp.dataset_version,
                exp.status.as_str(),
                exp.created_at.to_rfc3339(),
                exp.completed_at.map(|dt| dt.to_rfc3339()),
                exp.total_examples,
                exp.completed_examples,
                exp.avg_score,
                exp.min_score,
                exp.max_score,
                exp.pass_rate,
                exp.total_duration_ms,
                exp.total_tokens,
                exp.config_blob,
                exp.metadata.to_string(),
            ],
        )?;
        Ok(())
    }

    pub fn list_experiments(&self) -> Result<Vec<Experiment>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, dataset_id, dataset_version, status, created_at, completed_at, total_examples, completed_examples, avg_score, min_score, max_score, pass_rate, total_duration_ms, total_tokens, config_blob, metadata
             FROM experiments ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], Self::row_to_experiment)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_experiments_by_dataset(&self, dataset_name: &str) -> Result<Vec<Experiment>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.id, e.name, e.dataset_id, e.dataset_version, e.status, e.created_at, e.completed_at, e.total_examples, e.completed_examples, e.avg_score, e.min_score, e.max_score, e.pass_rate, e.total_duration_ms, e.total_tokens, e.config_blob, e.metadata
             FROM experiments e
             INNER JOIN datasets d ON e.dataset_id = d.id
             WHERE d.name = ?1
             ORDER BY e.created_at DESC",
        )?;
        let rows = stmt.query_map(params![dataset_name], Self::row_to_experiment)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_experiment(&self, id: &str) -> Result<Option<Experiment>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, dataset_id, dataset_version, status, created_at, completed_at, total_examples, completed_examples, avg_score, min_score, max_score, pass_rate, total_duration_ms, total_tokens, config_blob, metadata
             FROM experiments WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], Self::row_to_experiment)?;
        Ok(rows.next().transpose()?)
    }

    pub fn get_experiment_by_name(&self, name: &str) -> Result<Option<Experiment>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, dataset_id, dataset_version, status, created_at, completed_at, total_examples, completed_examples, avg_score, min_score, max_score, pass_rate, total_duration_ms, total_tokens, config_blob, metadata
             FROM experiments WHERE name = ?1 ORDER BY created_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![name], Self::row_to_experiment)?;
        Ok(rows.next().transpose()?)
    }

    pub fn update_experiment_status(&self, id: &str, status: ExperimentStatus) -> Result<()> {
        let completed_at = if status == ExperimentStatus::Completed || status == ExperimentStatus::Failed {
            Some(chrono::Utc::now().to_rfc3339())
        } else {
            None
        };
        self.conn.execute(
            "UPDATE experiments SET status = ?1, completed_at = ?2 WHERE id = ?3",
            params![status.as_str(), completed_at, id],
        )?;
        Ok(())
    }

    pub fn update_experiment_progress(&self, id: &str, completed_examples: u32) -> Result<()> {
        self.conn.execute(
            "UPDATE experiments SET completed_examples = ?1 WHERE id = ?2",
            params![completed_examples, id],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_experiment_aggregates(
        &self,
        id: &str,
        avg_score: f64,
        min_score: f64,
        max_score: f64,
        pass_rate: f64,
        total_duration_ms: u64,
        total_tokens: u64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE experiments SET avg_score = ?1, min_score = ?2, max_score = ?3, pass_rate = ?4, total_duration_ms = ?5, total_tokens = ?6 WHERE id = ?7",
            params![avg_score, min_score, max_score, pass_rate, total_duration_ms, total_tokens, id],
        )?;
        Ok(())
    }

    pub fn delete_experiment(&self, id: &str) -> Result<()> {
        // Cascades to experiment_results → experiment_scores
        self.conn.execute("DELETE FROM experiments WHERE id = ?1", params![id])?;
        Ok(())
    }

    fn row_to_experiment(row: &rusqlite::Row) -> rusqlite::Result<Experiment> {
        Ok(Experiment {
            id: row.get(0)?,
            name: row.get(1)?,
            dataset_id: row.get(2)?,
            dataset_version: row.get(3)?,
            status: ExperimentStatus::parse(&row.get::<_, String>(4)?),
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                .unwrap()
                .with_timezone(&chrono::Utc),
            completed_at: row.get::<_, Option<String>>(6)?
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc)),
            total_examples: row.get(7)?,
            completed_examples: row.get(8)?,
            avg_score: row.get(9)?,
            min_score: row.get(10)?,
            max_score: row.get(11)?,
            pass_rate: row.get(12)?,
            total_duration_ms: row.get(13)?,
            total_tokens: row.get(14)?,
            config_blob: row.get(15)?,
            metadata: serde_json::from_str(&row.get::<_, String>(16)?).unwrap_or_default(),
        })
    }

    // ── Experiment Results ─────────────────────────────────────

    pub fn create_experiment_result(&self, result: &ExperimentResult) -> Result<()> {
        self.conn.execute(
            "INSERT INTO experiment_results (id, experiment_id, example_id, ordinal, output_blob, trace_session_id, trace_timeline_id, duration_ms, tokens_in, tokens_out, status, error, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                result.id,
                result.experiment_id,
                result.example_id,
                result.ordinal,
                result.output_blob,
                result.trace_session_id,
                result.trace_timeline_id,
                result.duration_ms,
                result.tokens_in,
                result.tokens_out,
                result.status,
                result.error,
                result.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn get_experiment_results(&self, experiment_id: &str) -> Result<Vec<ExperimentResult>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, experiment_id, example_id, ordinal, output_blob, trace_session_id, trace_timeline_id, duration_ms, tokens_in, tokens_out, status, error, created_at
             FROM experiment_results WHERE experiment_id = ?1 ORDER BY ordinal",
        )?;
        let rows = stmt.query_map(params![experiment_id], |row| {
            Ok(ExperimentResult {
                id: row.get(0)?,
                experiment_id: row.get(1)?,
                example_id: row.get(2)?,
                ordinal: row.get(3)?,
                output_blob: row.get(4)?,
                trace_session_id: row.get(5)?,
                trace_timeline_id: row.get(6)?,
                duration_ms: row.get(7)?,
                tokens_in: row.get(8)?,
                tokens_out: row.get(9)?,
                status: row.get(10)?,
                error: row.get(11)?,
                created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(12)?)
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // ── Experiment Scores ──────────────────────────────────────

    pub fn create_experiment_score(&self, score: &ExperimentScore) -> Result<()> {
        self.conn.execute(
            "INSERT INTO experiment_scores (id, result_id, evaluator_id, score, passed, reasoning, metadata, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                score.id,
                score.result_id,
                score.evaluator_id,
                score.score,
                score.passed as i32,
                score.reasoning,
                score.metadata.to_string(),
                score.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn get_experiment_scores(&self, result_id: &str) -> Result<Vec<ExperimentScore>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, result_id, evaluator_id, score, passed, reasoning, metadata, created_at
             FROM experiment_scores WHERE result_id = ?1",
        )?;
        let rows = stmt.query_map(params![result_id], Self::row_to_score)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_all_experiment_scores(&self, experiment_id: &str) -> Result<Vec<ExperimentScore>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.result_id, s.evaluator_id, s.score, s.passed, s.reasoning, s.metadata, s.created_at
             FROM experiment_scores s
             INNER JOIN experiment_results r ON s.result_id = r.id
             WHERE r.experiment_id = ?1
             ORDER BY r.ordinal",
        )?;
        let rows = stmt.query_map(params![experiment_id], Self::row_to_score)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn row_to_score(row: &rusqlite::Row) -> rusqlite::Result<ExperimentScore> {
        Ok(ExperimentScore {
            id: row.get(0)?,
            result_id: row.get(1)?,
            evaluator_id: row.get(2)?,
            score: row.get(3)?,
            passed: row.get::<_, i32>(4)? != 0,
            reasoning: row.get(5)?,
            metadata: serde_json::from_str(&row.get::<_, String>(6)?).unwrap_or_default(),
            created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(7)?)
                .unwrap()
                .with_timezone(&chrono::Utc),
        })
    }

    // ── Timeline Scores ───────────────────────────────────────

    /// Upsert a timeline score (UNIQUE on timeline_id + evaluator_id).
    pub fn create_timeline_score(&self, score: &TimelineScore) -> Result<()> {
        self.conn.execute(
            "INSERT INTO timeline_scores (id, session_id, timeline_id, evaluator_id, score, passed, reasoning, input_blob, output_blob, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(timeline_id, evaluator_id) DO UPDATE SET
                score = excluded.score,
                passed = excluded.passed,
                reasoning = excluded.reasoning,
                input_blob = excluded.input_blob,
                output_blob = excluded.output_blob,
                created_at = excluded.created_at",
            params![
                score.id,
                score.session_id,
                score.timeline_id,
                score.evaluator_id,
                score.score,
                score.passed as i32,
                score.reasoning,
                score.input_blob,
                score.output_blob,
                score.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Get all timeline scores for a session.
    pub fn get_timeline_scores(&self, session_id: &str) -> Result<Vec<TimelineScore>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, timeline_id, evaluator_id, score, passed, reasoning, input_blob, output_blob, created_at
             FROM timeline_scores WHERE session_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(TimelineScore {
                id: row.get(0)?,
                session_id: row.get(1)?,
                timeline_id: row.get(2)?,
                evaluator_id: row.get(3)?,
                score: row.get(4)?,
                passed: row.get::<_, i32>(5)? != 0,
                reasoning: row.get(6)?,
                input_blob: row.get(7)?,
                output_blob: row.get(8)?,
                created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(9)?)
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Check if a timeline score already exists for the given timeline + evaluator.
    pub fn get_timeline_score(&self, timeline_id: &str, evaluator_id: &str) -> Result<Option<TimelineScore>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, timeline_id, evaluator_id, score, passed, reasoning, input_blob, output_blob, created_at
             FROM timeline_scores WHERE timeline_id = ?1 AND evaluator_id = ?2",
        )?;
        let mut rows = stmt.query_map(params![timeline_id, evaluator_id], |row| {
            Ok(TimelineScore {
                id: row.get(0)?,
                session_id: row.get(1)?,
                timeline_id: row.get(2)?,
                evaluator_id: row.get(3)?,
                score: row.get(4)?,
                passed: row.get::<_, i32>(5)? != 0,
                reasoning: row.get(6)?,
                input_blob: row.get(7)?,
                output_blob: row.get(8)?,
                created_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(9)?)
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    // ── Spans ─────────────────────────────────────────────────

    pub fn create_span(&self, span: &Span) -> Result<()> {
        self.conn.execute(
            "INSERT INTO spans (id, session_id, timeline_id, parent_span_id, span_type, name, status, started_at, ended_at, duration_ms, metadata, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                span.id,
                span.session_id,
                span.timeline_id,
                span.parent_span_id,
                span.span_type.as_str(),
                span.name,
                span.status,
                span.started_at.to_rfc3339(),
                span.ended_at.map(|dt| dt.to_rfc3339()),
                span.duration_ms,
                span.metadata.to_string(),
                span.error,
            ],
        )?;
        Ok(())
    }

    pub fn update_span_status(&self, span_id: &str, status: &str, ended_at: Option<chrono::DateTime<chrono::Utc>>, duration_ms: u64, error: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE spans SET status = ?1, ended_at = ?2, duration_ms = ?3, error = ?4 WHERE id = ?5",
            params![status, ended_at.map(|dt| dt.to_rfc3339()), duration_ms, error, span_id],
        )?;
        Ok(())
    }

    pub fn get_span(&self, span_id: &str) -> Result<Option<Span>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, timeline_id, parent_span_id, span_type, name, status, started_at, ended_at, duration_ms, metadata, error
             FROM spans WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![span_id], Self::row_to_span)?;
        Ok(rows.next().transpose()?)
    }

    pub fn get_spans_by_session(&self, session_id: &str) -> Result<Vec<Span>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, timeline_id, parent_span_id, span_type, name, status, started_at, ended_at, duration_ms, metadata, error
             FROM spans WHERE session_id = ?1 ORDER BY started_at",
        )?;
        let rows = stmt.query_map(params![session_id], Self::row_to_span)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_spans_by_timeline(&self, timeline_id: &str) -> Result<Vec<Span>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, timeline_id, parent_span_id, span_type, name, status, started_at, ended_at, duration_ms, metadata, error
             FROM spans WHERE timeline_id = ?1 ORDER BY started_at",
        )?;
        let rows = stmt.query_map(params![timeline_id], Self::row_to_span)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_child_spans(&self, parent_span_id: &str) -> Result<Vec<Span>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, timeline_id, parent_span_id, span_type, name, status, started_at, ended_at, duration_ms, metadata, error
             FROM spans WHERE parent_span_id = ?1 ORDER BY started_at",
        )?;
        let rows = stmt.query_map(params![parent_span_id], Self::row_to_span)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_steps_by_span(&self, span_id: &str) -> Result<Vec<Step>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, timeline_id, session_id, step_number, step_type, status, created_at, duration_ms, tokens_in, tokens_out, model, request_blob, response_blob, error, span_id, tool_name
             FROM steps WHERE span_id = ?1 ORDER BY step_number",
        )?;
        let rows = stmt.query_map(params![span_id], Self::row_to_step)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Check if a step with a given tool_name exists for a session.
    /// Used by transcript sync for UUID-based dedup.
    pub fn step_exists_by_tool_name(&self, session_id: &str, tool_name: &str) -> Result<bool> {
        let count: u32 = self.conn.query_row(
            "SELECT COUNT(*) FROM steps WHERE session_id = ?1 AND tool_name = ?2",
            params![session_id, tool_name],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Update a step's status, response blob, duration, and error (used by hook ingestion for PostToolUse).
    pub fn update_step_completion(
        &self,
        step_id: &str,
        status: StepStatus,
        response_blob: &str,
        duration_ms: u64,
        error: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE steps SET status = ?1, response_blob = ?2, duration_ms = ?3, error = ?4 WHERE id = ?5",
            params![status.as_str(), response_blob, duration_ms, error, step_id],
        )?;
        Ok(())
    }

    pub fn update_step_span_id(&self, step_id: &str, span_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE steps SET span_id = ?1 WHERE id = ?2",
            params![span_id, step_id],
        )?;
        Ok(())
    }

    // ── Threads (via session columns) ─────────────────────────

    pub fn get_sessions_by_thread(&self, thread_id: &str) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, created_at, updated_at, status, source, total_steps, total_tokens, metadata, thread_id, thread_ordinal
             FROM sessions WHERE thread_id = ?1 ORDER BY thread_ordinal, created_at",
        )?;
        let rows = stmt.query_map(params![thread_id], Self::row_to_session)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_thread_ids(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT thread_id FROM sessions WHERE thread_id IS NOT NULL ORDER BY thread_id",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn set_session_thread(&self, session_id: &str, thread_id: &str, ordinal: u32) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET thread_id = ?1, thread_ordinal = ?2 WHERE id = ?3",
            params![thread_id, ordinal, session_id],
        )?;
        Ok(())
    }

    // ── Raw SQL Query (read-only) ─────────────────────────────

    /// Execute a read-only SQL query and return column names + rows as strings.
    /// Rejects any statement that is not a SELECT (prevents mutations).
    /// Execute a read-only SQL query. Only SELECT and WITH are allowed.
    /// Rejects mutations even when disguised as CTEs (`WITH x AS (...) DELETE`).
    /// See docs/security-audit.md §HIGH-02.
    pub fn query_raw(&self, sql: &str) -> Result<QueryResult> {
        let trimmed = sql.trim_start();
        let first_word = trimmed.split_whitespace().next().unwrap_or("");
        if !first_word.eq_ignore_ascii_case("SELECT")
            && !first_word.eq_ignore_ascii_case("WITH")
        {
            anyhow::bail!(
                "Only SELECT and WITH queries are allowed. Got: '{}'. \
                 Use pragma_table_info() for schema introspection.",
                first_word
            );
        }

        let mut stmt = self.conn.prepare(sql)?;

        // Defense-in-depth: SQLite's own read-only classifier catches
        // `WITH x AS (...) DELETE/UPDATE/INSERT` which passes the first-word check.
        if !stmt.readonly() {
            anyhow::bail!("Query is not read-only (detected mutation via CTE or subquery)");
        }
        let col_count = stmt.column_count();
        let columns: Vec<String> = (0..col_count)
            .map(|i| stmt.column_name(i).unwrap_or("?").to_string())
            .collect();

        let rows = stmt.query_map([], |row| {
            let mut values = Vec::with_capacity(col_count);
            for i in 0..col_count {
                let val: rusqlite::types::Value = row.get(i)?;
                let s = match val {
                    rusqlite::types::Value::Null => "NULL".to_string(),
                    rusqlite::types::Value::Integer(n) => n.to_string(),
                    rusqlite::types::Value::Real(f) => format!("{:.2}", f),
                    rusqlite::types::Value::Text(s) => s,
                    rusqlite::types::Value::Blob(b) => format!("<blob {}B>", b.len()),
                };
                values.push(s);
            }
            Ok(values)
        })?;

        let data: Vec<Vec<String>> = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        Ok(QueryResult { columns, rows: data })
    }

    /// List all user-facing tables in the database.
    pub fn list_tables(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Safe wrapper around `PRAGMA table_info(table_name)`.
    /// Returns columns as (cid, name, type, notnull, dflt_value, pk).
    /// The table name is validated against `list_tables()` to prevent injection.
    pub fn pragma_table_info(&self, table_name: &str) -> Result<QueryResult> {
        let valid_tables = self.list_tables()?;
        if !valid_tables.iter().any(|t| t == table_name) {
            anyhow::bail!("Unknown table: '{}'", table_name);
        }

        // Safe: table_name is validated against sqlite_master output above.
        let sql = format!("PRAGMA table_info({})", table_name);
        let mut stmt = self.conn.prepare(&sql)?;
        let col_count = stmt.column_count();
        let columns: Vec<String> = (0..col_count)
            .map(|i| stmt.column_name(i).unwrap_or("?").to_string())
            .collect();

        let rows = stmt.query_map([], |row| {
            let mut values = Vec::with_capacity(col_count);
            for i in 0..col_count {
                let val: rusqlite::types::Value = row.get(i)?;
                let s = match val {
                    rusqlite::types::Value::Null => "NULL".to_string(),
                    rusqlite::types::Value::Integer(n) => n.to_string(),
                    rusqlite::types::Value::Real(f) => format!("{:.2}", f),
                    rusqlite::types::Value::Text(s) => s,
                    rusqlite::types::Value::Blob(b) => format!("<blob {}B>", b.len()),
                };
                values.push(s);
            }
            Ok(values)
        })?;

        let data: Vec<Vec<String>> = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        Ok(QueryResult { columns, rows: data })
    }
}

/// Result of a raw SQL query.
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// Validate that the data directory is owned by the current user and not
/// world- or group-writable. Guards against `REWIND_DATA` hijack (see
/// docs/security-audit.md §LOW-07).
#[cfg(unix)]
fn validate_owner(path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = std::fs::metadata(path)?;
    let dir_uid = meta.uid();
    let my_uid = unsafe { libc::geteuid() };
    if dir_uid != my_uid {
        anyhow::bail!(
            "Data directory {} is owned by uid {} but current user is uid {}. \
             Refusing to use a directory owned by another user (SSRF/hijack risk). \
             Delete the directory or fix ownership.",
            path.display(),
            dir_uid,
            my_uid,
        );
    }
    let mode = meta.mode();
    if mode & 0o022 != 0 {
        tracing::warn!(
            path = %path.display(),
            mode = format!("{:o}", mode & 0o777),
            "Data directory is group- or world-writable — tightening to 0700"
        );
        let perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

/// Resolve the Rewind data directory, honoring `REWIND_DATA` then `$HOME/.rewind`.
pub fn dirs_path() -> PathBuf {
    if let Ok(data_dir) = std::env::var("REWIND_DATA") {
        return PathBuf::from(data_dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".rewind")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Session, SessionSource, SessionStatus};
    use chrono::{Duration, Utc};

    fn test_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (store, dir)
    }

    fn insert_session(store: &Store, id: &str, status: SessionStatus, updated_at: chrono::DateTime<Utc>) {
        let session = Session {
            id: id.into(),
            name: format!("session-{id}"),
            created_at: updated_at,
            updated_at,
            status,
            source: SessionSource::Hooks,
            total_steps: 0,
            total_tokens: 0,
            metadata: serde_json::json!({}),
            thread_id: None,
            thread_ordinal: None,
        };
        store.create_session(&session).unwrap();
    }

    #[test]
    fn complete_stale_sessions_marks_old_recording_completed() {
        let (store, _dir) = test_store();
        let two_hours_ago = Utc::now() - Duration::hours(2);
        insert_session(&store, "stale-1", SessionStatus::Recording, two_hours_ago);

        let completed = store.complete_stale_sessions(Duration::minutes(30)).unwrap();
        assert_eq!(completed, vec!["stale-1"]);

        let session = store.get_session("stale-1").unwrap().unwrap();
        assert_eq!(session.status, SessionStatus::Completed);
    }

    #[test]
    fn complete_stale_sessions_skips_recent_recording() {
        let (store, _dir) = test_store();
        let just_now = Utc::now();
        insert_session(&store, "active-1", SessionStatus::Recording, just_now);

        let completed = store.complete_stale_sessions(Duration::minutes(30)).unwrap();
        assert!(completed.is_empty());

        let session = store.get_session("active-1").unwrap().unwrap();
        assert_eq!(session.status, SessionStatus::Recording);
    }

    #[test]
    fn complete_stale_sessions_skips_already_completed() {
        let (store, _dir) = test_store();
        let two_hours_ago = Utc::now() - Duration::hours(2);
        insert_session(&store, "done-1", SessionStatus::Completed, two_hours_ago);

        let completed = store.complete_stale_sessions(Duration::minutes(30)).unwrap();
        assert!(completed.is_empty());
    }

    #[test]
    fn complete_stale_sessions_returns_all_affected_ids() {
        let (store, _dir) = test_store();
        let old = Utc::now() - Duration::hours(3);
        let recent = Utc::now();
        insert_session(&store, "stale-a", SessionStatus::Recording, old);
        insert_session(&store, "stale-b", SessionStatus::Recording, old);
        insert_session(&store, "active-c", SessionStatus::Recording, recent);
        insert_session(&store, "done-d", SessionStatus::Completed, old);

        let mut completed = store.complete_stale_sessions(Duration::minutes(30)).unwrap();
        completed.sort();
        assert_eq!(completed, vec!["stale-a", "stale-b"]);

        // active-c still recording
        assert_eq!(store.get_session("active-c").unwrap().unwrap().status, SessionStatus::Recording);
        // done-d still completed (not double-updated)
        assert_eq!(store.get_session("done-d").unwrap().unwrap().status, SessionStatus::Completed);
    }
}
