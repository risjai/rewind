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
use rewind_store::{ReplayJob, ReplayJobState, Runner, Store};
use sha2::Sha256;

use crate::crypto::CryptoBox;

type HmacSha256 = Hmac<Sha256>;

/// Dispatch wire-format payload sent to the runner.
#[derive(Debug, serde::Serialize)]
struct DispatchBody<'a> {
    pub job_id: &'a str,
    pub session_id: &'a str,
    pub replay_context_id: &'a str,
    pub base_url: &'a str,
}

/// Default tunables.
pub const DISPATCH_TIMEOUT_SECS: u64 = 5;
pub const DISPATCH_DEADLINE_SECS: i64 = 10;
pub const INITIAL_LEASE_SECS: i64 = 300; // 5 minutes; extended on heartbeat

/// Build the canonical signing input for the wire-format signature.
fn signing_input(job_id: &str, body: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(job_id.len() + 1 + body.len());
    buf.extend_from_slice(job_id.as_bytes());
    buf.push(b'\n');
    buf.extend_from_slice(body);
    buf
}

/// Compute `HMAC-SHA256(key, signing_input(job_id, body))` as a
/// lowercase hex string. Pure function; tested directly.
pub fn compute_signature(key: &[u8], job_id: &str, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(key)
        .expect("HMAC-SHA256 accepts any key length");
    mac.update(&signing_input(job_id, body));
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
    pub async fn dispatch(&self, runner: &Runner, job: &ReplayJob) -> DispatchOutcome {
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
            base_url: &self.base_url,
        };
        let body_bytes = match serde_json::to_vec(&body) {
            Ok(b) => b,
            Err(e) => {
                return DispatchOutcome::Errored(format!("body serialization: {e}"));
            }
        };
        let signature = compute_signature(raw_token.expose().as_bytes(), &job.id, &body_bytes);

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

    /// Apply a [`DispatchOutcome`] to a job: writes the state
    /// transition + deadline/lease (success) OR errored row (failure).
    /// Synchronous — caller holds the Store lock while invoking.
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
                let advanced = store.advance_replay_job_state(
                    job_id,
                    ReplayJobState::Dispatched,
                    None,
                    None,
                )?;
                if advanced {
                    store.set_dispatch_deadline_and_lease(
                        job_id,
                        *dispatch_deadline_at,
                        *lease_expires_at,
                    )?;
                    let _ = store.touch_runner_last_seen(runner_id);
                }
                Ok(())
            }
            DispatchOutcome::Errored(msg) => {
                store.advance_replay_job_state(
                    job_id,
                    ReplayJobState::Errored,
                    Some(msg),
                    Some("dispatch"),
                )?;
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
    fn signing_input_format_is_job_id_newline_body() {
        let out = signing_input("abc", b"hello");
        assert_eq!(out, b"abc\nhello");
    }

    #[test]
    fn signing_input_handles_empty_body() {
        let out = signing_input("abc", b"");
        assert_eq!(out, b"abc\n");
    }

    #[test]
    fn signing_input_includes_full_job_id_and_body() {
        let out = signing_input(
            "550e8400-e29b-41d4-a716-446655440000",
            b"{\"k\":\"v\"}",
        );
        assert_eq!(
            out,
            b"550e8400-e29b-41d4-a716-446655440000\n{\"k\":\"v\"}".to_vec()
        );
    }

    #[test]
    fn compute_signature_is_stable() {
        let a = compute_signature(b"key", "job-1", b"body");
        let b = compute_signature(b"key", "job-1", b"body");
        assert_eq!(a, b);
    }

    #[test]
    fn compute_signature_changes_when_key_changes() {
        let a = compute_signature(b"key1", "job-1", b"body");
        let b = compute_signature(b"key2", "job-1", b"body");
        assert_ne!(a, b);
    }

    #[test]
    fn compute_signature_changes_when_job_id_changes() {
        let a = compute_signature(b"key", "job-1", b"body");
        let b = compute_signature(b"key", "job-2", b"body");
        assert_ne!(a, b);
    }

    #[test]
    fn compute_signature_changes_when_body_changes() {
        let a = compute_signature(b"key", "job-1", b"body-A");
        let b = compute_signature(b"key", "job-1", b"body-B");
        assert_ne!(a, b);
    }

    #[test]
    fn compute_signature_is_64_hex_chars() {
        let s = compute_signature(b"key", "job", b"body");
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// Cross-check against a known test vector. Generated with:
    ///   `python3 -c "import hmac,hashlib; print(hmac.new(b'secret', b'job-1\nbody', hashlib.sha256).hexdigest())"`
    /// The runner SDK in commit 9 verifies signatures using the same
    /// recipe; this test pins the wire-format so a Rust-side change
    /// doesn't silently drift away from the Python implementation.
    #[test]
    fn compute_signature_matches_python_reference() {
        let actual = compute_signature(b"secret", "job-1", b"body");
        let expected = "52fd281254f1f940a5f2ad83ebce5bfbf92f77187afa03db6902bd328abd31f9";
        assert_eq!(actual, expected, "drift from Python HMAC reference");
    }
}
