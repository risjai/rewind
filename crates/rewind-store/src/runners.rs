//! Runner registry + replay job tracking (Phase 3, commit 3/13).
//!
//! Three new tables, three Rust types:
//!
//! - **`runners`** ↔ [`Runner`]: registered agent processes that can
//!   accept replay-job webhooks. Stores name, mode (webhook/polling),
//!   webhook URL, encrypted auth token (under `REWIND_RUNNER_SECRET_KEY`),
//!   token hash for fast inbound auth lookup, status, last-seen timestamp.
//! - **`replay_jobs`** ↔ [`ReplayJob`]: dispatched replay jobs going
//!   through the state machine `pending → dispatched → in_progress →
//!   completed/errored`. Includes lease columns (`dispatch_deadline_at`,
//!   `lease_expires_at`) for the reaper task added in commit 5.
//! - **`replay_job_events`** ↔ [`ReplayJobEvent`]: append-only event log
//!   per job (started/progress/completed/errored). The dashboard's
//!   WebSocket re-broadcasts these.
//!
//! ## What this commit ships
//!
//! Pure data types + CRUD methods on [`Store`](crate::Store). No HTTP
//! endpoints (commit 4), no encryption logic (commit 4), no dispatcher
//! (commit 5). Keeping the storage-only piece reviewable on its own.
//!
//! ## Encryption boundary
//!
//! `Runner.encrypted_token` is **opaque bytes** to this module. The
//! `crypto` module added in commit 4 encrypts/decrypts via AES-256-GCM
//! under the app key. Runners.rs treats it as `Vec<u8>` and trusts
//! the caller (rewind-web's runner registration handler) to
//! encrypt before insert and decrypt at dispatch time.
//!
//! ## Token hash semantics
//!
//! `Runner.auth_token_hash` is `SHA-256(raw_token)` hex-encoded. Used
//! for the fast-path inbound-auth lookup: when a runner posts an event
//! with `X-Rewind-Runner-Auth: <token>`, the server hashes the supplied
//! value and looks up by `auth_token_hash` (indexed) — no decryption
//! needed in the hot path.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::Store;

// ── Runner ───────────────────────────────────────────────────────

/// How Rewind talks to a runner.
///
/// Phase 3 v1 ships only `Webhook` mode. The schema accommodates
/// `Polling` (NAT'd laptops) from day one so v3.1 can add it without
/// a migration; calls that supply `mode = Polling` today will return
/// `400 Bad Request` from the registration endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunnerMode {
    Webhook,
    Polling,
}

impl RunnerMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunnerMode::Webhook => "webhook",
            RunnerMode::Polling => "polling",
        }
    }
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "webhook" => Some(RunnerMode::Webhook),
            "polling" => Some(RunnerMode::Polling),
            _ => None,
        }
    }
}

/// Runner lifecycle state.
///
/// `Active` = runner can receive jobs. `Disabled` = registration
/// exists but the operator has explicitly turned it off (e.g. for
/// maintenance). `Stale` = no heartbeat for >1h; reaper can flip
/// jobs targeted at stale runners to `errored` faster than the
/// general lease timeout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunnerStatus {
    Active,
    Disabled,
    Stale,
}

impl RunnerStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunnerStatus::Active => "active",
            RunnerStatus::Disabled => "disabled",
            RunnerStatus::Stale => "stale",
        }
    }
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "active" => Some(RunnerStatus::Active),
            "disabled" => Some(RunnerStatus::Disabled),
            "stale" => Some(RunnerStatus::Stale),
            _ => None,
        }
    }
}

/// A registered agent process that can receive replay-job webhooks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Runner {
    pub id: String,
    pub name: String,
    pub mode: RunnerMode,
    /// `None` for `mode = Polling`; required for `mode = Webhook`.
    pub webhook_url: Option<String>,
    /// AES-256-GCM-encrypted raw auth token. Encryption is the caller's
    /// responsibility (commit 4 `crypto` module). This module treats
    /// the bytes as opaque.
    pub encrypted_token: Vec<u8>,
    /// AES-GCM nonce (12 bytes) used to encrypt `encrypted_token`.
    /// Stored alongside the ciphertext; not secret in the AES-GCM
    /// threat model.
    pub token_nonce: Vec<u8>,
    /// `SHA-256(raw_token)` hex-encoded. Used for fast inbound auth
    /// lookup — when a runner sends `X-Rewind-Runner-Auth: <token>`,
    /// server hashes + looks up by this column (indexed).
    pub auth_token_hash: String,
    /// First 8 chars + `***` of the raw token. UI display only;
    /// lets operators identify which token they have without
    /// triggering the secret-redaction path.
    pub auth_token_preview: String,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub status: RunnerStatus,
}

// ── ReplayJob ────────────────────────────────────────────────────

/// State machine: `pending → dispatched → in_progress → completed/errored`.
///
/// **Cancellation is intentionally NOT in v1** (per Phase 3 plan
/// HIGH #5 resolution). v3.1 will add cooperative cancel with a
/// proper protocol; v1 jobs run to natural completion or error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplayJobState {
    Pending,
    Dispatched,
    InProgress,
    Completed,
    Errored,
}

impl ReplayJobState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReplayJobState::Pending => "pending",
            ReplayJobState::Dispatched => "dispatched",
            ReplayJobState::InProgress => "in_progress",
            ReplayJobState::Completed => "completed",
            ReplayJobState::Errored => "errored",
        }
    }
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(ReplayJobState::Pending),
            "dispatched" => Some(ReplayJobState::Dispatched),
            "in_progress" => Some(ReplayJobState::InProgress),
            "completed" => Some(ReplayJobState::Completed),
            "errored" => Some(ReplayJobState::Errored),
            _ => None,
        }
    }
    pub fn is_terminal(&self) -> bool {
        matches!(self, ReplayJobState::Completed | ReplayJobState::Errored)
    }
}

/// A dispatched replay job. Tracks state, lease deadlines, and progress.
///
/// **Review #152 comment 3:** `runner_id` and `replay_context_id` are
/// `Option<String>` because the underlying columns are `ON DELETE
/// SET NULL`. Historical jobs survive runner deletion or replay-context
/// expiry/deletion; the dashboard renders "Runner deleted" or "Context
/// deleted" for the null cases. Active jobs (state ∈ {pending,
/// dispatched, in_progress}) should never have null FKs in practice
/// because they're created with both populated and the deletion that
/// nulled them out would normally be blocked by the in-flight state —
/// but we don't enforce that constraint at the storage layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayJob {
    pub id: String,
    pub runner_id: Option<String>,
    pub session_id: String,
    pub replay_context_id: Option<String>,
    pub state: ReplayJobState,
    pub error_message: Option<String>,
    /// `"dispatch"` (runner didn't reply 202 by `dispatch_deadline_at`),
    /// `"agent"` (runner accepted but reported `errored`), or
    /// `"lease_expired"` (lease lapsed without progress events; reaper
    /// transitioned the job).
    pub error_stage: Option<String>,
    pub created_at: DateTime<Utc>,
    pub dispatched_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Runner must reply 202 by this time or the reaper marks the job
    /// `errored` with `stage: "dispatch"`. Default: dispatched_at + 10s.
    pub dispatch_deadline_at: Option<DateTime<Utc>>,
    /// Extended on every heartbeat or progress event. Default:
    /// last_event_at + 5min. Reaper marks `errored` with
    /// `stage: "lease_expired"` when exceeded.
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub progress_step: u32,
    /// Runner-supplied total step count (optional). Useful for
    /// progress-bar UI; absent for streaming/unbounded runs.
    pub progress_total: Option<u32>,
}

// ── ReplayJobEvent ───────────────────────────────────────────────

/// A single event emitted by a runner during job execution. Append-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplayJobEventType {
    Started,
    Progress,
    Completed,
    Errored,
}

impl ReplayJobEventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReplayJobEventType::Started => "started",
            ReplayJobEventType::Progress => "progress",
            ReplayJobEventType::Completed => "completed",
            ReplayJobEventType::Errored => "errored",
        }
    }
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "started" => Some(ReplayJobEventType::Started),
            "progress" => Some(ReplayJobEventType::Progress),
            "completed" => Some(ReplayJobEventType::Completed),
            "errored" => Some(ReplayJobEventType::Errored),
            _ => None,
        }
    }
}

/// One row from `replay_job_events`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayJobEvent {
    pub id: String,
    pub job_id: String,
    pub event_type: ReplayJobEventType,
    pub step_number: Option<u32>,
    /// Free-form JSON payload (kept as a string at the storage layer;
    /// the dashboard parses it). Typical shapes per event_type are
    /// documented in the Phase 3 plan's "Status events" section.
    pub payload: Option<String>,
    pub created_at: DateTime<Utc>,
}

