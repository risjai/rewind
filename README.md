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
  <em>Built-in tracing, evals, and CI regression testing — no LangSmith required.</em>
  <br/>
  <br/>
  <a href="#the-problem">Why</a> &nbsp;&bull;&nbsp;
  <a href="#see-it-in-action">Demo</a> &nbsp;&bull;&nbsp;
  <a href="#install">Install</a> &nbsp;&bull;&nbsp;
  <a href="#quickstart">Quickstart</a> &nbsp;&bull;&nbsp;
  <a href="#feature-guides">Guides</a> &nbsp;&bull;&nbsp;
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

> **Agent broke at step 30? Fix step 30 — not steps 1 through 29 again.** Each re-run costs tokens, time, and a different answer.

## The Problem

AI agents are shipping to production — tool-calling chains with 10, 30, 50 LLM steps. When they fail, debugging is brutal:

- **You can't see what the model saw.** What was in the context window at step 41? What got truncated?
- **You can't reproduce it.** Re-run the agent and you get a different result. The LLM is non-deterministic.
- **You can't isolate the failure.** Was it step 5 or step 2? You have to re-run all 50 steps ($$$, minutes) just to test a theory.

Every existing observability tool shows you **what happened**. None of them let you **change the past and observe a different future**.

## The Solution

**Rewind is Chrome DevTools for AI agents.**

| Capability | What it means |
|:---|:---|
| **Record** | A transparent proxy captures every LLM call. Your agent doesn't know it's being recorded. Streaming works in real-time — zero added latency. |
| **Inspect** | See the *exact* context window at each step. Every message, system prompt, and tool response the model saw — displayed as human-readable, color-coded views. |
| **Fork** | Branch the execution timeline at any step. Edit the context (fix a stale tool response, tweak the prompt). Resume from there — re-run only the new steps. |
| **Diff** | Compare the original and forked timelines. See exactly where they diverge and why. |
| **Replay from Failure** | Agent fails at step 5? Fix your code, run `rewind replay --from 4`. Steps 1-4 served instantly from cache (0 tokens, 0ms). Only step 5 re-runs live. Diff the result. |
| **Instant Replay** | Identical requests are served from cache at **0 tokens, 0ms latency**. Run the same agent 10 times — only the first run hits the LLM. |
| **Regression Testing** | Turn any session into a baseline. After code changes, check the new behavior: step types, models, tool calls, token counts. Run in CI. |
| **Evaluation** | Create datasets of test cases, run your agent against them, score with built-in evaluators (exact match, contains, regex, JSON schema, tool use), compare experiments side-by-side. CI-ready with `--fail-below` thresholds. |
| **Multi-Agent Tracing** | See which agent made which decision. Span tree visualization groups LLM calls, tool invocations, and handoffs under their parent agent. Thread view tracks multi-turn conversations across sessions. |
| **Snapshots** | Capture your entire workspace at any point. Restore in one command if your agent breaks something. No git dependency. |

**The only tool where debugging, tracing, and evals share the same data model.** Fork a session, replay it, diff it, score it — all on the same timeline. No second tool needed.

## See It in Action

<p align="center">
  <img src="https://raw.githubusercontent.com/agentoptics/rewind/master/assets/demo.gif" alt="Rewind demo — trace, diff, cache" width="800">
</p>

```bash
rewind demo && rewind inspect latest   # try it now — no API keys needed
```

### See what the model saw — find the bug in 5 seconds

```
⏪ Rewind — Session Trace

  Session: customer-service   Steps: 12   Tokens: 3,450
  Agents: supervisor, researcher, writer

  ▼ ✓ 🤖 supervisor (agent)                          1.2s
    ├ ✓ 🧠  gpt-4o  "Route to researcher"           320ms  156↓ 28↑
    ▼ ✓ 🤖 researcher (agent)                        2.1s
    │ ├ ✓ 🧠  gpt-4o  "Search for information"      890ms  312↓ 35↑
    │ ├ ✓ 🔧  web_search("Tokyo population")          45ms
    │ └ ✓ 🧠  gpt-4o  "Synthesize results"          650ms  280↓ 95↑
    ├ ✓ 🔀 handoff: researcher → writer
    ▼ ✗ 🤖 writer (agent)                            1.8s
    │ ├ ✓ 🧠  gpt-4o  "Draft article"              1200ms  450↓ 180↑
    │ └ ✗ 🧠  gpt-4o  "Polish final draft"          600ms  320↓ 120↑
    │     ERROR: Hallucination — used stale data
    └ ✓ 🧠  gpt-4o  "Final review"                   400ms  200↓ 45↑
```

