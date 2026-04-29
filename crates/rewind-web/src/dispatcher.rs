//! Outbound webhook dispatcher (Phase 3, commit 5/13).
//!
//! Posts replay-job dispatches from the Rewind server to runner
//! webhook URLs, HMAC-signed under the runner's auth token. The
//! runner verifies the signature, replies 202 Accepted within 5
//! seconds, and asynchronously executes the agent run. Progress
//! flows back via [`runners::events`](crate::runners) (commit 6).
//!
//! ## Wire format
//!
//! Outbound request:
//!
//! ```text
//! POST <runner.webhook_url>
//! Content-Type: application/json
//! X-Rewind-Job-Id: <uuid>
//! X-Rewind-Signature: sha256=<hex>
//! User-Agent: rewind-dispatcher/<version>
//!
//! {
//!   "job_id": "<uuid>",
//!   "session_id": "<uuid>",
//!   "replay_context_id": "<uuid>",
//!   "replay_context_timeline_id": "<uuid>",
//!   "at_step": <u32>,
//!   "base_url": "<rewind-server-base>"
//! }
//! ```
//!
//! Signature: `HMAC-SHA256(raw_token, X-Rewind-Job-Id || "\n" || raw_body)`,
//! hex-encoded. Standard pattern (Stripe / GitHub webhooks use
//! the same shape) — gives runners a deterministic verification
//! recipe and protects against replay/forgery if the runner exposes
//! a public webhook URL.
//!
//! ## Token decryption
//!
//! The dispatcher reads `(encrypted_token, nonce)` from the runners
//! row, decrypts via the [`CryptoBox`](crate::crypto::CryptoBox),
//! computes the HMAC, and discards the plaintext. No in-memory
//! cache, no process-local state, multi-replica safe. Decrypted
//! token lives only inside [`SensitiveString`] to keep accidental
//! `Debug` / log output redacted.
//!
//! ## State transitions
//!
//! On 2xx response from the runner: `pending → dispatched`, sets
//! `dispatch_deadline_at = now + 10s`, `lease_expires_at = now + 5min`.
//! On any failure (non-2xx, timeout, network, signing error):
//! `pending → errored` with `error_stage = "dispatch"`. Both
//! transitions go through [`Store::advance_replay_job_state`] so
//! they're idempotent + race-safe (commit 3's atomic guard).
//!
//! ## SSRF
//!
//! The webhook URL was already validated against the SSRF policy
//! at registration time (commit 4 + review #153). The dispatcher
//! does NOT re-validate — that would create DNS-rebinding races —
//! relying on the registration-time check + reqwest's resolver.

use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use hmac::{Hmac, Mac};
use rewind_store::{ReplayJob, Runner, Store};
use sha2::Sha256;

use crate::crypto::CryptoBox;

type HmacSha256 = Hmac<Sha256>;

/// Dispatch wire-format payload sent to the runner.
///
/// **Review #154 F2:** `replay_context_timeline_id` is the timeline
/// the replay context targets — the SDK's `attach_replay_context`
/// uses it to set `_timeline_id` so any cache-miss live recordings
/// land in the fork rather than the original timeline.
///
/// **`at_step`** (added 2026-04-29): the original fork-point of the
/// replay-context's timeline (i.e. the step number the user clicked
/// "Run replay" at in the dashboard). Distinct from the replay
/// context's `from_step` which the `/api/sessions/{sid}/replay-jobs`
/// handler hardcodes to 0 (see runners.rs `create_replay_job` —
/// the agent re-runs the loop from scratch so the replay-lookup
/// ordinal cursor must start at recorded step #1). Runners use
/// `at_step` to know which conversation turn the user wanted to
/// start from — needed for multi-turn replay where edits to
/// step #N's user message in turn 2+ should drive the agent at
/// iteration N (companion ray-agent change).
#[derive(Debug, serde::Serialize)]
struct DispatchBody<'a> {
    pub job_id: &'a str,
    pub session_id: &'a str,
    pub replay_context_id: &'a str,
    pub replay_context_timeline_id: &'a str,
    pub at_step: u32,
    pub base_url: &'a str,
}

/// Default tunables.
pub const DISPATCH_TIMEOUT_SECS: u64 = 5;
pub const DISPATCH_DEADLINE_SECS: i64 = 10;
pub const INITIAL_LEASE_SECS: i64 = 300; // 5 minutes; extended on heartbeat

