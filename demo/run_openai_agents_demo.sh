#!/usr/bin/env bash
# Run the OpenAI Agents SDK + Rewind demo end-to-end.
# No API key needed — uses a local mock LLM server.
#
# Usage:
#   ./demo/run_openai_agents_demo.sh
#
# Prerequisites:
#   pip install rewind-agent openai-agents

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
MOCK_PORT=9999

echo ""
echo "  ⏪ OpenAI Agents SDK + Rewind Demo"
echo "  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Check deps
python3 -c "import agents" 2>/dev/null || {
    echo "  Installing openai-agents..."
    pip install openai-agents -q
}
python3 -c "import rewind_agent" 2>/dev/null || {
    echo "  Installing rewind-agent..."
    pip install rewind-agent -q
}

# Start mock LLM server in background
echo "  Starting mock LLM server on port $MOCK_PORT..."
python3 "$SCRIPT_DIR/mock_llm.py" $MOCK_PORT &
MOCK_PID=$!
sleep 1

# Cleanup on exit
cleanup() {
    kill $MOCK_PID 2>/dev/null || true
}
trap cleanup EXIT

# Run the demo
export MOCK_LLM_URL="http://127.0.0.1:$MOCK_PORT/v1"
python3 "$SCRIPT_DIR/openai_agents_demo.py"

# Show the recording
echo "  Showing recording..."
echo ""
rewind show latest 2>/dev/null || cargo run --bin rewind -- show latest 2>/dev/null || echo "  (rewind CLI not in PATH — build with: cargo build --release)"
echo ""
