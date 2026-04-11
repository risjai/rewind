#!/bin/bash
# Proxy Mode Verification Script
# Run this manually with your API credentials.
#
# Usage:
#   ./scripts/verify-proxy-mode.sh <upstream_url> <api_key>
#
# Examples:
#   # OpenAI
#   ./scripts/verify-proxy-mode.sh https://api.openai.com $OPENAI_API_KEY
#
#   # Anthropic
#   ./scripts/verify-proxy-mode.sh https://api.anthropic.com $ANTHROPIC_API_KEY
#
#   # Bedrock (via gateway that accepts bearer tokens)
#   ./scripts/verify-proxy-mode.sh https://your-bedrock-gateway.example.com $YOUR_TOKEN

set -euo pipefail

UPSTREAM="${1:?Usage: $0 <upstream_url> <api_key>}"
API_KEY="${2:?Usage: $0 <upstream_url> <api_key>}"
PROXY_PORT=8443
REWIND="./target/release/rewind"

echo "=== Proxy Mode Verification ==="
echo "Upstream: $UPSTREAM"
echo ""

# 1. Start proxy
echo "[1/5] Starting proxy on port $PROXY_PORT..."
$REWIND record --name "proxy-verify" --upstream "$UPSTREAM" --port "$PROXY_PORT" --replay &
PROXY_PID=$!
sleep 2

cleanup() {
    echo ""
    echo "[cleanup] Stopping proxy (PID $PROXY_PID)..."
    kill $PROXY_PID 2>/dev/null || true
}
trap cleanup EXIT

# 2. Non-streaming request
echo "[2/5] Testing non-streaming request..."
RESPONSE=$(curl -s http://127.0.0.1:$PROXY_PORT/v1/chat/completions \
    -H "Authorization: Bearer $API_KEY" \
    -H "Content-Type: application/json" \
    -d '{
        "model": "gpt-4o-mini",
        "messages": [{"role": "user", "content": "Say hello in exactly 3 words."}],
        "max_tokens": 20
    }')

if echo "$RESPONSE" | python3 -c "import sys,json; json.load(sys.stdin)['choices']" >/dev/null 2>&1; then
    echo "  ✓ Non-streaming: OK"
    echo "  Response: $(echo "$RESPONSE" | python3 -c "import sys,json; print(json.load(sys.stdin)['choices'][0]['message']['content'])")"
else
    echo "  ✗ Non-streaming: FAILED"
    echo "  Response: $RESPONSE"
fi

# 3. Streaming request
echo "[3/5] Testing streaming request..."
STREAM_OUTPUT=$(curl -s http://127.0.0.1:$PROXY_PORT/v1/chat/completions \
    -H "Authorization: Bearer $API_KEY" \
    -H "Content-Type: application/json" \
    -d '{
        "model": "gpt-4o-mini",
        "messages": [{"role": "user", "content": "Say goodbye in exactly 3 words."}],
        "max_tokens": 20,
        "stream": true
    }')

if echo "$STREAM_OUTPUT" | grep -q "data:"; then
    echo "  ✓ Streaming: OK (received SSE chunks)"
else
    echo "  ✗ Streaming: FAILED"
    echo "  Output: $(echo "$STREAM_OUTPUT" | head -3)"
fi

# 4. Verify recording
echo "[4/5] Checking recorded session..."
sleep 1
$REWIND show latest 2>&1 | head -15

# 5. Verify cache (replay same non-streaming request)
echo "[5/5] Testing Instant Replay cache..."
CACHED=$(curl -s http://127.0.0.1:$PROXY_PORT/v1/chat/completions \
    -H "Authorization: Bearer $API_KEY" \
    -H "Content-Type: application/json" \
    -d '{
        "model": "gpt-4o-mini",
        "messages": [{"role": "user", "content": "Say hello in exactly 3 words."}],
        "max_tokens": 20
    }')

if echo "$CACHED" | python3 -c "import sys,json; json.load(sys.stdin)['choices']" >/dev/null 2>&1; then
    echo "  ✓ Cache hit: OK (same response, 0 tokens)"
else
    echo "  ✗ Cache: FAILED"
fi

echo ""
echo "=== Verification Complete ==="
echo "Run '$REWIND inspect latest' to explore the recorded session."
