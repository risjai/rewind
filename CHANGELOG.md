# Changelog

## v0.4.0 (2026-04-10)

### Evaluation System

Enterprise-grade evaluation system for measuring agent output quality — not just structural regressions. Create datasets of test cases, run your agent against them, score with pluggable evaluators, compare experiments side-by-side.

**Datasets**
- **Versioned test-case collections** — each mutation creates a new version, experiments pin to exact versions.
- **`rewind eval dataset create/import/export/show/list/delete`** — full CRUD from CLI.
- **JSONL import/export** — `{"input": ..., "expected": ...}` per line.
- **Session extraction** — `rewind eval dataset add-from-session` pulls input/expected from recorded session steps.
- Content-addressed blob storage for inputs/expected (SHA-256 deduplication).

**Evaluators**
- **6 built-in types**: `exact_match`, `contains`, `regex`, `json_schema`, `tool_use_match`, `custom`.
- **Custom evaluator** — subprocess protocol: stdin receives `{input, output, expected}`, stdout returns `{score, passed, reasoning}`. Enables LLM-as-judge via user scripts.
- **`rewind eval evaluator create/list/delete`** — manage evaluators from CLI.

**Experiments**
- **`rewind eval run`** — execute a target command against every dataset example, score with evaluators, compute aggregates.
- **Subprocess protocol** — target command receives input JSON on stdin, writes output JSON on stdout. Language-agnostic.
- **CI integration** — `--fail-below 0.8` exits with code 1 if avg_score < threshold. `--json` outputs machine-parseable results with `schema_version: 1`.
- **`--metadata`** — attach JSON tags to experiments for grouping/filtering (e.g., `{"branch":"main","category":"booking"}`).
- Per-example results with individual evaluator scores, duration, and error tracking.

**Comparison**
- **`rewind eval compare`** — side-by-side experiment diff with color-coded deltas.
- Regression/improvement/unchanged classification per example.
- Enforces same dataset version by default (`--force` to override).

**Web API** (7 read-only routes under `/api/eval/`)
- `GET /eval/datasets`, `/eval/datasets/{name}`, `/eval/evaluators`
- `GET /eval/experiments`, `/eval/experiments/{id}`, `/eval/experiments/{id}/results`
- `GET /eval/compare?left={id}&right={id}`

**MCP Tools** (8 new, 21 total)
- Read: `list_eval_datasets`, `show_eval_dataset`, `list_eval_experiments`, `show_eval_experiment`, `compare_eval_experiments`
- Write: `create_eval_dataset`, `add_eval_example`, `dataset_from_session`

**Web UI**
- `EvalDashboard` with tabbed navigation (Datasets / Experiments / Compare).
- `DatasetBrowser` — list + detail with example previews.
- `ExperimentList` — table with status badges, score buckets (green/amber/red), pass rates.
- `ExperimentDetail` — per-example results with expandable scores, reasoning, and trace session links.
- `ExperimentComparison` — two-experiment diff with color-coded deltas and direction indicators.
- `ScoreBadge` — reusable score pill component.

**Python SDK (v0.7.0)**
- `rewind_agent.Dataset` — versioned test-case collections with `add()`, `add_many()`, `from_session()`, `from_jsonl()`, `examples()`.
- `rewind_agent.evaluate()` — run target function against dataset, score, compute aggregates. Persists results to store.
- `@rewind_agent.evaluator` — decorator to register custom evaluator functions.
- `rewind_agent.compare()` — compare two experiments programmatically.
- Built-in evaluators: `exact_match`, `contains_match`, `regex_match`, `tool_use_match`.
- `EvalScore`, `ExperimentResult`, `ComparisonResult`, `EvalFailedError` types.
- 65 tests covering all eval functionality.

**Storage**
- 6 new SQLite tables: `datasets`, `dataset_examples`, `evaluators`, `experiments`, `experiment_results`, `experiment_scores`.
- ~20 new Store methods in both Rust and Python.

---

## v0.3.0 (2026-04-10)

### Web UI & SQL Query

**Web UI**
- **Browser-based dashboard** — session explorer, step timeline, context window viewer, timeline diff, baseline manager, and live recording observability via WebSocket.
- **`rewind web`** — standalone web server. **`rewind record --web`** — recording with live dashboard.
- Embedded in the single binary — no Docker, no Node.js runtime needed.

**SQL Query Explorer**
- **`rewind query "SQL"`** — run read-only queries against the Rewind database.
- **`rewind query --tables`** — show all tables and column schemas.
- Only SELECT, WITH, EXPLAIN, and PRAGMA statements allowed.

---

## v0.2.0 (2026-04-10)

### Fork-and-Execute Replay

