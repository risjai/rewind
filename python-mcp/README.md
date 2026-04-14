# rewind-mcp

MCP server for [Rewind](https://github.com/agentoptics/rewind) — the time-travel debugger for AI agents.

This package bootstraps the native `rewind-mcp` binary so AI assistants like **Claude Code**, **Cursor**, and **Windsurf** can query your agent recordings directly.

## Install

```bash
pip install rewind-mcp
```

The native binary is auto-downloaded on first use — no Rust toolchain required.

## Configure

Find the absolute path (IDEs don't inherit your shell PATH):

```bash
which rewind-mcp
```

### Claude Code

Add to `.claude/settings.json`:

```json
{
  "mcpServers": {
    "rewind": {
      "command": "/absolute/path/to/rewind-mcp"
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
      "command": "/absolute/path/to/rewind-mcp"
    }
  }
}
```

Replace `/absolute/path/to/rewind-mcp` with the output of `which rewind-mcp`.

## Full Documentation

See [MCP Server docs](https://github.com/agentoptics/rewind/blob/master/docs/mcp-server.md) for available tools and usage examples.

## License

MIT
