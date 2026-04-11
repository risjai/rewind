# RFC: Claude Code Hooks Integration for Rewind

**Author:** Rishabh Jain
**Date:** 2026-04-11
**Status:** Draft v2 — Revised after technical review
**Reviewers:** Technical review (2026-04-11) — Approved with revisions

---

## 1. Motivation

### The Gap

Rewind records AI agent sessions via two modes:
- **Direct mode** — monkey-patches Python SDK clients (OpenAI, Anthropic)
- **Proxy mode** — HTTP proxy intercepts LLM API traffic

Neither mode can observe **Claude Code sessions**. Claude Code is a closed-source TypeScript CLI — we can't monkey-patch it, and routing its traffic through our proxy is clunky (environment variable overrides, misses tool-level semantics).

Meanwhile, **Claude Code has a hooks system** that fires structured JSON events on every tool call, agent spawn, session lifecycle event, etc. The open-source project [agents-observe](https://github.com/simple10/agents-observe) already uses this to build a real-time dashboard.

### Why This Matters

- **#community-claude-code** (internal Slack) is one of the largest concentrations of agent power users. Carson Kahn's `agents-observe` post got immediate engagement. Lu Han tested it and explicitly asked for: trace inspection with I/O, session management, grouping tool usage traces.
- Rewind already has everything agents-observe doesn't (fork, replay, diff, assertions, evals) — but can't capture Claude Code sessions at all.
- This is the fastest path to making Rewind relevant to the largest audience of people building with AI agents today.

### What agents-observe Does (Reference Implementation)

| Layer | Implementation |
|---|---|
| **Capture** | Claude Code hooks → bash script reads stdin JSON → backgrounds HTTP POST |
| **Store** | Node.js/Hono server → SQLite |
| **Stream** | WebSocket broadcast to connected clients |
| **Display** | React 19 dashboard with live agent/tool event stream |

Architecture: `Hook event (stdin JSON) → hook.sh (background &) → HTTP POST → SQLite → WebSocket → React UI`

**What it lacks:** No fork/replay, no diff, no assertions, no evals, no historical search. Pure real-time observation.

---

## 2. Claude Code Hooks — Technical Reference

### 2.1 Event Types

Claude Code fires 30+ hook event types. The ones relevant to Rewind:

**Session Lifecycle:**
| Event | Fires When | Key Fields |
|---|---|---|
| `SessionStart` | Session begins | `transcript_path`, `conversation_id`, `model` |
| `SessionEnd` | Session terminates | `conversation_id` |

**Tool Execution (highest value):**
| Event | Fires When | Key Fields |
|---|---|---|
| `PreToolUse` | Before tool invoked | `tool`, `server`, `input`, `conversation_id` |
| `PostToolUse` | After tool succeeds | `tool`, `output`, `status`, `conversation_id` |
| `PostToolUseFailure` | After tool fails | `tool`, `error`, `conversation_id` |

**Multi-Agent:**
| Event | Fires When | Key Fields |
|---|---|---|
| `SubagentStart` | Subagent spawned | `agent_name`, `agent_type`, `parent_agent_id` |
| `SubagentStop` | Subagent completes | `agent_name`, `conversation_id` |

**User Interaction:**
| Event | Fires When | Key Fields |
|---|---|---|
| `UserPromptSubmit` | User sends prompt | `prompt`, `conversation_id` |
| `Stop` | Session stopped | `conversation_id` |

**Lower priority (capture but don't model specially):**
`PreCompact`, `PostCompact`, `FileChanged`, `CwdChanged`, `WorktreeCreate`, `WorktreeRemove`, `Notification`, `TaskCreated`, `TaskCompleted`

### 2.2 Hook Event Delivery

- Events arrive as **JSON on stdin** to hook scripts
- Hook scripts must exit quickly — **15 second timeout**, then killed
- Best practice: read stdin, background the actual work via `&`, exit 0 immediately
- Exit code 0 = success, exit code 2 = blocking error (Claude Code stops/rolls back)

### 2.3 Hook Configuration

Hooks are configured in `.claude/settings.json`. Claude Code supports two formats:

**Standard format** (verified against Claude Code documentation):
```json
{
  "hooks": {
    "PreToolUse": [
      {
        "type": "command",
        "command": "bash ~/.rewind/hooks/claude-code-hook.sh"
      }
    ],
    "PostToolUse": [
      {
        "type": "command",
        "command": "bash ~/.rewind/hooks/claude-code-hook.sh"
      }
    ]
  }
}
```

**Plugin format** (used by Claude Code plugins — has an additional nesting level):
```json
{
  "hooks": {
    "PreToolUse": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "bash ${CLAUDE_PLUGIN_ROOT}/hooks/scripts/hook.sh"
          }
        ]
      }
    ]
  }
}
```

> **Note:** The standalone CLI (`rewind hooks install`) will use the standard format. The plugin packaging (Phase 5) will use the plugin format. The hook script is identical in both cases — only the settings.json structure differs.

### 2.4 Limitations

- **No LLM request/response content** — hooks fire on tool use, not raw API calls. We won't get the actual prompt/completion text or token counts from hooks alone.
- **No cost data** — token counts and model costs are not exposed in hook events.
- **Agent hierarchy is inferred** — parent/child relationships must be tracked from `SubagentStart`/`SubagentStop` event ordering, not from explicit fields.
- **One-way only** — hooks can't influence Claude Code's behavior (except exit code 2 to block).

---

## 3. Rewind's Existing Data Model (Quick Reference)

```
Thread (multi-turn conversation)
  └── Session (one turn / one recording)
       └── Timeline (main + forks)
            ├── Step (LlmCall | ToolCall | ToolResult)
            │     ├── request_blob  (SHA-256 → blob store)
            │     └── response_blob (SHA-256 → blob store)
            └── Span (Agent | Tool | Handoff | Custom)
                  └── child Spans (hierarchical nesting)
```

**Key files:**
- Schema: `crates/rewind-store/src/db.rs` (lines 42-272)
- Models: `crates/rewind-store/src/models.rs`
- Blobs: `crates/rewind-store/src/blobs.rs`
- Proxy ingestion: `crates/rewind-proxy/src/lib.rs`
- WebSocket: `crates/rewind-web/src/ws.rs`
- Polling: `crates/rewind-web/src/polling.rs` (300ms interval)

---

## 4. Proposed Design

### 4.1 Architecture Overview

```
Claude Code
  │
  │ (fires hook events on stdin)
  ▼
~/.rewind/hooks/claude-code-hook.sh
  │
  │ (reads stdin, wraps in envelope, backgrounds HTTP POST, exits 0)
  │ (on curl failure: appends to ~/.rewind/hooks/buffer.jsonl)
  ▼
POST http://127.0.0.1:{port}/api/hooks/event
  │
  │ (Rewind web server — same server as `rewind web`)
  ▼
Hook Ingestion Layer (new module in rewind-web)
  │
  ├── Deduplicates by event payload hash
  ├── Orders events per session via timestamp
  ├── Maps event → Session / Step / Span
  ├── Stores payloads in blob store
  ├── Writes to SQLite
  └── Emits StoreEvent on broadcast channel
        │
        ▼
  WebSocket → Web UI (live updates)
  +
  rewind show / inspect / diff / assert / eval (post-hoc analysis)
```

**Port:** The hook ingestion endpoint is served on the **same server and port** as `rewind web` (default 4800). No separate process. The hook script and `rewind web` share the port via `REWIND_PORT` environment variable.

### 4.1.1 Event Envelope Format

The hook script wraps the raw Claude Code event in a source-agnostic envelope before POSTing. This makes the endpoint extensible to other hook sources (Cursor, Windsurf, custom agents) in the future.

```json
{
  "source": "claude-code",
  "event_type": "PreToolUse",
  "timestamp": "2026-04-11T10:30:00.123Z",
  "payload": {
    "tool": "Read",
    "input": { "file_path": "/src/main.rs" },
    "conversation_id": "abc-123",
    ...
  }
}
```

The ingestion layer dispatches on `source` + `event_type`. Today only `"claude-code"` is supported; the envelope makes adding new sources a config change, not a code change.

A batch endpoint (`POST /api/hooks/events` accepting an array) will be added alongside the single-event endpoint for future use (e.g., buffer drain on server startup).

### 4.2 Event → Data Model Mapping

| Hook Event | Rewind Action |
|---|---|
| `SessionStart` | Create `Session` (status=Recording) + root `Timeline` + root `Span` (type=Agent, name="Claude Code") |
| `SessionEnd` / `Stop` | Update `Session` status → Completed, close root span |
| `PreToolUse` | Create `Step` (type=ToolCall, status=Pending). Store tool name + input as `request_blob`. Link to current agent's span via `span_id`. |
| `PostToolUse` | Update matching `Step`: status → Success, store output as `response_blob`, set `duration_ms`. |
| `PostToolUseFailure` | Update matching `Step`: status → Error, store error in `error` field + `response_blob`. |
| `SubagentStart` | Create child `Span` (type=Agent, name=agent_name, parent=current agent span). Push onto agent stack. |
| `SubagentStop` | Close `Span` (set ended_at, duration_ms). Pop from agent stack. |
| `UserPromptSubmit` | Create `Step` (type=UserPrompt [new], status=Success). Store prompt text as `request_blob`. |
| Other events | Create `Step` (type=HookEvent [new], status=Success). Store full payload as `request_blob`. Useful for timeline completeness. |

### 4.3 Step Types and Session Source

**Step types** — keep the existing `StepType` enum. Use generic `ToolCall` for all Claude Code tool events, with the tool name stored in metadata (not as distinct step types). This avoids coupling Rewind's data model to Claude Code's tool set, which will change over time.

```rust
pub enum StepType {
    LlmCall,      // Existing — raw LLM API call
    ToolCall,     // Existing — tool invocation (used for all hook tool events)
    ToolResult,   // Existing — tool result
    UserPrompt,   // NEW — user prompt submission
    HookEvent,    // NEW — catch-all for other hook events (FileChanged, CwdChanged, etc.)
}
```

The specific tool name (`Read`, `Edit`, `Bash`, `Agent`, `Grep`, etc.) is stored in `Step.metadata` as `{"tool_name": "Read", "file_path": "/src/main.rs"}`. The UI renders tool-specific icons and formatting based on this field.

**Session source** — add a `source` field to `Session`:

```rust
pub enum SessionSource {
    Proxy,       // HTTP proxy recording
    Direct,      // Python SDK monkey-patching
    Hooks,       // Claude Code hooks (or other hook sources)
}
```

```sql
ALTER TABLE sessions ADD COLUMN source TEXT NOT NULL DEFAULT 'proxy';
```

This lets the UI and assertion logic adapt behavior per source:

| Feature | Proxy / Direct sessions | Hook sessions |
|---|---|---|
| Token counts per step | Yes | No (always 0) |
| Model per step | Yes | Session-level only |
| Instant Replay cache | Yes | N/A |
| Fork & Replay (LLM level) | Yes | Not yet (needs transcript parsing) |
| Tool call I/O inspection | Yes | Yes |
| Assertion baselines | Full (tokens, model, steps) | Partial (steps, tool names, status) |
| Span tree / agent hierarchy | Yes | Yes |
| Live observation | Yes (via `rewind record`) | Yes (via hooks) |

### 4.4 Session Correlation and Concurrency Model

**Problem:** Multiple hook events belong to the same Claude Code session. We need to group them. Because the hook script backgrounds `curl`, events arrive as concurrent HTTP requests — event B may arrive before event A even though A happened first.

**Solution:** Use `conversation_id` from hook events as the session key.
- On first event with a new `conversation_id`: create Session (source=Hooks) + Timeline + root Span.
- On subsequent events with same `conversation_id`: look up existing Session, append Steps.
- On `SessionEnd`: mark Session completed.
- If the first event for a `conversation_id` is NOT `SessionStart` (e.g., server started mid-session): create Session anyway, mark as `partial` in metadata.

**Concurrency model:**

```rust
use dashmap::DashMap;

/// Global state shared across all Axum request handlers.
/// DashMap provides per-key locking — concurrent events for different
/// sessions don't block each other.
struct HookIngestionState {
    sessions: DashMap<String, HookSessionState>,  // conversation_id → state
}

struct HookSessionState {
    session_id: String,
    timeline_id: String,
    root_span_id: String,
    agent_span_stack: Vec<String>,
    step_counter: u32,
    pending_steps: HashMap<String, String>,  // invocation_key → step_id
}
```

**Event ordering:** Each event in the envelope carries a `timestamp` (ISO 8601, from the hook script using `date -u`). The ingestion layer uses this for:
1. Setting `Step.created_at` accurately (not arrival time).
2. Ordering Pre→Post matching — if `PostToolUse` arrives before its `PreToolUse` (rare but possible), buffer the Post and process it when the Pre arrives or after a short timeout (500ms).
3. `duration_ms` on Steps = `PostToolUse.timestamp - PreToolUse.timestamp`.

**DashMap semantics:** Each `conversation_id` entry is locked independently. Two events for the same session serialize through the DashMap entry lock. Two events for different sessions proceed in parallel. This matches the expected concurrency pattern — high parallelism across sessions, low parallelism within a session.

### 4.5 Pre/Post Tool Use Matching

`PreToolUse` and `PostToolUse` are separate events for the same tool invocation. Claude Code can fire parallel tool calls (e.g., multiple `Read` calls in one turn), so matching must handle concurrency.

**Phase 0 prerequisite:** Before implementation, inspect the actual Claude Code hook event payload for a unique invocation ID (e.g., `tool_use_id`, `invocation_id`, or a content-block ID). This determines the matching strategy:

**Strategy A — Unique invocation ID exists (preferred):**
1. On `PreToolUse`: create Step with status=Pending, store in `pending_steps[invocation_id] = step_id`.
2. On `PostToolUse`: look up `pending_steps[invocation_id]`, update Step with output and status, remove from pending.

**Strategy B — No unique ID (fallback):**
1. Use `{conversation_id}:{tool}:{sha256(input_json)}` as composite key.
2. Document collision risk: two identical tool calls with identical inputs in the same session will collide. In practice this is rare (e.g., reading the same file twice in the same turn), and the worst case is a merged step, not data loss.

**Edge cases for both strategies:**
3. If `PostToolUse` arrives with no matching `PreToolUse` (server started mid-invocation, or out-of-order delivery): create a complete Step directly from `PostToolUse` data. Mark `metadata.partial = true`.
4. If `PreToolUse` has no matching `PostToolUse` after 30 seconds: mark Step status as `Error` with `error = "No PostToolUse received (timeout)"`. This handles cases where Claude Code crashes or the hook fires but the tool never completes.
5. Stale pending steps are garbage-collected on `SessionEnd`.

### 4.6 Hook Script

```bash
#!/bin/bash
# ~/.rewind/hooks/claude-code-hook.sh
# Reads Claude Code hook event from stdin, wraps in envelope, POSTs to Rewind.
# On failure: buffers to local JSONL file for later drain.
# Exits immediately (backgrounds all work) to avoid blocking Claude Code.

REWIND_PORT="${REWIND_PORT:-4800}"
REWIND_BUFFER="${REWIND_BUFFER:-$HOME/.rewind/hooks/buffer.jsonl}"
EVENT_TYPE="${CLAUDE_HOOK_EVENT_NAME:-unknown}"

input=$(cat)
timestamp=$(date -u +"%Y-%m-%dT%H:%M:%S.%3NZ" 2>/dev/null || date -u +"%Y-%m-%dT%H:%M:%SZ")

# Wrap in envelope
envelope=$(printf '{"source":"claude-code","event_type":"%s","timestamp":"%s","payload":%s}' \
  "$EVENT_TYPE" "$timestamp" "$input")

# Background: POST to Rewind server, buffer on failure
(
  if ! curl -sf -X POST "http://127.0.0.1:${REWIND_PORT}/api/hooks/event" \
    -H "Content-Type: application/json" \
    -d "$envelope" > /dev/null 2>&1; then
    # Server unreachable — append to buffer file for later drain
    mkdir -p "$(dirname "$REWIND_BUFFER")"
    echo "$envelope" >> "$REWIND_BUFFER"
  fi
) &

exit 0
```

**Design notes:**
- `CLAUDE_HOOK_EVENT_NAME` is set by Claude Code as an environment variable when invoking hooks. This populates the envelope's `event_type` without parsing the payload.
- On curl failure (server not running), events are appended to `~/.rewind/hooks/buffer.jsonl`. The `rewind web` server drains this file on startup via `POST /api/hooks/events` (batch endpoint).
- The entire POST + fallback runs in background (`&`). The script exits in <5ms.
- `date -u` provides the timestamp. The `-u` flag ensures UTC. macOS `date` doesn't support `%3N` (milliseconds) — the fallback format omits them.

### 4.7 CLI Commands

```
rewind hooks install     # Configure Claude Code hooks → writes to .claude/settings.json
rewind hooks uninstall   # Remove hooks from .claude/settings.json
rewind hooks status      # Check if hooks are configured AND server is reachable
```

`rewind hooks install` would:
1. Write the hook script to `~/.rewind/hooks/claude-code-hook.sh`
2. Read `.claude/settings.json` (create if missing)
3. Add hook entries for the ~10 high-value events: `SessionStart`, `SessionEnd`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `SubagentStart`, `SubagentStop`, `UserPromptSubmit`, `Stop`
4. Print confirmation: *"Hooks installed. Start the server with `rewind web` to begin observing Claude Code sessions."*

`rewind hooks status` would:
1. Check `.claude/settings.json` for hook configuration → report installed/not installed
2. Check if Rewind server is reachable at the configured port → report running/not running
3. Check for buffered events in `~/.rewind/hooks/buffer.jsonl` → report count if any
4. **Warn loudly** if hooks are installed but server is not running

`rewind hooks install --verbose` (future): register for all 30+ event types instead of just the core 10.

---

## 5. Implementation Phases

### Phase 0: Payload Discovery (Before Any Code)

**Goal:** Inspect actual Claude Code hook event payloads to determine the Pre/Post matching strategy.

**Tasks:**
1. Write a minimal hook script that dumps raw stdin to a file for each event type
2. Run a Claude Code session with the dump hooks installed
3. Inspect payloads for: unique invocation ID fields, `conversation_id` format, parallel tool call behavior
4. Confirm the hook configuration schema (standard format from Section 2.3)
5. Document actual payload schemas for each event type

**Decision gate:** Choose Strategy A or B for Pre/Post matching (Section 4.5) based on findings.

### Phase 1: Hook Ingestion Endpoint (MVP)

**Goal:** Accept hook events via HTTP and store them in Rewind's database.

**Changes:**
- `crates/rewind-store/src/models.rs` — Add `UserPrompt`, `HookEvent` to `StepType`; add `SessionSource` enum; add `source` column to sessions
- `crates/rewind-store/src/db.rs` — Handle new types, migration for `source` column
- `crates/rewind-web/src/api.rs` — New `POST /api/hooks/event` + `POST /api/hooks/events` (batch) routes
- `crates/rewind-web/src/hooks.rs` — New module: envelope parsing, `DashMap`-based session correlation, step creation, Pre/Post matching, event deduplication (payload hash)
- `Cargo.toml` — Add `dashmap` dependency

**Verification:**
- `curl POST /api/hooks/event` with a synthetic envelope → step appears in `rewind sessions` / `rewind show`
- `rewind show latest` displays `source: hooks` on the session
- Web UI live dashboard shows the step via WebSocket
- Concurrent curl calls for the same session don't corrupt state
- Duplicate event (same payload) is deduplicated

### Phase 2: Hook Script + CLI + Buffer Drain

**Goal:** One-command setup: `rewind hooks install` and you're observing Claude Code. Events are buffered when server is down.

**Changes:**
- `crates/rewind-cli/src/main.rs` — Add `hooks` subcommand with `install`/`uninstall`/`status`
- `assets/claude-code-hook.sh` — The hook script with envelope wrapping and buffer-on-failure
- Install logic that reads/writes `.claude/settings.json`
- `crates/rewind-web/src/lib.rs` — On server startup, drain `~/.rewind/hooks/buffer.jsonl` via batch endpoint

**Verification:**
- `rewind hooks install` → `.claude/settings.json` has hooks configured
- `rewind hooks status` with server stopped → warns "server not running"
- Start Claude Code with server stopped → events buffered to `buffer.jsonl`
- Start `rewind web` → buffered events drained, session appears in `rewind sessions`
- Start Claude Code with server running → `rewind show latest` shows live session
- `rewind hooks uninstall` → hooks removed cleanly

### Phase 3: Agent Hierarchy (Spans)

**Goal:** SubagentStart/Stop events create proper span tree for multi-agent visualization.

**Changes:**
- `crates/rewind-web/src/hooks.rs` — Agent span stack management, span creation/closing
- Link Steps to their parent agent Span via `span_id`

**Verification:**
- Run Claude Code with subagents → `rewind show latest` shows hierarchical span tree
- MCP `get_span_tree` tool returns proper hierarchy

### Phase 4: Web UI Polish

**Goal:** Claude Code sessions look great in the dashboard.

**Changes:**
- `web/` — Show tool names prominently (Read, Edit, Bash, Agent, etc.)
- Display file paths, commands, agent names from hook metadata
- "Claude Code" badge on hook-originated sessions
- Agent hierarchy sidebar during live observation

**Verification:**
- Visual review of live Claude Code session in `rewind web`

### Phase 5: Claude Code Plugin (Stretch)

**Goal:** `claude plugin install rewind` — zero-config setup.

**Changes:**
- New `.claude-plugin/manifest.json` with hook registrations
- Skill commands: `/rewind`, `/rewind status`, `/rewind web`
- Auto-start server on plugin load

---

## 6. What Hooks DON'T Give Us (And How to Compensate)

| Missing from hooks | Impact | Possible workaround |
|---|---|---|
| Raw LLM prompt/completion text | Can't see what Claude "saw" or "said" | Parse Claude Code's transcript file (JSONL at `~/.claude/projects/.../transcript.jsonl`) — this contains full conversation history |
| Token counts / cost | No cost tracking for Claude Code sessions | Extract from transcript file, or estimate from payload sizes |
| Model name per step | Only session-level model info | Available in `SessionStart` event; assume consistent within session |
| Request/response latency for LLM calls | Only tool-level timing | Timestamp diff between consecutive events gives approximate timing |

**Transcript file parsing** is a potential Phase 6 enhancement. Claude Code writes a full JSONL transcript to `~/.claude/projects/{project}/sessions/{id}/transcript.jsonl`. This contains actual LLM request/response payloads, token usage, and would unlock Rewind's full power (fork, replay, diff on the actual LLM calls). However, the transcript format is undocumented and may change.

**Phase 6 approach (approved in review):**
- Pin to a known transcript format version
- Add a version check — if the format changes, log a warning and fall back to hooks-only mode
- Ship as opt-in: `rewind hooks install --with-transcript-parsing`
- This is where Rewind's full value proposition unlocks for Claude Code users — fork, replay, diff on actual LLM calls, not just tool calls

---

## 7. Competitive Positioning After This

| Capability | agents-observe | LangSmith | Rewind (after hooks) |
|---|---|---|---|
| Observe Claude Code live | Yes | No | **Yes** |
| Agent hierarchy visualization | Yes | N/A | **Yes** |
| Time-travel / fork / replay | No | No | **Yes** |
| Diff two sessions | No | No | **Yes** |
| Regression assertions | No | Partial | **Yes** |
| Evaluation system | No | Yes | **Yes** |
| Works with Python agents | No | Yes | **Yes** |
| Local-first, no cloud | Yes | No | **Yes** |
| Claude Code plugin | Yes | No | **Yes** (Phase 5) |

**Post for #community-claude-code:**
> "agents-observe shows you what's happening. Rewind shows you what happened, lets you fork from any point, diff two runs, and catch regressions — now with Claude Code hooks integration."

---

## 8. Open Questions (Resolved)

Resolved during technical review (2026-04-11):

1. **Should `rewind web` auto-start when hooks are installed?**
   **No.** `rewind hooks install` prints a clear message directing the user to run `rewind web`. `rewind hooks status` warns if the server isn't running. Consider a `--with-server` flag later.

2. **Event buffering:**
   **Yes, Phase 2.** Hook script appends to `~/.rewind/hooks/buffer.jsonl` on curl failure. Server drains the buffer on startup. See updated Section 4.6.

3. **Transcript file parsing (Phase 6):**
   **Yes, pursue cautiously.** Pin to a known format version. Add a version check — fall back to hooks-only mode if format changes. Ship as opt-in: `rewind hooks install --with-transcript-parsing`.

4. **Step type granularity:**
   **Generic `ToolCall` with metadata.** Store tool name in `Step.metadata["tool_name"]`. UI renders tool-specific icons based on this field. See updated Section 4.3.

5. **Scope of hooks to register:**
   **~10 high-value events for v1.** Add `--verbose` flag later for all 30+.

6. **Plugin vs standalone:**
   **Keep Phase 5 where it is.** `rewind hooks install` is a one-time 10-second setup. Don't block core value on plugin packaging. Accelerate if Anthropic ships a plugin marketplace.

## 8.1 Remaining Open Questions

1. **Pre/Post matching strategy:** Depends on Phase 0 payload discovery. See Section 4.5 — Strategy A (invocation ID) vs Strategy B (input hash). Must be resolved before Phase 1 implementation.

2. **Metrics/telemetry:** Should `rewind hooks status --verbose` expose ingestion metrics (`events_received`, `events_dropped`, `events_out_of_order`)? Low cost, high debug value. Leaning yes.

---

## 9. Revision History

| Version | Date | Changes |
|---|---|---|
| v1 | 2026-04-11 | Initial draft |
| v2 | 2026-04-11 | Revised after technical review. Key changes: (1) Added Phase 0 for payload discovery before implementation. (2) Explicit concurrency model with `DashMap` for session state. (3) Robust Pre/Post matching with two strategies depending on payload discovery. (4) Event envelope format for source-agnostic extensibility. (5) `source` field on Session to let UI/assertions adapt per recording mode. (6) Hook script buffers to local JSONL on server failure; server drains on startup. (7) `rewind hooks status` warns if server not running. (8) Generic `ToolCall` + metadata instead of per-tool step types. (9) Resolved all 6 original open questions. (10) Verified hook configuration schema (standard vs plugin format). |

---

## 10. References

- [agents-observe](https://github.com/simple10/agents-observe) — Reference implementation using Claude Code hooks
- Rewind store schema: `crates/rewind-store/src/db.rs:42-272`
- Rewind models: `crates/rewind-store/src/models.rs`
- Rewind WebSocket: `crates/rewind-web/src/ws.rs:34-175`
- Rewind proxy ingestion: `crates/rewind-proxy/src/lib.rs:168-547`
- Slack research: #community-claude-code, #agentforce-ai-tools-moments — Lu Han and Carson Kahn's feedback on agents-observe