The headline feature: agent fails at step 5 → fix your code → `rewind replay latest --from 4` → steps 1-4 served from cache (0ms, 0 tokens), step 5 re-runs live.

**Replay**
- **`rewind replay` CLI command** — starts proxy in fork-and-execute mode. Steps up to `--from` served from blob store, steps after forwarded to upstream LLM.
- **`rewind_agent.replay()` context manager** — Python-native replay, no proxy needed. Monkey-patches return cached SDK response objects for cached steps.
- **`replay_session` MCP tool** — AI assistants can set up replays and return connection info.
- **Proxy `ProxyServer::new_fork_execute()`** — new constructor for fork-and-execute mode with step-number-based cache intercept.

**Direct Recording Mode**
- **`rewind_agent.init(mode="direct")`** — records LLM calls in-process by monkey-patching OpenAI/Anthropic SDK clients. No proxy, no second terminal, one line of code.
- Supports both sync and async clients, streaming and non-streaming.

**Regression Testing**
- **`rewind assert baseline`** — create a regression baseline from any recorded session.
- **`rewind assert check`** — check a session against a baseline. Compares step types, models, tool calls, token usage, error status. Returns exit code 1 on failure.
- **`rewind assert list/show/delete`** — manage baselines.
- **Python `Assertions` class** — `Assertions().check("baseline", "latest")` for CI integration.

**MCP Server**
- New MCP server (`rewind-mcp`) for AI assistant integration (Claude Code, Cursor, Windsurf).
- 13 tools: `list_sessions`, `show_session`, `get_step_detail`, `diff_timelines`, `fork_timeline`, `replay_session`, `cache_stats`, `list_snapshots`, `create_baseline`, `check_baseline`, `list_baselines`, `show_baseline`, `delete_baseline`.

**Framework Integrations**
- **OpenAI Agents SDK** — `RewindTracingProcessor` subclasses `TracingProcessor`. Auto-registered on `init()`. Captures `GenerationSpanData`, `FunctionSpanData`, `HandoffSpanData`. Zero config.
- **Pydantic AI** — Hooks-based integration. Auto-patches `Agent.__init__` to inject recording hooks. Captures model requests/responses and tool executions.
- Install: `pip install rewind-agent[agents]` or `pip install rewind-agent[pydantic]`

**GitHub Action**
- **`agentoptics/rewind/action@v1`** — composite action for CI. Installs Rewind, runs `rewind assert check`, writes results to GitHub Step Summary, fails on regressions.
- **`REWIND_DATA` env var** — both Rust and Python stores respect custom data directory paths. Essential for CI.

**CI**
- Added `cargo test` to Rust build jobs.
- Added `ruff check` (lint) and `pytest` to Python job.
- Version-check ensures `CLI_VERSION` matches `Cargo.toml`.

**Python SDK (v0.5.4)**
- `rewind_agent.replay()` — fork-and-execute context manager.
- `rewind_agent.openai_agents_hooks()` — explicit RunHooks for OpenAI Agents SDK.
- `rewind_agent.pydantic_ai_hooks()` — explicit Hooks capability for Pydantic AI.
- Store query methods: `get_session()`, `get_steps()`, `get_full_timeline_steps()`, `create_fork_timeline()`.
- `REWIND_DATA` env var support.

---

## v0.1.0 (2026-04-09)

### Initial Release

**Core**
- **Recording proxy** — Local HTTP proxy intercepts all LLM API calls transparently. Streaming SSE pass-through for OpenAI and Anthropic. Zero code changes needed.
- **Interactive TUI** — Terminal UI with step-by-step timeline, context window viewer, and step details.
- **Timeline forking** — Branch execution at any step. Forked timelines share parent steps via structural sharing.
- **Timeline diffing** — Compare two timelines to see where they diverge.
- **Content-addressed storage** — SQLite + SHA-256 blob store (like git objects).

**Instant Replay**
- Proxy-level response caching by request hash. Identical requests served from cache at $0 cost, 0ms latency. Enable with `rewind record --replay`.

**Snapshots**
- Workspace checkpoint and restore without git. `rewind snapshot` captures a directory as compressed tar. `rewind restore` rolls back to any snapshot.

**Python SDK**
- `rewind_agent.init()` auto-patches OpenAI/Anthropic clients.
- `@step`, `@node`, `@tool` decorators and `trace()` context manager for enriching recordings.
- `wrap_langgraph()` and `wrap_crew()` for one-line framework integration.

**Compatibility**
- OpenAI, Anthropic, AWS Bedrock (via gateway), and any OpenAI-compatible API.
- Works with LangGraph, CrewAI, OpenAI Agents SDK, or custom code.
