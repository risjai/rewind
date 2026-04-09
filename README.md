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
  <em>Record. Inspect. Fork. Replay from failure. Diff.</em>
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
| **Replay from Failure** | Agent fails at step 5? Fix your code, run `rewind replay --from 4`. Steps 1-4 served instantly from cache (0 tokens, 0ms). Only step 5 re-runs live. Diff the result. |
| **Instant Replay** | Identical requests are served from cache at **0 tokens, 0ms latency**. Run the same agent 10 times — only the first run hits the LLM. |
| **Regression Testing** | Turn any session into a baseline. After code changes, check the new behavior: step types, models, tool calls, token counts. Run in CI. |
| **Snapshots** | Capture your entire workspace at any point. Restore in one command if your agent breaks something. No git dependency. |

### The before/after

```
Without Rewind                         With Rewind
─────────────────                      ─────────────────
Agent fails on step 5.                 Agent fails on step 5.
Re-run all 5 steps.                    Fix your code.
Burn tokens on all 5 calls.            rewind replay latest --from 4
Wait 30 seconds.                       Steps 1-4: cached (0ms, 0 tokens)
Hope it works this time.               Step 5: live (1 LLM call, 5 sec)
No idea what changed.                  rewind diff → see exactly what diverged.
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

### Replay from failure — fix and re-run from any step

The headline feature. Your agent failed at step 5? Fix your code, then replay — steps 1-4 are served from cache (instant, free), only step 5 re-runs live.

```bash
# Agent failed at step 5 — fix your code, then:
rewind replay latest --from 4
```

```
⏪ Rewind — Fork & Execute Replay

  Session: research-agent
  Fork at: Step 4
  Cached:  Steps 1-4 (0ms, 0 tokens)
  Live:    Steps 5+ (forwarded to upstream)

  → Point your agent at this proxy:
    export OPENAI_BASE_URL=http://127.0.0.1:8443/v1
```

Or from Python — no proxy needed:

```python
import rewind_agent

with rewind_agent.replay("latest", from_step=4):
    result = my_agent.run("Research Tokyo population")
    # Steps 1-4: instant cached responses (0ms, 0 tokens)
    # Step 5+: live LLM calls, recorded to new forked timeline
```

After the replay, diff the original against the replayed timeline:

```bash
rewind diff <session> main replayed
```

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

### Regression Testing — assert your agent still works

Turn any recorded session into a regression baseline. After changing prompts or code, check the new behavior against the baseline:

```bash
# Create a baseline from a known-good session
rewind assert baseline latest --name "booking-happy-path"

# After changes, check the new session for regressions
rewind assert check latest --against "booking-happy-path"
```

```
⏪ Rewind — Assertion Check

  Baseline: booking-happy-path (5 steps)
  Session:  a3f9e28c (latest)
  Tolerance: tokens ±20%, model changes = fail

  ┌ Step  1  🧠 LLM Call   ✓ PASS  match
  ├ Step  2  📋 Tool Result ✓ PASS  match
  ├ Step  3  🧠 LLM Call   ✓ PASS  tokens OK (312→298, -4.5%)
  ├ Step  4  📋 Tool Result ✓ PASS  match
  └ Step  5  🧠 LLM Call   ✗ FAIL  NEW ERROR: hallucination

  Result: FAILED (4 passed, 1 failed, 0 warnings)
```

Checks step types, models, tool calls, error status, and token usage. Use in CI:

```python
from rewind_agent import Assertions

result = Assertions().check("booking-happy-path", "latest")
assert result.passed, f"Regression: {result.failed_checks} checks failed"
```

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

### Direct recording — zero setup, one line of Python

No proxy, no second terminal, no environment variables. Just add one line and every LLM call is recorded:

<p align="center">
  <img src="https://raw.githubusercontent.com/agentoptics/rewind/master/assets/demo-direct.gif" alt="Rewind direct mode — one line, no proxy" width="800">
</p>

```python
import rewind_agent
import openai

rewind_agent.init()  # that's it — all LLM calls are now recorded

client = openai.OpenAI()
client.chat.completions.create(model="gpt-4o", messages=[...])
# Recorded to ~/.rewind/ — inspect with: rewind show latest
```

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

### Two ways to record

Choose the approach that fits your stack:

| | **Direct mode** (Python) | **Proxy mode** (any language) |
|:---|:---|:---|
| **Setup** | `rewind_agent.init()` — one line | `rewind record` in a second terminal |
| **Languages** | Python (OpenAI + Anthropic SDKs) | Any language that makes HTTP calls |
| **How it works** | Monkey-patches SDK clients in-process | HTTP proxy intercepts LLM traffic |
| **Streaming** | Captured via stream wrappers | SSE pass-through, zero added latency |
| **Best for** | Quick iteration, Python agents | Polyglot teams, non-Python agents |

**Direct mode** — add one line, everything is recorded:

```python
import rewind_agent

rewind_agent.init()  # patches OpenAI + Anthropic automatically

# Or as a scoped session:
with rewind_agent.session("my-agent"):
    client = openai.OpenAI()
    client.chat.completions.create(model="gpt-4o", messages=[...])
