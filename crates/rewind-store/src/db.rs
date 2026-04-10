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

        let db_path = root.join("rewind.db");
        let blobs_path = root.join("objects");

        let conn = Connection::open(&db_path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

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
            ",
        )?;
        Ok(())
    }

    // ── Sessions ──────────────────────────────────────────────

    pub fn create_session(&self, session: &Session) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sessions (id, name, created_at, updated_at, status, total_steps, total_tokens, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                session.id,
                session.name,
                session.created_at.to_rfc3339(),
                session.updated_at.to_rfc3339(),
                session.status.as_str(),
                session.total_steps,
                session.total_tokens,
                session.metadata.to_string(),
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

    pub fn update_session_status(&self, session_id: &str, status: SessionStatus) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status.as_str(), chrono::Utc::now().to_rfc3339(), session_id],
        )?;
        Ok(())
    }

    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, created_at, updated_at, status, total_steps, total_tokens, metadata
             FROM sessions ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
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
                total_steps: row.get(5)?,
                total_tokens: row.get(6)?,
                metadata: serde_json::from_str(&row.get::<_, String>(7)?).unwrap_or_default(),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_session(&self, session_id: &str) -> Result<Option<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, created_at, updated_at, status, total_steps, total_tokens, metadata
             FROM sessions WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![session_id], |row| {
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
                total_steps: row.get(5)?,
                total_tokens: row.get(6)?,
                metadata: serde_json::from_str(&row.get::<_, String>(7)?).unwrap_or_default(),
            })
        })?;
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
            "INSERT INTO steps (id, timeline_id, session_id, step_number, step_type, status, created_at, duration_ms, tokens_in, tokens_out, model, request_blob, response_blob, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
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
            ],
        )?;
        Ok(())
    }

    pub fn get_steps(&self, timeline_id: &str) -> Result<Vec<Step>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, timeline_id, session_id, step_number, step_type, status, created_at, duration_ms, tokens_in, tokens_out, model, request_blob, response_blob, error
             FROM steps WHERE timeline_id = ?1 ORDER BY step_number",
        )?;
        let rows = stmt.query_map(params![timeline_id], |row| {
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
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_step(&self, step_id: &str) -> Result<Option<Step>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, timeline_id, session_id, step_number, step_type, status, created_at, duration_ms, tokens_in, tokens_out, model, request_blob, response_blob, error
             FROM steps WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![step_id], |row| {
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
            })
        })?;
        Ok(rows.next().transpose()?)
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

    // ── Raw SQL Query (read-only) ─────────────────────────────

    /// Execute a read-only SQL query and return column names + rows as strings.
    /// Rejects any statement that is not a SELECT (prevents mutations).
    pub fn query_raw(&self, sql: &str) -> Result<QueryResult> {
        let trimmed = sql.trim_start();
        let first_word = trimmed.split_whitespace().next().unwrap_or("");
        if !first_word.eq_ignore_ascii_case("SELECT")
            && !first_word.eq_ignore_ascii_case("WITH")
            && !first_word.eq_ignore_ascii_case("EXPLAIN")
            && !first_word.eq_ignore_ascii_case("PRAGMA")
        {
            anyhow::bail!(
                "Only SELECT, WITH, EXPLAIN, and PRAGMA queries are allowed. Got: '{}'",
                first_word
            );
        }

        let mut stmt = self.conn.prepare(sql)?;
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
}

/// Result of a raw SQL query.
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

fn dirs_path() -> PathBuf {
    if let Ok(data_dir) = std::env::var("REWIND_DATA") {
        return PathBuf::from(data_dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".rewind")
}
