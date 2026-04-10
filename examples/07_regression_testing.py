"""
Regression Testing — Create a baseline and check new sessions against it.

Rewind's assertion system captures the "shape" of a known-good agent run
(step types, models, token counts, tool calls) and checks that future runs
don't regress. This is the CI gate for agent quality.

Setup:
    # Terminal 1: Start the mock LLM server
    python demo/mock_llm.py 9999

    # Terminal 2: Run this script
    pip install rewind-agent openai
    python examples/07_regression_testing.py

    # For CI integration, see the GitHub Action:
    #   agentoptics/rewind/action@v1
"""

import subprocess
import openai
import rewind_agent
from rewind_agent import Assertions

MOCK_URL = "http://127.0.0.1:9999/v1"


def run_agent():
    """Simulate a 3-step agent."""
    client = openai.OpenAI(api_key="mock-key", base_url=MOCK_URL)
    client.chat.completions.create(
        model="gpt-4o",
        messages=[{"role": "user", "content": "What is the population of Tokyo?"}],
    )
    client.chat.completions.create(
        model="gpt-4o",
        messages=[{"role": "user", "content": "What about the decade trend?"}],
    )
    client.chat.completions.create(
        model="gpt-4o",
        messages=[{"role": "user", "content": "Summarize the population data."}],
    )


# ── Step 1: Record a known-good session ───────────────────────────
print("=== Step 1: Recording known-good session ===\n")

with rewind_agent.session("baseline-run"):
    run_agent()
print("Recorded 3-step session.\n")

# ── Step 2: Create a baseline from it ─────────────────────────────
print("=== Step 2: Creating baseline ===\n")

subprocess.run(
    ["rewind", "assert", "baseline", "latest", "--name", "tokyo-agent-v1"],
    check=True,
)
print()

# ── Step 3: Run the agent again and check ─────────────────────────
print("=== Step 3: Running agent again and checking ===\n")

with rewind_agent.session("regression-check"):
    run_agent()

result = Assertions().check("tokyo-agent-v1", "latest")

print(f"Passed: {result.passed}")
print(f"Checks: {result.passed_checks}/{result.total_checks} passed, {result.warnings} warnings")

if result.passed:
    print("\nNo regressions detected!")
else:
    print(f"\nREGRESSION: {result.failed_checks} checks failed")
    for step_result in result.step_results:
        if step_result.get("verdict") != "pass":
            print(f"  Step {step_result['step_number']}: {step_result['verdict']}")

# ── CI usage ──────────────────────────────────────────────────────
print("\n--- CI Usage ---")
print("In your CI pipeline, assert and fail the build on regression:")
print()
print("  result = Assertions().check('tokyo-agent-v1', 'latest')")
print("  assert result.passed, f'Regression: {result.failed_checks} checks failed'")
