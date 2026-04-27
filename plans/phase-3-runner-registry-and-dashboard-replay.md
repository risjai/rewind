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

### Auth model: HMAC-signed webhooks

The runner is registered with a shared secret (the auth token). Every webhook from Rewind→runner carries:

```
X-Rewind-Signature: sha256=<hex>
X-Rewind-Job-Id: <uuid>
```

Signature is `HMAC-SHA256(shared_secret, X-Rewind-Job-Id || \n || raw_body)`. Standard pattern (Stripe / GitHub webhooks use the same shape). Runner verifies before processing; we provide a helper in `rewind_agent.runner` for this.

The shared secret is generated by Rewind at runner-registration time and shown ONCE in the dashboard / CLI output. After that the runner stores it; Rewind stores its `SensitiveString`-wrapped form (which never appears in logs / debug / API responses). This is why `SensitiveString` is in scope for this PR — auth tokens are precisely the thing we don't want leaking through the obvious channels.

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

### Token storage

Auth tokens are NEVER stored raw. At registration:
1. Server generates a 32-byte random token, base64-url encoded.
2. Server returns the raw token to the registering client (CLI or dashboard) ONCE in the response.
3. Server stores SHA-256(token) in `auth_token_hash` and the first 8 chars + `***` in `auth_token_preview` (for UI display so users can identify which token they have).
4. On subsequent requests, the runner sends the raw token in `X-Rewind-Auth`; server hashes it and looks up the runner.

Same model as GitHub personal access tokens. Token rotation: dashboard "regenerate" button creates a new token, updates the hash, returns the new raw token once, invalidates the old one.

### Replay job state machine

```
pending → dispatched → in_progress → completed
            ↓              ↓             
            errored        errored
            
            cancelled (from any non-terminal state)
```

Stored in `replay_jobs` table with timestamps for each transition. Append-only event log in `replay_job_events` for the per-step progress messages.

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

**Token re-cache concern:** the dispatcher needs the raw token to sign webhooks, but we only stored the hash. Solution: keep the raw token in an in-memory cache (process-local `Arc<RwLock<HashMap<runner_id, SensitiveString>>>`) populated at registration. On server restart, the cache is empty until the runner re-registers OR the dispatcher 401s and the dashboard prompts the operator to regenerate. **Documented limitation: server restart breaks dispatch until token re-issuance.** v3.1 fix: secure-storage backend (keyring on macOS, libsecret on Linux, OS-encrypted file).

### Python `rewind_agent.runner`

```python
# python/rewind_agent/runner.py
from rewind_agent import ExplicitClient, intercept
from rewind_agent.runner import RewindRunner

runner = RewindRunner(
    rewind_url="http://rewind.corp.example",
    auth_token=os.environ["REWIND_RUNNER_TOKEN"],
)

@runner.handle_replay
async def run_agent(job):
    # job.session_id, job.replay_context_id provided
    intercept.install()
    client = ExplicitClient(rewind_url=job.base_url)
    client.start_replay(job.session_id, replay_context_id=job.replay_context_id)
    try:
        await runner.report_progress(job.id, "started")
        await my_agent.run()  # operator's existing agent code
        await runner.report_progress(job.id, "completed")
    except Exception as e:
        await runner.report_progress(job.id, "errored", error=str(e))
        raise

# Mount on operator's existing FastAPI / aiohttp / Flask app:
app.add_route("/rewind-webhook", runner.asgi_handler())
# OR start a standalone server for one-off use:
runner.serve(host="0.0.0.0", port=8080)
```

The `RewindRunner` class handles signature verification, job-level error catching, and progress reporting. Operators write their `@handle_replay` body around their existing agent invocation.

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
  → "OR show me the CLI command instead" toggle (escape hatch — generates the equivalent `rewind replay --session-id ... --runner ...` command for copy-paste, useful when no runners are registered or the operator wants local debugging)
  → "Run replay" button
  → POST /api/sessions/{sid}/replay-jobs
  → Modal switches to live-progress view (subscribes to WebSocket)
  → Shows step-by-step progress as the runner reports back
  → On `completed`: shows the new replay timeline ID + "Open" button
  → On `errored`: shows the error + "Retry" button
```

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
13. `chore: bump versions — Rust 0.13.0 → 0.14.0, Python SDK 0.15.0 → 0.16.0, MCP 0.13.0 → 0.13.1 (no MCP API change)`

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

## Estimated scope

- **New code:** ~2500 LOC across ~15 new files (1500 Rust + 600 web + 250 Python + 150 CLI).
- **Tests:** ~80 cases across Rust unit, Rust integration, web component, Python, end-to-end.
- **Docs:** `docs/runners.md` (~250 lines) + decision-matrix updates in `docs/recording.md` and `docs/getting-started.md`.
- **Versions:** Rust 0.13.0 → **0.14.0** (schema migration); Python SDK 0.15.0 → **0.16.0** (new package surface); MCP 0.13.0 (unchanged).
- **Effort estimate:** ~5-7 days of focused work, depending on Q1/Q2/Q3 answers.

## Plan reference for predecessors

- [`plans/phase-1-http-transport-adapters.md`](./phase-1-http-transport-adapters.md) — Phase 1 architecture (`_flow`, intercept, ExplicitClient cache APIs)
- [`plans/phase-2-cached-llm-call-decorator.md`](./phase-2-cached-llm-call-decorator.md) — Phase 2 architecture (decorator, contextvar suppression)
