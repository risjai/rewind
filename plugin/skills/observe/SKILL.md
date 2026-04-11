---
name: observe
description: Observe and debug Claude Code sessions with Rewind. Use when the user wants to see what happened in an agent session, inspect tool calls, check session status, or open the Rewind dashboard.
---

# Rewind Observe

Check the status of Rewind's Claude Code observation and open the dashboard.

## Usage

When the user asks about Rewind status, session observation, or wants to debug agent behavior:

1. Check if the Rewind server is running:
   ```bash
   rewind hooks status
   ```

2. List recorded sessions:
   ```bash
   rewind sessions
   ```

3. Show details of a specific session:
   ```bash
   rewind show <session-id>
   ```

4. Open the web dashboard:
   ```bash
   rewind web --port 4800
   ```
   Then direct the user to http://127.0.0.1:4800

## What Rewind Captures

When the Rewind server is running (`rewind web`), it automatically captures:
- Every tool call (Read, Edit, Bash, Write, Grep, Agent, MCP tools)
- User prompts
- Session lifecycle events
- Token usage (from Claude Code transcript files)

## Troubleshooting

If sessions aren't being recorded:
1. Check hooks are installed: `rewind hooks status`
2. Check server is running: `curl -s http://127.0.0.1:4800/api/health`
3. Check for buffered events: look at `~/.rewind/hooks/buffer.jsonl`
