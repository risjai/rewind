#!/usr/bin/env python3
"""
Demo: OpenAI Agents SDK + Rewind — zero-config recording.

Shows how rewind_agent.init() auto-detects the Agents SDK and records
every LLM call, tool execution, and agent handoff.

No API key needed — uses a local mock LLM server.

Usage:
    # Terminal 1: start the mock LLM
    python demo/mock_llm.py 9999

    # Terminal 2: run this demo
    python demo/openai_agents_demo.py

    # Then inspect:
    rewind show latest
    rewind inspect latest
"""

import asyncio
import sys
import os

# ── Setup: point at mock LLM, init Rewind ─────────────────────

MOCK_URL = os.environ.get("MOCK_LLM_URL", "http://127.0.0.1:9999/v1")

# Init Rewind BEFORE importing agents — this auto-registers the tracing processor
import rewind_agent
rewind_agent.init(session_name="openai-agents-demo")

from agents import Agent, Runner, function_tool, RunConfig
from openai import AsyncOpenAI
from agents.models.openai_chatcompletions import OpenAIChatCompletionsModel


# ── Tools ──────────────────────────────────────────────────────

@function_tool
def web_search(query: str) -> str:
    """Search the web for information."""
    # Simulated tool responses
    if "current population" in query.lower() or "2024" in query.lower():
        return (
            "Tokyo metropolitan area population (2024): approximately 13.96 million "
            "in the 23 special wards, 37.4 million in the Greater Tokyo Area. "
            "The population peaked in 2020 at 14.04 million before a slight decline. "
            "Source: Tokyo Metropolitan Government Statistics Bureau."
        )
    elif "decade" in query.lower() or "trend" in query.lower():
        return (
            "ERROR: Search API rate limited. Cached result from 2019 dataset. "
            "Tokyo population trend 2014-2019: steady growth from 13.35M to 13.96M "
            "(+4.6%). National Institute projections (2019): expected continued growth "
            "through 2025, reaching 14.2M. Note: this data predates COVID-19 impacts."
        )
    return f"No results found for: {query}"


# ── Agent ──────────────────────────────────────────────────────

# Point at the mock LLM server (no real API key needed)
mock_client = AsyncOpenAI(api_key="mock-key", base_url=MOCK_URL)
mock_model = OpenAIChatCompletionsModel(model="gpt-4o", openai_client=mock_client)

researcher = Agent(
    name="researcher",
    instructions=(
        "You are a research assistant. When asked about a topic, use the "
        "web_search tool to find information and synthesize an accurate answer."
    ),
    tools=[web_search],
    model=mock_model,
)


# ── Run ────────────────────────────────────────────────────────

async def main():
    print()
    print("  \033[36m\033[1mOpenAI Agents SDK + Rewind Demo\033[0m")
    print("  \033[2m━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\033[0m")
    print()
    print("  Running research agent with web_search tool...")
    print("  (Using mock LLM — no API key needed)")
    print()

    try:
        result = await Runner.run(
            researcher,
            "What is the current population of Tokyo, and how has it changed over the last decade?",
        )

        print("  \033[32m\033[1mAgent output:\033[0m")
        print()
        # Indent each line of the output
        for line in str(result.final_output).split("\n"):
            print(f"    {line}")
        print()

    except Exception as e:
        print(f"  \033[31mError: {e}\033[0m")
        print()
        print("  Make sure the mock LLM server is running:")
        print("    python demo/mock_llm.py 9999")
        print()
        sys.exit(1)

    print("  \033[2m━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\033[0m")
    print()
    print("  \033[36mNow inspect the recording:\033[0m")
    print("    \033[32mrewind show latest\033[0m       \033[2m# see the trace\033[0m")
    print("    \033[32mrewind inspect latest\033[0m    \033[2m# interactive TUI\033[0m")
    print()


if __name__ == "__main__":
    asyncio.run(main())
