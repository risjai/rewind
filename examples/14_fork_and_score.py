"""
Fork and Score — The Rewind differentiator.

This example demonstrates the full "replay -> score -> prove it works" loop:

1. Record an agent session (simulated)
2. Fork the timeline at a failure point
3. Score both timelines with LLM-as-judge
4. Compare scores to prove the fix works

This is a conceptual walkthrough — the CLI commands below show the real workflow.

Setup:
    pip install rewind-agent[openai]
    export OPENAI_API_KEY=sk-...
    export PATH="./target/release:$PATH"
"""

print("""
=== Fork and Score Workflow ===

Rewind lets you prove that a fix actually works by scoring both the
original and forked timelines with LLM-as-judge evaluators.

--- Step 1: Record an agent session ---

  Your agent runs and Rewind records every LLM call:

    $ rewind sessions
    ID        Name              Steps  Status
    abc123    research-agent    12     completed

--- Step 2: Create an LLM judge evaluator ---

  $ rewind eval evaluator create "quality" -t llm_judge -c correctness
  $ rewind eval evaluator create "safe" -t llm_judge -c safety

--- Step 3: Score the original timeline ---

  $ rewind eval score latest -e quality -e safe

  ⏪ Rewind — Timeline Scores

    Session: research-agent
    Evaluators: quality, safe

    Timeline     quality   safe    avg
    ──────────   ───────   ────    ────
    main         0.200     1.000   0.600

--- Step 4: Fork and replay from the failure point ---

  $ rewind replay latest --from 5

  This creates a new "fixed" timeline from step 5 onward.

--- Step 5: Score all timelines and compare ---

  $ rewind eval score latest -e quality -e safe --compare-timelines

  ⏪ Rewind — Timeline Scores

    Session: research-agent
    Evaluators: quality, safe

    Timeline     quality   safe    avg
    ──────────   ───────   ────    ────
    main         0.200     1.000   0.600
    fixed        0.900     1.000   0.950

    Delta (fixed vs main): +0.35 avg  ✓

--- Step 6: Re-score after changes (use --force) ---

  $ rewind eval score latest -e quality --compare-timelines --force

  The --force flag bypasses the score cache to re-evaluate.

--- Step 7: JSON output for CI ---

  $ rewind eval score latest -e quality --compare-timelines --json

  [
    {"timeline_label": "main", "avg_score": 0.6, ...},
    {"timeline_label": "fixed", "avg_score": 0.95, ...}
  ]

=== Python SDK equivalent ===
""")

# The Python SDK can do this in-process:
print("""
import rewind_agent

# Use llm_judge in evaluate()
result = rewind_agent.evaluate(
    dataset=ds,
    target_fn=my_agent,
    evaluators=[
        rewind_agent.llm_judge_evaluator(criteria="correctness", model="gpt-4o"),
        rewind_agent.llm_judge_evaluator(criteria="safety"),
    ],
    name="agent-v2",
)
print(f"Score: {result.avg_score:.2f}")

# Or use the string shorthand for default config:
result = rewind_agent.evaluate(
    dataset=ds,
    target_fn=my_agent,
    evaluators=["llm_judge"],  # default: correctness + gpt-4o-mini
)
""")

print("=== Summary ===")
print()
print("  Langfuse tells you what happened.")
print("  Rewind lets you fix it.")
print()
print("  rewind replay -> rewind eval score -> proof the fix works")
