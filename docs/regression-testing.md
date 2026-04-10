# Regression Testing

**Rewind** is a time-travel debugger for AI agents. It records every LLM call your agent makes and lets you inspect, fork, replay, diff, and evaluate agent behavior.

This guide covers regression testing: turning recorded sessions into baselines and checking new sessions for regressions.

---

## Overview

Turn any recorded session into a regression baseline. After changing prompts or code, check the new behavior against the baseline:

```bash
# Create a baseline from a known-good session
rewind assert baseline latest --name "booking-happy-path"

# After changes, check the new session for regressions
rewind assert check latest --against "booking-happy-path"
```

```
⏪ Rewind — Assertion Check

  Baseline: booking-happy-path (5 steps)
  Session:  a3f9e28c (latest)
  Tolerance: tokens ±20%, model changes = fail

  ┌ Step  1  🧠 LLM Call   ✓ PASS  match
  ├ Step  2  📋 Tool Result ✓ PASS  match
  ├ Step  3  🧠 LLM Call   ✓ PASS  tokens OK (312→298, -4.5%)
  ├ Step  4  📋 Tool Result ✓ PASS  match
  └ Step  5  🧠 LLM Call   ✗ FAIL  NEW ERROR: hallucination

  Result: FAILED (4 passed, 1 failed, 0 warnings)
```

Checks step types, models, tool calls, error status, and token usage.

## Tolerance Configuration

By default, assertions use the following tolerance rules:

- **Tokens**: ±20% change is allowed (e.g., 312 to 298 tokens is -4.5%, within tolerance)
- **Model changes**: Any model change is a failure (e.g., switching from `gpt-4o` to `gpt-3.5-turbo`)

## Python API

```python
from rewind_agent import Assertions

result = Assertions().check("booking-happy-path", "latest")
assert result.passed, f"Regression: {result.failed_checks} checks failed"
```

## GitHub Actions Integration

Add agent regression testing to your CI in 3 lines:

```yaml
- uses: agentoptics/rewind/action@v1
  with:
    baseline: "booking-happy-path"
```

The action installs Rewind, runs `rewind assert check` against your baseline, and fails the job if regressions are found. Results are written to the GitHub Step Summary. See [action/README.md](../action/README.md) for full docs.

## CLI Commands

| Command | Description |
|:--------|:------------|
| `rewind assert baseline <id> --name <name>` | Create a regression baseline from a session |
| `rewind assert check <id> --against <name>` | Check a session against a baseline |
| `rewind assert list` | List all baselines |
| `rewind assert show <name>` | Show baseline step signatures |
| `rewind assert delete <name>` | Delete a baseline |

## Examples

See [`examples/07_regression_testing.py`](../examples/07_regression_testing.py) for a complete working example.