/// Build the canonical signing input for the wire-format signature.
///
/// **Review #154 F5:** when `timestamp` is `Some(ts)`, the input is
/// `ts || \n || job_id || \n || body` (defeats long-window replays
/// of captured signatures). When `None`, the legacy two-line input
/// is used (kept so the pinned reference-vector test that documents
/// the pre-#154 wire format still passes; production dispatch
/// always uses Some).
fn signing_input(timestamp: Option<i64>, job_id: &str, body: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(20 + job_id.len() + 2 + body.len());
    if let Some(ts) = timestamp {
        buf.extend_from_slice(ts.to_string().as_bytes());
        buf.push(b'\n');
    }
    buf.extend_from_slice(job_id.as_bytes());
    buf.push(b'\n');
    buf.extend_from_slice(body);
    buf
}

/// Compute `HMAC-SHA256(key, signing_input(timestamp, job_id, body))`
/// as a lowercase hex string. Pure function; tested directly.
pub fn compute_signature(
    key: &[u8],
    timestamp: Option<i64>,
    job_id: &str,
    body: &[u8],
) -> String {
    let mut mac = HmacSha256::new_from_slice(key)
        .expect("HMAC-SHA256 accepts any key length");
    mac.update(&signing_input(timestamp, job_id, body));
    let bytes = mac.finalize().into_bytes();
    let mut hex = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        write!(&mut hex, "{:02x}", b).unwrap();
    }
    hex
}

/// Outbound webhook dispatcher. Cloning is cheap (reqwest::Client
/// shares its connection pool internally).
#[derive(Clone)]
pub struct Dispatcher {
    client: reqwest::Client,
    crypto: CryptoBox,
    base_url: String,
}

