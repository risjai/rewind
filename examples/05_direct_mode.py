"""
Direct Mode — Record LLM calls with one line of code, no proxy needed.

Unlike proxy mode (examples 01-04), direct mode monkey-patches the OpenAI
and Anthropic SDKs in-process. Just call rewind_agent.init() and every
LLM call is recorded automatically.

Setup:
    # Terminal 1: Start the mock LLM server (no API key needed)
    python demo/mock_llm.py 9999

    # Terminal 2: Run this script
    pip install rewind-agent openai
    python examples/05_direct_mode.py

    # After it finishes:
    rewind show latest
    rewind inspect latest
"""

import openai
import rewind_agent

# One line to start recording — everything after this is captured
rewind_agent.init(session_name="direct-mode-demo")

# Point at the mock LLM (in production, just use your real API key)
client = openai.OpenAI(api_key="mock-key", base_url="http://127.0.0.1:9999/v1")

# Step 1: Ask a question
print("Step 1: Asking about Tokyo...")
resp1 = client.chat.completions.create(
    model="gpt-4o",
    messages=[{"role": "user", "content": "What is the population of Tokyo?"}],
)
print(f"  -> {resp1.choices[0].message.content or '[tool call]'}")

# Step 2: Follow up
print("Step 2: Following up...")
resp2 = client.chat.completions.create(
    model="gpt-4o",
    messages=[
        {"role": "user", "content": "What is the population of Tokyo?"},
        {"role": "assistant", "content": "Let me search for that."},
        {"role": "user", "content": "What about the population trend over the last decade?"},
    ],
)
print(f"  -> {resp2.choices[0].message.content or '[tool call]'}")

# Step 3: Summarize
print("Step 3: Summarizing...")
resp3 = client.chat.completions.create(
    model="gpt-4o",
    messages=[
        {"role": "user", "content": "Summarize the population data for Tokyo."},
    ],
)
print(f"  -> {resp3.choices[0].message.content or '[tool call]'}")

# Stop recording
rewind_agent.uninit()

print("\nDone! 3 steps recorded in direct mode.")
print("Run 'rewind show latest' to see the full trace.")
print("Run 'rewind inspect latest' for the interactive TUI.")
