<p align="center">
  <br/>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/agentoptics/rewind/master/assets/banner-dark.svg">
    <source media="(prefers-color-scheme: light)" srcset="https://raw.githubusercontent.com/agentoptics/rewind/master/assets/banner-light.svg">
    <img alt="Rewind" src="https://raw.githubusercontent.com/agentoptics/rewind/master/assets/banner-light.svg" width="500">
  </picture>
  <br/>
  <br/>
  <strong>The time-travel debugger for AI agents</strong>
  <br/>
  <em>Record. Inspect. Fork. Replay. Diff.</em>
  <br/>
  <br/>
  <a href="#the-problem">Why</a> &nbsp;&bull;&nbsp;
  <a href="#see-it-in-action">Demo</a> &nbsp;&bull;&nbsp;
  <a href="#install">Install</a> &nbsp;&bull;&nbsp;
  <a href="#quickstart">Quickstart</a> &nbsp;&bull;&nbsp;
  <a href="#how-it-works">Architecture</a> &nbsp;&bull;&nbsp;
  <a href="#roadmap">Roadmap</a>
  <br/>
  <br/>

  [![CI](https://github.com/agentoptics/rewind/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/agentoptics/rewind/actions)
  [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
  [![GitHub Release](https://img.shields.io/github/v/release/agentoptics/rewind?label=release)](https://github.com/agentoptics/rewind/releases)
  [![PyPI](https://img.shields.io/pypi/v/rewind-agent?style=flat)](https://pypi.org/project/rewind-agent/)
  [![GitHub stars](https://img.shields.io/github/stars/agentoptics/rewind?style=social)](https://github.com/agentoptics/rewind)
  <br/>
  <sub>Single binary &middot; 9 MB &middot; zero dependencies &middot; MIT licensed</sub>
</p>

---

## The Problem

AI agents are shipping to production — tool-calling chains with 10, 30, 50 LLM steps. When they fail, debugging is brutal:

- **You can't see what the model saw.** What was in the context window at step 41? What got truncated? What stale tool response poisoned the reasoning?
- **You can't reproduce it.** Re-run the agent and you get a different result. The LLM is non-deterministic. The tool responses may have changed.
- **You can't isolate the failure.** Was it step 5 that went wrong, or step 2? You have to re-run all 50 steps ($$$, minutes of waiting) just to test a theory.

Every existing observability tool (LangSmith, Langfuse, AgentOps) shows you **what happened**. None of them let you **change the past and observe a different future**.

## The Solution

**Rewind is Chrome DevTools for AI agents.**

It records every LLM call your agent makes — the full request, response, context window, token counts, and timing. Then it lets you:

| Capability | What it means |
|:---|:---|
| **Record** | A transparent proxy captures every LLM call. Your agent doesn't know it's being recorded. Streaming works in real-time — zero added latency. |
| **Inspect** | See the *exact* context window at each step. Every message, system prompt, and tool response the model saw — displayed as human-readable, color-coded views. |
| **Fork** | Branch the execution timeline at any step. Edit the context (fix a stale tool response, tweak the prompt). Resume from there — re-run only the new steps. |
| **Diff** | Compare the original and forked timelines. See exactly where they diverge and why. |
| **Instant Replay** | Identical requests are served from cache at **0 tokens, 0ms latency**. Run the same agent 10 times — only the first run hits the LLM. |
| **Snapshots** | Capture your entire workspace at any point. Restore in one command if your agent breaks something. No git dependency. |

### The before/after

```
Without Rewind                         With Rewind
─────────────────                      ─────────────────
Agent fails on step 5.                 Agent fails on step 5.
Re-run all 5 steps.                    rewind fork --at 4
Burn tokens on all 5 calls.            Fix the stale tool response.
Wait 30 seconds.                       Re-run only step 5.
Hope it works this time.               1 LLM call. 5 seconds.
No idea what changed.                  Diff shows exactly what diverged.
```

## See It in Action

<p align="center">
  <img src="https://raw.githubusercontent.com/agentoptics/rewind/master/assets/demo.gif" alt="Rewind demo — trace, diff, cache" width="800">
</p>

### Agent trace — see where it went wrong

```
⏪ Rewind — Session Trace

  Session: research-agent
  Steps: 5    Tokens: 1,231

  ┌ ✓ 🧠  Step 1  gpt-4o    320ms   156↓  28↑
  │   → tool_calls: web_search("Tokyo population 2024")
  ├ ✓ 📋  Step 2  tool        45ms
  ├ ✓ 🧠  Step 3  gpt-4o    890ms   312↓  35↑
  │   → tool_calls: web_search("Tokyo population decade trend")
  ├ ✓ 📋  Step 4  tool        38ms
  │        ⚠ Stale cached data returned (2019 dataset)
  └ ✗ 🧠  Step 5  gpt-4o   1450ms   520↓ 180↑
       ERROR: Hallucination — used 2019 projection as 2024 fact
```

The trace shows the agent succeeded on steps 1-4, then hallucinated on step 5 because step 4 returned stale cached data from a search API.

### Timeline diff — fork, fix, compare

```
⏪ Rewind — Timeline Diff

  main vs fixed (diverge at step 5)

  ═ Step  1  identical
  ═ Step  2  identical
  ═ Step  3  identical
  ═ Step  4  identical
  ≠ Step  5  [error] 700tok  →  [success] 715tok
```

Steps 1-4 are shared (zero re-execution). Only step 5 was re-run with corrected context.

### Instant Replay — same task, 0 tokens

When you enable `--replay`, Rewind caches every successful LLM response. The next time your agent sends the exact same request, the cached response is returned instantly — no upstream call, no tokens burned.

```bash
# Enable caching
rewind record --name "my-agent" --upstream https://api.openai.com --replay
```

```
  Call 1: gpt-4o   320ms   156↓ 28↑    ← cache miss (hits upstream)
  Call 2: gpt-4o     0ms   156↓ 28↑    ← ⚡ cache hit (instant, 0 tokens)
  Call 3: gpt-4o   890ms   312↓ 35↑    ← cache miss (different request)
```

```bash
rewind cache   # see stats
# Cached responses: 2
# Total cache hits: 1
# Tokens saved: 184
```

This is especially useful for iterative development — re-run your agent 20 times while tweaking a prompt, and only the changed steps hit the LLM.

### Snapshots — workspace checkpoint/restore

Before your agent starts modifying files, take a snapshot. If it goes wrong, restore in one command.

```bash
# Checkpoint before agent runs
rewind snapshot ./my-project --label "pre-agent"

# Agent runs, creates files, modifies code...
python3 my_agent.py

# Something went wrong — roll back everything
rewind restore pre-agent

# List all snapshots
rewind snapshots
```

```
⏪ Rewind Snapshots

            ID           LABEL  FILES       SIZE CREATED
  ─────────────────────────────────────────────────────────────────
  df89be3f-b0e       pre-agent     47      2.1MB 5m ago
  a3c1e82d-19f    after-search     52      2.4MB 2m ago
```

No git required. Works on any directory. Compressed tar+gz stored in the blob store.

### Interactive TUI

The `rewind inspect` command opens a full terminal UI:

- **Left panel**: Step-by-step timeline with status icons, timing, and token counts
- **Right panel**: Full context window at the selected step — every message, tool call, and system prompt
- Navigate with arrow keys, Tab to switch panels, scroll through context

<p align="center">
  <img src="https://raw.githubusercontent.com/agentoptics/rewind/master/assets/tui-screenshot.svg" alt="Rewind TUI — interactive debugger" width="800">
</p>

## Install

### pip (recommended)

```bash
pip install rewind-agent
```

This installs both the Python SDK **and** the `rewind` CLI. The native binary is auto-downloaded on first use — no Rust toolchain required.

### Quick install (binary only, macOS / Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/agentoptics/rewind/master/install.sh | sh
```

### From source (requires Rust)

```bash
cargo install --git https://github.com/agentoptics/rewind rewind-cli
```

### Build locally

```bash
git clone https://github.com/agentoptics/rewind.git
cd rewind
cargo build --release
# Binary at ./target/release/rewind (9 MB, no dependencies)
```

## Quickstart

**30 seconds to your first recording:**

```bash
# 1. Start the recording proxy (forwards to OpenAI)
#    Add --replay to enable instant replay caching
rewind record --name "my-agent" --upstream https://api.openai.com --replay

# 2. In another terminal — point your agent at the proxy
export OPENAI_BASE_URL=http://127.0.0.1:8443/v1

# 3. Snapshot your workspace before the agent runs
rewind snapshot . --label "before-agent"

# 4. Run your agent as usual
python3 my_agent.py

# 5. See what happened
rewind show latest           # quick trace view
rewind inspect latest        # interactive TUI
rewind cache                 # see replay savings

# 6. Something broke? Roll back
rewind restore before-agent
```

Or try the self-contained demo (no API keys needed):

```bash
rewind demo && rewind inspect latest
```

### Works with any LLM provider

```bash
# OpenAI
rewind record --upstream https://api.openai.com

# Anthropic
rewind record --upstream https://api.anthropic.com

# AWS Bedrock (via gateway)
rewind record --upstream https://your-bedrock-gateway.com

# Any OpenAI-compatible API (Ollama, vLLM, etc.)
rewind record --upstream http://localhost:11434
```

### Python SDK (optional)

```python
import rewind_agent

# Auto-patches OpenAI/Anthropic clients to route through the proxy
rewind_agent.init()

# Or use as a context manager
with rewind_agent.session("my-agent"):
    client = openai.OpenAI()
    client.chat.completions.create(model="gpt-4o", messages=[...])
```

### Agent hooks — enrich recordings with semantic labels

Without hooks, the proxy records "LLM Call 1", "LLM Call 2". With hooks, steps show up as "search", "plan", "execute" — much more useful when debugging.

```python
import rewind_agent

@rewind_agent.step("search")
def search_web(query: str) -> str:
    return client.chat.completions.create(...)

@rewind_agent.tool("calculator")
def calculate(a: float, b: float) -> float:
    return a + b

@rewind_agent.node("planner")       # LangGraph-style node
def plan(state: dict) -> dict:
    return {"steps": ["research", "write", "review"]}

with rewind_agent.trace("analysis_phase"):
    rewind_agent.annotate("confidence", 0.92)
    result = search_web("Tokyo population")
```

### Framework adapters

One-line integration for popular agent frameworks:

```python
# LangGraph — wraps all graph nodes automatically
graph = rewind_agent.wrap_langgraph(compiled_graph)
result = graph.invoke({"input": "..."})

# CrewAI — hooks into step and task callbacks
crew = rewind_agent.wrap_crew(crew)
result = crew.kickoff()
```

## How It Works

```
                                ┌─────────────────────┐
  Your Agent  ──HTTP──▶  Rewind Proxy (:8443)  ──▶  LLM API
                                │                (OpenAI / Anthropic / Bedrock)
                                │
                         Record everything
                                │
                                ▼
                          ~/.rewind/
                    ┌───────────────────┐
                    │   rewind.db       │  Sessions, timelines, steps (SQLite)
                    │   objects/        │  Content-addressed blobs (SHA-256)
                    └───────────────────┘
```

**Key design decisions:**

- **Proxy-based instrumentation.** No SDK required, no code changes. Works with any agent framework that makes HTTP calls to an LLM API — Python, TypeScript, Rust, Go, anything.
- **Content-addressed storage.** Requests and responses are stored by SHA-256 hash, like git objects. Identical payloads are deduplicated automatically. The same blob store powers Instant Replay caching and Snapshot storage.
- **Timeline DAG.** Forks share parent steps via structural sharing. Forking at step 40 of a 50-step run uses zero storage for steps 1-40.
- **Instant Replay at the transport layer.** Request hash → cached response. Works with any LLM provider, any framework, any language. No SDK-level instrumentation required.
- **Streaming pass-through.** SSE streams are forwarded to the agent in real-time while being accumulated for recording. The agent sees zero added latency.
- **Single binary, zero dependencies.** 9 MB static Rust binary. Data stored in SQLite + flat files. No Docker, no database server, no cloud account.

## Commands

| Command | Description |
|:--------|:------------|
| `rewind record [--replay]` | Start the recording proxy. `--replay` enables instant replay caching. |
| `rewind sessions` | List all recorded sessions |
| `rewind show <id\|latest>` | Print a session's step-by-step trace |
| `rewind inspect <id\|latest>` | Open the interactive TUI |
| `rewind fork <id> --at <step>` | Create a timeline branch at a specific step |
| `rewind diff <id> <left> <right>` | Compare two timelines side by side |
| `rewind snapshot [dir] --label <name>` | Capture workspace state as a checkpoint |
| `rewind restore <id\|label>` | Restore workspace from a snapshot |
| `rewind snapshots` | List all snapshots |
| `rewind cache` | Show instant replay cache statistics |
| `rewind demo` | Seed demo data to explore without API keys |

## Compatibility

| Provider | Non-streaming | Streaming (SSE) |
|:---------|:---:|:---:|
| OpenAI (GPT-4o, o1, etc.) | ✅ | ✅ |
| Anthropic (Claude) | ✅ | ✅ |
| AWS Bedrock | ✅ | — |
| Any OpenAI-compatible (Ollama, vLLM, LiteLLM) | ✅ | ✅ |

Works with any agent framework: **LangGraph**, **CrewAI**, **OpenAI Agents SDK**, **Autogen**, **smolagents**, or custom code.

## Architecture

```
rewind/
├── crates/
│   ├── rewind-cli/        CLI entry point (clap)
│   ├── rewind-proxy/      HTTP proxy with SSE streaming
│   ├── rewind-store/      SQLite + content-addressed blob store
│   ├── rewind-replay/     Fork engine, timeline DAG, diffing
│   ├── rewind-tui/        Interactive terminal UI (ratatui)
│   └── rewind-mcp/        MCP server for AI assistant integration
├── python/
│   └── rewind_agent/      Python SDK
└── demo/                  Demo scripts, mock servers, test scripts
```

**Built with:** Rust (hyper, tokio, ratatui, rusqlite), Python.

## MCP Server — AI Assistant Integration

Rewind ships an MCP (Model Context Protocol) server so AI assistants like **Claude Code**, **Cursor**, and **Windsurf** can query your agent recordings directly.

### Build

```bash
cargo build --release -p rewind-mcp
```

### Configure

**Claude Code** — add to `.claude/settings.json`:

```json
{
  "mcpServers": {
    "rewind": {
      "command": "/path/to/rewind-mcp"
    }
  }
}
```

**Cursor** — add to `.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "rewind": {
      "command": "/path/to/rewind-mcp"
    }
  }
}
```

### Available tools

| Tool | Description |
|:-----|:------------|
| `list_sessions` | List all recorded sessions with stats |
| `show_session` | Step-by-step trace with token counts, errors, response previews |
| `get_step_detail` | Full request/response content from blob store |
| `diff_timelines` | Compare two timelines side by side |
| `fork_timeline` | Create a fork at a specific step |
| `list_snapshots` | List workspace snapshots |
| `cache_stats` | Instant Replay cache statistics |

### Example

Once configured, ask your AI assistant:

> "Why did my agent fail on the research task?"

The assistant calls `show_session` → reads the trace → identifies that step 4 returned stale data → explains the hallucination in step 5.

## Roadmap

Rewind is in active development. Here's what's coming:

| Phase | Features | Status |
|:------|:---------|:-------|
| **v0.1** | Record, inspect, fork, diff, TUI, streaming, Instant Replay, Snapshots, Python SDK with hooks, LangGraph + CrewAI adapters | ✅ Shipped |
| **v0.2** | Web UI, fork-and-execute (live re-run from fork point), multi-agent tracing | Building |
| **v1.0** | Live breakpoints, Rewind Cloud (team collab), OTel export, semantic regression, on-prem | Planned |

### What we're solving next

- [ ] **Web UI** — Browser-based timeline explorer with interactive context window viewer
- [ ] **Fork-and-execute** — Edit context at a fork point, re-run live from there (hermetic replay + live execution)
- [ ] **Live breakpoints** — Pause a running agent at any step, inspect state, modify, resume
- [ ] **Semantic regression** — "This agent handled this input correctly last week — what changed?"
- [ ] **Rewind Cloud** — Share sessions with teammates, persistent storage, alerts on agent failures

## Why "Rewind"?

Agent debugging today is where web debugging was before Chrome DevTools. You had `alert()` and `console.log()`. Then DevTools gave you breakpoints, time-travel debugging, network inspection, and DOM inspection — and everything changed.

Rewind brings that same leap to AI agents. It's not a log viewer. It's not a dashboard. It's a **debugger** — built for the moment when your agent does something wrong and you need to understand *exactly why*.

## Contributing

We welcome contributions! See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

```bash
git clone https://github.com/agentoptics/rewind.git
cd rewind
cargo build          # build all crates
cargo run -- demo    # seed demo data
cargo run -- inspect latest   # open TUI
```

## License

MIT License. See [LICENSE](LICENSE) for details.
