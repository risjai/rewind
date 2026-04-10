"""
Pydantic AI Integration — Auto-record all LLM calls from Pydantic AI agents.

Rewind auto-detects Pydantic AI when you call init(). Every model request
and tool execution is recorded with full context.

Setup:
    # Terminal 1: Start the mock LLM server
    python demo/mock_llm.py 9999

    # Terminal 2: Run this script
    pip install rewind-agent[pydantic]
    python examples/09_pydantic_ai.py

    # After it finishes:
    rewind show latest
"""

import os
import rewind_agent

# Init first — auto-detects Pydantic AI and injects recording hooks
rewind_agent.init(session_name="pydantic-ai-demo")

from pydantic_ai import Agent

# Point Pydantic AI at the mock LLM server
os.environ["OPENAI_API_KEY"] = "mock-key"
os.environ["OPENAI_BASE_URL"] = "http://127.0.0.1:9999/v1"

# Create a Pydantic AI agent
agent = Agent(
    "openai:gpt-4o",
    system_prompt="You are a helpful assistant that answers questions about world cities.",
)

# Run the agent — all LLM calls are automatically recorded by Rewind
print("Running Pydantic AI agent...")
result = agent.run_sync("What is the population of Tokyo?")
print(f"Result: {result.data}\n")

rewind_agent.uninit()

print("Done! Pydantic AI calls recorded automatically.")
print("Run 'rewind show latest' to see the trace with model requests and tool calls.")
