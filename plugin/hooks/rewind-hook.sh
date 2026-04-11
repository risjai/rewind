#!/bin/bash
# Rewind plugin hook for Claude Code
# Reads hook event from stdin, wraps in envelope, POSTs to Rewind server.
# On failure: buffers to local JSONL file for later drain.

REWIND_PORT="${REWIND_PORT:-4800}"
REWIND_BUFFER="${REWIND_BUFFER:-$HOME/.rewind/hooks/buffer.jsonl}"

input=$(cat)

# Get timestamp with millisecond precision (portable)
timestamp=$(python3 -c "from datetime import datetime,timezone;print(datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%S.%f')[:-3]+'Z')" 2>/dev/null || date -u +"%Y-%m-%dT%H:%M:%SZ")

# Extract event type from the payload's hook_event_name field
event_type=$(echo "$input" | python3 -c "import sys,json;print(json.loads(sys.stdin.read()).get('hook_event_name','unknown'))" 2>/dev/null || echo "unknown")

# Wrap in envelope
envelope=$(python3 -c "
import sys, json
try:
    payload = json.loads(sys.stdin.read())
except:
    payload = {}
envelope = {
    'source': 'claude-code',
    'event_type': '$event_type',
    'timestamp': '$timestamp',
    'payload': payload
}
print(json.dumps(envelope))
" <<< "$input" 2>/dev/null)

# If python failed, construct manually
if [ -z "$envelope" ]; then
    envelope="{\"source\":\"claude-code\",\"event_type\":\"$event_type\",\"timestamp\":\"$timestamp\",\"payload\":$input}"
fi

# Background: POST to Rewind server, buffer on failure
(
    if ! curl -sf -X POST "http://127.0.0.1:${REWIND_PORT}/api/hooks/event" \
        -H "Content-Type: application/json" \
        -d "$envelope" > /dev/null 2>&1; then
        mkdir -p "$(dirname "$REWIND_BUFFER")"
        echo "$envelope" >> "$REWIND_BUFFER"
    fi
) &

exit 0
