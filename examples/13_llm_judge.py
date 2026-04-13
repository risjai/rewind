"""
LLM-as-Judge Evaluation — Use an LLM to score agent outputs.

Rewind's llm_judge evaluator uses function calling to get structured
scores from an LLM on criteria like correctness, coherence, and safety.

Setup:
    pip install rewind-agent[openai]
    export OPENAI_API_KEY=sk-...
    export PATH="./target/release:$PATH"
    python examples/13_llm_judge.py
"""

from rewind_agent import (
    Dataset,
    evaluate,
    llm_judge_evaluator,
    EvalScore,
    evaluator,
)


# ── Step 1: Create a Q&A dataset ────────────────────────────────
print("=== Step 1: Creating dataset ===\n")

ds = Dataset("geography-qa")
ds.add(
    input={"question": "What is the capital of France?"},
    expected={"answer": "Paris"},
)
ds.add(
    input={"question": "What is the largest ocean?"},
    expected={"answer": "The Pacific Ocean is the largest ocean on Earth."},
)
ds.add(
    input={"question": "Who wrote Romeo and Juliet?"},
    expected={"answer": "William Shakespeare"},
)

print(f"Created dataset '{ds.name}' with {ds.count} examples\n")


# ── Step 2: Define an imperfect agent ────────────────────────────
def my_agent(input_data: dict) -> dict:
    """Simulated agent — mostly correct but with stylistic differences."""
    answers = {
        "What is the capital of France?": "The capital of France is Paris.",
        "What is the largest ocean?": "Pacific Ocean",
        "Who wrote Romeo and Juliet?": "Shakespeare wrote it in the 1590s.",
    }
    return {"answer": answers.get(input_data["question"], "I don't know")}


# ── Step 3: Evaluate with LLM-as-judge ───────────────────────────
print("=== Step 2: Evaluating with LLM-as-judge ===\n")

# The llm_judge evaluator uses an LLM to score outputs.
# Criteria "correctness" compares the submission against the expected answer,
# ignoring stylistic differences (unlike exact_match which would fail on all 3).

result = evaluate(
    dataset=ds,
    target_fn=my_agent,
    evaluators=[
        llm_judge_evaluator(criteria="correctness", model="gpt-4o-mini"),
        llm_judge_evaluator(criteria="coherence"),
    ],
    name="geography-llm-judge",
)

print(f"  Avg score: {result.avg_score:.1%}")
print(f"  Pass rate: {result.pass_rate:.1%}")
print(f"  Examples:  {result.total_examples}")
print()

for r in result.results:
    print(f"  Q: {r.input['question']}")
    print(f"  A: {r.output['answer']}")
    for s in r.scores:
        name = s.get("evaluator", "?")
        score = s["score"]
        print(f"    {name}: {score.score:.2f} — {score.reasoning[:80]}")
    print()


# ── Step 4: Compare with exact_match ─────────────────────────────
print("=== Step 3: Compare LLM judge vs exact_match ===\n")
print("Note: exact_match would score 0/3 (stylistic differences).")
print("LLM judge understands that 'Paris' and 'The capital of France is Paris'")
print("are factually equivalent.\n")

# You can also use the built-in name as a string:
# evaluators=["llm_judge"]  — uses default: correctness + gpt-4o-mini
