# Phase 3 — Runner Registry + Dashboard "Run replay" Button (Tier 3)

**Status:** Planning. Branch `feat/phase-3-runner-registry` off master `6a80731` (post-Phase 2 merge).

**Predecessors:**
- [PR #149 (Phase 1 — HTTP transport adapters)](https://github.com/agentoptics/rewind/pull/149) — merged. Provides the `_flow` orchestration + `ExplicitClient.start_replay` that runners will invoke.
- [PR #151 (Phase 2 — `cached_llm_call` decorator)](https://github.com/agentoptics/rewind/pull/151) — merged. Provides the per-function caching primitive runners can use without per-call-site changes.

**One-line goal:** _Click "Run replay" in the dashboard and your registered agent runs the session — no terminal, no scripts._

## Why this PR exists

Today the replay flow is operator-driven and CLI-only:

```
1. Operator opens dashboard, sees a recorded session with a fork
2. Operator copies session_id, fork_step, etc.
3. Operator drops to a terminal, runs:
   $ python my_agent.py --rewind-replay-session <id> --from-step <n>
4. Operator watches stdout, hopes nothing breaks
5. Operator goes back to the dashboard to see the result
```

The dashboard is a viewer, not a control surface. **Tier 3 closes the loop:** registered agent processes (runners) listen for replay-job webhooks. The dashboard's "Run replay" button POSTs a job; the runner picks it up, runs the agent under `intercept.install()` against the replay context, and posts progress events back. The dashboard streams those events live.

Net effect: one-click replay from the same UI you used to inspect the session. No context-switching to a terminal.

> **REVIEW SUMMARY (ordered by severity):**
> 1. **BLOCKER:** the token model is internally inconsistent: the plan says raw runner tokens are never stored, but the server must know the raw shared secret to HMAC-sign Rewind→runner webhooks after registration/restart. The proposed in-memory cache makes registered runners unusable after process restart and breaks multi-instance deployments.
> 2. **BLOCKER:** `/api/replay-jobs/{id}/events` needs runner-specific authentication and ownership checks that bypass or compose with the existing dashboard bearer-token middleware. Otherwise external runners will get `401`, or unauthenticated clients can spoof job progress/completion.
> 3. **BLOCKER:** the dashboard job API takes a `replay_context_id`, but the UI flow never creates one. The API should probably create fork + replay context atomically from `{timeline_id, at_step, runner_id}` or explicitly reuse an existing fork context with validation.
> 4. **HIGH:** the Python runner example uses non-existent SDK parameters (`ExplicitClient(rewind_url=...)`, `start_replay(..., replay_context_id=...)`), so implementers will copy broken code.
> 5. **HIGH:** cancellation is listed as a state/event but has no runner-side cancellation protocol. Marking a job cancelled while the agent continues writing steps will corrupt user expectations.
> 6. **MEDIUM:** jobs can get stuck forever in `dispatched`/`in_progress`; the plan needs a lease/timeout/reaper policy.
> 7. **MEDIUM:** the modal fallback mentions a `rewind replay --runner` command that is not in the CLI scope.
> 8. **LOW:** version bump instructions should stay conditional on release state per `CLAUDE.md`, especially Python SDK `0.16.0`.

## Plan revision (post-review v1)

All 8 review comments verified against the codebase and accepted. Resolutions:

| # | Severity | Resolution |
| --- | :---: | --- |
| 1 | BLOCKER | Token model rewritten. Raw runner secret now stored encrypted-at-rest under a server app key (`REWIND_RUNNER_SECRET_KEY` env var). Registrations are durable across restarts and multi-replica safe. `SensitiveString` redacts the encrypted form on logs/API responses. See revised "Auth model" + "Token storage" sections below. |
| 2 | BLOCKER | New auth path on `POST /api/replay-jobs/{id}/events` — bypasses the dashboard bearer-token middleware in favor of `X-Rewind-Runner-Auth: <runner-token>`. Server hashes the supplied token, compares against `auth_token_hash`, and verifies the matched runner owns the job (`job.runner_id == runner.id`). See revised "Status events" section. |
| 3 | BLOCKER | API shape revised. `POST /api/sessions/{sid}/replay-jobs` accepts EITHER `{runner_id, source_timeline_id, at_step, [strict_match]}` (creates fork + replay context atomically) OR `{runner_id, replay_context_id}` (validates ownership, ensures cursor isn't being consumed). Modal flow defaults to (a). See revised "High-level flow" diagram + "Replay job dispatch endpoint" section. |
| 4 | HIGH | New `ExplicitClient.attach_replay_context(session_id, replay_context_id)` API added to Phase 3 scope. Sets the contextvars for an existing context without creating a new one. Python runner example rewritten with correct signatures. Env-var bootstrap (`REWIND_SESSION_ID`, `REWIND_REPLAY_CONTEXT_ID`) also added so a runner spawned as a subprocess can be configured without touching the SDK. |
| 5 | HIGH | `cancelled` removed from v1. Cooperative cancel deferred to v3.1 (proper protocol: server cancel-flag → runner polls/receives → reports `cancelled`). v1 state machine: `pending → dispatched → in_progress → completed`/`errored` (no cancellation). |
| 6 | MEDIUM | Lease + reaper added. `replay_jobs.dispatch_deadline_at` (10s — runner must reply 202) and `replay_jobs.lease_expires_at` (5min — extended on heartbeat or progress event). Background tokio task (`crates/rewind-web/src/reaper.rs`) scans for expired leases every 30s, marks them `errored` with `stage: "lease_expired"`. |
| 7 | MEDIUM | Modal fallback rewritten to use the existing `rewind replay <session> --from <step> --fork-id <fork>` CLI command instead of a hypothetical `--runner` flag. CLI scope unchanged at `rewind runners {list,add,remove}`. |
| 8 | LOW | Version bumps now conditional. Per CLAUDE.md track-2 rule: master Python SDK is 0.15.0 but PyPI is at 0.14.8 → 0.15.0 is unpublished → Phase 3 RIDES 0.15.0, **does NOT** bump to 0.16.0. Same logic for python-mcp 0.13.0 (unpublished, no MCP API change anyway → stays at 0.13.0). Rust bump 0.13.0 → 0.14.0 IS warranted (master 0.13.0 has been released as v0.13.0 GitHub tag, and Phase 3 has a schema migration). |

The original blockquote comments are preserved inline below as an audit trail. Each affected section has been rewritten to reflect the resolutions above; affected sections are marked **(REVISED)** in their headers.

## What ships in this PR

### Rust

- **`crates/rewind-store/src/sensitive.rs`** (new) — `SensitiveString` newtype wrapping `String`. Redacts in `Debug` / `Display` / `serde_json::to_value`. Used for runner auth tokens (HMAC keys), so accidental logging never leaks the raw token.
- **`crates/rewind-store/src/runners.rs`** (new) — `Runner` + `ReplayJob` data models, schema migrations, CRUD methods on `Store`.
- **`crates/rewind-web/src/runners.rs`** (new) — HTTP endpoints for runner lifecycle and replay job dispatch.
- **`crates/rewind-web/src/dispatcher.rs`** (new) — outbound webhook dispatcher (Rewind server → runner). HMAC-signed POST with retry + timeout.

### Web (dashboard)

- **`web/src/components/RunReplayButton.tsx`** (new) — button on session-detail / fork-timeline views. Clicking opens a modal: pick a registered runner (or "show me the CLI command instead"), confirm, dispatch.
- **`web/src/pages/RunnersPage.tsx`** (new) — list / register / unregister runners. Shows last-seen heartbeat, status, recent jobs.
- **`web/src/hooks/use-replay-job.ts`** (new) — submit a replay job, subscribe to progress events via the existing WebSocket channel, expose `{ status, progress, error }` to the button.

### Python

- **`python/rewind_agent/runner.py`** (new) — small library helping operators stand up a runner. Provides `RewindRunner` class with HTTP server stub, `@runner.handle_replay` decorator, auth verification, and a progress-reporting helper. ~250 LOC.

### CLI

- **`rewind runners list`** / **`rewind runners add <url>`** / **`rewind runners remove <id>`** — registry management without going through the dashboard. Ships in `crates/rewind-cli/`.

### Docs

- **`docs/runners.md`** (new) — operator-facing how-to: register a runner, set up the webhook endpoint, click the button. Quickstart example using the Python library.
- **`docs/recording.md`** + **`docs/getting-started.md`** — extend the decision matrix from 4 ways to **5 ways** (add "Dashboard-triggered runner replay"). Bundles the deferred 3→4 way update from Phase 2.

### Tests

Estimated ~80 cases across:

- Rust: `sensitive.rs` redaction (Debug, Display, serde), runner CRUD, replay-job state machine, webhook dispatcher (success / timeout / 5xx / signature verify).
- Web: button → modal → dispatch flow, WebSocket progress streaming, runners page CRUD, optimistic updates.
- Python: `runner.py` HTTP server + `@handle_replay` decorator + progress reporting.
- Integration: end-to-end smoke test — register runner, fire button, runner kicks off agent, progress events stream back.

## Architecture

### High-level flow

```
┌────────────────────────────────────────────────────────────────────┐
│ Dashboard (web)                                                    │
│                                                                    │
│   User clicks "Run replay" on session detail                       │
│      │                                                             │
│      ▼                                                             │
│   POST /api/sessions/{sid}/replay-jobs                             │
│      { runner_id, replay_context_id }                              │
└────────────────┬───────────────────────────────────────────────────┘
                 │
                 ▼
┌────────────────────────────────────────────────────────────────────┐
│ Rewind server (Rust)                                               │
│                                                                    │
│   1. Validates runner is registered + alive (heartbeat < 5min)     │
│   2. Inserts ReplayJob row (status=pending)                        │
│   3. Async: webhook_dispatcher.dispatch(runner, job)               │
│      └─ POST runner.webhook_url                                    │
│         Headers: X-Rewind-Job-Id, X-Rewind-Signature (HMAC)        │
│         Body:    { job_id, session_id, replay_context_id, base_url}│
└────────────────┬───────────────────────────────────────────────────┘
                 │
                 ▼
┌────────────────────────────────────────────────────────────────────┐
│ Runner (operator's agent process, exposing an HTTP webhook)        │
│                                                                    │
│   1. Verifies X-Rewind-Signature against shared secret             │
│   2. Replies 202 Accepted immediately                              │
│   3. Async: spawns the agent under `intercept.install()` +         │
│      `ExplicitClient.start_replay(replay_context_id)`              │
│   4. As agent progresses, runner POSTs to                          │
│      /api/replay-jobs/{job_id}/events                              │
│         { event: "started" | "progress" | "completed" | "errored", │
│           step_number?, error? }                                   │
└────────────────┬───────────────────────────────────────────────────┘
                 │
                 ▼ (event POST back to Rewind)
┌────────────────────────────────────────────────────────────────────┐
│ Rewind server                                                      │
│                                                                    │
│   1. Updates ReplayJob status                                      │
│   2. Broadcasts on existing WebSocket channel                      │
│      → dashboard sees live progress                                │
└────────────────────────────────────────────────────────────────────┘
```

**Resolution (BLOCKER #3): Replay job dispatch endpoint accepts two shapes.** The original plan assumed the dashboard already had a `replay_context_id`, but the existing fork flow creates a fork timeline and shows a CLI command — no persisted replay context. Endpoint shape revised:

```
POST /api/sessions/{sid}/replay-jobs

# Shape (a): create-and-dispatch (typical from the dashboard button)
{
    "runner_id": "<uuid>",
    "source_timeline_id": "<timeline_id>",  # the fork point's parent timeline
    "at_step": <int>,                       # fork at this step
    "strict_match": false                   # optional, default false
}
# → server atomically creates fork timeline + replay_context + replay_job
# → all three rolled back on any failure (e.g. runner suddenly disabled)

# Shape (b): reuse-existing-context (CLI / programmatic)
{
    "runner_id": "<uuid>",
    "replay_context_id": "<uuid>"
}
# → server validates: replay_context exists, belongs to session sid,
#   is not already being consumed by another in-flight job
#   (replay cursor would be a hot-spot otherwise)
# → server creates replay_job referencing existing context
```

Validation in shape (b):
- `replay_context_id` row exists AND `session_id` matches the URL param
- replay context's `current_step == from_step` (no other job has advanced the cursor)
- No other in-flight `replay_job` references the same `replay_context_id`
- Replay context is not expired (Phase 0's TTL still applies)

Returns `{job_id, replay_context_id, fork_timeline_id?, dispatch_deadline_at}`. Dashboard subscribes to WebSocket and shows live progress.

### Auth model: HMAC-signed webhooks (REVISED post-review)

**Resolution to BLOCKER #1.** The original plan had two contradictory storage stories; this revision picks one model and uses it consistently across registration, dispatch, and event ingestion.

The runner is registered with a shared secret (the auth token). Every webhook from Rewind→runner carries:

```
X-Rewind-Signature: sha256=<hex>
X-Rewind-Job-Id: <uuid>
```

Signature is `HMAC-SHA256(shared_secret, X-Rewind-Job-Id || \n || raw_body)`. Standard pattern (Stripe / GitHub webhooks use the same shape). Runner verifies before processing; we provide a helper in `rewind_agent.runner` for this.

**Storage:** the shared secret is stored **encrypted at rest** in the `runners` table, under a server-managed app key (`REWIND_RUNNER_SECRET_KEY` env var, base64-32-bytes). At registration, server generates the raw secret, encrypts it with the app key (AES-256-GCM via the `aes-gcm` Rust crate), persists the ciphertext + nonce. At dispatch, server decrypts on-demand. The plaintext exists only briefly during dispatch; never logged, never serialized through API. `SensitiveString` redacts the (rare) cases where it would appear in error messages or debug output.

**Why encrypted-at-rest beats hash-only or in-memory-only:**
- **Durable across server restart** — registered runners stay usable.
- **Multi-replica safe** — any replica can dispatch to any runner (they all share the app key + DB).
- **Honest about secrets at rest** — admins know what's stored and how it's protected; no surprise "runner suddenly stopped working" after a deploy.

**Bootstrap:** if `REWIND_RUNNER_SECRET_KEY` is unset at server startup, the runner registry endpoint returns `503 Service Unavailable` with a clear error pointing operators at the env var. Documented in `docs/runners.md`. Generation: `openssl rand -base64 32`. Rotation requires re-registering all runners (acceptable for v1; v3.1 adds key versioning).

**Why not hash-only:** `SHA-256(token)` works for inbound auth (verify a runner-supplied token by hashing + comparing) but the server must HMAC-sign OUTBOUND webhooks, and hash → raw is a one-way function. A hash-only model would force in-memory caching of the raw token, which doesn't survive restart or scale to multi-replica.

**Why not invert the protocol (no outbound signing):** if Rewind doesn't authenticate to the runner, anyone who can reach the runner's webhook URL can trigger an agent run. Operators with public-ish webhook endpoints (the typical deployment) need the signature. Inverting only works if the runner is on a private network, which contradicts the Tier 3 use case.

### Why webhooks (push) and not long-polling (pull)

Webhooks are simpler when the runner can expose a public-ish HTTP endpoint:

- Operator's agent already runs as a long-lived process (FastAPI service, ray-agent, etc.) — adding an `/rewind-webhook` route is one block of code.
- Push semantics fit the dashboard's "click button → see thing happen" expectation.
- No polling overhead on either side.

Long-polling would be the right choice for runners behind NAT (laptops, agents in restrictive environments), but for v1 we assume the runner has an addressable URL. **Pull-based runners are explicitly deferred to a v3.1 follow-up** — the registration shape supports both modes (`mode: "webhook" | "polling"`) but only `webhook` is implemented in this PR.

### Status events (5 types)

Runners post these to `POST /api/replay-jobs/{id}/events`:

| event | when | payload |
| --- | --- | --- |
| `started` | runner accepted the job and the agent is about to begin | `{ event: "started" }` |
| `progress` | agent finished a step (LLM call, tool call) | `{ event: "progress", step_number, total_steps?, last_step_label? }` |
| `completed` | agent ran to its natural end successfully | `{ event: "completed", total_steps, duration_ms }` |
| `errored` | agent threw or runner couldn't kick it off | `{ event: "errored", error: <string>, stage?: "dispatch" \| "agent" }` |
| `cancelled` | dashboard cancelled the job mid-flight | `{ event: "cancelled" }` |

Server records these into `replay_job_events` (append-only) and broadcasts via the existing `StoreEvent` channel that the dashboard's WebSocket already subscribes to.

**Event endpoint auth (REVISED post-review, resolves BLOCKER #2).** The dashboard auth middleware (`auth::auth_middleware` in `crates/rewind-web/src/lib.rs:350`) protects all `/api/*` routes today and expects a dashboard/server bearer token that runners don't have. Three options were considered:

1. Give every runner a dashboard-level bearer token. **Rejected** — over-privileged. A compromised runner shouldn't be able to read all sessions or delete data.
2. Layer the existing middleware to allow EITHER bearer OR runner-token. **Rejected** — fragile, easy to introduce auth-bypass bugs.
3. **Adopted:** dedicated runner-auth path. `POST /api/replay-jobs/{id}/events` is registered OUTSIDE the dashboard-bearer middleware (via Axum's `Router::nest`/`merge` shape), and gets its own middleware that:
   - Reads `X-Rewind-Runner-Auth: <runner-token>`
   - Hashes the supplied token, looks up the matching `runners` row by `auth_token_hash`
   - Verifies `runner.id == job.runner_id` (the runner posting events owns this job)
   - On success, attaches the runner to the request extensions so the handler doesn't re-lookup
   - On failure, returns `401 Unauthorized` (token invalid) or `403 Forbidden` (token valid but runner doesn't own the job)

**Terminal-state protection:** event handlers reject any event arriving after a terminal state (`completed`/`errored`). The job's `state` is checked under a row-level lock; concurrent events get serialized at the DB level so there's no TOCTOU race.

**Cancellation: NOT in v1 (REVISED post-review, resolves HIGH #5).** The original plan listed `cancelled` as both a state and an event but had no protocol for the dashboard to actually tell the runner to stop. Rather than ship half a feature, v1 has 4 events (`started`/`progress`/`completed`/`errored`) and the state machine is:

```
pending → dispatched → in_progress → completed
                            └──────→ errored
```

No cancellation. v3.1 will add cooperative cancel: server sets a cancel flag → runner polls or receives `POST {runner.webhook_url}/cancel/{job_id}` → runner reports `cancelled` → server records. The Python `RewindRunner` library will expose a cancellation token that decorated agents can check. This is a meaningful protocol, not a one-line addition; deferring it to v3.1 keeps v1 honest.

## Component-by-component

### `SensitiveString` newtype (Rust)

```rust
// crates/rewind-store/src/sensitive.rs
pub struct SensitiveString(String);

impl SensitiveString {
    pub fn new(s: impl Into<String>) -> Self { Self(s.into()) }
    pub fn expose(&self) -> &str { &self.0 }   // explicit unwrap
    pub fn into_inner(self) -> String { self.0 }
}

impl fmt::Debug for SensitiveString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SensitiveString(***)")
    }
}

impl fmt::Display for SensitiveString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "***")
    }
}

impl Serialize for SensitiveString {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("***")  // Never serialize the raw string.
    }
}

// Deserialize unchanged — when reading from DB, we get the raw bytes.
```

The asymmetry (de-serialize unchanged, serialize redacted) is deliberate: the type is for OUTBOUND data (logs, API responses, telemetry), not data at rest. Storage layer hashes secrets separately for verification — see "Token storage" below.

### Runner data model

```rust
// crates/rewind-store/src/runners.rs
pub struct Runner {
    pub id: String,            // UUID
    pub name: String,          // operator-supplied label, e.g. "ray-agent-prod"
    pub mode: RunnerMode,      // Webhook (v1) | Polling (v3.1+)
    pub webhook_url: Option<String>,  // None for polling mode
    pub auth_token_hash: String,      // SHA-256 of the raw token
    pub auth_token_preview: String,   // first 8 chars + "***" for UI display
    pub created_at: DateTime<Utc>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub status: RunnerStatus,         // Active | Disabled | Stale
}

pub enum RunnerMode { Webhook, Polling }

pub enum RunnerStatus { Active, Disabled, Stale }
```

### Token storage (REVISED post-review, resolves BLOCKER #1)

The original "hash-only" model worked for inbound auth (verify runner-supplied token) but failed for outbound HMAC-signed webhooks (server can't recover raw token from a hash). Encrypted-at-rest is the unambiguous fix.

At registration:

1. Server generates a 32-byte random token, base64-url encoded (the "raw token" returned to the client).
2. Server encrypts the raw token with the app key (`REWIND_RUNNER_SECRET_KEY`, AES-256-GCM, fresh nonce per row).
3. Server stores `(ciphertext, nonce)` in `runners.encrypted_token` + `runners.token_nonce`.
4. Server stores SHA-256(raw_token) in `runners.auth_token_hash` (for fast inbound auth lookups — no decryption needed in the hot path).
5. Server stores the first 8 chars + `***` in `runners.auth_token_preview` (UI display, so operators can identify which token they have).
6. The raw token is returned ONCE in the registration response and never persisted in plaintext on the server.

At dispatch:
1. Server fetches `(encrypted_token, nonce)` from the `runners` row.
2. Decrypts with the app key.
3. Uses the plaintext to compute `HMAC-SHA256(raw_token, ...)` for the outbound webhook.
4. Discards the plaintext immediately after signing.

At inbound auth (runner posting events):
1. Server reads `X-Rewind-Runner-Auth` header (the raw token).
2. Server computes SHA-256 of the supplied token.
3. Server looks up `runners.auth_token_hash` by that SHA — fast, no decryption.
4. On match, server verifies `runner.id == job.runner_id` (ownership).

**Token rotation:** dashboard "regenerate" button creates a new raw token, encrypts and re-stores both the ciphertext and the new hash, invalidates the old hash. Returns the new raw token once. Existing in-flight jobs continue using the new token (signing happens at dispatch time, not at job-creation time). Multi-replica safe because every replica reads the freshest DB row.

**Why this is honest about secrets:** an admin running the Rewind server can choose to (a) backup the DB but not the env var (separate handling for secrets-at-rest from data-at-rest), or (b) include both — explicit choice. Encrypted-at-rest with a key-management story matches industry norm.

**Open: app key rotation.** Rotating `REWIND_RUNNER_SECRET_KEY` requires re-encrypting every `runners.encrypted_token` row. v1 does NOT support key rotation; admins generate the key once and keep it. v3.1 adds key versioning (`encrypted_token_key_version` column, rolling rotation).

### Replay job state machine (REVISED post-review, resolves MEDIUM #6 + HIGH #5)

```
pending → dispatched → in_progress → completed
            ↓              ↓             
            errored        errored
```

Cancellation removed from v1 per HIGH #5 resolution. Lease/timeout/reaper added per MEDIUM #6.

Stored in `replay_jobs` table with timestamps for each transition + lease columns:

```sql
CREATE TABLE replay_jobs (
    id TEXT PRIMARY KEY,
    runner_id TEXT NOT NULL REFERENCES runners(id),
    session_id TEXT NOT NULL REFERENCES sessions(id),
    replay_context_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending','dispatched','in_progress','completed','errored')),
    error_message TEXT,
    error_stage TEXT,                  -- 'dispatch' | 'agent' | 'lease_expired'
    -- Timestamps
    created_at TEXT NOT NULL,
    dispatched_at TEXT,
    started_at TEXT,
    completed_at TEXT,
    -- Leases
    dispatch_deadline_at TEXT,         -- runner must reply 202 by this time
    lease_expires_at TEXT,             -- extended on heartbeat / progress event
    -- Stats
    progress_step INTEGER NOT NULL DEFAULT 0,
    progress_total INTEGER             -- optional; runner reports if known
);

CREATE TABLE replay_job_events (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL REFERENCES replay_jobs(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL CHECK (event_type IN ('started','progress','completed','errored')),
    step_number INTEGER,
    payload TEXT,                      -- JSON
    created_at TEXT NOT NULL
);
```

**Lease semantics:**
- `dispatch_deadline_at` = `dispatched_at + 10s`. If no `started` event by then, reaper marks job `errored` with `stage: "dispatch"`.
- `lease_expires_at` = `last_event_at + 5min`. Extended on every heartbeat or progress event. If exceeded, reaper marks job `errored` with `stage: "lease_expired"`.

**Reaper task** (`crates/rewind-web/src/reaper.rs`): tokio task spawned at server startup, scans every 30s. Single SQL `UPDATE` transitions expired jobs to `errored` atomically. Reaper logs each transition for observability.

**Dashboard behavior on lease expiry:** the WebSocket broadcast includes the `errored` event, so the dashboard's modal sees it and shows "Lease expired — runner stopped responding. Last seen: <timestamp>" with a "Retry" button. Documented in `docs/runners.md` troubleshooting.

### Webhook dispatcher

```rust
// crates/rewind-web/src/dispatcher.rs
pub struct Dispatcher { client: reqwest::Client, ... }

impl Dispatcher {
    pub async fn dispatch(&self, runner: &Runner, job: &ReplayJob) -> Result<()> {
        let raw_token = ...;  // looked up from a secure cache (NOT the DB)
        let body = json!({
            "job_id": job.id,
            "session_id": job.session_id,
            "replay_context_id": job.replay_context_id,
            "base_url": self.base_url,
        });
        let body_bytes = serde_json::to_vec(&body)?;
        let signature = hmac_sha256(&raw_token, &job.id, &body_bytes);
        let resp = self.client
            .post(runner.webhook_url.as_ref().unwrap())
            .header("X-Rewind-Job-Id", &job.id)
            .header("X-Rewind-Signature", format!("sha256={signature}"))
            .timeout(Duration::from_secs(5))  // accept-only; runner replies fast
            .body(body_bytes)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(...);
        }
        Ok(())
    }
}
```

**Token decryption flow (REVISED post-review, resolves BLOCKER #1 part 2):** at dispatch, the server reads `(encrypted_token, nonce)` from the `runners` row, decrypts via AES-256-GCM with `REWIND_RUNNER_SECRET_KEY`, uses the plaintext to compute the HMAC, and discards the plaintext. No in-memory cache, no process-local state, no restart fragility, multi-replica safe.

Concrete code:

```rust
let raw_token = self.crypto.decrypt(&runner.encrypted_token, &runner.token_nonce)?;
let signature = hmac_sha256(raw_token.expose(), &job.id, &body_bytes);
// raw_token (SensitiveString) drops at end of scope; never logged.
```

The `CryptoBox` abstraction wraps AES-256-GCM via the `aes-gcm` crate. App key is read once at server startup (`crates/rewind-web/src/main.rs`) and held in `AppState`. If `REWIND_RUNNER_SECRET_KEY` is unset, runner endpoints return `503` with the clear bootstrap error.

### Python `rewind_agent.runner` (REVISED post-review, resolves HIGH #4)

The original example used non-existent SDK params (`ExplicitClient(rewind_url=...)`, `start_replay(replay_context_id=...)`). Two bridges are needed in Phase 3 to make this work:

**(a)** New `ExplicitClient.attach_replay_context(session_id, replay_context_id)` method that sets `_session_id` and `_replay_context_id` ContextVars to bind an existing context (without creating a new one — `start_replay` always creates).

**(b)** Env-var bootstrap. When `REWIND_SESSION_ID` and `REWIND_REPLAY_CONTEXT_ID` are set in the process environment, `intercept.install()` automatically calls `attach_replay_context` so the agent doesn't need to know about replay at all. Critical for the runner's spawn-subprocess pattern.

Both are added to Phase 3 scope. Code:

```python
# In rewind_agent.explicit:
def attach_replay_context(self, session_id: str, replay_context_id: str) -> None:
    """Bind an EXISTING replay context (created server-side by Phase 3
    runner dispatch). Sets contextvars without creating a new context.
    For decorator + intercept users who received the context from
    a runner job dispatch, not from start_replay.
    """
    _session_id.set(session_id)
    _replay_context_id.set(replay_context_id)

# In rewind_agent.intercept._install:
def install(predicates=None) -> None:
    # … existing patching logic …
    # NEW: env-var bootstrap for runner subprocesses
    sid = os.environ.get("REWIND_SESSION_ID")
    rcid = os.environ.get("REWIND_REPLAY_CONTEXT_ID")
    if sid and rcid:
        ExplicitClient().attach_replay_context(sid, rcid)
```

Now the corrected runner example:

```python
# python/rewind_agent/runner.py — operator's webhook endpoint
import os
from rewind_agent import ExplicitClient, intercept
from rewind_agent.runner import RewindRunner

runner = RewindRunner(
    base_url=os.environ["REWIND_BASE_URL"],          # NOTE: base_url, not rewind_url
    auth_token=os.environ["REWIND_RUNNER_TOKEN"],
)

@runner.handle_replay
async def run_agent(job):
    # job.session_id and job.replay_context_id provided by the webhook payload.
    # Bind them to ExplicitClient before running the agent.
    intercept.install()
    client = ExplicitClient(base_url=job.base_url)
    client.attach_replay_context(job.session_id, job.replay_context_id)

    try:
        await runner.report_progress(job.id, "started")
        await my_agent.run()  # operator's existing agent code
        await runner.report_progress(job.id, "completed")
    except Exception as e:
        await runner.report_progress(job.id, "errored", error=str(e))
        raise

# Mount on the operator's existing FastAPI / aiohttp / Flask app:
app.add_route("/rewind-webhook", runner.asgi_handler())
# OR start a standalone server for one-off use:
runner.serve(host="0.0.0.0", port=8080)
```

The `RewindRunner` class handles HMAC signature verification (using the runner's stored token, the SAME token Rewind used to sign), job-level error catching, and the progress-reporting helper. The operator writes their `@handle_replay` body around the existing agent invocation.

For runners that spawn the agent as a subprocess (e.g. the agent is a separate `python my_agent.py`), use the env-var bootstrap pattern instead:

```python
@runner.handle_replay
async def run_agent(job):
    env = os.environ.copy()
    env["REWIND_BASE_URL"] = job.base_url
    env["REWIND_SESSION_ID"] = job.session_id
    env["REWIND_REPLAY_CONTEXT_ID"] = job.replay_context_id
    proc = await asyncio.create_subprocess_exec(
        "python", "my_agent.py",
        env=env,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    await runner.report_progress(job.id, "started")
    rc = await proc.wait()
    if rc == 0:
        await runner.report_progress(job.id, "completed")
    else:
        await runner.report_progress(job.id, "errored", error=f"exit code {rc}")
```

In this mode the spawned `my_agent.py` doesn't need to know it's running under a replay — `intercept.install()` reads the env vars and binds the context automatically.

### Dashboard UI

#### "Run replay" button placement

- On `SessionView`: a button next to the existing "Fork" button on each fork timeline. Disabled if no runners are registered (with a tooltip pointing at the Runners page).
- On `StepDetailPanel`: same button on individual steps that are fork-points.

#### Modal flow

```
[Run replay] click
  → Modal opens
  → Lists registered runners (name, last-seen, status)
  → Operator picks one
  → "OR show me the CLI command instead" toggle (escape hatch — generates the equivalent `rewind replay <session_id> --from <step> --fork-id <fork>` command using the EXISTING `rewind replay` CLI that's already shipped, useful when no runners are registered or the operator wants local debugging. NO new `--runner` flag introduced; if the operator wants runner-based replay, they go through the dashboard button. Resolves MEDIUM #7.)
  → "Run replay" button
  → POST /api/sessions/{sid}/replay-jobs
  → Modal switches to live-progress view (subscribes to WebSocket)
  → Shows step-by-step progress as the runner reports back
  → On `completed`: shows the new replay timeline ID + "Open" button
  → On `errored`: shows the error + "Retry" button
```

**Resolution (MEDIUM #7):** modal fallback now shows the existing `rewind replay <session_id> --from <step> --fork-id <fork>` command — no new CLI flag introduced. CLI scope unchanged at `rewind runners {list,add,remove}`. The runner-driven flow is dashboard-only; the CLI fallback is for local debugging where the operator runs the agent themselves.

#### Runners management page

`/runners` route. Lists registered runners with:
- Name, mode (webhook URL), status, last-seen
- "Recent jobs" sparkline / count
- Per-runner actions: regenerate token, disable, delete
- "Register new runner" button → modal with name + webhook_url fields, returns the raw auth token ONCE in a copyable code block

### CLI subcommand

```bash
rewind runners list
# id                                    name              status   last-seen
# 7c0a...                               ray-agent-prod    active   2m ago
# 8d1b...                               local-dev         stale    3d ago

rewind runners add --name "my-agent" --webhook http://localhost:8080/rewind-webhook
# Registered. Auth token (save this; it won't be shown again):
#   rwd_runner_aBc123xyz789...

rewind runners remove 7c0a...
# Removed.
```

The CLI is a thin wrapper around the same `/api/runners` endpoints the dashboard uses.

## Sequencing within the PR

13 commits, ordered for review-friendliness. Each green on `scripts/pre-push-check.sh`.

1. `plan: phase 3 runner registry + dashboard replay button` (this doc as anchor)
2. `feat(rust): SensitiveString newtype + tests` (~150 LOC, foundational)
3. `feat(rust): Runner + ReplayJob data models + schema migrations`
4. `feat(rust): rewind-web /api/runners CRUD endpoints + tests`
5. `feat(rust): webhook dispatcher with HMAC signing + retry`
6. `feat(rust): /api/sessions/{id}/replay-jobs + /api/replay-jobs/{id}/events endpoints`
7. `feat(web): Runners page (list / register / remove / regenerate token)`
8. `feat(web): RunReplayButton + ReplayJobModal + WebSocket progress`
9. `feat(python): rewind_agent.runner library + asgi_handler + progress reporting`
10. `feat(cli): rewind runners {list,add,remove}`
11. `test: end-to-end integration test (register runner → fire button → progress events back → completion)`
12. `docs: runners.md + decision-matrix update from 3-way to 5-way` (bundles the deferred Phase 2 doc gap)
13. `chore: conditional version bumps` — see "Versioning" section below for the exact decision tree.

**Versioning (REVISED post-review, resolves LOW #8).** Per CLAUDE.md track-1 + track-2 rules, the version bumps are conditional on what's already been released:

- **Rust workspace `0.13.0 → 0.14.0`:** YES, warranted. Master 0.13.0 has been released as GitHub tag `v0.13.0` (cut Apr 27 07:26 UTC). Phase 3 has a schema migration (`runners` + `replay_jobs` tables) and new endpoints, so a minor bump is correct. All 5 mirror files (`Cargo.toml`, `Cargo.lock`, `python/rewind_cli.py`, `python-mcp/pyproject.toml`, `python-mcp/rewind_mcp_cli.py`) move together to keep CLI_VERSION in lockstep.
- **Python SDK `0.15.0`:** STAYS at 0.15.0. Master is at 0.15.0 but PyPI is at 0.14.8 — 0.15.0 is unpublished. Per CLAUDE.md track-2: "Has the current SDK version already been published to PyPI? NO → No bump needed (changes ride with the unreleased version)." Phase 3's Python additions (`rewind_agent.runner`, `ExplicitClient.attach_replay_context`, env-var bootstrap in `intercept.install`) ride 0.15.0.
- **Python MCP `0.13.0`:** STAYS at 0.13.0. No MCP API change AND PyPI is at 0.12.10 (unpublished). Track-1 rule fires only if `crates/` or `web/src/` changes AND the current Rust version has been released — `crates/` IS changing in this PR, so `python-mcp/pyproject.toml` and `python-mcp/rewind_mcp_cli.py` need to update CLI_VERSION to `0.14.0` to match the Rust binary they're a thin wrapper over (the binary version, not the package version). The python-mcp package version itself stays at `0.13.0`.

Concrete file changes in commit 13:
- `Cargo.toml`: `0.13.0 → 0.14.0`
- `Cargo.lock`: rebuild (auto)
- `python/rewind_cli.py`: `CLI_VERSION = "0.14.0"`
- `python-mcp/pyproject.toml`: stays at `0.13.0`, but `CLI_VERSION` constant inside it (if any — TBD when implementing) tracks `0.14.0`
- `python-mcp/rewind_mcp_cli.py`: `CLI_VERSION = "0.14.0"`
- `python/pyproject.toml`: stays at `0.15.0`
- `python/rewind_agent/__init__.py`: stays at `__version__ = "0.15.0"`

## Acceptance criteria

- [ ] `SensitiveString` redacts in Debug, Display, JSON serde; round-trips raw bytes through deserialize for DB reads
- [ ] Runners can be registered via dashboard + CLI; raw auth token returned once, hash stored
- [ ] Dashboard "Run replay" button is visible and dispatches a webhook to the chosen runner
- [ ] Runner receives the webhook, verifies HMAC signature, accepts (202)
- [ ] Runner posts progress events back; dashboard streams them via WebSocket
- [ ] Replay job state machine transitions correctly through pending → dispatched → in_progress → completed (or errored / cancelled)
- [ ] Token regeneration works without breaking in-flight jobs
- [ ] Pre-push routine all 6 stages green
- [ ] No regressions in Phase 0/1/2 tests
- [ ] Python `rewind_agent.runner` works under `intercept.install()` AND `cached_llm_call` (composition with prior phases)
- [ ] CLI `rewind runners {list,add,remove}` round-trips through the same API

## Open questions (please review before I start)

These are real decisions where I want your input before deep implementation. None of them block opening the PR for plan review — happy to defer any of them to "do the obvious thing and document".

### Q1. Webhook-only for v1, or also polling-based runners?

**My read:** webhook-only for v1 keeps the scope manageable (~5-7 days of focused work). Polling-based runners (for laptops behind NAT) are a v3.1 follow-up. The schema includes `mode: Webhook | Polling` from day one so adding polling later doesn't require a migration.

**Concrete trade-off:** without polling, `ray-agent` (which has a public-ish service URL inside the corp network) works; a developer running an agent locally on their laptop doesn't, unless they tunnel via ngrok. Acceptable for v1?

### Q2. Token storage — in-memory cache vs filesystem keyring vs always-prompt?

**My read:** in-memory cache (`Arc<RwLock<HashMap>>`) populated at runner-registration. Server restart means tokens are gone until the operator clicks "regenerate" or the runner re-registers. **Documented limitation.**

The alternatives:
- **Filesystem keyring (macOS Keychain / Linux libsecret):** more code, OS-specific, secure across restart. Probably a v3.1 follow-up.
- **Always-prompt:** dashboard prompts the operator to paste the token before each "Run replay" click. Annoying.

OK with the in-memory + documented restart limitation?

### Q3. Job concurrency — one in-flight per runner, or unlimited?

**My read:** one in-flight per runner for v1. Simpler state machine, predictable behavior, matches the typical "one agent process per runner" deployment. If the operator triggers a second job while one is in-flight, the second goes to `pending` and dispatches when the first completes/errors.

Alternative: dispatch all jobs immediately, runner's responsibility to queue. More flexible but pushes complexity to the runner library. v3.1?

### Q4. CLI command scope — full `rewind runners {list,add,remove,regenerate-token,heartbeat}` or just MVP?

**My read:** ship `list`/`add`/`remove` in v1; defer `regenerate-token` and `heartbeat` to v3.1. The dashboard provides regenerate-token (no need to duplicate); heartbeat is something runners do automatically, not a manual CLI gesture.

### Q5. Should this PR also ship the v0.13.0 → v0.14.0 Rust release tag?

**My read:** yes, this is the natural cut. Phase 3 is a meaningful schema migration + new endpoints; bumping Rust workspace minor and cutting the release post-merge is how it lands cleanly. PyPI publish of SDK 0.16.0 is also natural here. (Note: those are user-initiated post-merge actions per CLAUDE.md.)

## NOT in this PR (deferred)

- **Polling-mode runners** — Q1; v3.1.
- **Filesystem keyring for token storage** — Q2; v3.1.
- **Job concurrency > 1 per runner** — Q3; v3.1.
- **CLI `regenerate-token` + `heartbeat`** — Q4; v3.1.
- **Runner discovery / mDNS / service mesh integration** — out of scope; operators register runners explicitly.
- **Multi-step replay scheduling (run N replays in sequence with diff at the end)** — adjacent feature; separate planning.
- **Fork-from-replay-button** — when a replay errors mid-flight, "fork from this step" is a natural follow-on. Adjacent UI work.

## Estimated scope (REVISED post-review)

- **New code:** ~2800 LOC across ~16 new files (1800 Rust + 600 web + 300 Python + 100 CLI). Bumped from 2500 due to the encrypted-token storage path (BLOCKER #1) and the lease/reaper subsystem (MEDIUM #6).
- **Tests:** ~95 cases (was 80) — added coverage for the new encryption round-trip, lease expiry, runner-token auth on `/events`, atomic fork+context+job creation, and the `attach_replay_context` API.
- **Docs:** `docs/runners.md` (~280 lines including bootstrap of `REWIND_RUNNER_SECRET_KEY` and lease/restart troubleshooting) + decision-matrix updates in `docs/recording.md` and `docs/getting-started.md`.
- **Versions:** Rust workspace 0.13.0 → **0.14.0**. Python SDK rides at **0.15.0** (unpublished, per CLAUDE.md track-2). MCP package stays at **0.13.0**; CLI_VERSION constants in MCP move to `0.14.0` to track the Rust binary.
- **Effort estimate:** ~6-8 days of focused work (was 5-7). Encryption layer + reaper + atomic-fork-creation are new work; the offset is some scope removal (cancellation deferred to v3.1).

## Plan reference for predecessors

- [`plans/phase-1-http-transport-adapters.md`](./phase-1-http-transport-adapters.md) — Phase 1 architecture (`_flow`, intercept, ExplicitClient cache APIs)
- [`plans/phase-2-cached-llm-call-decorator.md`](./phase-2-cached-llm-call-decorator.md) — Phase 2 architecture (decorator, contextvar suppression)
