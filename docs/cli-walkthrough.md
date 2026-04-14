# CLI Walkthrough — Every Command, Real Output

All output captured from a clean `pip install rewind-agent` on Python 3.14, macOS (Apple Silicon). For the command reference table, see [cli-reference.md](cli-reference.md).

---

## 1. Install and Seed Demo Data

```
$ pip install rewind-agent
Successfully installed rewind-agent-0.14.0
```

```
$ rewind demo
⏪ Rewind — Seeding Demo Data

  ├ Created main timeline (5 steps, fails at step 5 — hallucination)
  ├ Created fork at step 4 with corrected tool response
  └ Fork step 5 produces accurate answer
  ✓ Demo session created!

  Try these commands:
    rewind sessions — list all sessions
    rewind show latest — see the trace
    rewind inspect latest — interactive TUI
    rewind web — web dashboard
    rewind assert check latest --against demo-baseline — check for regressions
```

The demo seeds a 5-step research agent that fails at step 5 (hallucination), plus a forked timeline with the corrected output. No API keys needed.

---

## 2. List Sessions

```
$ rewind sessions
⏪ Rewind Sessions

  STATUS      ID              NAME          STEPS     TOKENS CREATED
  ────────────────────────────────────────────────────────────────────────
  ✗  77be29d1-d3c  research-agent-demo      5       1231 just now

  Run rewind inspect <session-id> to inspect a session.
  Run rewind web to open the web dashboard.
```

Status indicators: `✓` = completed, `✗` = failed, `●` = recording/in-progress.

---

## 3. Show Session Trace

```
$ rewind show latest
⏪ Rewind — Session Trace

  Session: research-agent-demo
  ID: 77be29d1-d3cd-4b2e-8beb-260dc41fe124
  Steps: 5
  Tokens: 1231
  Agents: supervisor, researcher, writer

  ▼ ✗ 🤖 supervisor (agent)  2743ms
    ▼ ✓ 🤖 researcher (agent)  1293ms
      ├ ✓ 🧠  gpt-4o  320ms  156↓ 28↑
      ├ ✓ 🧠  gpt-4o  890ms  312↓ 35↑
      ▼ ✓ 🔧 web_search (tool)  45ms
        └ ✓ 📋  tool  45ms
      ▼ ✓ 🔧 web_search (tool)  38ms
        └ ✓ 📋  tool  38ms
    ▼ ✗ 🤖 writer (agent)  1450ms  Hallucination — used stale 2019 projection as current fact
      └ ✗ 🧠  gpt-4o  1450ms  520↓ 180↑
      │   ERROR: HALLUCINATION: Agent used stale 2019 projection (14.2M) as current fact,
      │   ignored COVID-19 dip to 13.96M, and claimed 'no significant disruptions' despite
      │   search result explicitly noting COVID impacts.

  ⏪ Replay Savings
    Steps: 4/5 cached (served from fork cache)
    Tokens saved: 531
    Cost saved: $0.00
    Time saved: 1.2s
```

Shows the full span tree: agents, LLM calls (with token counts `↓`=input `↑`=output), tool calls, and errors. The "Replay Savings" section appears when a fork exists.

---

## 4. Fork a Timeline

```
$ rewind fork latest --at 3
⏪ Rewind — Fork Created

  Fork ID: 0cdfb313-a93b-455e-935c-c8ae5d6be479
  Label: fork
  Forked at: Step 3
  → Steps 1-3 are shared with the parent timeline.

  To diff: rewind diff 77be29d1 a4c0536a 0cdfb313
```

Creates a new timeline branch at the specified step. Steps before the fork point are shared with the parent (zero duplication). You can then modify the agent code and replay from the fork.

---

## 5. Replay from a Fork Point

`replay` creates its own fork internally — this is a separate operation from the manual fork in section 4.

