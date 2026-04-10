# Evaluation

**Rewind** is a time-travel debugger for AI agents. It records every LLM call your agent makes and lets you inspect, fork, replay, diff, and evaluate agent behavior.

This guide covers the evaluation system: creating datasets, running experiments, scoring with evaluators, and comparing results.

---

## Overview

Go beyond structural regression checks. Create datasets of test cases, run your agent against them, score the outputs, and compare experiments side-by-side.

## CLI Workflow

### 1. Create a dataset with test cases

```bash
rewind eval dataset create "booking-test" -d "Booking agent eval"
```

### 2. Import test cases from a JSONL file

```bash
cat > test_cases.jsonl << 'EOF'
{"input":{"query":"Book a table for 2 at 7pm"},"expected":{"action":"create_booking","guests":2}}
{"input":{"query":"Cancel my reservation"},"expected":{"action":"cancel_booking"}}
EOF
rewind eval dataset import "booking-test" test_cases.jsonl
```

### 3. Create evaluators

```bash
rewind eval evaluator create "action-check" -t contains -c '{"substring":"booking"}'
rewind eval evaluator create "schema-valid" -t json_schema -c '{"schema":{"required":["action"]}}'
```

### 4. Run your agent against the dataset

```bash
rewind eval run "booking-test" -c "python my_agent.py" \
    -e action-check -e schema-valid --name "v1-baseline"
```

```
⏪ Rewind — Running Experiment

  Dataset: booking-test
  Command: python my_agent.py
  Evaluators: action-check, schema-valid

  Experiment: v1-baseline
  Status: completed
  Examples: 2
  Avg Score: 0.750
  Pass Rate: 75.0%

  ✓ #1   1.000 1.00 | 1.00
  ✓ #2   0.500 0.00 | 1.00
```

## Compare Experiments

After code changes, run a new experiment and compare (e.g., PR vs main):

```bash
# Run candidate experiment
rewind eval run "booking-test" -c "python my_agent_v2.py" \
    -e action-check -e schema-valid --name "v2-candidate"

# Compare side-by-side
rewind eval compare "v1-baseline" "v2-candidate"
```

```
⏪ Rewind — Experiment Comparison

  Comparing: v1-baseline (avg: 0.750) vs v2-candidate (avg: 1.000)

  Overall delta: +0.250
  Summary: 0 regressions, 1 improvements, 1 unchanged

  Changes:
    ▲ #2   0.50 → 1.00 (+0.500) {"query":"Cancel my reservation...
```

## CI Integration

Fail the build if quality drops below a threshold:

```bash
rewind eval run "booking-test" -c "./agent" \
    -e action-check --fail-below 0.8 --json
# Exit code 1 if avg_score < 0.8
# JSON output for dashboard ingestion
```

## Built-in Evaluators

| Evaluator | Description |
|:----------|:------------|
| `exact_match` | Output must exactly match the expected value |
| `contains` | Output must contain a specified substring |
| `regex` | Output must match a regular expression pattern |
| `json_schema` | Output must validate against a JSON schema |
| `tool_use_match` | Checks that the agent used the expected tools |

## Python SDK

```python
import rewind_agent

ds = rewind_agent.Dataset("my-test")
ds.add(input={"q": "hello"}, expected={"a": "hi"})

@rewind_agent.evaluator("custom_check")
def custom_check(input, output, expected):
    return rewind_agent.EvalScore(
        score=1.0 if output.get("a") == expected.get("a") else 0.0,
        passed=output.get("a") == expected.get("a"),
        reasoning="Answer match check"
    )

result = rewind_agent.evaluate(
    dataset=ds,
    target_fn=my_agent,
    evaluators=[custom_check, "exact_match"],
    fail_below=0.8,
)
print(f"Score: {result.avg_score:.2f}, Pass rate: {result.pass_rate:.0%}")
```

## CLI Commands

| Command | Description |
|:--------|:------------|
| `rewind eval dataset create <name>` | Create a new evaluation dataset |
| `rewind eval dataset import <name> <file.jsonl>` | Import test cases from JSONL |
| `rewind eval dataset show <name>` | Show dataset with example previews |
| `rewind eval evaluator create <name> -t <type>` | Create an evaluator (exact_match, contains, regex, json_schema, tool_use_match) |
| `rewind eval run <dataset> -c <cmd> -e <evaluator>` | Run experiment — execute command per example, score, aggregate |
| `rewind eval compare <left> <right>` | Compare two experiments side-by-side |
| `rewind eval show <experiment>` | Show detailed experiment results |
| `rewind eval experiments` | List all experiments |

## Examples

- [`examples/08_evaluation.py`](../examples/08_evaluation.py) — Basic evaluation workflow
- [`examples/12_custom_evaluator.py`](../examples/12_custom_evaluator.py) — Custom evaluator with the Python SDK
