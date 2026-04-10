"""
Replay from Failure — Fork a session and re-run from a specific step.

When an agent fails at step N, you don't want to re-run steps 1 through N-1
(which costs tokens and time). Rewind lets you fork the timeline and replay
from the failure point — earlier steps are served from cache instantly.

Setup:
    # Terminal 1: Start the mock LLM server
    python demo/mock_llm.py 9999

    # Terminal 2: Run this script
    pip install rewind-agent openai
    python examples/06_replay_from_failure.py

    # After it finishes:
    rewind diff latest          # compare original vs forked timeline
    rewind inspect latest       # browse both timelines in TUI
"""

import openai
import rewind_agent

MOCK_URL = "http://127.0.0.1:9999/v1"


def run_agent(client):
    """Simulate a 3-step agent that makes LLM calls."""
    # Step 1
    resp1 = client.chat.completions.create(
        model="gpt-4o",
        messages=[{"role": "user", "content": "What is the population of Tokyo?"}],
    )
    # Step 2
    resp2 = client.chat.completions.create(
        model="gpt-4o",
        messages=[
            {"role": "user", "content": "What is the population of Tokyo?"},
            {"role": "assistant", "content": resp1.choices[0].message.content or "searching..."},
            {"role": "user", "content": "What about the decade trend?"},
        ],
    )
    # Step 3
    resp3 = client.chat.completions.create(
        model="gpt-4o",
        messages=[{"role": "user", "content": "Summarize the Tokyo population data."}],
    )
    return resp3


# ── Phase 1: Record the original session ──────────────────────────
print("=== Phase 1: Recording original session ===\n")

with rewind_agent.session("replay-demo"):
    client = openai.OpenAI(api_key="mock-key", base_url=MOCK_URL)
    result = run_agent(client)
    print(f"Original result: {result.choices[0].message.content or '[tool call]'}\n")

print("Original session recorded with 3 steps.")
print("Imagine step 3 produced a bad result. Let's replay from step 2.\n")

# ── Phase 2: Fork and replay from step 2 ─────────────────────────
print("=== Phase 2: Replaying from step 2 ===\n")

with rewind_agent.replay("latest", from_step=2):
    client = openai.OpenAI(api_key="mock-key", base_url=MOCK_URL)
    # Steps 1-2: served from cache (0ms, 0 tokens)
    # Step 3: re-executed with live LLM call
    result = run_agent(client)
    print(f"Replayed result: {result.choices[0].message.content or '[tool call]'}\n")

print("Done! The forked timeline reused steps 1-2 from cache.")
print("Run 'rewind diff latest' to compare the two timelines.")