The writer agent hallucinated at step 8 because the researcher used stale data. Without the span tree, you'd see a flat list of 12 steps with no agent boundaries.

### Fix and replay — only re-run what changed

```bash
rewind replay latest --from 4          # fix your code, then:
# Steps 1-3: cached instantly (0ms, 0 tokens)
# Steps 4-5: re-run live with corrected context
rewind diff latest main fixed           # see exactly what diverged
```

```
⏪ Rewind — Timeline Diff (main vs fixed, diverge at step 4)

  ═ Step  1  identical
  ═ Step  2  identical
  ═ Step  3  identical
  ≠ Step  4  [stale data]  →  [fresh data]
  ≠ Step  5  [error] 700tok   →  [success] 715tok
```

### Evaluate before shipping — catch regressions in CI

```python
result = rewind_agent.evaluate(
    dataset="booking-tests",
    target_fn=my_agent,
    evaluators=[exact_match, tool_use_match],
    fail_below=0.9,   # CI fails if score drops below 90%
)
# Score: 95.0%, Pass rate: 100% — ship it
```

## Install

**pip** (recommended — installs Python SDK + CLI):

```bash
pip install rewind-agent
```

**Binary only** (macOS / Linux):

```bash
curl -fsSL https://raw.githubusercontent.com/agentoptics/rewind/master/install.sh | sh
```

**From source** (requires Rust):

```bash
cargo install --git https://github.com/agentoptics/rewind rewind-cli
```

## Quickstart

**Direct mode** — one line, no proxy:

```python
import rewind_agent
import openai

rewind_agent.init()  # that's it — all LLM calls are now recorded

client = openai.OpenAI()
client.chat.completions.create(model="gpt-4o", messages=[...])
# rewind show latest → see the trace
```

**Proxy mode** — works with any language:

```bash
# Terminal 1: Start the recording proxy
rewind record --name "my-agent" --upstream https://api.openai.com --replay

# Terminal 2: Point your agent at the proxy
export OPENAI_BASE_URL=http://127.0.0.1:8443/v1
python3 my_agent.py   # or node, go, rust — anything

# See what happened
rewind show latest           # trace view
rewind inspect latest        # interactive TUI
```

**Claude Code** — observe sessions via plugin:

```bash
# Install the plugin (one-time)
claude marketplace add agentoptics --source github --repo agentoptics/rewind-plugin
claude plugin install agentoptics/rewind

# Start the dashboard
rewind web --port 4800
# Open http://127.0.0.1:4800 — sessions appear automatically
```

Or manually with the CLI: `rewind hooks install`

See the [Getting Started guide](docs/getting-started.md) for more options.

## Feature Guides

