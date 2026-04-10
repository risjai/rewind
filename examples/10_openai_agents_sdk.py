"""
OpenAI Agents SDK — Auto-record agent runs, tool calls, and handoffs.

Rewind auto-detects the OpenAI Agents SDK when you call init(). It registers
a TracingProcessor that captures every generation, function call, and handoff.

Setup:
    # Terminal 1: Start the mock LLM server
    python demo/mock_llm.py 9999

    # Terminal 2: Run this script
    pip install rewind-agent[agents]
    python examples/10_openai_agents_sdk.py

    # After it finishes:
    rewind show latest
    rewind inspect latest
"""

import asyncio
import rewind_agent

# Init first — auto-detects OpenAI Agents SDK and registers TracingProcessor
rewind_agent.init(session_name="agents-sdk-demo")

from openai import AsyncOpenAI
from agents import Agent, Runner, function_tool
from agents.models.openai_chatcompletions import OpenAIChatCompletionsModel

MOCK_URL = "http://127.0.0.1:9999/v1"


# Define a tool the agent can call
@function_tool
def web_search(query: str) -> str:
    """Search the web for current information."""
    # In production, this would call a real search API
    return f"Mock search result for: {query}"


async def main():
    # Point at the mock LLM server
    mock_client = AsyncOpenAI(api_key="mock-key", base_url=MOCK_URL)
    mock_model = OpenAIChatCompletionsModel(
        model="gpt-4o", openai_client=mock_client,
    )

    # Create an agent with tools
    agent = Agent(
        name="researcher",
        instructions="You are a research assistant. Use web_search to find current data.",
        tools=[web_search],
        model=mock_model,
    )

    # Run the agent — Rewind captures everything automatically
    print("Running OpenAI Agents SDK agent...")
    result = await Runner.run(agent, "What is the current population of Tokyo?")
    print(f"Result: {result.final_output}\n")


asyncio.run(main())
rewind_agent.uninit()

print("Done! Agent run recorded with all generations, tool calls, and handoffs.")
print("Run 'rewind show latest' to see the full trace.")
