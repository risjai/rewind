# Rewind — Comprehensive Product Summary

> **Tagline**: The time-travel debugger for AI agents — "Chrome DevTools for AI agents"
>
> **GitHub**: https://github.com/agentoptics/rewind
> **PyPI**: https://pypi.org/project/rewind-agent/
> **License**: MIT

---

## What Rewind Is

Rewind is a time-travel debugger for AI agents. It records every LLM call, tool invocation, and context window during an agent session, then lets you inspect, fork, replay from any failure point, diff timelines, and evaluate output quality — without re-running (or paying for) the steps that already succeeded.

## The Problem

AI agents in production run complex tool-calling chains (10–50+ LLM steps). When they fail:

- You can't see what the model saw at each step (context window contents, truncated data, stale tool responses).
- You can't reproduce the failure — LLMs are non-deterministic, and tool responses may have changed.
- You can't isolate the failure — you must re-run all steps ($$$, minutes of waiting) just to test a theory.
- You can't measure quality over time — did your prompt change improve or regress agent behavior?

Existing observability tools (LangSmith, Langfuse, AgentOps) show **what happened** but don't let you **change the past and observe a different future**.

## Core Value Proposition

- **Record** — Transparent proxy or in-process monkey-patching. Captures full request/response/context/tokens/timing. Streaming pass-through with zero added latency.
- **Inspect** — See the exact context window at each step. Interactive TUI (`rewind inspect`) and Web UI (`rewind web`).
- **Fork & Replay** — Branch the timeline at any step, edit context, resume from there. Steps before the fork point are served from cache (0 tokens, 0ms).
- **Diff** — Compare original vs. forked timelines to see exactly where/why they diverge.
- **Instant Replay** — Request-hash-based caching. Identical requests return cached responses instantly.
- **Regression Testing** (`rewind assert`) — Create baselines from known-good sessions. Check new sessions against baselines (step types, models, tool calls, token drift, error status). CI-ready via Python API and GitHub Action.
- **Evaluation** (`rewind eval`) — Create versioned datasets of test cases, run your agent against them, score with 6 evaluator types (exact_match, contains, regex, json_schema, tool_use_match, custom), compare experiments side-by-side with regression/improvement detection. CI-ready with `--fail-below` thresholds and `--json` output.
- **Snapshots** — Workspace checkpoint/restore without git. Compressed tar+gz in content-addressed blob store.
- **MCP Server** — 21 tools for AI assistant integration (Claude Code, Cursor, Windsurf) to query recordings, diff timelines, create baselines, manage eval datasets, and browse experiment results.
- **Web UI** — Browser-based dashboard with session explorer, step timeline, context window viewer, live WebSocket recording, and evaluation dashboard (datasets, experiments, comparison).

---

## Installation & Versions

| Component | Version | Install |
|---|---|---|
| Rust CLI/core | 0.4.0 | `curl -fsSL https://raw.githubusercontent.com/agentoptics/rewind/master/install.sh \| sh` |
| Python SDK | 0.7.0 | `pip install rewind-agent` |

**Optional extras**:

```
pip install rewind-agent[openai]      # OpenAI SDK
pip install rewind-agent[anthropic]   # Anthropic SDK
pip install rewind-agent[agents]      # OpenAI Agents SDK
pip install rewind-agent[pydantic]    # Pydantic AI
pip install rewind-agent[all]         # everything
```

**Requirements**: Python >= 3.9. Zero runtime dependencies (only stdlib: `sqlite3`, `hashlib`, `json`, `threading`).

**Tech stack**: CLI in Rust (hyper, tokio, ratatui, rusqlite). SDK in Python. Storage: SQLite + content-addressed blob store (SHA-256, like git objects). Single binary: 9 MB, statically linked.

---

## Two Recording Modes

| | Direct mode (Python) | Proxy mode (any language) |
|---|---|---|
| Setup | `rewind_agent.init()` | `rewind record` in a second terminal |
| Languages | Python (OpenAI + Anthropic SDKs) | Any language |
| How | Monkey-patches SDK clients in-process | HTTP proxy intercepts LLM traffic |

---

## Python SDK API

### Core functions

```python
import rewind_agent

rewind_agent.init()                          # Start recording (direct mode)
rewind_agent.init(mode="proxy")              # Use proxy mode
rewind_agent.uninit()                        # Stop recording

with rewind_agent.session("my-agent"):       # Scoped session
    ...

with rewind_agent.replay("latest", from_step=4):  # Fork-and-execute replay
    ...
```

### Decorators & hooks

```python
@rewind_agent.step("search")                 # Named step decorator
@rewind_agent.node("planner")                # LangGraph-style node decorator
@rewind_agent.tool("calculator")             # Tool decorator

with rewind_agent.trace("analysis"):         # Trace context manager
    rewind_agent.annotate("confidence", 0.92)
```

### Framework adapters

```python
graph = rewind_agent.wrap_langgraph(compiled_graph)   # LangGraph
crew  = rewind_agent.wrap_crew(crew)                   # CrewAI
hooks = rewind_agent.openai_agents_hooks()              # OpenAI Agents SDK
hooks = rewind_agent.pydantic_ai_hooks()                # Pydantic AI
```

### Assertions (regression testing)

```python
from rewind_agent import Assertions

result = Assertions().check("booking-happy-path", "latest")
assert result.passed, f"Regression: {result.failed_checks} checks failed"
```

### Evaluation