| Feature | Description | Guide | Example |
|:---|:---|:---|:---|
| **Recording** | One line to start (`init()`), or a transparent HTTP proxy for any language. Monkey-patches OpenAI + Anthropic SDKs in-process. Streaming pass-through with zero added latency. | [recording.md](docs/recording.md) | [05_direct_mode.py](examples/05_direct_mode.py) |
| **Replay from Failure** | Agent fails at step 5? Fix your code, replay from step 4. Steps 1-4 served from cache (0 tokens, 0ms). Only the fixed step re-runs live. Diff the original vs replayed timeline. | [replay-and-forking.md](docs/replay-and-forking.md) | [06_replay_from_failure.py](examples/06_replay_from_failure.py) |
| **Regression Testing** | Turn any recorded session into a baseline. After code changes, check step types, models, tool calls, token counts, and error status. 3-line GitHub Action for CI. | [regression-testing.md](docs/regression-testing.md) | [07_regression_testing.py](examples/07_regression_testing.py) |
| **Evaluation** | Create datasets of test cases, run your agent against them, score with built-in evaluators (exact match, contains, regex, JSON schema, tool use), compare experiments side-by-side. CI-ready with `--fail-below` thresholds. | [evaluation.md](docs/evaluation.md) | [08_evaluation.py](examples/08_evaluation.py) |
| **Custom Evaluators** | Define domain-specific scoring with `@evaluator()` — check keyword coverage, format compliance, or any custom logic. Plug into the same experiment/comparison pipeline. | [evaluation.md](docs/evaluation.md) | [12_custom_evaluator.py](examples/12_custom_evaluator.py) |
| **Snapshots** | Checkpoint your entire workspace before an agent runs. If it breaks something, restore in one command. Compressed tar+gz in the blob store — no git required. | [snapshots.md](docs/snapshots.md) | [11_snapshots.sh](examples/11_snapshots.sh) |
| **Web Dashboard** | Browser-based session explorer with step timeline, context window viewer, timeline diff, and live recording via WebSocket. Everything embedded in the single binary. | [web-ui.md](docs/web-ui.md) | — |
| **Multi-Agent Tracing** | Hierarchical span tree for multi-agent workflows. Auto-captures agent boundaries, tool calls, and handoffs from OpenAI Agents SDK. Manual `@span()` decorator for custom grouping. Thread view for multi-turn conversations. | [multi-agent-tracing.md](docs/multi-agent-tracing.md) | — |
| **Framework Integrations** | Native support for OpenAI Agents SDK, Pydantic AI, LangGraph, and CrewAI. Auto-detected on `init()` — zero config for most frameworks. | [framework-integrations.md](docs/framework-integrations.md) | [09_pydantic_ai.py](examples/09_pydantic_ai.py), [10_openai_agents_sdk.py](examples/10_openai_agents_sdk.py) |
| **Claude Code Observation** | Observe Claude Code sessions in real-time via hooks. See every tool call (Read, Edit, Bash, Grep, Agent), user prompts, and session lifecycle. Token usage extracted from transcripts. One-command setup: `rewind hooks install`. | — | — |
| **MCP Server** | 25 tools for AI assistants (Claude Code, Cursor, Windsurf) to query recordings, view span trees, browse threads, diff timelines, create baselines, run evals — all from your IDE. | [mcp-server.md](docs/mcp-server.md) | — |
| **SQL Query Explorer** | Run ad-hoc SQL against the Rewind database. Token usage by model, average step duration, sessions with errors, cost estimation — read-only, safe to explore. | [sql-queries.md](docs/sql-queries.md) | — |
| **CLI Reference** | Full command reference for all 28 CLI commands. | [cli-reference.md](docs/cli-reference.md) | — |

## Compatibility

| Provider | Non-streaming | Streaming (SSE) |
|:---------|:---:|:---:|
| OpenAI (GPT-4o, o1, etc.) | ✅ | ✅ |
| Anthropic (Claude) | ✅ | ✅ |
| AWS Bedrock | ✅ | — |
| Any OpenAI-compatible (Ollama, vLLM, LiteLLM) | ✅ | ✅ |

Works with any agent framework: **[OpenAI Agents SDK](https://github.com/openai/openai-agents-python)** (native), **[Pydantic AI](https://ai.pydantic.dev/)** (native), **LangGraph**, **CrewAI**, **Autogen**, **smolagents**, or custom code.

## Roadmap

| Phase | Features | Status |
|:------|:---------|:-------|
| **v0.1** | Record, inspect, fork, diff, TUI, streaming, Instant Replay, Snapshots, Python SDK, LangGraph + CrewAI | ✅ Shipped |
| **v0.2** | Direct recording, fork-and-execute replay, regression testing, MCP server | ✅ Shipped |
| **v0.3** | Web UI (flight recorder + live dashboard) | ✅ Shipped |
| **v0.4** | Evaluation system — datasets, evaluators, experiments, comparison, CI | ✅ Shipped |
| **v0.5** | Multi-agent tracing (spans, threads, span tree UI) | ✅ Shipped |
| **v0.6** | Claude Code hooks integration, transcript token parsing, session observation | ✅ Shipped |
| **v0.7** | OTel export, LLM-as-judge evaluators | Planned |
| **v1.0** | Rewind Cloud, shared sessions, team dashboards, live breakpoints, semantic diff | Planned |

## Why "Rewind"?

Agent debugging today is where web debugging was before Chrome DevTools. You had `alert()` and `console.log()`. Then DevTools gave you breakpoints, time-travel debugging, and network inspection — and everything changed.

Rewind brings that same leap to AI agents.

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
