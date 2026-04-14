# MCP Server -- AI Assistant Integration

Rewind is a time-travel debugger for AI agents that records every LLM call for inspection, forking, replay, and diffing. It ships an MCP (Model Context Protocol) server so AI assistants like **Claude Code**, **Cursor**, and **Windsurf** can query your agent recordings directly.

## Install

### Option 1: `cargo install` (easiest)

```bash
cargo install --git https://github.com/agentoptics/rewind rewind-mcp
```

This puts `rewind-mcp` in your Cargo bin directory (usually `~/.cargo/bin/`).

### Option 2: Build from source

```bash
git clone https://github.com/agentoptics/rewind.git
cd rewind
cargo build --release -p rewind-mcp
```

The binary will be at `./target/release/rewind-mcp`.

> **Note:** `rewind-mcp` is a separate binary from the `rewind` CLI.
> Installing via `pip install rewind-agent` does **not** include the MCP server.
> You need Rust (`cargo`) to build it — install Rust at https://rustup.rs if needed.

## Configure

Find the full path to the binary — IDE MCP clients don't inherit your shell PATH:

```bash
which rewind-mcp
# typical output: /Users/you/.cargo/bin/rewind-mcp
```

### Claude Code

Add to `.claude/settings.json`:

```json
{
  "mcpServers": {
    "rewind": {
      "command": "/Users/you/.cargo/bin/rewind-mcp"
    }
  }
}
```

### Cursor

Add to `.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "rewind": {
      "command": "/Users/you/.cargo/bin/rewind-mcp"
    }
  }
}
```

Replace `/Users/you/.cargo/bin/rewind-mcp` with the output of `which rewind-mcp`.

## Available Tools

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
| `list_eval_datasets` | List evaluation datasets with example counts |
| `show_eval_dataset` | Show dataset details with example previews |
| `list_eval_experiments` | List experiments, filter by dataset |
| `show_eval_experiment` | Show experiment results with per-example scores |
| `compare_eval_experiments` | Compare two experiments side-by-side |

## Example Usage

Once configured, ask your AI assistant:

> "Why did my agent fail on the research task?"

The assistant calls `show_session` -> reads the trace -> identifies that step 4 returned stale data -> explains the hallucination in step 5.

## See Also

- [OpenAI Agents SDK integration](openai-agents-sdk.md)
- [Framework integrations](framework-integrations.md)