```
$ rewind replay latest --from 4
⏪ Rewind — Fork & Execute Replay

  Session: research-agent-demo
  Fork at: Step 4
  Cached: Steps 1-4 (0ms, 0 tokens)
  Live: Steps 5+ (forwarded to upstream)
  Fork ID: 29183b03-658
  Proxy: http://127.0.0.1:8443
  Upstream: https://api.openai.com

  → Point your agent at this proxy:
    export OPENAI_BASE_URL=http://127.0.0.1:8443/v1

  Ctrl+C to stop. Then diff with:
    rewind diff 77be29d1 a4c0536a 29183b03
```

Starts a local proxy that serves cached responses for steps 1-4 (zero tokens, zero latency) and forwards steps 5+ to the real API. Point your agent's base URL at the proxy, re-run it, and only the broken steps cost tokens. Press Ctrl+C when done, then diff the timelines.

---

## 6. Diff Timelines

```
$ rewind diff latest main fixed
⏪ Rewind — Timeline Diff

  main vs fixed (diverge at step 5)

  ═ Step  1 identical
  ═ Step  2 identical
  ═ Step  3 identical
  ═ Step  4 identical
  ≠ Step  5 [error] 700tok → [success] 715tok
```

The first argument is the session (`latest`), and `main`/`fixed` are timeline labels within that session. Shows where timelines diverge. Steps 1-4 are identical (cached), step 5 changed from error to success.

---

## 7. Inspect Interactively (TUI)

```
$ rewind inspect latest
```

Opens a full-screen terminal UI for navigating the session. Requires a real terminal (not a subshell or CI). Supports keyboard navigation through steps, expanding/collapsing spans, and viewing full request/response content.

---

## 8. Assertion Baselines

### List Baselines

```
$ rewind assert list
⏪ Rewind Baselines

                  NAME       SOURCE  STEPS     TOKENS CREATED
  ─────────────────────────────────────────────────────────────────
         demo-baseline dcc0642b-930      5       1231 1d ago
```

### Show Baseline Detail

```
$ rewind assert show demo-baseline
⏪ Rewind — Baseline Detail

  Name: demo-baseline
  ID: ab449024-77eb-49b6-9cbd-fcfda3d737cb
  Source Session: dcc0642b-930a-4b90-bce0-a6f1171d783e
  Steps: 5
  Tokens: 1231
  Description: Demo baseline for regression testing

  Expected Steps:
    Step  1  🧠     gpt-4o  156↓ 28↑ → web_search
    Step  2  📋       tool  0↓ 0↑
    Step  3  🧠     gpt-4o  312↓ 35↑ → web_search
    Step  4  📋       tool  0↓ 0↑
    Step  5  🧠     gpt-4o  520↓ 180↑ ERROR
```

### Check Session Against Baseline

```
$ rewind assert check latest --against demo-baseline
⏪ Rewind — Assertion Check

  Baseline: demo-baseline (5 steps)
  Session: 77be29d1-d3c (research-agent-demo)
  Tolerance: tokens ±20%, model changes = fail

  ┌ Step  1  🧠 LLM Call  ✓ PASS  match
  ├ Step  2  📋 Tool Result  ✓ PASS  match
  ├ Step  3  🧠 LLM Call  ✓ PASS  match
  ├ Step  4  📋 Tool Result  ✓ PASS  match
  └ Step  5  🧠 LLM Call  ✓ PASS  match

  Result: PASSED (37 passed, 0 warnings)
```

Creates a regression test: checks that the session's step structure, models, and token counts match the baseline within tolerance.

### Create a New Baseline

```
$ rewind assert baseline latest --name my-baseline
```

Captures the current session as a new regression baseline.

---

## 9. Snapshots (Workspace Checkpoints)

### Create a Snapshot

```
$ rewind snapshot ./my-project --label before-refactor
⏪ Rewind — Creating Snapshot

  Directory: /path/to/my-project
  Label: before-refactor
  Files: 1
  Size: 127B
  ID: f918cfa1-fa5

  ✓ Snapshot saved!
  Restore with: rewind restore f918cfa1
```

### List Snapshots

