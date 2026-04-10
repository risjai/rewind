"""
Custom Evaluator — Define domain-specific scoring with @evaluator().

Built-in evaluators (exact_match, contains_match) work for simple cases.
For domain-specific logic — like checking if a summary covers key topics
or if a code generation passes tests — write a custom evaluator.

No LLM or mock server needed — this example is pure Python.

Setup:
    pip install rewind-agent
    # Ensure the Rust CLI is in PATH (not the Python shim):
    export PATH="./target/release:$PATH"
    python examples/12_custom_evaluator.py
"""

from rewind_agent import (
    Dataset, evaluate, compare, evaluator, EvalScore, exact_match,
)


# ── Define custom evaluators ──────────────────────────────────────

@evaluator("keyword_coverage")
def keyword_coverage(input: dict, output: dict, expected: dict) -> EvalScore:
    """
    Check what fraction of required keywords appear in the output.
    Useful for evaluating whether summaries cover key topics.
    """
    required = expected.get("keywords", [])
    if not required:
        return EvalScore(score=1.0, passed=True, reasoning="No keywords to check")

    output_text = str(output).lower()
    found = [kw for kw in required if kw.lower() in output_text]
    score = len(found) / len(required)

    missing = [kw for kw in required if kw.lower() not in output_text]
    reasoning = f"Found {len(found)}/{len(required)} keywords"
    if missing:
        reasoning += f". Missing: {missing}"

    return EvalScore(score=score, passed=score >= 0.8, reasoning=reasoning)


@evaluator("format_check")
def format_check(input: dict, output: dict, expected: dict) -> EvalScore:
    """Check that the output matches the expected format (e.g., has required keys)."""
    required_keys = expected.get("required_keys", [])
    if not required_keys:
        return EvalScore(score=1.0, passed=True, reasoning="No format requirements")

    present = [k for k in required_keys if k in output]
    score = len(present) / len(required_keys)

    missing = [k for k in required_keys if k not in output]
    reasoning = f"Has {len(present)}/{len(required_keys)} required keys"
    if missing:
        reasoning += f". Missing: {missing}"

    return EvalScore(score=score, passed=score >= 1.0, reasoning=reasoning)


# ── Create a dataset ──────────────────────────────────────────────
print("=== Creating dataset ===\n")

ds = Dataset("summary-quality")
ds.add(
    input={"topic": "machine learning"},
    expected={
        "keywords": ["training", "model", "data", "prediction"],
        "required_keys": ["summary", "confidence"],
    },
)
ds.add(
    input={"topic": "climate change"},
    expected={
        "keywords": ["temperature", "emissions", "carbon", "warming"],
        "required_keys": ["summary", "confidence"],
    },
)
ds.add(
    input={"topic": "quantum computing"},
    expected={
        "keywords": ["qubit", "superposition", "entanglement"],
        "required_keys": ["summary", "confidence"],
    },
)
print(f"Dataset: {ds.count} examples\n")


# ── Simulated agents ─────────────────────────────────────────────

def agent_v1(input_data: dict) -> dict:
    """Basic agent — hits some keywords, misses the format."""
    topic = input_data["topic"]
    summaries = {
        "machine learning": {
            "summary": "ML uses training data to build a model for prediction.",
            # Has confidence ✓
            "confidence": 0.9,
        },
        "climate change": {
            "summary": "Global temperature is rising due to carbon emissions.",
            # Missing "warming" keyword
            "confidence": 0.85,
        },
        "quantum computing": {
            # Missing "entanglement" keyword
            "text": "Qubits use superposition to compute in parallel.",
            # Missing required keys: summary, confidence
        },
    }
    return summaries.get(topic, {"summary": "Unknown topic", "confidence": 0.0})


def agent_v2(input_data: dict) -> dict:
    """Improved agent — better keyword coverage and format compliance."""
    topic = input_data["topic"]
    summaries = {
        "machine learning": {
            "summary": "ML uses training data to build a model for prediction and classification.",
            "confidence": 0.95,
        },
        "climate change": {
            "summary": "Global warming causes temperature rise due to carbon emissions from human activity.",
            "confidence": 0.9,
        },
        "quantum computing": {
            "summary": "Qubits exploit superposition and entanglement for computation.",
            "confidence": 0.88,
        },
    }
    return summaries.get(topic, {"summary": "Unknown topic", "confidence": 0.0})


# ── Run evaluations ──────────────────────────────────────────────
print("=== Evaluating agent v1 ===\n")

result_v1 = evaluate(
    dataset=ds,
    target_fn=agent_v1,
    evaluators=[keyword_coverage, format_check],
    name="summary-v1",

)

print(f"  Avg score: {result_v1.avg_score:.1%}")
print(f"  Pass rate: {result_v1.pass_rate:.1%}")
for r in result_v1.results:
    print(f"    [{r.input['topic']}]")
    for s in r.scores:
        print(f"      {s['evaluator']}: {s['score'].score:.0%} — {s['score'].reasoning}")
print()

print("=== Evaluating agent v2 ===\n")

result_v2 = evaluate(
    dataset=ds,
    target_fn=agent_v2,
    evaluators=[keyword_coverage, format_check],
    name="summary-v2",

)

print(f"  Avg score: {result_v2.avg_score:.1%}")
print(f"  Pass rate: {result_v2.pass_rate:.1%}")
print()

# ── Compare ───────────────────────────────────────────────────────
print("=== Comparing v1 vs v2 ===\n")

diff = compare(result_v1, result_v2)
print(f"  Score delta:     {diff.score_delta:+.1%}")
print(f"  Pass rate delta: {diff.pass_rate_delta:+.1%}")
print(f"  Improved: {diff.improved}")

print("\nDone! Custom evaluators let you define exactly what 'good' means for your agent.")
