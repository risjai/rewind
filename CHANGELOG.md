# Changelog

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