```
$ rewind snapshots
⏪ Rewind Snapshots

            ID           LABEL  FILES       SIZE CREATED
  ─────────────────────────────────────────────────────────────────
  f918cfa1-fa5  before-refactor      1       127B just now
  75f13897-a9c   adv-snap-test      1       131B 1d ago
```

### Restore a Snapshot

```
$ rewind restore before-refactor
⏪ Rewind — Restoring Snapshot

  Label: before-refactor
  Directory: /path/to/my-project
  Files: 1

  ✓ Restored to /path/to/my-project
```

---

## 10. Instant Replay Cache

```
$ rewind cache
⏪ Rewind — Instant Replay Cache

  ○ Cache is empty.
  Run rewind record --replay to enable.
```

When populated (via `rewind record --replay`), shows cache hit statistics, stored models, and total tokens cached.

---

## 11. Evaluation System

### Create a Dataset

```
$ rewind eval dataset create demo-dataset
⏪ Rewind — Dataset Created

  Name: demo-dataset
  Version: 1

  Add examples: rewind eval dataset import demo-dataset examples.jsonl
```

### Show Dataset

```
$ rewind eval dataset show demo-dataset
⏪ Rewind — Dataset Detail

  Name: demo-dataset
  Version: v1
  Examples: 0

  (no examples)
```

### Import Examples

```
$ rewind eval dataset import demo-dataset examples.jsonl
```

JSONL format: each line is `{"input": "...", "expected": "..."}`.

### Create an Evaluator

```
$ rewind eval evaluator create exact -t exact_match
⏪ Rewind — Evaluator Created

  Name: exact
  Type: exact_match
```

Evaluator types: `exact_match`, `contains`, `regex`, `json_schema`, `tool_use_match`, `llm_judge`, `custom`.

### Run an Experiment

```
$ rewind eval run demo-dataset -c "python agent.py" -e exact
```

Runs the command once per example in the dataset, captures the session, scores it with the evaluator, and aggregates results.

### List Experiments

```
$ rewind eval experiments
No experiments yet.
  Run one: rewind eval run <dataset> -c <command> -e <evaluator>
```

### Compare Experiments

```
$ rewind eval compare experiment-1 experiment-2
```

Side-by-side comparison of two experiment runs (pass rate, scores, regressions).

### Score a Session

```
$ rewind eval score latest -e correctness
```

Scores a session's timeline with the named evaluator. Supports `--compare-timelines` to score original vs forked.

---

## 12. AI-Powered Fix

### Diagnose

```
$ rewind fix latest
```

Uses an LLM to analyze the failure, identify root cause, and suggest a fix (prompt change, model swap, temperature adjustment, retry strategy). Requires `OPENAI_API_KEY` or `ANTHROPIC_API_KEY`.

### Diagnose + Apply

```
$ rewind fix latest --apply
```

Applies the suggested fix: forks the session, starts a replay proxy with rewrites, waits for the agent to re-run.

### Fully Automated

```
$ rewind fix latest --apply -c "python agent.py"
```

One command: diagnose failure, fork, start proxy, re-run agent, score both timelines, report savings.

### Test a Specific Hypothesis

```
$ rewind fix latest --hypothesis "swap_model:gpt-4o"
```

Skips AI diagnosis. Tests a specific fix directly.

---

## 13. Share

```
$ rewind share latest -o debug-session.html
⏪ Exporting session research-agent-demo...

✓ Shared session saved to: debug-session.html
   → Open in any browser. Share via Slack, email, or any file-sharing tool.
   → Contains: metadata only (no LLM content) (16KB)
```

With full content:

```
$ rewind share latest --include-content --yes -o debug-session.html
⏪ Exporting session research-agent-demo...

✓ Shared session saved to: debug-session.html
   → Open in any browser. Share via Slack, email, or any file-sharing tool.
   → Contains: metadata + full content (25KB)
```

Generates a self-contained HTML file. No install needed to view. Drop in Slack, attach to a PR, or email.

---

## 14. Import / Export

### Import from Langfuse