impl Dispatcher {
    pub fn new(crypto: CryptoBox, base_url: String) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(DISPATCH_TIMEOUT_SECS))
            .user_agent(format!("rewind-dispatcher/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .context("build reqwest client for dispatcher")?;
        Ok(Self {
            client,
            crypto,
            base_url,
        })
    }

    /// Outcome of a dispatch attempt. The dispatcher is now pure
    /// (no store handle) so the async HTTP work doesn't have to hold
    /// a `MutexGuard<Store>` across an `.await`. Caller applies the
    /// outcome via [`Self::apply_outcome`].
    ///
    /// **Review #154 F2:** caller passes `replay_context_timeline_id`
    /// so the dispatch payload can carry it and the SDK's
    /// `attach_replay_context` can set `_timeline_id` correctly.
    ///
    /// `at_step` (added 2026-04-29): the fork-point of the timeline
    /// (= the step number the user clicked Run replay at). Forwarded
    /// in the dispatch body for runner-side multi-turn replay.
    pub async fn dispatch(
        &self,
        runner: &Runner,
        job: &ReplayJob,
        replay_context_timeline_id: &str,
        at_step: u32,
    ) -> DispatchOutcome {
        let webhook_url = match runner.webhook_url.as_deref() {
            Some(u) => u,
            None => {
                return DispatchOutcome::Errored(format!(
                    "runner {} has no webhook_url (polling not implemented)",
                    runner.id
                ));
            }
        };
        let replay_context_id = match job.replay_context_id.as_deref() {
            Some(c) => c,
            None => {
                return DispatchOutcome::Errored(format!(
                    "job {} has null replay_context_id (cannot dispatch)",
                    job.id
                ));
            }
        };

        // 1. Decrypt the runner's auth token (lives only inside
        //    SensitiveString; dropped at end of scope).
        let raw_token = match self
            .crypto
            .decrypt(&runner.encrypted_token, &runner.token_nonce)
        {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(
                    "dispatcher: token decrypt failed for runner {}: {e}",
                    runner.id
                );
                return DispatchOutcome::Errored(
                    "token decrypt failed (server misconfig)".to_string(),
                );
            }
        };

        // 2. Build body + signature.
        let body = DispatchBody {
            job_id: &job.id,
            session_id: &job.session_id,
            replay_context_id,
            replay_context_timeline_id,
            at_step,
            base_url: &self.base_url,
        };
        let body_bytes = match serde_json::to_vec(&body) {
            Ok(b) => b,
            Err(e) => {
                return DispatchOutcome::Errored(format!("body serialization: {e}"));
            }
        };
        // F5: include a timestamp in the signed input + send it in
        // a header so the runner can enforce a tolerance window.
        let timestamp = Utc::now().timestamp();
        let signature = compute_signature(
            raw_token.expose().as_bytes(),
            Some(timestamp),
            &job.id,
            &body_bytes,
        );

        // F6: revalidate webhook_url against the SSRF policy at
        // dispatch time, not just at registration. Closes the
        // window where a host's DNS records change between
        // registration and dispatch. Note: there's still a
        // residual race between this check and the reqwest
        // connect; a custom connector would be required to fully
        // close it (deferred — pre-existing url_guard limitation
        // documented in docs/runners.md).
        let bypass_ssrf = std::env::var("REWIND_ALLOW_LOOPBACK_WEBHOOKS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        if !bypass_ssrf
            && let Err(msg) = crate::url_guard::validate_export_endpoint(webhook_url).await
        {
            tracing::warn!(
                "dispatcher: webhook_url for runner {} no longer passes SSRF check: {msg}",
                runner.id
            );
            return DispatchOutcome::Errored(format!(
                "webhook_url failed dispatch-time SSRF check: {msg}"
            ));
        }

        // 3. POST. Single attempt with timeout — runners must reply
        //    within 5s. The reaper handles dispatch_deadline_at if
        //    the runner accepts the request but never emits Started.
        tracing::info!(
            "dispatching job {} to runner {} ({})",
            job.id, runner.id, webhook_url
        );
        let resp = match self
            .client
            .post(webhook_url)
            .header("X-Rewind-Job-Id", &job.id)
            .header("X-Rewind-Signature", format!("sha256={signature}"))
            .header("X-Rewind-Timestamp", timestamp.to_string())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body_bytes)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("dispatcher: HTTP error dispatching job {}: {e}", job.id);
                return DispatchOutcome::Errored(format!("dispatch HTTP error: {e}"));
            }
        };

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            tracing::warn!(
                "dispatcher: runner {} returned {} for job {}: {}",
                runner.id,
                status,
                job.id,
                text.chars().take(200).collect::<String>()
            );
            return DispatchOutcome::Errored(format!("runner returned HTTP {status}"));
        }

        // 4. Success: caller handles state transition + deadline/lease.
        let now = Utc::now();
        DispatchOutcome::Dispatched {
            dispatch_deadline_at: now + chrono::Duration::seconds(DISPATCH_DEADLINE_SECS),
            lease_expires_at: now + chrono::Duration::seconds(INITIAL_LEASE_SECS),
            runner_id: runner.id.clone(),
        }
    }

    /// Apply a [`DispatchOutcome`] to a job: sets deadline/lease on
    /// success or transitions to `errored` on failure.
    ///
    /// **Review #154 round 2 fix:** the caller now transitions
    /// `pending → dispatched` SYNCHRONOUSLY before the dispatcher's
    /// async work (see `runners::create_replay_job`). That closes
    /// the race where the runner could call back with `started`
    /// before the dispatcher's apply_outcome flipped state, which
    /// the post-#152 strict state machine would reject as an illegal
    /// `Pending → Started` transition. As a result, this function
    /// no longer needs to handle the `Pending → Dispatched`
    /// transition itself — it's already done. The success path here
    /// only sets the deadline + lease on the (already-dispatched)
    /// row; the failure path transitions `Dispatched → Errored`
    /// which the SQL state guard accepts (Errored from Dispatched is
    /// the documented startup-failure path from #152 round 2).
    pub fn apply_outcome(
        outcome: &DispatchOutcome,
        job_id: &str,
        store: &Store,
    ) -> Result<()> {
        match outcome {
            DispatchOutcome::Dispatched {
                dispatch_deadline_at,
                lease_expires_at,
                runner_id,
            } => {
                // State already advanced by the caller pre-spawn.
                // We just set the deadline/lease and touch the
                // runner liveness column.
                store.set_dispatch_deadline_and_lease(
                    job_id,
                    *dispatch_deadline_at,
                    *lease_expires_at,
                )?;
                let _ = store.touch_runner_last_seen(runner_id);
                Ok(())
            }
            DispatchOutcome::Errored(msg) => {
                // Dispatched → Errored (legal per the state machine).
                // Use the strict `mark_dispatched_job_as_errored`
                // helper rather than `advance_replay_job_state`
                // because the latter matches `in_progress` rows
                // too — we'd corrupt state if a fast runner emitted
                // `started` between the pre-spawn transition and
                // this branch (the run is genuinely in progress
                // even though we observed an HTTP error). The
                // strict helper's `WHERE state = 'dispatched'`
                // makes the failure-path UPDATE a no-op when the
                // runner has already started.
                store.mark_dispatched_job_as_errored(job_id, msg, "dispatch")?;
                Ok(())
            }
        }
    }
}

