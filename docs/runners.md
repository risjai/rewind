# Runners — operator-driven replays from the dashboard

> Phase 3 (Tier 3) of the Rewind architecture. Closes the dashboard's
> "viewer" loop into a control surface.

A **runner** is a long-lived agent process that exposes an HTTP
webhook endpoint. The Rewind dashboard's "Run replay" button POSTs
an HMAC-signed dispatch to the runner; the runner runs the agent
under a replay context and posts progress events back, which the
dashboard renders live.

This page is the operator how-to. For the storage / API design, see
[plans/phase-3-runner-registry-and-dashboard-replay.md](../plans/phase-3-runner-registry-and-dashboard-replay.md).

---

## Bootstrap (server side)

The runner registry endpoints encrypt auth tokens at rest with
AES-256-GCM. Generate a 32-byte app key once and set it in the env
before starting the server:

```bash
export REWIND_RUNNER_SECRET_KEY="$(openssl rand -base64 32)"
rewind web --port 4800
```

If the env var is unset, `/api/runners` endpoints return 503 with
a clear bootstrap message. If it's set but malformed (bad base64 or
wrong length), the server **panics at startup** so misconfig fails
loud rather than silently breaking dispatch.

`REWIND_PUBLIC_URL` is the externally-reachable base URL the
runner SDK posts events to. Defaults to `http://127.0.0.1:4800`;
**override in any non-local deployment** so dispatched runners
know where to call back:

```bash
export REWIND_PUBLIC_URL="https://rewind.your-company.example"
```

---

## Register a runner

### From the dashboard

1. Open the dashboard (defaults to `http://127.0.0.1:4800`).
2. Click **Runners** in the left nav.
3. Click **Register runner**.
4. Enter a friendly **name** (1-100 chars) and a **webhook URL**
   (`http(s)://...`, must be public-routable). Click **Register**.
5. The next screen reveals the **raw auth token in yellow.** Copy
   it now — the server won't surface it again. The dashboard prints
   the export-ready line for your runner's environment.

### From the CLI

```bash
rewind runners add \
  --name my-agent-runner \
  --webhook-url https://your-agent.example.com/rewind-webhook
```

The CLI prints the raw token in bold yellow with the same
"save this now" warning.

### List / remove / regenerate

```bash
rewind runners list
rewind runners remove <runner-id>
rewind runners regenerate-token <runner-id>
```

`remove` and `regenerate-token` return **409 Conflict** if the
runner has any non-terminal jobs (`pending`, `dispatched`,
`in_progress`). Drain or cancel those first.

---

## Implement the runner side

```python
from fastapi import FastAPI, Request
from rewind_agent import runner

config = runner.RunnerConfig.from_env()  # reads REWIND_RUNNER_TOKEN, REWIND_URL
app = FastAPI()

@runner.handle_replay
async def my_replay_handler(payload, reporter):
    # Auto-emitted before this runs: reporter.started() → state in_progress
    from rewind_agent import intercept
    from rewind_agent.explicit import ExplicitClient

    client = ExplicitClient(base_url=payload.base_url)
    client.attach_replay_context(
        session_id=payload.session_id,
        replay_context_id=payload.replay_context_id,
    )
    intercept.install()

    # Re-execute your agent under the replay context.
    for i, step in enumerate(my_agent_run(), start=1):
        await reporter.progress(i)

    await reporter.completed()

@app.post("/rewind-webhook")
async def webhook(request: Request):
    body = await request.body()
    return await runner.asgi_handler(
        config=config,
        headers=dict(request.headers),
        body_bytes=body,
        handler=my_replay_handler,
    )
```

Set the runner's environment:

```bash
export REWIND_RUNNER_TOKEN='<the raw token from registration>'
export REWIND_URL='https://rewind.your-company.example'
```

Alternative: env-var bootstrap. If you spawn the runner subprocess
with `REWIND_SESSION_ID`, `REWIND_REPLAY_CONTEXT_ID`, and
`REWIND_REPLAY_CONTEXT_TIMELINE_ID`, `intercept.install()` attaches
automatically — no SDK calls needed in the subprocess body. Useful
when the runner shells out to a separate agent script.

