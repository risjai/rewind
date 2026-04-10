# Architecture

Rewind is a time-travel debugger for AI agents -- record, inspect, fork, replay from failure, and diff. This document covers how the proxy-based recording works and the internal crate layout.

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

## Key Design Decisions

- **Proxy-based instrumentation.** No SDK required, no code changes. Works with any agent framework that makes HTTP calls to an LLM API -- Python, TypeScript, Rust, Go, anything.
- **Content-addressed storage.** Requests and responses are stored by SHA-256 hash, like git objects. Identical payloads are deduplicated automatically. The same blob store powers Instant Replay caching and Snapshot storage.
- **Timeline DAG.** Forks share parent steps via structural sharing. Forking at step 40 of a 50-step run uses zero storage for steps 1-40.
- **Instant Replay at the transport layer.** Request hash -> cached response. Works with any LLM provider, any framework, any language. No SDK-level instrumentation required.
- **Streaming pass-through.** SSE streams are forwarded to the agent in real-time while being accumulated for recording. The agent sees zero added latency.
- **Single binary, zero dependencies.** 9 MB static Rust binary. Data stored in SQLite + flat files. No Docker, no database server, no cloud account.

## Crate Layout

```
rewind/
├── crates/
│   ├── rewind-cli/        CLI entry point (clap)
│   ├── rewind-proxy/      HTTP proxy with SSE streaming
│   ├── rewind-store/      SQLite + content-addressed blob store
│   ├── rewind-replay/     Fork engine, timeline DAG, diffing
│   ├── rewind-assert/     Regression testing — baselines and assertion checks
│   ├── rewind-eval/       Evaluation system — datasets, evaluators, experiments
│   ├── rewind-tui/        Interactive terminal UI (ratatui)
│   └── rewind-mcp/        MCP server for AI assistant integration
├── python/
│   └── rewind_agent/      Python SDK
└── demo/                  Demo scripts, mock servers, test scripts
```

**Built with:** Rust (hyper, tokio, ratatui, rusqlite), Python.
