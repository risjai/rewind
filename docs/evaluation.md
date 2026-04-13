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

## LLM-as-Judge

Use an LLM to score agent outputs on dimensions like correctness, coherence, and safety. This is the recommended approach for evaluating open-ended agent behavior where exact matching isn't practical.

### Setup

```bash
# Create an LLM judge evaluator
rewind eval evaluator create "quality" -t llm_judge -c correctness

# Requires OpenAI SDK: pip install rewind-agent[openai]
# Set your API key: export OPENAI_API_KEY=sk-...
```

### Score a session

```bash
# Score the main timeline
rewind eval score latest -e quality

# Score with expected output for reference
rewind eval score latest -e quality --expected '{"answer": "Tokyo"}'
```

### Compare original vs. forked timelines

```bash
# After a replay: compare all timelines
rewind eval score latest -e quality --compare-timelines
```

```
⏪ Rewind — Timeline Scores

  Session: research-agent
  Evaluators: quality

  Timeline       quality    avg
  ────────────   ───────    ────
  main           0.200      0.200
  fixed          0.900      0.900

  Delta (fixed vs main): +0.70 avg  ✓
```

### Built-in Criteria

| Criteria | What It Scores | Needs `expected`? |
|:---------|:---------------|:------------------|
| `correctness` | Factual accuracy against a reference answer | Yes |
| `coherence` | Logical flow and clarity | No |
| `relevance` | Whether the response addresses the query | No |
| `safety` | Whether content is harmful or toxic | No |
| `task_completion` | Whether the agent accomplished the task | No |

### Config options

```bash
# Full JSON config
rewind eval evaluator create "custom-judge" -t llm_judge \
  -c '{"criteria":"correctness","model":"gpt-4o","temperature":0}'

# Use a different API endpoint (Ollama, vLLM, LiteLLM)
rewind eval evaluator create "local-judge" -t llm_judge \
  -c '{"criteria":"safety","api_key_env":"OLLAMA_API_KEY","api_base_env":"OLLAMA_BASE_URL"}'
```

### Python SDK

```python
import rewind_agent

result = rewind_agent.evaluate(
    dataset=ds,
    target_fn=my_agent,
    evaluators=["llm_judge"],  # default: correctness + gpt-4o-mini
)

# Or with custom criteria
result = rewind_agent.evaluate(
    dataset=ds,
    target_fn=my_agent,
    evaluators=[
        rewind_agent.llm_judge_evaluator(criteria="correctness", model="gpt-4o"),
        rewind_agent.llm_judge_evaluator(criteria="safety"),
    ],
)
```

### Cost awareness

LLM judge calls are paid API calls. Rough estimates per dataset example:
- `gpt-4o-mini`: ~$0.001/call
- `gpt-4o`: ~$0.025/call

Scores are cached per timeline+evaluator. Use `--force` to re-score.

---

## Built-in Evaluators

| Evaluator | Description |
|:----------|:------------|
| `exact_match` | Output must exactly match the expected value |
| `contains` | Output must contain a specified substring |
| `regex` | Output must match a regular expression pattern |
| `json_schema` | Output must validate against a JSON schema |
| `tool_use_match` | Checks that the agent used the expected tools |
| `llm_judge` | LLM scores output on criteria (correctness, coherence, safety, etc.) |

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
| `rewind eval evaluator create <name> -t <type>` | Create an evaluator (exact_match, contains, regex, json_schema, tool_use_match, llm_judge) |
| `rewind eval run <dataset> -c <cmd> -e <evaluator>` | Run experiment — execute command per example, score, aggregate |
| `rewind eval compare <left> <right>` | Compare two experiments side-by-side |
| `rewind eval show <experiment>` | Show detailed experiment results |
| `rewind eval experiments` | List all experiments |
| `rewind eval score <session> -e <evaluator>` | Score a session's timeline outputs (LLM-as-judge) |

## Examples

- [`examples/08_evaluation.py`](../examples/08_evaluation.py) — Basic evaluation workflow
- [`examples/12_custom_evaluator.py`](../examples/12_custom_evaluator.py) — Custom evaluator with the Python SDK
- [`examples/13_llm_judge.py`](../examples/13_llm_judge.py) — LLM-as-judge evaluation
- [`examples/14_fork_and_score.py`](../examples/14_fork_and_score.py) — Fork, replay, and score timelines
