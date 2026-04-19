# Explicit Recording API

A wire-format-agnostic HTTP API for recording, replaying, and forking agent sessions. Works with any LLM provider (OpenAI, Anthropic, Salesforce LLM Gateway, Ollama, etc.) in any language.

## When to use this API

| Your situation | Use this |
|:---|:---|
| Python agent using OpenAI/Anthropic SDK | Direct mode (`rewind_agent.init()`) -- zero config |
| Any language, OpenAI/Anthropic wire format | Proxy mode (`rewind record`) |
| Custom LLM client, non-standard wire format | **This API** |
| Non-Python agent needing replay/fork | **This API** |

## Endpoints

All endpoints are under `/api`. JSON request/response. Default bind: `127.0.0.1:4800`.

### Session lifecycle

```
POST /api/sessions/start
  Body: { name, source?, thread_id?, metadata? }
  Returns: { session_id, root_timeline_id }
  Status: 201

POST /api/sessions/{id}/end
  Body: { status: "completed" | "errored", error? }
  Returns: { session_id }
  Status: 200
```

### LLM call recording

```
POST /api/sessions/{id}/llm-calls
  Body: {
    timeline_id?,        // defaults to root timeline
    client_step_id?,     // UUID for idempotency (optional)
    request_body: <json>,
    response_body: <json>,
    model: <string>,
    duration_ms: <number>,
    tokens_in?, tokens_out?
  }
  Returns: { step_number }
  Status: 201 (or 200 if duplicate client_step_id)
```

The `request_body` and `response_body` are stored as opaque blobs. Rewind does not parse or validate them -- any JSON structure works.

### Tool call recording

```
POST /api/sessions/{id}/tool-calls
  Body: {
    timeline_id?,
    client_step_id?,
    tool_name: <string>,
    request_body: <json>,
    response_body: <json>,
    duration_ms: <number>,
    error?: <string>
  }
  Returns: { step_number }
  Status: 201
```

### Replay lookup

```
POST /api/sessions/{id}/llm-calls/replay-lookup
  Body: { replay_context_id }
  Returns: { hit: true, response_body, model, step_number, active_timeline_id }
        or { hit: false, active_timeline_id }

POST /api/sessions/{id}/tool-calls/replay-lookup
  Body: { replay_context_id, tool_name? }
  Returns: { hit: true, response_body, ... } or { hit: false }
```

Each lookup advances the replay context cursor by one step. When the cursor passes the last cached step, `hit: false` is returned.

### Fork

```
POST /api/sessions/{id}/fork
  Body: { at_step, label, timeline_id? }
  Returns: { fork_timeline_id }
  Status: 201
```

`timeline_id` defaults to the root timeline. Pass a fork's timeline_id to create a fork-of-fork.

### Replay context

```
POST /api/replay-contexts
  Body: { session_id, from_step, fork_timeline_id }
  Returns: { replay_context_id, parent_steps_count, fork_at_step }
  Status: 201

DELETE /api/replay-contexts/{id}
  Returns: { released: true }
```

Replay contexts are persisted to SQLite and survive server restarts. Maximum 100 concurrent contexts.

### Steps with blobs

```
GET /api/sessions/{id}/steps?timeline_id=X&include_blobs=1
  Returns: [{ step_number, step_type, tool_name, request_body?, response_body?, model, ... }]
```

When `include_blobs=1`, full request/response bodies are inlined. This is the primary path for client-side replay (prefetch all steps, track cursor locally).

## Iteration-to-step mapping

For ReAct agents, "iteration N" means the Nth LLM call, not the Nth step. With tool calls as first-class steps, a single iteration may span multiple steps:

| Step | Type | Iteration |
|:---|:---|:---|
| 1 | LLM Call | 1 |
| 2 | Tool Call (get_pods) | 1 |
| 3 | Tool Call (get_logs) | 1 |
| 4 | LLM Call | 2 |
| 5 | Tool Call (analyze) | 2 |
| 6 | LLM Call (final answer) | 3 |

**Definition**: iteration N = the Nth step where `step_type == "llm_call"`.

## Examples

### Python (httpx)

```python
import httpx

REWIND = "http://127.0.0.1:4800"
client = httpx.Client(timeout=2.0)

# Start session
r = client.post(f"{REWIND}/api/sessions/start", json={
    "name": "my-agent",
    "metadata": {"question": "how is mulesoft?"}
})
session_id = r.json()["session_id"]

# Record an LLM call
r = client.post(f"{REWIND}/api/sessions/{session_id}/llm-calls", json={
    "request_body": {"messages": [{"role": "user", "content": "hello"}]},
    "response_body": {"content": "Hi there!"},
    "model": "gpt-4o",
    "duration_ms": 500,
    "tokens_in": 10,
    "tokens_out": 5,
})
step = r.json()["step_number"]  # 1

# Record a tool call
client.post(f"{REWIND}/api/sessions/{session_id}/tool-calls", json={
    "tool_name": "get_cluster_pods",
    "request_body": {"cluster": "mulesoft"},
    "response_body": {"pods": [{"name": "head-0"}]},
    "duration_ms": 234,
})

# End session
client.post(f"{REWIND}/api/sessions/{session_id}/end", json={
    "status": "completed"
})
```

### Go (net/http)

```go
body := `{"name":"my-go-agent","source":"go"}`
resp, _ := http.Post(rewindURL+"/api/sessions/start",
    "application/json", strings.NewReader(body))

var start struct {
    SessionID      string `json:"session_id"`
    RootTimelineID string `json:"root_timeline_id"`
}
json.NewDecoder(resp.Body).Decode(&start)

// Record LLM call
llmBody, _ := json.Marshal(map[string]any{
    "request_body":  map[string]any{"messages": []any{}},
    "response_body": map[string]any{"content": "Hello"},
    "model":         "gpt-4o",
    "duration_ms":   500,
})
http.Post(rewindURL+"/api/sessions/"+start.SessionID+"/llm-calls",
    "application/json", bytes.NewReader(llmBody))
```

### curl

```bash
# Start session
curl -s -X POST http://127.0.0.1:4800/api/sessions/start \
  -H "Content-Type: application/json" \
  -d '{"name": "curl-test"}' | jq .

# Record LLM call (use session_id from above)
curl -s -X POST http://127.0.0.1:4800/api/sessions/$SID/llm-calls \
  -H "Content-Type: application/json" \
  -d '{
    "request_body": {"messages": [{"role": "user", "content": "hi"}]},
    "response_body": {"content": "hello"},
    "model": "gpt-4o",
    "duration_ms": 100
  }' | jq .

# Fork from step 1
curl -s -X POST http://127.0.0.1:4800/api/sessions/$SID/fork \
  -H "Content-Type: application/json" \
  -d '{"at_step": 1, "label": "experiment-1"}' | jq .
```
