# Contributing to Rewind

Thanks for your interest in contributing to Rewind! Here's how to get started.

## Development setup

```bash
# Clone the repo
git clone https://github.com/agentoptics/rewind.git
cd rewind

# Build (requires Rust 1.80+)
cargo build

# Run the demo to verify everything works
cargo run -- demo
cargo run -- show latest
cargo run -- inspect latest  # TUI, press q to quit
```

## Project structure

| Crate | Purpose |
|-------|---------|
| `rewind-cli` | CLI entry point, all user-facing commands |
| `rewind-proxy` | HTTP proxy that intercepts LLM API calls |
| `rewind-store` | SQLite database + content-addressed blob storage |
| `rewind-replay` | Fork engine, timeline DAG, diff algorithm |
| `rewind-tui` | Interactive terminal UI (ratatui) |

The Python SDK lives in `python/rewind_agent/`.

## Making changes

1. **Fork the repo** and create a feature branch
2. **Write your code** — follow the existing patterns
3. **Test locally** — `cargo build && cargo run -- demo`
4. **Submit a PR** with a clear description of what and why

## Areas where help is most welcome

- **Framework integrations** — OpenAI Agents SDK, Pydantic AI (native); LangGraph, CrewAI (wrapper)
- **Web UI** — browser-based timeline explorer
- **Testing** — more test coverage across all crates
- **Documentation** — usage guides, examples, tutorials
- **Platform support** — Windows testing, ARM Linux

## Code style

- Rust: Follow standard `rustfmt` formatting
- Python: PEP 8, type hints where practical
- Keep PRs focused — one feature or fix per PR

## Reporting issues

Open a [GitHub issue](https://github.com/agentoptics/rewind/issues) with:
- What you expected to happen
- What actually happened
- Steps to reproduce
- Your OS and Rust version (`rustc --version`)
