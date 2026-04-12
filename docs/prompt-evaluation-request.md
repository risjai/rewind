# Rewind Evaluation Request Prompt

*Send this to friends, colleagues, and companies to get honest feedback on Rewind.*

---

I want you to evaluate a developer tool called **Rewind** to see if it would be useful for our project. Rewind bills itself as "Chrome DevTools for AI agents" — a unified platform for **tracing**, **evaluation**, and **time-travel debugging** of AI agent sessions. It records LLM calls, tool invocations, and context windows, then lets you inspect, fork, replay from any failure point, diff timelines, run regression tests, and score agent outputs with a built-in eval system — all from a single local-first binary.

This is not a trace viewer or a hosted SaaS. It's a 9 MB statically-linked binary with a SQLite-backed store, a Python SDK, a web dashboard, an MCP server, and a CLI with 28+ commands. MIT licensed.

Here's what I need you to do:

**1. Read their docs**: Fetch and read https://github.com/agentoptics/rewind — focus on the README, the Python SDK at `python/rewind_agent/`, the examples in `examples/`, and the docs in `docs/`. Also check the PyPI page at https://pypi.org/project/rewind-agent/ for install info. Current versions: Rust CLI v0.6.0, Python SDK v0.9.0.

**2. Understand our codebase**: Look at how we currently build and run agents — what frameworks we use (OpenAI SDK, Anthropic SDK, OpenAI Agents SDK, Pydantic AI, LangGraph, CrewAI, custom loops, etc.), how we handle failures, how we test agent behavior, and what observability we already have (LangSmith, Langfuse, Helicone, Braintrust, or nothing).

**3. Write an evaluation report** covering:

**a. Integration fit**: Does Rewind support our agent framework and LLM provider? They have native adapters for OpenAI Agents SDK (`TracingProcessor` subclass), Pydantic AI (auto-patched Hooks), LangGraph (`wrap_langgraph`), and CrewAI (`wrap_crew`), plus a transparent HTTP proxy that works with any language. They also have Claude Code / Cursor / Windsurf integration via an MCP server with 25 tools. How many lines of code would it take to integrate? Show a concrete before/after snippet using our actual code.

**b. Problem/solution match**: Do we actually have the problems Rewind solves? Specifically:
   - Are we spending time/money re-running full agent chains to debug failures? (Rewind's fork-and-replay serves steps 1–N from cache at 0 tokens.)
   - Do we lack visibility into what the model saw at each step? (Rewind captures the exact context window, system prompt, and tool responses per step.)
   - Do we have agent regression testing today? (Rewind's `rewind assert` creates baselines from known-good sessions and checks new runs against them — with a GitHub Action for CI.)
   - Do we have an eval system for scoring agent outputs? (Rewind has datasets, 6 evaluator types including custom, experiments, comparison, and `--fail-below` for CI gates.)
   - Are we building multi-agent workflows? (Rewind v0.5 added hierarchical span trees with agent boundaries, tool calls, handoffs, and thread views.)
   - Are we using Claude Code / Cursor / Windsurf for development? (Rewind has an MCP server and Claude Code hooks integration for observing coding agent sessions in real-time.)

**c. Cost/benefit**: Rewind claims replay-from-failure saves tokens and time. Given our typical agent runs (estimate step count, tokens per run, cost per run), what would the savings look like for a debugging session? Their claim: a 20-step agent at $2/run × 8 debug cycles = $16 wasted; with replay-from-failure, only the steps after the fork point re-run.

**d. Comparison to our current stack**: If we're already using LangSmith, Langfuse, Braintrust, or similar — what does Rewind do that our current tool doesn't? The key differentiators to evaluate:
   - Time-travel debugging (fork, replay, diff) — no observability tool has this
   - Instant Replay caching (identical requests → 0 tokens, 0ms)
   - Local-first (single binary, no cloud dependency, no API keys for infrastructure)
   - Tracing + evals + debugging in one data model (fork a session → replay → diff → score)
   - Language-agnostic proxy mode (any language, not just Python)

**e. Gaps and risks**: What doesn't Rewind cover that we need? Evaluate these known gaps:
   - No LLM-as-judge evaluators yet (planned for v0.7)
   - No OTel export yet (planned for v0.7)
   - No team collaboration / shared sessions (planned for v1.0)
   - No production monitoring / alerting
   - No prompt management / playground
   - Any concerns about the proxy intercepting our traffic? Is the project mature enough? (Check commit activity, contributor count, release cadence — they've shipped 6 versions: v0.1–v0.6.)

**f. Verdict**: On a scale of 1–5, how useful would this be for us right now? Answer three questions:
   1. What's the single strongest reason to adopt it?
   2. What's the single strongest reason to skip it?
   3. If we were to try it, what's the fastest path to value? (Their suggestion: `pip install rewind-agent`, add `rewind_agent.init()` to your agent script, run it, then `rewind inspect latest`.)

Be honest and specific to our project — I don't want a generic product review. If it's not useful for us, say so. If it overlaps with something we already have, call that out.
