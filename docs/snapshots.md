# Snapshots

**Rewind** is a time-travel debugger for AI agents. It records every LLM call your agent makes and lets you inspect, fork, replay, diff, and evaluate agent behavior.

This guide covers workspace snapshots: capturing directory state before an agent runs and restoring it if something goes wrong.

---

## Overview

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

## Use Cases

- **Checkpoint before agent runs** — Take a snapshot of your workspace before letting an agent modify files. If it creates broken code, deletes important files, or goes off-track, restore to the clean state instantly.
- **Roll back on failure** — If your agent produces bad output or corrupts your project state, a single `rewind restore` command brings everything back to the exact state at snapshot time.

## CLI Commands

| Command | Description |
|:--------|:------------|
| `rewind snapshot [dir] --label <name>` | Capture workspace state as a checkpoint |
| `rewind restore <id\|label>` | Restore workspace from a snapshot |
| `rewind snapshots` | List all snapshots |

## Examples

See [`examples/11_snapshots.sh`](../examples/11_snapshots.sh) for a complete working example.