/// What `dispatch()` decided. Caller applies it via
/// [`Dispatcher::apply_outcome`].
#[derive(Debug, Clone)]
pub enum DispatchOutcome {
    Dispatched {
        dispatch_deadline_at: chrono::DateTime<Utc>,
        lease_expires_at: chrono::DateTime<Utc>,
        runner_id: String,
    },
    Errored(String),
}

// ──────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_input_legacy_format_when_no_timestamp() {
        let out = signing_input(None, "abc", b"hello");
        assert_eq!(out, b"abc\nhello");
    }

    #[test]
    fn signing_input_with_timestamp_prepends_three_lines() {
        let out = signing_input(Some(1700000000), "abc", b"hello");
        assert_eq!(out, b"1700000000\nabc\nhello");
    }

    #[test]
    fn signing_input_handles_empty_body() {
        let out = signing_input(None, "abc", b"");
        assert_eq!(out, b"abc\n");
    }

    #[test]
    fn compute_signature_is_stable() {
        let a = compute_signature(b"key", Some(1700000000), "job-1", b"body");
        let b = compute_signature(b"key", Some(1700000000), "job-1", b"body");
        assert_eq!(a, b);
    }

    #[test]
    fn compute_signature_changes_when_timestamp_changes() {
        // F5: same key/job/body with different timestamps must produce
        // different signatures, so a captured signature can't be
        // replayed at a fresh timestamp without re-signing.
        let a = compute_signature(b"key", Some(1700000000), "job-1", b"body");
        let b = compute_signature(b"key", Some(1700000300), "job-1", b"body");
        assert_ne!(a, b);
    }

    #[test]
    fn compute_signature_changes_when_key_changes() {
        let a = compute_signature(b"key1", Some(1700000000), "job-1", b"body");
        let b = compute_signature(b"key2", Some(1700000000), "job-1", b"body");
        assert_ne!(a, b);
    }

    #[test]
    fn compute_signature_changes_when_job_id_changes() {
        let a = compute_signature(b"key", Some(1700000000), "job-1", b"body");
        let b = compute_signature(b"key", Some(1700000000), "job-2", b"body");
        assert_ne!(a, b);
    }

    #[test]
    fn compute_signature_changes_when_body_changes() {
        let a = compute_signature(b"key", Some(1700000000), "job-1", b"body-A");
        let b = compute_signature(b"key", Some(1700000000), "job-1", b"body-B");
        assert_ne!(a, b);
    }

    #[test]
    fn compute_signature_is_64_hex_chars() {
        let s = compute_signature(b"key", Some(1700000000), "job", b"body");
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// Cross-check against a known test vector. Generated with:
    ///   `python3 -c "import hmac,hashlib; print(hmac.new(b'secret', b'1700000000\\njob-1\\nbody', hashlib.sha256).hexdigest())"`
    ///
    /// Pinned alongside the matching Python test
    /// (`python/tests/test_runner.py::test_compute_signature_matches_rust_reference`)
    /// so any change to either side that would silently drift the
    /// wire format breaks both tests in lockstep.
    #[test]
    fn compute_signature_matches_python_reference_with_timestamp() {
        let actual = compute_signature(b"secret", Some(1700000000), "job-1", b"body");
        let expected = "ea61914f63b2516960203b8bf3f4e8ee5c9a379ca941c8bd6edef2a1681944bb";
        assert_eq!(actual, expected, "drift from Python HMAC reference");
    }

    /// Pre-#154 legacy two-line input still works for backward-compat
    /// and matches the original Python reference vector.
    #[test]
    fn compute_signature_legacy_no_timestamp_matches_python_reference() {
        let actual = compute_signature(b"secret", None, "job-1", b"body");
        let expected = "52fd281254f1f940a5f2ad83ebce5bfbf92f77187afa03db6902bd328abd31f9";
        assert_eq!(actual, expected, "drift from legacy Python HMAC reference");
    }
}