```

**Proxy mode** — works with any language or framework:

```bash
rewind record --name "my-agent" --upstream https://api.openai.com
# In another terminal:
export OPENAI_BASE_URL=http://127.0.0.1:8443/v1
python3 my_agent.py   # or node, go, rust — anything that calls the LLM
```

### Agent hooks — enrich recordings with semantic labels

Without hooks, the recording shows "LLM Call 1", "LLM Call 2". With hooks, steps show up as "search", "plan", "execute" — much more useful when debugging.

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

#### OpenAI Agents SDK — zero-config, auto-detected

If you're using the [OpenAI Agents SDK](https://github.com/openai/openai-agents-python), Rewind auto-detects it. Just `init()` and every agent run is recorded — LLM calls, tool executions, and handoffs between agents:

```python
import rewind_agent
from agents import Agent, Runner

rewind_agent.init()  # auto-detects Agents SDK, registers tracing

agent = Agent(name="researcher", instructions="You are a research assistant.", tools=[web_search])
result = await Runner.run(agent, "What is the population of Tokyo?")

# rewind show latest → trace with agent names, tool calls, handoffs
```

```
⏪ Rewind — Session Trace

  ┌ ✓ 🧠  Step 1  gpt-4o       researcher     320ms   156↓  28↑
  │   → tool_calls: web_search
  ├ ✓ 🔧  Step 2  tool:web_search               45ms
  ├ ✓ 🧠  Step 3  gpt-4o       researcher     890ms   312↓  35↑
  │   → final answer
```

For explicit control over lifecycle hooks:

```python
hooks = rewind_agent.openai_agents_hooks()
result = await Runner.run(agent, input, hooks=hooks)
```

Install with: `pip install rewind-agent[agents]`

#### LangGraph & CrewAI

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
| `rewind replay <id> --from <step>` | Replay from a fork point — cached steps instant, live from fork onward |
| `rewind fork <id> --at <step>` | Create a timeline branch at a specific step |
| `rewind diff <id> <left> <right>` | Compare two timelines side by side |
| `rewind snapshot [dir] --label <name>` | Capture workspace state as a checkpoint |
| `rewind restore <id\|label>` | Restore workspace from a snapshot |
| `rewind snapshots` | List all snapshots |
| `rewind cache` | Show instant replay cache statistics |
| `rewind assert baseline <id> --name <name>` | Create a regression baseline from a session |
| `rewind assert check <id> --against <name>` | Check a session against a baseline |
| `rewind assert list` | List all baselines |
| `rewind assert show <name>` | Show baseline step signatures |
| `rewind assert delete <name>` | Delete a baseline |
| `rewind demo` | Seed demo data to explore without API keys |

## Compatibility

| Provider | Non-streaming | Streaming (SSE) |
|:---------|:---:|:---:|
| OpenAI (GPT-4o, o1, etc.) | ✅ | ✅ |
| Anthropic (Claude) | ✅ | ✅ |
| AWS Bedrock | ✅ | — |
| Any OpenAI-compatible (Ollama, vLLM, LiteLLM) | ✅ | ✅ |

Works with any agent framework: **[OpenAI Agents SDK](https://github.com/openai/openai-agents-python)** (native integration), **LangGraph**, **CrewAI**, **Autogen**, **smolagents**, or custom code.

## Architecture

```
rewind/
├── crates/
│   ├── rewind-cli/        CLI entry point (clap)
│   ├── rewind-proxy/      HTTP proxy with SSE streaming
│   ├── rewind-store/      SQLite + content-addressed blob store
│   ├── rewind-replay/     Fork engine, timeline DAG, diffing
│   ├── rewind-assert/     Regression testing — baselines and assertion checks
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
| `replay_session` | Set up fork-and-execute replay with cached/live step split |
| `list_snapshots` | List workspace snapshots |
| `cache_stats` | Instant Replay cache statistics |
| `create_baseline` | Create a regression baseline from a session |
| `check_baseline` | Check a session against a baseline for regressions |
| `list_baselines` | List all baselines |
| `show_baseline` | Show baseline details and expected step signatures |
| `delete_baseline` | Delete a baseline |

### Example

Once configured, ask your AI assistant:

> "Why did my agent fail on the research task?"

The assistant calls `show_session` → reads the trace → identifies that step 4 returned stale data → explains the hallucination in step 5.

## Roadmap

Rewind is in active development. Here's what's shipped and what's coming:

| Phase | Features | Status |
|:------|:---------|:-------|
| **v0.1** | Record, inspect, fork, diff, TUI, streaming, Instant Replay, Snapshots, Python SDK with hooks, LangGraph + CrewAI adapters | ✅ Shipped |
| **v0.2** | Direct recording (no proxy), fork-and-execute replay, regression testing (`rewind assert`), MCP server, replay context manager | ✅ Shipped |
| **v0.3** | Web UI, multi-agent tracing, OTel export | Building |
| **v1.0** | Live breakpoints, Rewind Cloud (team collab), semantic diff, on-prem | Planned |

### What we're solving next

- [ ] **Web UI** — Browser-based timeline explorer with interactive context window viewer
- [ ] **Multi-agent tracing** — Follow execution across parent/child agent handoffs
- [ ] **Live breakpoints** — Pause a running agent at any step, inspect state, modify, resume
- [ ] **OTel export** — Push traces to Grafana, Datadog, or any OpenTelemetry collector
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
