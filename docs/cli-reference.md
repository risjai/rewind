# CLI Reference

Rewind is a time-travel debugger for AI agents -- a single binary with zero dependencies. Below is the complete command reference.

## Commands

| Command | Description |
|:--------|:------------|
| `rewind record [--replay]` | Start the recording proxy. `--replay` enables instant replay caching. |
| `rewind sessions` | List all recorded sessions |
| `rewind show <id\|latest>` | Print a session's step-by-step trace |
| `rewind inspect <id\|latest>` | Open the interactive TUI |
| `rewind replay <id> --from <step>` | Replay from a fork point -- cached steps instant, live from fork onward |
| `rewind fork <id> --at <step>` | Create a timeline branch at a specific step |
| `rewind diff <id> <left> <right>` | Compare two timelines side by side |
| `rewind snapshot [dir] --label <name>` | Capture workspace state as a checkpoint |
| `rewind restore <id\|label>` | Restore workspace from a snapshot |
| `rewind snapshots` | List all snapshots |
| `rewind cache` | Show instant replay cache statistics |
| `rewind assert baseline <id> --name <name>` | Create a regression baseline from a session |
| `rewind assert check <id> --against <name>` | Check a session against a baseline |
| `rewind assert list` | List all baselines |
| `rewind assert show <name>` | Show baseline step signatures |
| `rewind assert delete <name>` | Delete a baseline |
| `rewind eval dataset create <name>` | Create a new evaluation dataset |
| `rewind eval dataset import <name> <file.jsonl>` | Import test cases from JSONL |
| `rewind eval dataset show <name>` | Show dataset with example previews |
| `rewind eval evaluator create <name> -t <type>` | Create an evaluator (exact_match, contains, regex, json_schema, tool_use_match) |
| `rewind eval run <dataset> -c <cmd> -e <evaluator>` | Run experiment -- execute command per example, score, aggregate |
| `rewind eval compare <left> <right>` | Compare two experiments side-by-side |
| `rewind eval show <experiment>` | Show detailed experiment results |
| `rewind eval experiments` | List all experiments |
| `rewind fix <session>` | AI-powered diagnosis -- find root cause, suggest fix, optionally fork + replay with patch ([guide](fix.md)) |
| `rewind fix <session> --apply` | Apply the suggested fix: fork, start proxy with rewrites, wait for re-run |
| `rewind fix <session> --apply -c <cmd>` | Fully automated: diagnose, fork, run agent, report savings |
| `rewind fix <session> --hypothesis <fix>` | Skip diagnosis, test a fix directly (e.g., `swap_model:gpt-4o`) |
| `rewind eval score <session> -e <evaluator>` | Score a session's timeline with LLM-as-judge or other evaluators |
| `rewind share <session>` | Generate a self-contained HTML file for sharing (no install needed) |
| `rewind import otel --json-file <path>` | Import OTLP traces from JSON or protobuf files |
| `rewind import from-langfuse --trace <id>` | Import a trace from Langfuse by ID |
| `rewind hooks install` | Install Claude Code hooks for session observation |
| `rewind export otel <session>` | Export a session as OpenTelemetry traces via OTLP ([guide](otel-export.md)) |
| `rewind query "SQL"` | Run a read-only SQL query against the Rewind database |
| `rewind query --tables` | Show all tables and their column schemas |
| `rewind web [--port 4800]` | Start the web dashboard (flight recorder + live) |
| `rewind record --web` | Start recording with live web dashboard |
| `rewind demo` | Seed demo data to explore without API keys |
