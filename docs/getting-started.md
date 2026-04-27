# Getting Started

**Rewind** is a time-travel debugger for AI agents. It records every LLM call your agent makes, then lets you inspect, fork, replay from failure, and diff timelines — without re-running (or paying for) the steps that already succeeded.

---

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
# Binary at ./target/release/rewind (no dependencies)
```

---

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

# 5. See what happened (trace view, interactive TUI, cache stats)
rewind show latest
rewind inspect latest
rewind cache

# 6. Something broke? Roll back
rewind restore before-agent
```

### Try without API keys

The `rewind demo` command seeds sample data so you can explore the TUI, web dashboard, and CLI without needing any API keys:

```bash
rewind demo && rewind inspect latest
```

### Already-Python alternative — no proxy

If your agent is Python, you can skip the proxy entirely:

```python
from rewind_agent import intercept
intercept.install()  # patches httpx / requests / aiohttp; recording starts

# … your existing code unchanged …
```

This is the **HTTP intercept** path — works with any HTTP-based LLM client (OpenAI SDK, Anthropic SDK, LangChain, custom gateways via custom predicates, etc.). See the [HTTP Intercept Quickstart](intercept-quickstart.md).

---

## Works with any LLM provider

Rewind sits between your agent and the LLM API. Point it at any provider:

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

| Provider | Non-streaming | Streaming (SSE) |
|:---------|:---:|:---:|
| OpenAI (GPT-4o, o1, etc.) | Yes | Yes |
| Anthropic (Claude) | Yes | Yes |
| AWS Bedrock | Yes | -- |
| Any OpenAI-compatible (Ollama, vLLM, LiteLLM) | Yes | Yes |

---

## Next steps

For a deeper look at how recording works — direct mode vs proxy mode, agent hooks, and streaming — see [recording.md](recording.md).
