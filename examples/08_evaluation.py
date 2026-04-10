"""
Evaluation — Create a dataset, run experiments, compare results.

Rewind's evaluation system lets you measure agent quality across a dataset
of input/expected pairs. Score with built-in evaluators (exact_match,
contains_match, regex_match, tool_use_match) or custom ones.

No LLM or mock server needed — this example is pure Python.

Setup:
    pip install rewind-agent
    # Ensure the Rust CLI is in PATH (not the Python shim):
    export PATH="./target/release:$PATH"
    python examples/08_evaluation.py
"""

from rewind_agent import Dataset, evaluate, compare, exact_match

# ── Step 1: Create a dataset ─────────────────────────────────────
print("=== Step 1: Creating dataset ===\n")

ds = Dataset("capital-cities")
ds.add(input={"question": "What is the capital of France?"}, expected={"answer": "Paris"})
ds.add(input={"question": "What is the capital of Japan?"}, expected={"answer": "Tokyo"})
ds.add(input={"question": "What is the capital of Brazil?"}, expected={"answer": "Brasilia"})

print(f"Created dataset '{ds.name}' with {ds.count} examples (v{ds.version})\n")


# ── Step 2: Define a target function ──────────────────────────────
# In production, this would call your actual agent. Here we simulate it.

def my_agent_v1(input_data: dict) -> dict:
    """Simulated agent that gets 2 out of 3 correct."""
    answers = {
        "What is the capital of France?": "Paris",
        "What is the capital of Japan?": "Tokyo",
        "What is the capital of Brazil?": "Rio de Janeiro",  # Wrong!
    }
    question = input_data["question"]
    return {"answer": answers.get(question, "I don't know")}


def my_agent_v2(input_data: dict) -> dict:
    """Improved agent that gets all 3 correct."""
    answers = {
        "What is the capital of France?": "Paris",
        "What is the capital of Japan?": "Tokyo",
        "What is the capital of Brazil?": "Brasilia",  # Fixed!
    }
    question = input_data["question"]
    return {"answer": answers.get(question, "I don't know")}


# ── Step 3: Run evaluation with v1 ───────────────────────────────
print("=== Step 2: Evaluating agent v1 ===\n")

result_v1 = evaluate(
    dataset=ds,
    target_fn=my_agent_v1,
    evaluators=[exact_match],
    name="capital-cities-v1",

)

print(f"  Avg score: {result_v1.avg_score:.1%}")
print(f"  Pass rate: {result_v1.pass_rate:.1%}")
print(f"  Examples:  {result_v1.total_examples}")

for r in result_v1.results:
    status = "PASS" if r.scores[0]["score"].passed else "FAIL"
    print(f"    [{status}] {r.input['question']} -> {r.output['answer']}")

print()

# ── Step 4: Run evaluation with v2 ───────────────────────────────
print("=== Step 3: Evaluating agent v2 ===\n")

result_v2 = evaluate(
    dataset=ds,
    target_fn=my_agent_v2,
    evaluators=[exact_match],
    name="capital-cities-v2",

)

print(f"  Avg score: {result_v2.avg_score:.1%}")
print(f"  Pass rate: {result_v2.pass_rate:.1%}")
print()

# ── Step 5: Compare the two experiments ───────────────────────────
print("=== Step 4: Comparing v1 vs v2 ===\n")

diff = compare(result_v1, result_v2)

print(f"  Score delta:     {diff.score_delta:+.1%}")
print(f"  Pass rate delta: {diff.pass_rate_delta:+.1%}")
print(f"  Improved: {diff.improved}")
print(f"  Regressed: {diff.regressed}")

print("\n  Per-example breakdown:")
for ex in diff.per_example:
    print(f"    Example {ex['ordinal']}: {ex['left_score']:.0%} -> {ex['right_score']:.0%} ({ex['delta']:+.0%})")

# ── CI usage ──────────────────────────────────────────────────────
print("\n--- CI Usage ---")
print("Use fail_below to gate deployments:")
print()
print("  result = evaluate(ds, my_agent, evaluators=[exact_match], fail_below=0.9)")
print("  # Raises EvalFailedError if avg_score < 90%")