// ── CRUD on Store ────────────────────────────────────────────────

impl Store {
    // Runners
    // -------------------------------------------------------------

    /// Insert a new runner. Caller (rewind-web) is responsible for:
    /// 1. Generating the raw token + UUID id
    /// 2. Encrypting via the app key (AES-256-GCM)
    /// 3. Hashing for the auth_token_hash field
    /// 4. Calling this with the populated Runner row
    ///
    /// We keep the Runner→encryption boundary explicit (commit 4
    /// adds the encryption layer; this module stores opaque bytes).
    pub fn create_runner(&self, runner: &Runner) -> Result<()> {
        self.conn.execute(
            "INSERT INTO runners (id, name, mode, webhook_url, encrypted_token, token_nonce, auth_token_hash, auth_token_preview, created_at, last_seen_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                runner.id,
                runner.name,
                runner.mode.as_str(),
                runner.webhook_url,
                runner.encrypted_token,
                runner.token_nonce,
                runner.auth_token_hash,
                runner.auth_token_preview,
                runner.created_at.to_rfc3339(),
                runner.last_seen_at.as_ref().map(|t| t.to_rfc3339()),
                runner.status.as_str(),
            ],
        )?;
        Ok(())
    }

    /// Lookup by primary key.
    pub fn get_runner(&self, id: &str) -> Result<Option<Runner>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, mode, webhook_url, encrypted_token, token_nonce, auth_token_hash, auth_token_preview, created_at, last_seen_at, status
             FROM runners WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], Self::row_to_runner)?;
        Ok(rows.next().transpose()?)
    }

    /// Fast-path inbound-auth lookup: given the SHA-256 hex of the
    /// runner-supplied `X-Rewind-Runner-Auth` header, find the matching
    /// runner. The `auth_token_hash` column is indexed.
    pub fn get_runner_by_auth_hash(&self, hash: &str) -> Result<Option<Runner>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, mode, webhook_url, encrypted_token, token_nonce, auth_token_hash, auth_token_preview, created_at, last_seen_at, status
             FROM runners WHERE auth_token_hash = ?1",
        )?;
        let mut rows = stmt.query_map(params![hash], Self::row_to_runner)?;
        Ok(rows.next().transpose()?)
    }

    /// List all runners ordered by created_at desc (newest first).
    pub fn list_runners(&self) -> Result<Vec<Runner>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, mode, webhook_url, encrypted_token, token_nonce, auth_token_hash, auth_token_preview, created_at, last_seen_at, status
             FROM runners ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], Self::row_to_runner)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Hard-delete a runner. Replay jobs referencing it remain in the
    /// DB (foreign-key constraint not cascading) so historical job
    /// records aren't silently lost; the dashboard's runner-detail
    /// view shows them as "Runner deleted".
    pub fn delete_runner(&self, id: &str) -> Result<bool> {
        let n = self.conn.execute("DELETE FROM runners WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    /// Update the runner's status (active / disabled / stale).
    pub fn set_runner_status(&self, id: &str, status: RunnerStatus) -> Result<()> {
        self.conn.execute(
            "UPDATE runners SET status = ?1 WHERE id = ?2",
            params![status.as_str(), id],
        )?;
        Ok(())
    }

    /// Update `last_seen_at` to `now`. Called from the heartbeat
    /// endpoint (commit 4).
    pub fn touch_runner_last_seen(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE runners SET last_seen_at = ?1 WHERE id = ?2",
            params![Utc::now().to_rfc3339(), id],
        )?;
        Ok(())
    }

    /// Set the dispatch deadline and initial lease on a freshly-
    /// dispatched job. Used by the dispatcher in commit 5/13 right
    /// after the runner returns 2xx.
    ///
    /// Both timestamps are absolute (caller computes them from
    /// `Utc::now()`), so dispatcher-side test mocking can supply
    /// deterministic values without monkey-patching time.
    pub fn set_dispatch_deadline_and_lease(
        &self,
        id: &str,
        dispatch_deadline_at: DateTime<Utc>,
        lease_expires_at: DateTime<Utc>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE replay_jobs
             SET dispatch_deadline_at = ?1, lease_expires_at = ?2
             WHERE id = ?3",
            params![
                dispatch_deadline_at.to_rfc3339(),
                lease_expires_at.to_rfc3339(),
                id
            ],
        )?;
        Ok(())
    }

    /// Reaper-side query: jobs in state `dispatched` whose
    /// `dispatch_deadline_at < now`. Means the runner accepted the
    /// dispatch but never emitted a `Started` event in time.
    pub fn list_dispatch_deadline_expired(&self, now: DateTime<Utc>) -> Result<Vec<ReplayJob>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, runner_id, session_id, replay_context_id, state, error_message, error_stage, created_at, dispatched_at, started_at, completed_at, dispatch_deadline_at, lease_expires_at, progress_step, progress_total
             FROM replay_jobs
             WHERE state = 'dispatched' AND dispatch_deadline_at IS NOT NULL AND dispatch_deadline_at < ?1
             ORDER BY dispatch_deadline_at ASC LIMIT 1000",
        )?;
        let rows = stmt.query_map(params![now.to_rfc3339()], Self::row_to_replay_job)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Count non-terminal jobs that reference this replay context.
    /// **Phase 3 commit 6** uses this to enforce one-job-per-context
    /// for shape-B job creation: the cursor would otherwise be a
    /// hot-spot if two runners advanced it concurrently.
    pub fn count_in_flight_jobs_for_replay_context(&self, replay_context_id: &str) -> Result<u32> {
        let n: u32 = self.conn.query_row(
            "SELECT COUNT(*) FROM replay_jobs
             WHERE replay_context_id = ?1
               AND state NOT IN ('completed', 'errored')",
            params![replay_context_id],
            |row| row.get(0),
        )?;
        Ok(n)
    }

    /// Count non-terminal jobs (`pending`, `dispatched`, `in_progress`)
    /// owned by this runner.
    ///
    /// **Used by review #153 HIGH 3 + MEDIUM 4:** the HTTP layer
    /// uses this to refuse `DELETE /api/runners/{id}` and
    /// `POST /api/runners/{id}/regenerate-token` while in-flight
    /// jobs would be orphaned (deletion → null `runner_id`,
    /// rotation → in-flight callbacks fail auth).
    pub fn count_active_jobs_for_runner(&self, runner_id: &str) -> Result<u32> {
        let n: u32 = self.conn.query_row(
            "SELECT COUNT(*) FROM replay_jobs
             WHERE runner_id = ?1
               AND state NOT IN ('completed', 'errored')",
            params![runner_id],
            |row| row.get(0),
        )?;
        Ok(n)
    }

    /// Rotate a runner's auth token. Replaces the encrypted token,
    /// nonce, hash, and preview atomically. Returns `true` if a row
    /// was updated, `false` if the id doesn't exist.
    ///
    /// **Phase 3 commit 4:** the dashboard's "regenerate token"
    /// button calls this. The old hash is invalidated immediately
    /// so any in-flight inbound auth attempts using the old token
    /// will fail. In-flight outbound dispatches use the new token
    /// because dispatch signs at dispatch time, not at job-creation
    /// time.
    pub fn rotate_runner_token(
        &self,
        id: &str,
        encrypted_token: &[u8],
        token_nonce: &[u8],
        auth_token_hash: &str,
        auth_token_preview: &str,
    ) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE runners
             SET encrypted_token = ?1,
                 token_nonce = ?2,
                 auth_token_hash = ?3,
                 auth_token_preview = ?4
             WHERE id = ?5",
            params![
                encrypted_token,
                token_nonce,
                auth_token_hash,
                auth_token_preview,
                id
            ],
        )?;
        Ok(n > 0)
    }

    fn row_to_runner(row: &rusqlite::Row) -> rusqlite::Result<Runner> {
        Ok(Runner {
            id: row.get(0)?,
            name: row.get(1)?,
            mode: {
                let s: String = row.get(2)?;
                RunnerMode::from_db_str(&s).ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        format!("invalid runner mode: {s}").into(),
                    )
                })?
            },
            webhook_url: row.get(3)?,
            encrypted_token: row.get(4)?,
            token_nonce: row.get(5)?,
            auth_token_hash: row.get(6)?,
            auth_token_preview: row.get(7)?,
            created_at: parse_dt(row, 8)?,
            last_seen_at: parse_dt_opt(row, 9)?,
            status: {
                let s: String = row.get(10)?;
                RunnerStatus::from_db_str(&s).ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        10,
                        rusqlite::types::Type::Text,
                        format!("invalid runner status: {s}").into(),
                    )
                })?
            },
        })
    }

    // ReplayJobs
    // -------------------------------------------------------------

    /// Insert a new replay job.
    ///
    /// **Review #152 comment 5:** rejects creation of *active* jobs
    /// (`pending`, `dispatched`, `in_progress`) with null `runner_id`
    /// or `replay_context_id`. Such jobs would be undispatchable —
    /// no runner to send the webhook to, no context to replay against
    /// — and would silently rot in the queue forever.
    ///
    /// Null FKs are *only* legal for terminal-state rows (`completed`,
    /// `errored`) which represent historical / imported / orphaned-by-
    /// cascade jobs whose original runner or context no longer exists.
    pub fn create_replay_job(&self, job: &ReplayJob) -> Result<()> {
        if !job.state.is_terminal() {
            if job.runner_id.is_none() {
                return Err(anyhow!(
                    "replay_jobs.runner_id is required for non-terminal jobs (state={}); null is only allowed for completed/errored historical rows after ON DELETE SET NULL cascade",
                    job.state.as_str()
                ));
            }
            if job.replay_context_id.is_none() {
                return Err(anyhow!(
                    "replay_jobs.replay_context_id is required for non-terminal jobs (state={}); null is only allowed for completed/errored historical rows after ON DELETE SET NULL cascade",
                    job.state.as_str()
                ));
            }
        }
        self.conn.execute(
            "INSERT INTO replay_jobs (id, runner_id, session_id, replay_context_id, state, error_message, error_stage, created_at, dispatched_at, started_at, completed_at, dispatch_deadline_at, lease_expires_at, progress_step, progress_total)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                job.id,
                job.runner_id,
                job.session_id,
                job.replay_context_id,
                job.state.as_str(),
                job.error_message,
                job.error_stage,
                job.created_at.to_rfc3339(),
                job.dispatched_at.as_ref().map(|t| t.to_rfc3339()),
                job.started_at.as_ref().map(|t| t.to_rfc3339()),
                job.completed_at.as_ref().map(|t| t.to_rfc3339()),
                job.dispatch_deadline_at.as_ref().map(|t| t.to_rfc3339()),
                job.lease_expires_at.as_ref().map(|t| t.to_rfc3339()),
                job.progress_step,
                job.progress_total,
            ],
        )?;
        Ok(())
    }

    /// Find expired jobs by runner_id (used for diagnostic queries).
    /// Returns jobs whose runner_id matches the supplied id (NOT the
    /// null-runner case).
    fn _list_replay_jobs_by_runner_inner(&self, runner_id: &str, limit: u32) -> Result<Vec<ReplayJob>> {
        // helper for tests / diagnostics — see list_replay_jobs_by_runner
        let mut stmt = self.conn.prepare(
            "SELECT id, runner_id, session_id, replay_context_id, state, error_message, error_stage, created_at, dispatched_at, started_at, completed_at, dispatch_deadline_at, lease_expires_at, progress_step, progress_total
             FROM replay_jobs WHERE runner_id = ?1 ORDER BY created_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![runner_id, limit], Self::row_to_replay_job)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_replay_job(&self, id: &str) -> Result<Option<ReplayJob>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, runner_id, session_id, replay_context_id, state, error_message, error_stage, created_at, dispatched_at, started_at, completed_at, dispatch_deadline_at, lease_expires_at, progress_step, progress_total
             FROM replay_jobs WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], Self::row_to_replay_job)?;
        Ok(rows.next().transpose()?)
    }

    /// List jobs for a runner (newest first). Excludes jobs whose
    /// `runner_id` was nulled out by `ON DELETE SET NULL` after the
    /// runner was deleted — those still exist in the table for
    /// historical context but aren't owned by any runner.
    pub fn list_replay_jobs_by_runner(&self, runner_id: &str, limit: u32) -> Result<Vec<ReplayJob>> {
        self._list_replay_jobs_by_runner_inner(runner_id, limit)
    }

    /// List jobs whose `runner_id` is null (i.e. their runner has
    /// been deleted). Useful for the dashboard's "orphaned jobs"
    /// view and for cleanup tooling.
    pub fn list_orphaned_replay_jobs(&self, limit: u32) -> Result<Vec<ReplayJob>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, runner_id, session_id, replay_context_id, state, error_message, error_stage, created_at, dispatched_at, started_at, completed_at, dispatch_deadline_at, lease_expires_at, progress_step, progress_total
             FROM replay_jobs WHERE runner_id IS NULL ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], Self::row_to_replay_job)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// List jobs for a session, newest first.
    pub fn list_replay_jobs_by_session(&self, session_id: &str) -> Result<Vec<ReplayJob>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, runner_id, session_id, replay_context_id, state, error_message, error_stage, created_at, dispatched_at, started_at, completed_at, dispatch_deadline_at, lease_expires_at, progress_step, progress_total
             FROM replay_jobs WHERE session_id = ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![session_id], Self::row_to_replay_job)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find expired jobs (dispatch deadline OR lease expired) that
    /// are still in non-terminal states. Used by the reaper task in
    /// commit 5.
    pub fn list_expired_replay_jobs(&self) -> Result<Vec<ReplayJob>> {
        let now = Utc::now().to_rfc3339();
        let mut stmt = self.conn.prepare(
            "SELECT id, runner_id, session_id, replay_context_id, state, error_message, error_stage, created_at, dispatched_at, started_at, completed_at, dispatch_deadline_at, lease_expires_at, progress_step, progress_total
             FROM replay_jobs
             WHERE state IN ('dispatched', 'in_progress')
               AND (
                   (state = 'dispatched' AND dispatch_deadline_at IS NOT NULL AND dispatch_deadline_at < ?1)
                OR (state = 'in_progress' AND lease_expires_at IS NOT NULL AND lease_expires_at < ?1)
               )",
        )?;
        let rows = stmt.query_map(params![now], Self::row_to_replay_job)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Transition a job to a new state.
    ///
    /// **Review #152 comment 2 fix:** terminal-state protection is now
    /// enforced at the SQL level via `WHERE id = ?N AND state NOT IN
    /// ('completed', 'errored')`. This closes the TOCTOU race where
    /// the previous read-then-write check let two concurrent transactions
    /// (e.g. reaper + late `completed` event) both pass the check and
    /// then race to write conflicting terminal states.
    ///
    /// Returns `true` if the row was updated (state advanced),
    /// `false` if no row matched (either the job doesn't exist OR
    /// it's already in a terminal state). Idempotent caller behavior.
    ///
    /// Sets the corresponding timestamp column for the new state.
    pub fn advance_replay_job_state(
        &self,
        id: &str,
        new_state: ReplayJobState,
        error_message: Option<&str>,
        error_stage: Option<&str>,
    ) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let (timestamp_col, timestamp_val): (Option<&str>, Option<&str>) = match new_state {
            ReplayJobState::Dispatched => (Some("dispatched_at"), Some(now.as_str())),
            ReplayJobState::InProgress => (Some("started_at"), Some(now.as_str())),
            ReplayJobState::Completed | ReplayJobState::Errored => {
                (Some("completed_at"), Some(now.as_str()))
            }
            ReplayJobState::Pending => (None, None),
        };

        // ATOMIC: state guard inlined into the WHERE clause so two
        // concurrent transactions can't both pass a read-side check
        // and then race to write conflicting terminal states. SQLite
        // serializes the UPDATE; whichever lands first wins, the
        // second sees 0 rows-affected and returns false.
        let n = if let (Some(col), Some(val)) = (timestamp_col, timestamp_val) {
            self.conn.execute(
                &format!(
                    "UPDATE replay_jobs SET state = ?1, error_message = ?2, error_stage = ?3, {col} = ?4
                     WHERE id = ?5 AND state NOT IN ('completed', 'errored')"
                ),
                params![new_state.as_str(), error_message, error_stage, val, id],
            )?
        } else {
            self.conn.execute(
                "UPDATE replay_jobs SET state = ?1, error_message = ?2, error_stage = ?3
                 WHERE id = ?4 AND state NOT IN ('completed', 'errored')",
                params![new_state.as_str(), error_message, error_stage, id],
            )?
        };
        Ok(n > 0)
    }

    /// Mark a `dispatched` job as `errored` from the dispatch path
    /// (review #154 round 2 follow-up).
    ///
    /// Stricter than `advance_replay_job_state` because it ONLY
    /// matches rows currently in `dispatched`. Used by the
    /// dispatcher's apply_outcome failure branch so a fast runner
    /// that already emitted `started` (transitioning the row to
    /// `in_progress`) isn't overwritten by a later HTTP-error
    /// transition. The general advance helper's
    /// `WHERE state NOT IN ('completed', 'errored')` guard would
    /// otherwise match the in_progress row and corrupt the state.
    ///
    /// Returns `true` if the row was updated, `false` if no row
    /// matched (job missing OR already in `in_progress`/terminal).
    pub fn mark_dispatched_job_as_errored(
        &self,
        id: &str,
        error_message: &str,
        error_stage: &str,
    ) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let n = self.conn.execute(
            "UPDATE replay_jobs
             SET state = 'errored',
                 error_message = ?1,
                 error_stage = ?2,
                 completed_at = ?3
             WHERE id = ?4 AND state = 'dispatched'",
            params![error_message, error_stage, now, id],
        )?;
        Ok(n > 0)
    }

    /// Atomically record an event AND apply the state/progress/lease
    /// update it implies. Single transaction; full state-machine
    /// guarded in SQL.
    ///
    /// **Review #152 comment 1 fix:** event insertion and state
    /// transition happen in one transaction. If the guard rejects
    /// the event, no event row is inserted (rolled back).
    ///
    /// **Review #152 comment 6 fix:** rejects illegal event/state
    /// combinations *before* terminal state, not just terminal
    /// rewrites. The legal transitions are:
    ///
    /// | event       | required current state         | new state      |
    /// |-------------|--------------------------------|----------------|
    /// | `Started`   | `Dispatched`                   | `InProgress`   |
    /// | `Progress`  | `InProgress`                   | `InProgress`   |
    /// | `Completed` | `InProgress`                   | `Completed`    |
    /// | `Errored`   | `Dispatched` or `InProgress`   | `Errored`      |
    ///
    /// `Errored` accepts `Dispatched` because a runner can fail at
    /// startup before emitting `Started` (e.g. webhook delivered but
    /// the agent process crashed). All other illegal combinations
    /// (e.g. `Progress` on `pending`, `Completed` on `pending`,
    /// duplicate `Started` on `in_progress`) are rejected with
    /// `Ok(false)` and roll back the transaction.
    ///
    /// `lease_extension_seconds` controls how far in the future
    /// `lease_expires_at` is bumped on Started and Progress events.
    /// 300 (5 min) is the production default; tests may pass smaller
    /// values to exercise expiry behavior.
    pub fn record_replay_job_event_atomic(
        &mut self,
        event: &ReplayJobEvent,
        progress_total: Option<u32>,
        error_message: Option<&str>,
        error_stage: Option<&str>,
        lease_extension_seconds: i64,
    ) -> Result<bool> {
        let tx = self.conn.transaction()?;

        // 1. Read current state in the same transaction. SQLite's
        // default tx semantics give us serializable isolation —
        // concurrent transactions block until commit/rollback.
        let current_state: Option<String> = tx
            .query_row(
                "SELECT state FROM replay_jobs WHERE id = ?1",
                params![event.job_id],
                |row| row.get(0),
            )
            .ok();
        let Some(state_str) = current_state else {
            tx.rollback()?;
            return Ok(false);
        };

        // 2. Enforce the full state machine: this event_type must be
        // legal from the current state. Rejected combos roll back
        // with no event row inserted.
        let current_state_enum = ReplayJobState::from_db_str(&state_str).ok_or_else(|| {
            anyhow!(
                "invalid state in DB for job {}: {}",
                event.job_id,
                state_str
            )
        })?;
        let legal = match event.event_type {
            ReplayJobEventType::Started => {
                matches!(current_state_enum, ReplayJobState::Dispatched)
            }
            ReplayJobEventType::Progress => {
                matches!(current_state_enum, ReplayJobState::InProgress)
            }
            ReplayJobEventType::Completed => {
                matches!(current_state_enum, ReplayJobState::InProgress)
            }
            ReplayJobEventType::Errored => matches!(
                current_state_enum,
                ReplayJobState::Dispatched | ReplayJobState::InProgress
            ),
        };
        if !legal {
            tx.rollback()?;
            return Ok(false);
        }

        // 3. Insert the event row.
        tx.execute(
            "INSERT INTO replay_job_events (id, job_id, event_type, step_number, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.id,
                event.job_id,
                event.event_type.as_str(),
                event.step_number,
                event.payload,
                event.created_at.to_rfc3339(),
            ],
        )?;

        // 4. Apply the state/progress/lease change for this event_type.
        let now = Utc::now().to_rfc3339();
        let new_lease = (Utc::now() + chrono::Duration::seconds(lease_extension_seconds))
            .to_rfc3339();
        match event.event_type {
            ReplayJobEventType::Started => {
                tx.execute(
                    "UPDATE replay_jobs SET state = 'in_progress', started_at = ?1, lease_expires_at = ?2
                     WHERE id = ?3",
                    params![now, new_lease, event.job_id],
                )?;
            }
            ReplayJobEventType::Progress => {
                tx.execute(
                    "UPDATE replay_jobs SET progress_step = ?1, progress_total = COALESCE(?2, progress_total), lease_expires_at = ?3
                     WHERE id = ?4",
                    params![event.step_number.unwrap_or(0), progress_total, new_lease, event.job_id],
                )?;
            }
            ReplayJobEventType::Completed => {
                tx.execute(
                    "UPDATE replay_jobs SET state = 'completed', completed_at = ?1
                     WHERE id = ?2",
                    params![now, event.job_id],
                )?;
            }
            ReplayJobEventType::Errored => {
                tx.execute(
                    "UPDATE replay_jobs SET state = 'errored', completed_at = ?1, error_message = ?2, error_stage = ?3
                     WHERE id = ?4",
                    params![now, error_message, error_stage, event.job_id],
                )?;
            }
        }

        tx.commit()?;
        Ok(true)
    }

    /// Extend the lease (called on heartbeat / progress events). The
    /// new `lease_expires_at` is computed by the caller (typically
    /// `Utc::now() + 5min`).
    pub fn extend_replay_job_lease(
        &self,
        id: &str,
        lease_expires_at: DateTime<Utc>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE replay_jobs SET lease_expires_at = ?1 WHERE id = ?2 AND state IN ('dispatched', 'in_progress')",
            params![lease_expires_at.to_rfc3339(), id],
        )?;
        Ok(())
    }

    /// Update progress counters. Called when a `progress` event arrives.
    pub fn update_replay_job_progress(
        &self,
        id: &str,
        step_number: u32,
        progress_total: Option<u32>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE replay_jobs SET progress_step = ?1, progress_total = COALESCE(?2, progress_total) WHERE id = ?3",
            params![step_number, progress_total, id],
        )?;
        Ok(())
    }

    fn row_to_replay_job(row: &rusqlite::Row) -> rusqlite::Result<ReplayJob> {
        Ok(ReplayJob {
            id: row.get(0)?,
            // ON DELETE SET NULL means runner_id can be NULL after
            // runner deletion (Review #152 comment 3); model as Option<>.
            runner_id: row.get(1)?,
            session_id: row.get(2)?,
            // Same: nullable after replay-context deletion / TTL.
            replay_context_id: row.get(3)?,
            state: {
                let s: String = row.get(4)?;
                ReplayJobState::from_db_str(&s).ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        4,
                        rusqlite::types::Type::Text,
                        format!("invalid replay job state: {s}").into(),
                    )
                })?
            },
            error_message: row.get(5)?,
            error_stage: row.get(6)?,
            created_at: parse_dt(row, 7)?,
            dispatched_at: parse_dt_opt(row, 8)?,
            started_at: parse_dt_opt(row, 9)?,
            completed_at: parse_dt_opt(row, 10)?,
            dispatch_deadline_at: parse_dt_opt(row, 11)?,
            lease_expires_at: parse_dt_opt(row, 12)?,
            progress_step: row.get(13)?,
            progress_total: row.get(14)?,
        })
    }

    // ReplayJobEvents (append-only — see record_replay_job_event_atomic
    // for the production write path; this raw method is footgun-protected
    // pub(crate) for diagnostic / migration tooling only).
    // -------------------------------------------------------------

    /// Insert an event row without checking job state.
    ///
    /// **Review #152 comment 1:** this raw insert is gated behind
    /// `#[cfg(test)]` because it bypasses the terminal-state guard.
    /// Production callers (HTTP event ingestion in commit 6) MUST go
    /// through [`Self::record_replay_job_event_atomic`] which
    /// validates state in a transaction. Tests use this to seed
    /// out-of-band event histories that wouldn't be reachable via
    /// the production state machine. If a future operator tool
    /// genuinely needs raw event backfill, promote this to
    /// `pub(crate)` then and document the migration use case.
    #[cfg(test)]
    pub(crate) fn append_replay_job_event(&self, event: &ReplayJobEvent) -> Result<()> {
        self.conn.execute(
            "INSERT INTO replay_job_events (id, job_id, event_type, step_number, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.id,
                event.job_id,
                event.event_type.as_str(),
                event.step_number,
                event.payload,
                event.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// List events for a job in insertion order (created_at asc).
    /// The dashboard's modal needs them in chronological order to
    /// render the progress timeline.
    pub fn list_replay_job_events(&self, job_id: &str) -> Result<Vec<ReplayJobEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, job_id, event_type, step_number, payload, created_at
             FROM replay_job_events WHERE job_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![job_id], |row| {
            Ok(ReplayJobEvent {
                id: row.get(0)?,
                job_id: row.get(1)?,
                event_type: {
                    let s: String = row.get(2)?;
                    ReplayJobEventType::from_db_str(&s).ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            format!("invalid event type: {s}").into(),
                        )
                    })?
                },
                step_number: row.get(3)?,
                payload: row.get(4)?,
                created_at: parse_dt(row, 5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

// ── Helpers ──────────────────────────────────────────────────────

fn parse_dt(row: &rusqlite::Row, col: usize) -> rusqlite::Result<DateTime<Utc>> {
    let s: String = row.get(col)?;
    DateTime::parse_from_rfc3339(&s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                col,
                rusqlite::types::Type::Text,
                format!("bad datetime in column {col}: {e}").into(),
            )
        })
}

fn parse_dt_opt(row: &rusqlite::Row, col: usize) -> rusqlite::Result<Option<DateTime<Utc>>> {
    let s: Option<String> = row.get(col)?;
    s.map(|s| {
        DateTime::parse_from_rfc3339(&s)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    col,
                    rusqlite::types::Type::Text,
                    format!("bad datetime in column {col}: {e}").into(),
                )
            })
    })
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn test_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (store, dir)
    }

    fn fake_runner(name: &str) -> Runner {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(format!("token-{name}").as_bytes());
        Runner {
            id: Uuid::new_v4().to_string(),
            name: name.to_string(),
            mode: RunnerMode::Webhook,
            webhook_url: Some("http://example.com/webhook".to_string()),
            encrypted_token: vec![1, 2, 3, 4],
            token_nonce: vec![0; 12],
            auth_token_hash: format!("{:x}", h.finalize()),
            auth_token_preview: "tok12345***".to_string(),
            created_at: Utc::now(),
            last_seen_at: None,
            status: RunnerStatus::Active,
        }
    }

    #[test]
    fn runner_round_trip() {
        let (store, _dir) = test_store();
        let runner = fake_runner("ray-agent");
        store.create_runner(&runner).unwrap();

        let fetched = store.get_runner(&runner.id).unwrap().unwrap();
        assert_eq!(fetched.name, "ray-agent");
        assert_eq!(fetched.mode, RunnerMode::Webhook);
        assert_eq!(fetched.encrypted_token, vec![1, 2, 3, 4]);
        assert_eq!(fetched.token_nonce, vec![0; 12]);
        assert_eq!(fetched.status, RunnerStatus::Active);
    }

    #[test]
    fn list_runners_orders_newest_first() {
        let (store, _dir) = test_store();
        let r1 = fake_runner("first");
        store.create_runner(&r1).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let r2 = fake_runner("second");
        store.create_runner(&r2).unwrap();

        let list = store.list_runners().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "second");
        assert_eq!(list[1].name, "first");
    }

    #[test]
    fn lookup_by_auth_hash_is_indexed_path() {
        // Sanity that the indexed lookup returns the right runner. The
        // hash is unique per token; collision testing is the crypto
        // module's concern.
        let (store, _dir) = test_store();
        let runner = fake_runner("ray-agent");
        let hash = runner.auth_token_hash.clone();
        store.create_runner(&runner).unwrap();

        let found = store.get_runner_by_auth_hash(&hash).unwrap().unwrap();
        assert_eq!(found.id, runner.id);

        // Wrong hash → None.
        assert!(store.get_runner_by_auth_hash("nonexistent").unwrap().is_none());
    }

    #[test]
    fn delete_runner_returns_true_on_hit_false_on_miss() {
        let (store, _dir) = test_store();
        let runner = fake_runner("ephemeral");
        store.create_runner(&runner).unwrap();
        assert!(store.delete_runner(&runner.id).unwrap());
        // Second delete: row already gone.
        assert!(!store.delete_runner(&runner.id).unwrap());
        assert!(store.get_runner(&runner.id).unwrap().is_none());
    }

    #[test]
    fn set_runner_status_changes_active_to_disabled() {
        let (store, _dir) = test_store();
        let runner = fake_runner("toggle-me");
        store.create_runner(&runner).unwrap();
        assert_eq!(
            store.get_runner(&runner.id).unwrap().unwrap().status,
            RunnerStatus::Active
        );
        store.set_runner_status(&runner.id, RunnerStatus::Disabled).unwrap();
        assert_eq!(
            store.get_runner(&runner.id).unwrap().unwrap().status,
            RunnerStatus::Disabled
        );
    }

    #[test]
    fn touch_last_seen_updates_timestamp() {
        let (store, _dir) = test_store();
        let runner = fake_runner("heartbeat");
        store.create_runner(&runner).unwrap();
        assert!(store.get_runner(&runner.id).unwrap().unwrap().last_seen_at.is_none());

        store.touch_runner_last_seen(&runner.id).unwrap();
        let after = store.get_runner(&runner.id).unwrap().unwrap();
        assert!(after.last_seen_at.is_some());
    }

    /// Seed a session + timeline + replay_context and return the
    /// context id. Used by tests that exercise the replay_context_id
    /// FK behavior (e.g. ON DELETE SET NULL cascade).
    fn fake_replay_context_for_job(store: &Store, session_id: &str) -> String {
        let timeline_id = Uuid::new_v4().to_string();
        store
            .conn
            .execute(
                "INSERT INTO timelines (id, session_id, parent_timeline_id, fork_at_step, created_at, label)
                 VALUES (?1, ?2, NULL, NULL, ?3, 'main')",
                params![timeline_id, session_id, Utc::now().to_rfc3339()],
            )
            .unwrap();
        let ctx_id = Uuid::new_v4().to_string();
        store
            .create_replay_context(&ctx_id, session_id, &timeline_id, 0)
            .unwrap();
        ctx_id
    }

    fn fake_session_for_job(store: &Store) -> String {
        // Replay jobs FK to sessions, so we need a real session row.
        use crate::{Session, SessionSource, SessionStatus};
        let session = Session {
            id: Uuid::new_v4().to_string(),
            name: "test-session".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            status: SessionStatus::Recording,
            source: SessionSource::Hooks,
            total_steps: 0,
            total_tokens: 0,
            metadata: serde_json::json!({}),
            thread_id: None,
            thread_ordinal: None,
        };
        store.create_session(&session).unwrap();
        session.id
    }

    /// Build a *valid* active job referencing seeded fixtures.
    ///
    /// Seeds a fresh replay_context (timeline + context) on the
    /// supplied store and assigns it to `replay_context_id`. This is
    /// the production-shaped fixture: caller has both FKs populated
    /// so `create_replay_job` accepts it for active states.
    /// (Review #152 comment 5: non-terminal jobs require both FKs.)
    fn fake_job(store: &Store, runner_id: &str, session_id: &str) -> ReplayJob {
        let ctx_id = fake_replay_context_for_job(store, session_id);
        ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: Some(runner_id.to_string()),
            session_id: session_id.to_string(),
            replay_context_id: Some(ctx_id),
            state: ReplayJobState::Pending,
            error_message: None,
            error_stage: None,
            created_at: Utc::now(),
            dispatched_at: None,
            started_at: None,
            completed_at: None,
            dispatch_deadline_at: None,
            lease_expires_at: None,
            progress_step: 0,
            progress_total: None,
        }
    }

    #[test]
    fn replay_job_round_trip() {
        let (store, _dir) = test_store();
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);

        let job = fake_job(&store, &runner.id, &session_id);
        store.create_replay_job(&job).unwrap();
        let fetched = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(fetched.state, ReplayJobState::Pending);
        assert_eq!(fetched.runner_id.as_deref(), Some(runner.id.as_str()));
        assert_eq!(fetched.session_id, session_id);
        assert_eq!(fetched.progress_step, 0);
    }

    #[test]
    fn advance_state_sets_correct_timestamp() {
        let (store, _dir) = test_store();
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);
        let job = fake_job(&store, &runner.id, &session_id);
        store.create_replay_job(&job).unwrap();

        // pending → dispatched: dispatched_at set.
        assert!(store
            .advance_replay_job_state(&job.id, ReplayJobState::Dispatched, None, None)
            .unwrap());
        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(after.state, ReplayJobState::Dispatched);
        assert!(after.dispatched_at.is_some());

        // dispatched → in_progress: started_at set.
        assert!(store
            .advance_replay_job_state(&job.id, ReplayJobState::InProgress, None, None)
            .unwrap());
        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert!(after.started_at.is_some());

        // in_progress → completed: completed_at set.
        assert!(store
            .advance_replay_job_state(&job.id, ReplayJobState::Completed, None, None)
            .unwrap());
        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert!(after.completed_at.is_some());
    }

    #[test]
    fn advance_state_refuses_terminal_transitions() {
        let (store, _dir) = test_store();
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);
        let job = fake_job(&store, &runner.id, &session_id);
        store.create_replay_job(&job).unwrap();

        // Move to errored.
        store
            .advance_replay_job_state(&job.id, ReplayJobState::Errored, Some("agent died"), Some("agent"))
            .unwrap();

        // Subsequent transition attempt: refused (returns false).
        // This protects against a runner that crashed and recovered
        // sending late events that would corrupt the terminal state.
        let result = store
            .advance_replay_job_state(&job.id, ReplayJobState::Completed, None, None)
            .unwrap();
        assert!(!result, "terminal state must not accept further transitions");

        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(after.state, ReplayJobState::Errored);
        assert_eq!(after.error_message.as_deref(), Some("agent died"));
        assert_eq!(after.error_stage.as_deref(), Some("agent"));
    }

    #[test]
    fn list_expired_jobs_finds_lease_expired() {
        let (store, _dir) = test_store();
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);

        // Job 1: in_progress with expired lease.
        let mut job1 = fake_job(&store, &runner.id, &session_id);
        job1.state = ReplayJobState::InProgress;
        job1.lease_expires_at = Some(Utc::now() - chrono::Duration::seconds(60));
        store.create_replay_job(&job1).unwrap();

        // Job 2: in_progress with future lease — NOT expired.
        let mut job2 = fake_job(&store, &runner.id, &session_id);
        job2.state = ReplayJobState::InProgress;
        job2.lease_expires_at = Some(Utc::now() + chrono::Duration::minutes(5));
        store.create_replay_job(&job2).unwrap();

        // Job 3: dispatched with expired dispatch deadline.
        let mut job3 = fake_job(&store, &runner.id, &session_id);
        job3.state = ReplayJobState::Dispatched;
        job3.dispatch_deadline_at = Some(Utc::now() - chrono::Duration::seconds(30));
        store.create_replay_job(&job3).unwrap();

        // Job 4: terminal — must NOT appear in the expired list even
        // if its lease column happens to be in the past.
        let mut job4 = fake_job(&store, &runner.id, &session_id);
        job4.state = ReplayJobState::Completed;
        job4.lease_expires_at = Some(Utc::now() - chrono::Duration::hours(1));
        store.create_replay_job(&job4).unwrap();

        let expired = store.list_expired_replay_jobs().unwrap();
        let ids: std::collections::HashSet<_> = expired.iter().map(|j| j.id.clone()).collect();
        assert!(ids.contains(&job1.id), "lease-expired in_progress job should be listed");
        assert!(ids.contains(&job3.id), "dispatch-deadline-expired job should be listed");
        assert!(!ids.contains(&job2.id), "future-lease job should NOT be listed");
        assert!(!ids.contains(&job4.id), "terminal job should NOT be listed");
    }

    #[test]
    fn extend_lease_only_works_on_active_states() {
        let (store, _dir) = test_store();
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);
        let mut job = fake_job(&store, &runner.id, &session_id);
        job.state = ReplayJobState::InProgress;
        store.create_replay_job(&job).unwrap();

        // Active state — extend works.
        let new_lease = Utc::now() + chrono::Duration::minutes(10);
        store.extend_replay_job_lease(&job.id, new_lease).unwrap();
        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert!(after.lease_expires_at.is_some());

        // Move to terminal; subsequent extend is a no-op (the
        // UPDATE statement filters by state).
        store
            .advance_replay_job_state(&job.id, ReplayJobState::Completed, None, None)
            .unwrap();
        let lease_after_terminal = store.get_replay_job(&job.id).unwrap().unwrap().lease_expires_at;
        let attempt = Utc::now() + chrono::Duration::minutes(20);
        store.extend_replay_job_lease(&job.id, attempt).unwrap();
        let after_attempt = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(
            after_attempt.lease_expires_at, lease_after_terminal,
            "lease extend on terminal job should be no-op"
        );
    }

    #[test]
    fn append_and_list_events_preserves_order() {
        let (store, _dir) = test_store();
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);
        let job = fake_job(&store, &runner.id, &session_id);
        store.create_replay_job(&job).unwrap();

        // Three events in a row.
        for (i, ev_type) in [
            ReplayJobEventType::Started,
            ReplayJobEventType::Progress,
            ReplayJobEventType::Completed,
        ]
        .iter()
        .enumerate()
        {
            std::thread::sleep(std::time::Duration::from_millis(2));
            store
                .append_replay_job_event(&ReplayJobEvent {
                    id: Uuid::new_v4().to_string(),
                    job_id: job.id.clone(),
                    event_type: ev_type.clone(),
                    step_number: Some(i as u32),
                    payload: Some(format!(r#"{{"i":{i}}}"#)),
                    created_at: Utc::now(),
                })
                .unwrap();
        }

        let events = store.list_replay_job_events(&job.id).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, ReplayJobEventType::Started);
        assert_eq!(events[1].event_type, ReplayJobEventType::Progress);
        assert_eq!(events[2].event_type, ReplayJobEventType::Completed);
    }

    #[test]
    fn job_state_terminal_check() {
        // Pin the terminal-state classification so a future state-
        // machine refactor doesn't accidentally let the reaper
        // overwrite a completed job.
        assert!(!ReplayJobState::Pending.is_terminal());
        assert!(!ReplayJobState::Dispatched.is_terminal());
        assert!(!ReplayJobState::InProgress.is_terminal());
        assert!(ReplayJobState::Completed.is_terminal());
        assert!(ReplayJobState::Errored.is_terminal());
    }

    // ── Review #152 regression coverage ────────────────────────

    /// Comment 4: auth_token_hash must be UNIQUE — duplicate insertion
    /// must fail at the schema boundary. Pre-fix this was a non-unique
    /// index and two runners could collide.
    #[test]
    fn duplicate_auth_token_hash_is_rejected_by_schema() {
        let (store, _dir) = test_store();

        let r1 = fake_runner("first");
        // Override r2's hash to match r1's, simulating a collision.
        // (In production a hash collision is astronomically unlikely
        // but operator import bugs / fixture mistakes can produce one.)
        let mut r2 = fake_runner("second");
        r2.auth_token_hash = r1.auth_token_hash.clone();

        store.create_runner(&r1).unwrap();
        let result = store.create_runner(&r2);

        assert!(
            result.is_err(),
            "duplicate auth_token_hash should fail at the unique-index boundary"
        );
        // Sanity: the original runner is still findable by hash.
        let found = store.get_runner_by_auth_hash(&r1.auth_token_hash).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, r1.id);
    }

    /// Comment 3: deleting a runner must SET NULL on dependent
    /// replay_jobs.runner_id rather than block deletion or cascade
    /// delete the historical job rows.
    #[test]
    fn delete_runner_nulls_runner_id_on_replay_jobs() {
        let (store, _dir) = test_store();
        let runner = fake_runner("doomed");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);

        // Create a job referencing the runner.
        let job = fake_job(&store, &runner.id, &session_id);
        store.create_replay_job(&job).unwrap();
        assert_eq!(
            store.get_replay_job(&job.id).unwrap().unwrap().runner_id.as_deref(),
            Some(runner.id.as_str())
        );

        // Delete the runner. The job should survive but with runner_id NULL.
        assert!(store.delete_runner(&runner.id).unwrap());
        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert!(
            after.runner_id.is_none(),
            "ON DELETE SET NULL should null out runner_id, not delete the job"
        );
        // Job state preserved (historical context).
        assert_eq!(after.state, ReplayJobState::Pending);

        // The job appears in the orphaned-jobs listing.
        let orphans = store.list_orphaned_replay_jobs(10).unwrap();
        assert!(orphans.iter().any(|j| j.id == job.id));
    }

    /// Comment 3: same cascade behavior for replay_context_id.
    /// Deleting a replay_context (e.g. by TTL cleanup) nulls out
    /// dependent replay_jobs.replay_context_id but leaves the job
    /// row intact.
    #[test]
    fn delete_replay_context_nulls_context_id_on_replay_jobs() {
        let (store, _dir) = test_store();
        let runner = fake_runner("doomed-ctx");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);
        let ctx_id = fake_replay_context_for_job(&store, &session_id);

        // Create a job referencing both runner and context.
        let mut job = fake_job(&store, &runner.id, &session_id);
        job.replay_context_id = Some(ctx_id.clone());
        store.create_replay_job(&job).unwrap();
        assert_eq!(
            store
                .get_replay_job(&job.id)
                .unwrap()
                .unwrap()
                .replay_context_id
                .as_deref(),
            Some(ctx_id.as_str())
        );

        // Delete the replay_context — TTL cleanup or explicit user action.
        store
            .conn
            .execute(
                "DELETE FROM replay_contexts WHERE id = ?1",
                params![ctx_id],
            )
            .unwrap();

        // Job survives, with replay_context_id nulled out.
        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert!(
            after.replay_context_id.is_none(),
            "ON DELETE SET NULL on replay_context_id should null it out"
        );
        assert_eq!(after.runner_id.as_deref(), Some(runner.id.as_str()));
    }

    /// Comment 2: terminal-state protection must be atomic. Two
    /// concurrent transitions to terminal states (e.g. reaper sets
    /// errored while a late `completed` event arrives) must both
    /// not "succeed" — only one wins, the second sees the row in a
    /// terminal state and reports false.
    ///
    /// SQLite serializes concurrent UPDATEs at the connection level
    /// rather than truly running them in parallel; the simulation
    /// below reads-then-writes from two different angles and confirms
    /// the SQL-level guard rejects the second write.
    #[test]
    fn advance_state_atomic_guard_rejects_post_terminal_writes() {
        let (store, _dir) = test_store();
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);
        let job = fake_job(&store, &runner.id, &session_id);
        store.create_replay_job(&job).unwrap();

        // Reaper transitions to errored first.
        let reaper_won = store
            .advance_replay_job_state(
                &job.id,
                ReplayJobState::Errored,
                Some("lease expired"),
                Some("lease_expired"),
            )
            .unwrap();
        assert!(reaper_won, "reaper's transition should succeed");

        // Late event tries to transition to completed. SQL-level
        // guard rejects it; rows-affected is 0; we return false.
        let late_event_won = store
            .advance_replay_job_state(&job.id, ReplayJobState::Completed, None, None)
            .unwrap();
        assert!(
            !late_event_won,
            "late completed event after terminal errored should NOT succeed (SQL guard)"
        );

        // Confirm the job is still in errored state with reaper's
        // error message — the late completed didn't bleed through.
        let final_job = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(final_job.state, ReplayJobState::Errored);
        assert_eq!(final_job.error_message.as_deref(), Some("lease expired"));
        assert_eq!(final_job.error_stage.as_deref(), Some("lease_expired"));
    }

    /// Comment 1: the new atomic `record_replay_job_event_atomic`
    /// rejects late events arriving after the job is already terminal.
    /// Same threat model as comment 2 but via the event-ingestion
    /// path that commit 6 will use.
    #[test]
    fn atomic_event_record_rejects_late_event_after_terminal() {
        let (mut store, _dir) = test_store();
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);
        let job = fake_job(&store, &runner.id, &session_id);
        store.create_replay_job(&job).unwrap();

        // Move to terminal state via the legitimate path.
        store
            .advance_replay_job_state(
                &job.id,
                ReplayJobState::Completed,
                None,
                None,
            )
            .unwrap();

        // Now try to record a late `progress` event. Atomic guard
        // sees the terminal state, rolls back, returns false.
        let late_event = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Progress,
            step_number: Some(99),
            payload: Some(r#"{"step":"99"}"#.to_string()),
            created_at: Utc::now(),
        };
        let accepted = store
            .record_replay_job_event_atomic(&late_event, None, None, None, 300)
            .unwrap();
        assert!(
            !accepted,
            "late progress event after terminal must be rejected"
        );

        // The event row was NOT inserted (transaction rolled back).
        let events = store.list_replay_job_events(&job.id).unwrap();
        assert!(
            !events.iter().any(|e| e.id == late_event.id),
            "rolled-back transaction left the event row in the DB"
        );

        // Job state untouched.
        let final_job = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(final_job.state, ReplayJobState::Completed);
    }

    /// Comment 1: atomic `started` event transitions state +
    /// extends lease + inserts event row in one transaction.
    #[test]
    fn atomic_started_event_transitions_state_and_extends_lease() {
        let (mut store, _dir) = test_store();
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);
        let mut job = fake_job(&store, &runner.id, &session_id);
        job.state = ReplayJobState::Dispatched;
        store.create_replay_job(&job).unwrap();

        let started_event = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Started,
            step_number: None,
            payload: None,
            created_at: Utc::now(),
        };
        let accepted = store
            .record_replay_job_event_atomic(&started_event, None, None, None, 300)
            .unwrap();
        assert!(accepted);

        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(after.state, ReplayJobState::InProgress);
        assert!(after.started_at.is_some());
        assert!(after.lease_expires_at.is_some());

        // Lease should be ~5min in the future (within tolerance).
        let lease = after.lease_expires_at.unwrap();
        let expected_min = Utc::now() + chrono::Duration::seconds(290);
        let expected_max = Utc::now() + chrono::Duration::seconds(310);
        assert!(
            lease >= expected_min && lease <= expected_max,
            "lease should be ~5min in the future, got {lease}"
        );

        let events = store.list_replay_job_events(&job.id).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, ReplayJobEventType::Started);
    }

    /// Comment 1: atomic `progress` event updates progress_step +
    /// extends lease without changing state.
    #[test]
    fn atomic_progress_event_updates_progress_and_lease_no_state_change() {
        let (mut store, _dir) = test_store();
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);
        let mut job = fake_job(&store, &runner.id, &session_id);
        job.state = ReplayJobState::InProgress;
        store.create_replay_job(&job).unwrap();

        let progress_event = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Progress,
            step_number: Some(7),
            payload: Some(r#"{"step":"7"}"#.to_string()),
            created_at: Utc::now(),
        };
        store
            .record_replay_job_event_atomic(&progress_event, Some(20), None, None, 300)
            .unwrap();

        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(after.state, ReplayJobState::InProgress, "progress events don't change state");
        assert_eq!(after.progress_step, 7);
        assert_eq!(after.progress_total, Some(20));
    }

    // ── Review #152 round 2: comment 5 (FK requirement) ──────

    /// Active jobs (state ∈ {pending, dispatched, in_progress})
    /// MUST have `runner_id` populated. Without a runner there's
    /// no webhook target and the job would rot in the queue.
    #[test]
    fn create_active_job_without_runner_id_is_rejected() {
        let (store, _dir) = test_store();
        let session_id = fake_session_for_job(&store);
        let ctx_id = fake_replay_context_for_job(&store, &session_id);
        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: None, // ← deliberately missing
            session_id: session_id.clone(),
            replay_context_id: Some(ctx_id),
            state: ReplayJobState::Pending,
            error_message: None,
            error_stage: None,
            created_at: Utc::now(),
            dispatched_at: None,
            started_at: None,
            completed_at: None,
            dispatch_deadline_at: None,
            lease_expires_at: None,
            progress_step: 0,
            progress_total: None,
        };
        let err = store.create_replay_job(&job).unwrap_err();
        assert!(
            err.to_string().contains("runner_id is required"),
            "expected runner_id required error, got: {err}"
        );
    }

    /// Same rule for replay_context_id.
    #[test]
    fn create_active_job_without_replay_context_id_is_rejected() {
        let (store, _dir) = test_store();
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);
        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: Some(runner.id),
            session_id: session_id.clone(),
            replay_context_id: None, // ← deliberately missing
            state: ReplayJobState::Pending,
            error_message: None,
            error_stage: None,
            created_at: Utc::now(),
            dispatched_at: None,
            started_at: None,
            completed_at: None,
            dispatch_deadline_at: None,
            lease_expires_at: None,
            progress_step: 0,
            progress_total: None,
        };
        let err = store.create_replay_job(&job).unwrap_err();
        assert!(
            err.to_string().contains("replay_context_id is required"),
            "expected replay_context_id required error, got: {err}"
        );
    }

    /// Terminal-state jobs CAN have null FKs — represents the
    /// historical / imported / orphan-by-cascade case where the
    /// original runner or context no longer exists.
    #[test]
    fn create_terminal_job_with_null_fks_is_allowed_for_import_path() {
        let (store, _dir) = test_store();
        let session_id = fake_session_for_job(&store);

        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: None,
            session_id: session_id.clone(),
            replay_context_id: None,
            state: ReplayJobState::Completed,
            error_message: None,
            error_stage: None,
            created_at: Utc::now(),
            dispatched_at: None,
            started_at: None,
            completed_at: Some(Utc::now()),
            dispatch_deadline_at: None,
            lease_expires_at: None,
            progress_step: 0,
            progress_total: None,
        };
        store.create_replay_job(&job).unwrap();

        let fetched = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(fetched.state, ReplayJobState::Completed);
        assert!(fetched.runner_id.is_none());
        assert!(fetched.replay_context_id.is_none());
    }

    /// Same for errored jobs — agent might have errored after
    /// the runner / context was deleted; we should still be able
    /// to record the historical row.
    #[test]
    fn create_errored_job_with_null_fks_is_allowed() {
        let (store, _dir) = test_store();
        let session_id = fake_session_for_job(&store);

        let job = ReplayJob {
            id: Uuid::new_v4().to_string(),
            runner_id: None,
            session_id: session_id.clone(),
            replay_context_id: None,
            state: ReplayJobState::Errored,
            error_message: Some("agent crashed".into()),
            error_stage: Some("agent".into()),
            created_at: Utc::now(),
            dispatched_at: None,
            started_at: None,
            completed_at: Some(Utc::now()),
            dispatch_deadline_at: None,
            lease_expires_at: None,
            progress_step: 0,
            progress_total: None,
        };
        store.create_replay_job(&job).unwrap();
    }

    // ── Review #152 round 2: comment 6 (state machine) ───────

    /// `Progress` event on a `Pending` job is illegal — the runner
    /// hasn't started yet. Must be rejected; no event row inserted.
    #[test]
    fn pending_state_rejects_progress_event() {
        let (mut store, _dir) = test_store();
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);
        let job = fake_job(&store, &runner.id, &session_id);
        store.create_replay_job(&job).unwrap();
        // Job is Pending — never been dispatched.

        let event = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Progress,
            step_number: Some(1),
            payload: Some("{}".into()),
            created_at: Utc::now(),
        };
        let accepted = store
            .record_replay_job_event_atomic(&event, None, None, None, 300)
            .unwrap();
        assert!(!accepted, "Progress on Pending must be rejected");
        let events = store.list_replay_job_events(&job.id).unwrap();
        assert!(events.is_empty(), "rejected event must not insert a row");

        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(after.state, ReplayJobState::Pending);
        assert_eq!(after.progress_step, 0);
    }

    /// `Completed` jumping straight from `Pending` skips InProgress.
    /// Must be rejected — completion must imply the job actually ran.
    #[test]
    fn pending_state_rejects_completed_event() {
        let (mut store, _dir) = test_store();
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);
        let job = fake_job(&store, &runner.id, &session_id);
        store.create_replay_job(&job).unwrap();

        let event = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Completed,
            step_number: None,
            payload: None,
            created_at: Utc::now(),
        };
        let accepted = store
            .record_replay_job_event_atomic(&event, None, None, None, 300)
            .unwrap();
        assert!(!accepted, "Completed on Pending must be rejected");
        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(after.state, ReplayJobState::Pending);
    }

    /// `Progress` on `Dispatched` is illegal — runner must emit
    /// `Started` first.
    #[test]
    fn dispatched_state_rejects_progress_event() {
        let (mut store, _dir) = test_store();
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);
        let mut job = fake_job(&store, &runner.id, &session_id);
        job.state = ReplayJobState::Dispatched;
        store.create_replay_job(&job).unwrap();

        let event = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Progress,
            step_number: Some(1),
            payload: None,
            created_at: Utc::now(),
        };
        let accepted = store
            .record_replay_job_event_atomic(&event, None, None, None, 300)
            .unwrap();
        assert!(!accepted, "Progress on Dispatched must be rejected");
        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(after.state, ReplayJobState::Dispatched);
    }

    /// Duplicate `Started` events on an already-`InProgress` job
    /// must be rejected — otherwise `started_at` would silently
    /// reset on every retry, masking duplicate dispatches.
    #[test]
    fn in_progress_state_rejects_duplicate_started() {
        let (mut store, _dir) = test_store();
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);
        let mut job = fake_job(&store, &runner.id, &session_id);
        job.state = ReplayJobState::InProgress;
        job.started_at = Some(Utc::now() - chrono::Duration::seconds(60));
        store.create_replay_job(&job).unwrap();
        let original_started_at = job.started_at;

        let event = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Started,
            step_number: None,
            payload: None,
            created_at: Utc::now(),
        };
        let accepted = store
            .record_replay_job_event_atomic(&event, None, None, None, 300)
            .unwrap();
        assert!(!accepted, "Duplicate Started on InProgress must be rejected");

        // started_at must NOT have been reset.
        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(after.started_at, original_started_at);
    }

    /// `Errored` is allowed from `Dispatched` — covers the agent-
    /// failed-at-startup case where the webhook was delivered but
    /// the runner crashed before emitting `Started`.
    #[test]
    fn dispatched_state_accepts_errored_for_startup_failure() {
        let (mut store, _dir) = test_store();
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);
        let mut job = fake_job(&store, &runner.id, &session_id);
        job.state = ReplayJobState::Dispatched;
        store.create_replay_job(&job).unwrap();

        let event = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Errored,
            step_number: None,
            payload: None,
            created_at: Utc::now(),
        };
        let accepted = store
            .record_replay_job_event_atomic(
                &event,
                None,
                Some("agent crashed at startup"),
                Some("agent"),
                300,
            )
            .unwrap();
        assert!(accepted, "Errored from Dispatched (startup failure) must be allowed");

        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(after.state, ReplayJobState::Errored);
        assert_eq!(
            after.error_message.as_deref(),
            Some("agent crashed at startup")
        );
    }

    /// Full happy-path lifecycle: `Pending → Dispatched → Started →
    /// Progress → Progress → Completed`. Each transition is the only
    /// legal event from its respective state.
    #[test]
    fn legal_full_lifecycle_accepts_all_events_in_order() {
        let (mut store, _dir) = test_store();
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);
        let job = fake_job(&store, &runner.id, &session_id);
        store.create_replay_job(&job).unwrap();
        // pending → dispatched (via dispatcher path, not events).
        store
            .advance_replay_job_state(&job.id, ReplayJobState::Dispatched, None, None)
            .unwrap();

        // Started → InProgress
        let started = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Started,
            step_number: None,
            payload: None,
            created_at: Utc::now(),
        };
        assert!(store
            .record_replay_job_event_atomic(&started, None, None, None, 300)
            .unwrap());

        // Progress → still InProgress (step 1)
        let prog1 = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Progress,
            step_number: Some(1),
            payload: Some(r#"{"step":1}"#.into()),
            created_at: Utc::now(),
        };
        assert!(store
            .record_replay_job_event_atomic(&prog1, Some(10), None, None, 300)
            .unwrap());

        // Progress → still InProgress (step 5)
        let prog5 = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Progress,
            step_number: Some(5),
            payload: None,
            created_at: Utc::now(),
        };
        assert!(store
            .record_replay_job_event_atomic(&prog5, Some(10), None, None, 300)
            .unwrap());

        // Completed → Completed
        let completed = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Completed,
            step_number: None,
            payload: None,
            created_at: Utc::now(),
        };
        assert!(store
            .record_replay_job_event_atomic(&completed, None, None, None, 300)
            .unwrap());

        let final_job = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(final_job.state, ReplayJobState::Completed);
        assert_eq!(final_job.progress_step, 5);
        assert_eq!(final_job.progress_total, Some(10));
        assert_eq!(
            store.list_replay_job_events(&job.id).unwrap().len(),
            4,
            "all 4 events should be persisted"
        );
    }

    /// Comment 1 sanity: atomic `errored` event transitions state +
    /// records error_message / error_stage in one transaction.
    #[test]
    fn atomic_errored_event_records_error_fields() {
        let (mut store, _dir) = test_store();
        let runner = fake_runner("r1");
        store.create_runner(&runner).unwrap();
        let session_id = fake_session_for_job(&store);
        let mut job = fake_job(&store, &runner.id, &session_id);
        job.state = ReplayJobState::InProgress;
        store.create_replay_job(&job).unwrap();

        let errored = ReplayJobEvent {
            id: Uuid::new_v4().to_string(),
            job_id: job.id.clone(),
            event_type: ReplayJobEventType::Errored,
            step_number: None,
            payload: Some(r#"{"error":"agent died"}"#.to_string()),
            created_at: Utc::now(),
        };
        store
            .record_replay_job_event_atomic(
                &errored,
                None,
                Some("agent died at step 5"),
                Some("agent"),
                300,
            )
            .unwrap();

        let after = store.get_replay_job(&job.id).unwrap().unwrap();
        assert_eq!(after.state, ReplayJobState::Errored);
        assert_eq!(after.error_message.as_deref(), Some("agent died at step 5"));
        assert_eq!(after.error_stage.as_deref(), Some("agent"));
        assert!(after.completed_at.is_some());
    }
}
