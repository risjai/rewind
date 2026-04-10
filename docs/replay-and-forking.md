# Replay and Forking

**Rewind** is a time-travel debugger for AI agents. It records every LLM call, then lets you rewind to any point in the execution, fix what went wrong, and replay forward — without re-running (or paying for) the steps that already succeeded.

This page covers replay from failure, forking and diffing timelines, and instant replay caching.

---

## Replay from failure

The headline feature. Your agent failed at step 5? Fix your code, then replay — steps 1-4 are served from cache (instant, free), only step 5 re-runs live.

### CLI

```bash
# Agent failed at step 5 — fix your code, then:
rewind replay latest --from 4
```

```
⏪ Rewind — Fork & Execute Replay

  Session: research-agent
  Fork at: Step 4
  Cached:  Steps 1-4 (0ms, 0 tokens)
  Live:    Steps 5+ (forwarded to upstream)

  → Point your agent at this proxy:
    export OPENAI_BASE_URL=http://127.0.0.1:8443/v1
```

### Python

Or from Python — no proxy needed:

```python
import rewind_agent

with rewind_agent.replay("latest", from_step=4):
    result = my_agent.run("Research Tokyo population")
    # Steps 1-4: instant cached responses (0ms, 0 tokens)
    # Step 5+: live LLM calls, recorded to new forked timeline
```

### Diff after replay

After the replay, diff the original against the replayed timeline:

```bash
rewind diff <session> main replayed
```

---

## Fork and diff

Fork creates a branch in the execution timeline at a specific step. Diff compares two timelines side by side.

```bash
# Create a fork at step 3
rewind fork <session-id> --at 3

# Compare two timelines
rewind diff <session-id> main fixed
```

### Timeline diff

```
⏪ Rewind — Timeline Diff

  main vs fixed (diverge at step 5)

  ═ Step  1  identical
  ═ Step  2  identical
  ═ Step  3  identical
  ═ Step  4  identical
  ≠ Step  5  [error] 700tok  →  [success] 715tok
```

Steps 1-4 are shared (zero re-execution). Only step 5 was re-run with corrected context.

Forks use structural sharing internally — forking at step 40 of a 50-step run uses zero storage for steps 1-40.

---

## Instant Replay — same task, 0 tokens

When you enable `--replay`, Rewind caches every successful LLM response. The next time your agent sends the exact same request, the cached response is returned instantly — no upstream call, no tokens burned.

```bash
# Enable caching
rewind record --name "my-agent" --upstream https://api.openai.com --replay
```

```
  Call 1: gpt-4o   320ms   156↓ 28↑    ← cache miss (hits upstream)
  Call 2: gpt-4o     0ms   156↓ 28↑    ← ⚡ cache hit (instant, 0 tokens)
  Call 3: gpt-4o   890ms   312↓ 35↑    ← cache miss (different request)
```

### Cache stats

```bash
rewind cache   # see stats
# Cached responses: 2
# Total cache hits: 1
# Tokens saved: 184
```

### How it works

Instant Replay operates at the transport layer. Each LLM request is hashed (SHA-256 of the full request body). On a cache hit, the stored response is returned immediately — no upstream call. This works with any LLM provider, any framework, any language.

This is especially useful for iterative development — re-run your agent 20 times while tweaking a prompt, and only the changed steps hit the LLM.

### The before/after

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

---

## Examples

See these example scripts for working code:

- [`examples/02_instant_replay.py`](../examples/02_instant_replay.py) — Instant Replay caching in action
- [`examples/06_replay_from_failure.py`](../examples/06_replay_from_failure.py) — Fork-and-execute replay from a failure point