```python
import rewind_agent

# Create dataset
ds = rewind_agent.Dataset("booking-test")
ds.add(input={"query": "Book a table"}, expected={"action": "create_booking"})

# Custom evaluator
@rewind_agent.evaluator("quality")
def quality_check(input, output, expected):
    return rewind_agent.EvalScore(
        score=1.0 if output.get("action") == expected.get("action") else 0.0,
        passed=output.get("action") == expected.get("action"),
        reasoning="Action match check"
    )

# Run experiment
result = rewind_agent.evaluate(
    dataset=ds,
    target_fn=my_agent,
    evaluators=[quality_check, "exact_match"],
    fail_below=0.8,
)

# Compare experiments
comparison = rewind_agent.compare("v1-baseline", "v2-candidate")
```

---

## CLI Commands

| Command | Description |
|---|---|
| `rewind record [--replay]` | Start recording proxy |
| `rewind sessions` | List sessions |
| `rewind show <id\|latest>` | Print step trace |
| `rewind inspect <id\|latest>` | Interactive TUI |
| `rewind replay <id> --from <step>` | Fork-and-execute replay |
| `rewind fork`, `rewind diff` | Timeline branching and comparison |
| `rewind snapshot`, `rewind restore` | Workspace checkpoint/restore |
| `rewind assert baseline/check/list/show/delete` | Regression testing |
| `rewind eval dataset create/import/export/show/list/delete` | Evaluation datasets |
| `rewind eval evaluator create/list/delete` | Evaluator management |
| `rewind eval run <dataset> -c <cmd> -e <evaluator>` | Run experiment |
| `rewind eval compare <left> <right>` | Compare experiments |
| `rewind eval experiments`, `rewind eval show` | List/inspect experiments |
| `rewind query "SQL"` / `rewind query --tables` | SQL query explorer |
| `rewind web` | Browser-based dashboard |
| `rewind demo` | Seed demo data |

---

## Languages & LLM Providers

**Languages**:
- **Python** — First-class SDK with direct recording mode
- **Any language** — Via proxy mode (HTTP proxy intercepts LLM traffic)

**LLM providers**:
- OpenAI (GPT-4o, o1, etc.) — streaming + non-streaming
- Anthropic (Claude) — streaming + non-streaming
- AWS Bedrock (via gateway) — non-streaming
- Any OpenAI-compatible API (Ollama, vLLM, LiteLLM) — streaming + non-streaming

---

## Agent Framework Integrations

| Framework | Integration Style | Install Extra |
|---|---|---|
| **OpenAI Agents SDK** | Native `TracingProcessor` subclass, auto-registered on `init()` | `rewind-agent[agents]` |
| **Pydantic AI** | Auto-patches `Agent.__init__` to inject Hooks | `rewind-agent[pydantic]` |
| **LangGraph** | `wrap_langgraph(graph)` wraps all graph nodes with `@step` | core |
| **CrewAI** | `wrap_crew(crew)` hooks into `step_callback` / `task_callback` | core |
| **Claude Code / Cursor / Windsurf** | MCP server (`rewind-mcp`) exposing 21 tools | built-in (Rust) |
| **Custom agents** | Direct mode patches OpenAI/Anthropic clients; proxy mode works with any HTTP-based agent | core |
| **GitHub Actions** | `agentoptics/rewind/action@v1` composite action for CI regression testing | N/A |

---

## Evaluation System (v0.4.0)

### Datasets
Versioned collections of (input, expected_output) pairs. Each mutation creates a new version. Content stored in SHA-256 blob store (deduplication). Import from JSONL files or extract from recorded sessions.

### Evaluators
6 built-in types: `exact_match`, `contains`, `regex`, `json_schema`, `tool_use_match`, `custom`. Custom evaluators use subprocess protocol (stdin: `{input, output, expected}` → stdout: `{score, passed, reasoning}`). Python SDK supports `@evaluator` decorator for in-process custom evaluators.

### Experiments
Run a target command against every dataset example, score with evaluators, compute aggregates. CI integration with `--fail-below` thresholds (exit code 1) and `--json` output for dashboard ingestion. Metadata tags for grouping/filtering.

### Comparison
Side-by-side experiment diff with per-example regression/improvement classification. Enforces same dataset version by default.

### Web UI
EvalDashboard with tabbed navigation (Datasets / Experiments / Compare). Score badges with 3-tier color coding (green/amber/red). Expandable per-example results with evaluator reasoning.

---

## Key Directories

```
├── crates/
│   ├── rewind-cli/        CLI entry point (clap)
│   ├── rewind-proxy/      HTTP proxy with SSE streaming
│   ├── rewind-store/      SQLite + content-addressed blob store
│   ├── rewind-replay/     Fork engine, timeline DAG, diffing
│   ├── rewind-assert/     Regression testing — baselines and assertion checks
│   ├── rewind-eval/       Evaluation — datasets, evaluators, experiments, comparison
│   ├── rewind-tui/        Interactive terminal UI (ratatui)
│   ├── rewind-web/        Web server + REST API + WebSocket
│   └── rewind-mcp/        MCP server for AI assistant integration
├── python/
│   └── rewind_agent/      Python SDK
├── web/                   Browser dashboard (React/TypeScript)
├── examples/              Example scripts
├── docs/                  Documentation
├── action/                GitHub Action for CI regression testing
└── tests/                 Integration tests
```

---

## Competitive Positioning

| Tool | What it does | What Rewind adds |
|---|---|---|
| LangSmith / Langfuse | Trace logging, eval, prompt management | Fork-and-replay, cached replay, timeline diff, local-first (no license needed) |
| AgentOps | Session recording | Time-travel (replay from failure at 0 cost), evaluation system |
| Braintrust | Evals & scoring | Inline regression assertions, snapshot/restore, custom subprocess evaluators |
| OpenTelemetry | Distributed tracing | Agent-native step model, context window capture, built-in eval |

Rewind combines **debugging** (fork, replay, diff) with **evaluation** (datasets, scorers, experiments) in a single local-first tool. No cloud account, no API keys for the infrastructure itself, no license gating.