```bash
# In the runner's webhook handler, when shelling out to a separate
# agent script (e.g. an existing CLI binary):
env_for_subprocess = {
    **os.environ,
    "REWIND_SESSION_ID": payload.session_id,
    "REWIND_REPLAY_CONTEXT_ID": payload.replay_context_id,
    "REWIND_REPLAY_CONTEXT_TIMELINE_ID": payload.replay_context_timeline_id,
    "REWIND_URL": payload.base_url,
}
subprocess.run(["./my-agent"], env=env_for_subprocess)
```

`REWIND_REPLAY_CONTEXT_TIMELINE_ID` is recommended (review #154 round 2):
without it, live cache misses during the replay won't have a defined
recording target — they'd land in whatever timeline `_timeline_id`
defaults to. Always pass the fork timeline id from the dispatch
payload so live recordings stay in the fork.

---

## Dispatch a replay

### From the dashboard

1. Open a session.
2. Hover any step → click the cyan **Run replay** button (next to
   the amber **Fork** button).
3. Pick an active runner from the dropdown. (Optionally tick
   **Strict cache match** to surface cache divergences as 409s
   rather than warn-only headers.)
4. Click **Dispatch**. Progress streams live via WebSocket.

### From the CLI / programmatic

There is no `rewind dispatch` CLI command yet (deferred). To
dispatch from Python or curl, POST to
`/api/sessions/{sid}/replay-jobs`:

```bash
curl -X POST "$REWIND_URL/api/sessions/$SID/replay-jobs" \
  -H "Authorization: Bearer $REWIND_AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "runner_id": "...",
    "source_timeline_id": "...",
    "at_step": 3
  }'
```

The body accepts two shapes:

- **Shape A** (`runner_id`, `source_timeline_id`, `at_step`,
  `strict_match?`) — server forks + creates replay context
  atomically.
- **Shape B** (`runner_id`, `replay_context_id`) — caller
  already has a context (e.g. from the explicit API). Server
  validates the context belongs to the session and isn't being
  consumed by another in-flight job.

Returns `202 Accepted` with `{job_id, replay_context_id, fork_timeline_id?, state, dispatch_deadline_at}`.

---

## State machine

```
pending → dispatched → in_progress → completed
            ↓              ↓
            errored        errored
```

Transitions:

| From | Trigger | To |
|---|---|---|
| `pending` | dispatcher posts to runner; 2xx | `dispatched` |
| `pending` | dispatcher network/HTTP error | `errored` (`stage="dispatch"`) |
| `dispatched` | runner posts `started` event | `in_progress` |
| `dispatched` | reaper sees `dispatch_deadline_at` (10s) elapsed | `errored` (`stage="dispatch"`) |
| `dispatched` or `in_progress` | runner posts `errored` event | `errored` (`stage="agent"`) |
| `in_progress` | runner posts `progress` event | `in_progress` (lease extended) |
| `in_progress` | runner posts `completed` event | `completed` |
| `in_progress` | reaper sees `lease_expires_at` (5min) elapsed without heartbeat | `errored` (`stage="lease_expired"`) |

Cancellation: deferred to v3.1 (cooperative cancel protocol).
There is no `DELETE /api/replay-jobs/{id}` in v1 — operators who
need to abandon a runaway runner kill the runner process
externally; the lease reaper subsequently marks the job `errored`
with `stage='lease_expired'` once the heartbeat window passes.

---

## Security model

**Outbound (Rewind → runner):** every dispatch carries
`X-Rewind-Job-Id` + `X-Rewind-Signature: sha256=<hex>` headers.
Signature recipe:

```
HMAC-SHA256(raw_token, X-Rewind-Job-Id || "\n" || raw_body)
```

The runner library's `verify_signature(...)` does the comparison
in constant time (`hmac.compare_digest`). Rust ↔ Python wire
format is locked by reference vectors in both
`crates/rewind-web/src/dispatcher.rs::compute_signature_matches_python_reference`
and `python/tests/test_runner.py::test_compute_signature_matches_rust_reference`.

**Inbound (runner → Rewind events endpoint):** runners send
`X-Rewind-Runner-Auth: <raw-token>`. The server SHA-256 hashes
the supplied value and looks up `runners.auth_token_hash`
(UNIQUE indexed). Ownership check: `job.runner_id` must equal
the authenticated runner's id (returns 403 otherwise).

**Webhook URL SSRF guard:** webhook URLs are parsed with the
`url` crate (rejects userinfo `http://user:pass@...`) and run
through `url_guard::validate_export_endpoint` — the same policy
that gates `export_otel`. Refuses loopback (`127.0.0.1`, `::1`,
`localhost`), RFC 1918 private ranges, link-local + cloud
metadata IPs (`169.254.169.254`), and parser-differential
numeric forms (octal/hex/decimal IP literals).

The check runs at **registration time** (POST /api/runners) AND
again at **dispatch time** (each outbound webhook). Dispatch-time
re-validation closes the window where a registered host's DNS
records change between registration and dispatch (Review #154 F6).
There's still a residual race between the dispatch-time DNS lookup
and reqwest's connection-time re-resolve — a custom hyper connector
that rejects private IPs at TCP connect would be required to fully
close it; planned for v3.1.

**Outbound webhook freshness (Review #154 F5):** every dispatch
includes an `X-Rewind-Timestamp: <unix-seconds>` header AND the
timestamp is part of the signed input (`HMAC-SHA256(token,
timestamp || \\n || job_id || \\n || body)`). The runner SDK
rejects requests outside a ±5 minute tolerance window, defeating
long-window replays of captured signed dispatches. The SDK also
keeps a process-local seen-cache of recent `job_id`s so a captured
signature INSIDE the tolerance window still can't re-launch the
agent (200 + `duplicate=true` on the second arrival).

**Dev escape hatch:** `REWIND_ALLOW_LOOPBACK_WEBHOOKS=1`
bypasses the SSRF guard. **NEVER set this in production.**
Strictly for local dev where the runner stub lives on
`127.0.0.1`.

---

## Lease + reaper

The reaper tokio task scans every 30s for two failure classes:

- **Dispatch deadline** — `dispatched_at + 10s`. If the runner
  doesn't emit `started` by then, the job is marked `errored`
  with `stage="dispatch"`. Means the runner accepted (replied
  202) but never actually started.
- **Lease expiry** — `lease_expires_at`. Initial value is
  `dispatched_at + 5min`; extended on every `started` /
  `progress` event the runner emits. If exceeded, the job is
  marked `errored` with `stage="lease_expired"` — the runner
  stopped sending heartbeats mid-run.

Reaper transitions go through the same SQL-level terminal-state
guard as the rest of the state machine, so they can't race a
late `completed` event into corrupted state.

---

## Troubleshooting

**"Endpoint resolves to blocked IP 127.0.0.1" on registration.**
You're trying to register a runner whose webhook URL points at
loopback / private IP. Either expose the runner on a routable
URL or set `REWIND_ALLOW_LOOPBACK_WEBHOOKS=1` for local dev.

**"polling mode is not implemented in v1".**
The schema accepts `mode = "polling"` for forward-compat with v3.1.
Until then, use webhook mode (the only implemented dispatch path).

**"runner has N non-terminal job(s)" on remove or rotate.**
Drain or cancel in-flight jobs first. The dashboard's session
detail page lists active jobs; cancel them from the
`Run replay` modal's open-jobs list, or via
`DELETE /api/replay-jobs/{id}`.

**"signature mismatch" in runner logs.**
Likely token drift between dispatcher and runner. Check that
`REWIND_RUNNER_TOKEN` in the runner matches the *most recently
generated* token. Token rotation invalidates the old token
immediately; in-flight dispatches that the runner accepted
with the old token will fail their callback verification.

**"runner does not own this job" (403) on event POST.**
The runner authenticated successfully but is trying to post
events on a job belonging to a different runner. Usually
means the runner is processing a stale/leaked dispatch — check
runner logs for an old job_id.
