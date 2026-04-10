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
  <em>Record. Inspect. Fork. Replay from failure. Diff. Evaluate.</em>
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
| **Record** | Transparent proxy captures every LLM call. Streaming pass-through — zero added latency. |
| **Inspect** | See the *exact* context window at each step. Every message, system prompt, and tool response. |
| **Fork** | Branch the timeline at any step. Edit context, resume from there — re-run only what changed. |
| **Diff** | Compare original and forked timelines. See exactly where and why they diverge. |
| **Replay from Failure** | Agent fails at step 5? Fix your code, run `rewind replay --from 4`. Steps 1-4 cached (0 tokens, 0ms). Only step 5 re-runs live. |
| **Instant Replay** | Identical requests cached at 0 tokens, 0ms. Run the same agent 10 times — only the first hits the LLM. |
| **Regression Testing** | Turn any session into a baseline. Check new behavior against it. Run in CI. |
| **Evaluation** | Datasets, scoring, experiments, comparison. CI-ready with `--fail-below`. |
| **Snapshots** | Checkpoint/restore your workspace. No git dependency. |

### Before / After

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

Or try the self-contained demo (no API keys needed):

```bash
rewind demo && rewind inspect latest
```

## Install

```bash
# pip (recommended — installs Python SDK + CLI)
pip install rewind-agent

# Binary only (macOS / Linux)
curl -fsSL https://raw.githubusercontent.com/agentoptics/rewind/master/install.sh | sh

# From source
cargo install --git https://github.com/agentoptics/rewind rewind-cli
```

Optional extras for framework integrations:

```bash
pip install rewind-agent[agents]      # OpenAI Agents SDK
pip install rewind-agent[pydantic]    # Pydantic AI
pip install rewind-agent[all]         # everything
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

See the [Getting Started guide](docs/getting-started.md) for more options.

## Feature Guides

| Feature | Guide | Example |
|:---|:---|:---|
| Recording (direct + proxy) | [docs/recording.md](docs/recording.md) | [05_direct_mode.py](examples/05_direct_mode.py) |
| Replay from Failure & Forking | [docs/replay-and-forking.md](docs/replay-and-forking.md) | [06_replay_from_failure.py](examples/06_replay_from_failure.py) |
| Regression Testing | [docs/regression-testing.md](docs/regression-testing.md) | [07_regression_testing.py](examples/07_regression_testing.py) |
| Evaluation System | [docs/evaluation.md](docs/evaluation.md) | [08_evaluation.py](examples/08_evaluation.py) |
| Custom Evaluators | [docs/evaluation.md](docs/evaluation.md) | [12_custom_evaluator.py](examples/12_custom_evaluator.py) |
| Snapshots | [docs/snapshots.md](docs/snapshots.md) | [11_snapshots.sh](examples/11_snapshots.sh) |
| Web Dashboard | [docs/web-ui.md](docs/web-ui.md) | — |
| Framework Integrations | [docs/framework-integrations.md](docs/framework-integrations.md) | [09_pydantic_ai.py](examples/09_pydantic_ai.py), [10_openai_agents_sdk.py](examples/10_openai_agents_sdk.py) |
| MCP Server (Claude, Cursor) | [docs/mcp-server.md](docs/mcp-server.md) | — |
| SQL Query Explorer | [docs/sql-queries.md](docs/sql-queries.md) | — |
| CLI Reference | [docs/cli-reference.md](docs/cli-reference.md) | — |
| Architecture | [docs/architecture.md](docs/architecture.md) | — |

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
| **v0.5** | Multi-agent tracing, OTel export | Building |
| **v1.0** | LLM-as-judge, live breakpoints, Rewind Cloud, semantic diff | Planned |

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