```
$ export LANGFUSE_PUBLIC_KEY=pk-lf-...
$ export LANGFUSE_SECRET_KEY=sk-lf-...
$ rewind import from-langfuse --trace abc123
```

Fetches the trace via Langfuse REST API and creates a browsable session.

### Import OpenTelemetry Traces

```
$ rewind import otel --json-file traces.json
```

Imports OTLP traces from JSON or protobuf files (Datadog, Grafana Tempo, Jaeger, etc.).

### Export as OpenTelemetry

```
$ rewind export otel latest --endpoint https://cloud.langfuse.com/api/public/otel
```

Exports a session as OTel traces via OTLP. Works with any OTel-compatible backend.

---

## 15. SQL Query

### Show Schema

```
$ rewind query --tables
⏪ Rewind — Database Tables

  sessions
    id TEXT PK
    name TEXT
    created_at TEXT
    updated_at TEXT
    status TEXT
    total_steps INTEGER
    total_tokens INTEGER
    metadata TEXT
    thread_id TEXT
    thread_ordinal INTEGER
    source TEXT

  steps
    id TEXT PK
    timeline_id TEXT
    session_id TEXT
    step_number INTEGER
    step_type TEXT
    status TEXT
    ...

  timelines
    id TEXT PK
    session_id TEXT
    parent_timeline_id TEXT
    fork_at_step INTEGER
    created_at TEXT
    label TEXT

  (+ baselines, datasets, evaluators, experiments, replay_cache, snapshots, spans, ...)
```

### Run Queries

```
$ rewind query "SELECT name, total_steps, total_tokens, status FROM sessions ORDER BY created_at DESC LIMIT 3"
  name                    total_steps  total_tokens  status
  ────────────────────────────────────────────────────────────
  research-agent-demo     5            1231          failed
  claude-code (38a9afa6)  29           0             recording

  2 row(s)
```

Read-only SQL against the Rewind SQLite database. Full access to all tables.

---

## 16. Recording

### Proxy Mode

```
$ rewind record
```

Starts a recording proxy. Point your agent's base URL at the proxy. Works with any language.

### Proxy + Instant Replay

```
$ rewind record --replay
```

Records and caches responses. Subsequent runs with identical requests get instant cached responses.

### Proxy + Live Dashboard

```
$ rewind record --web
```

Records with a live web dashboard showing steps as they arrive.

### Python SDK (Programmatic)

```python
import rewind_agent
rewind_agent.init()  # one line — monkey-patches OpenAI/Anthropic clients
```

---

## 17. Web Dashboard

```
$ rewind web
```

Starts a browser-based dashboard at `http://127.0.0.1:4800`. Shows all sessions, step traces, timelines, diffs, and replay savings. Optional `--port` flag.

---

## 18. Hooks (Claude Code Integration)

```
$ rewind hooks install
⏪ Rewind — Installing Claude Code Hooks

  ✓ Hook script written to ~/.claude/rewind-hook.sh
  ✓ Claude Code settings updated at ~/.claude/settings.json
  ✓ 5 hook event types configured

  → Start the server with rewind web to begin observing Claude Code sessions.
```

Writes a hook script and registers it in Claude Code's `settings.json`. Every Claude Code session gets recorded as a Rewind session. Idempotent — safe to re-run.

---

## Command Quick Reference

| Task | Command |
|------|---------|
| First-time setup | `pip install rewind-agent && rewind demo` |
| See what happened | `rewind show latest` |
| Explore interactively | `rewind inspect latest` |
| Fork at a step | `rewind fork latest --at 3` |
| Replay from fork | `rewind replay latest --from 4` |
| Compare timelines | `rewind diff latest main fixed` |
| Regression test | `rewind assert check latest --against demo-baseline` |
| Save workspace state | `rewind snapshot . --label before-change` |
| Undo changes | `rewind restore before-change` |
| AI-powered fix | `rewind fix latest --apply -c "python agent.py"` |
| Share with team | `rewind share latest -o debug.html` |
| Web dashboard | `rewind web` |
| Query raw data | `rewind query "SELECT * FROM sessions LIMIT 10"` |
