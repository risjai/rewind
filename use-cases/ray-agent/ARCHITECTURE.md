# Architecture: Rewind x Ray Agent

## System Overview

```
┌──────────────────────────────────────────────────────────────────┐
│                    Kubernetes Cluster (dev1)                      │
│                    Namespace: ids                                 │
│                                                                   │
│  ┌─────────────────────────────────────────────────────────────┐ │
│  │  RayService: ray-agent                                       │ │
│  │                                                               │ │
│  │  ┌─────────────────────┐    ┌─────────────────────┐          │ │
│  │  │  Replica 1 (Pod)     │    │  Replica 2 (Pod)     │          │ │
│  │  │                      │    │                      │          │ │
│  │  │  ┌────────────────┐ │    │  ┌────────────────┐ │          │ │
│  │  │  │  AgentIngress   │ │    │  │  AgentIngress   │ │          │ │
│  │  │  │  (FastAPI)      │ │    │  │  (FastAPI)      │ │          │ │
│  │  │  │                 │ │    │  │                 │ │          │ │
│  │  │  │  ┌───────────┐ │ │    │  │  ┌───────────┐ │ │          │ │
│  │  │  │  │ ReactAgent │ │ │    │  │  │ ReactAgent │ │ │          │ │
│  │  │  │  │ + Rewind   │ │ │    │  │  │ + Rewind   │ │ │          │ │
│  │  │  │  │   Hook     │─┼─┼────┼──┼──│   Hook     │─┼─┼──┐      │ │
│  │  │  │  └───────────┘ │ │    │  │  └───────────┘ │ │  │      │ │
│  │  │  │                 │ │    │  │                 │ │  │      │ │
│  │  │  │  AlertResponder │ │    │  │  AlertResponder │ │  │      │ │
│  │  │  │  + Rewind Hook ─┼─┼────┼──┼──+ Rewind Hook ─┼─┼──┤      │ │
│  │  │  └────────────────┘ │    │  └────────────────┘ │  │      │ │
│  │  └─────────────────────┘    └─────────────────────┘  │      │ │
│  └──────────────────────────────────────────────────────┼──────┘ │
│                                                          │        │
│                              async POST /api/hooks/event │        │
│                                                          ▼        │
│  ┌───────────────────────────────────────────────────────────┐   │
│  │  Rewind Server (Deployment, 1 replica)                     │   │
│  │  ClusterIP: rewind.ids.svc.cluster.local:4800              │   │
│  │                                                             │   │
│  │  ┌─────────────┐  ┌───────────────┐  ┌─────────────────┐ │   │
│  │  │ Hook Handler │  │ SQLite + WAL  │  │ Content-Addressed│ │   │
│  │  │ /api/hooks/* │  │ rewind.db     │  │ Blob Store       │ │   │
│  │  └─────────────┘  └───────────────┘  └─────────────────┘ │   │
│  │  ┌─────────────┐                                          │   │
│  │  │ Web Dashboard│  ← port-forward for local access        │   │
│  │  │ :4800       │                                          │   │
│  │  └─────────────┘  PVC: 5Gi (survives restarts)            │   │
│  └───────────────────────────────────────────────────────────┘   │
│                                                                   │
│  External Dependencies:                                           │
│  ┌──────────────┐ ┌──────────────┐ ┌──────────────┐             │
│  │ bot-svc-llm  │ │ K8s API      │ │ Splunk       │             │
│  │ (GPT-4o mini)│ │ (in-cluster) │ │ (mTLS)       │             │
│  │ mTLS         │ │ read-only    │ │ optional     │             │
│  └──────────────┘ └──────────────┘ └──────────────┘             │
└──────────────────────────────────────────────────────────────────┘
```

## Local Development Architecture

When testing from your Mac against the dev1 cluster:

```
┌─────────────────────────────────────────────────────────┐
│  Your Mac                                                │
│                                                          │
│  ┌──────────────────┐                                    │
│  │  Rewind Server    │ ← cargo run --release -- web       │
│  │  127.0.0.1:4800   │                                    │
│  │                    │                                    │
│  │  Web Dashboard ◄──┼── open http://127.0.0.1:4800       │
│  │  SQLite + Blobs   │                                    │
│  └────────▲─────────┘                                    │
│           │                                               │
│           │ HTTP POST /api/hooks/event                    │
│           │ (from ray-agent pod via port-forward)         │
│           │                                               │
│  ┌────────┴─────────┐                                    │
│  │  kubectl          │                                    │
│  │  port-forward     │                                    │
│  │                    │                                    │
│  │  localhost:8000 ──┼── → ray-agent-serve-svc:8000      │
│  │  localhost:4800 ◄─┼── ← (already local, no fwd needed)│
│  └──────────────────┘                                    │
│                                                          │
│  Test with:                                               │
│  curl localhost:8000/query -d '{"question":"..."}'       │
└───────────────────────┬──────────────────────────────────┘
                        │
                        │ kubectl port-forward
                        │
┌───────────────────────▼──────────────────────────────────┐
│  dev1 Kubernetes Cluster                                  │
│  Namespace: ids                                           │
│                                                          │
│  ┌──────────────────────────────────────────────────┐   │
│  │  RayService: ray-agent (2 replicas)               │   │
│  │                                                    │   │
│  │  ray-agent pod ──► LLM Gateway (mTLS)             │   │
│  │                 ──► K8s API (in-cluster)           │   │
│  │                 ──► Splunk (mTLS)                  │   │
│  │                 ──► Rewind Hook ──► localhost:4800 │   │
│  │                     (but localhost = pod, not Mac!) │   │
│  └──────────────────────────────────────────────────┘   │
│                                                          │
│  PROBLEM: ray-agent's localhost:4800 is inside the pod,  │
│  not your Mac. See LOCAL-TESTING-RUNBOOK.md for          │
│  solutions (reverse tunnel or sidecar).                  │
└──────────────────────────────────────────────────────────┘
```

## Data Flow: Single Query

```
User → POST /query {"question": "how is mulesoft?"}
         │
         ▼
┌─ AgentIngress.process_query() ────────────────────────────┐
│                                                            │
│  session_id = uuid4()                                      │
│                                                            │
│  ┌─ ReactAgent.run() ──────────────────────────────────┐  │
│  │                                                      │  │
│  │  ═══ SESSION START → Rewind ═══                     │  │
│  │  {event_type: "SessionStart",                        │  │
│  │   payload: {session_id, cwd: "/ray-agent/mulesoft"}} │  │
│  │                                                      │  │
│  │  ┌─ Iteration 1 ──────────────────────────────────┐ │  │
│  │  │                                                  │ │  │
│  │  │  ── PRE LLM CALL → Rewind ──                   │ │  │
│  │  │  {PreToolUse, tool_name: "__llm_call__",        │ │  │
│  │  │   tool_input: {iteration: 1, message_count: 3}} │ │  │
│  │  │                                                  │ │  │
│  │  │  LLM Gateway ← chat_with_tools(messages, tools) │ │  │
│  │  │  LLM responds: "I'll check the pods"            │ │  │
│  │  │     + tool_call: get_cluster_pods(mulesoft)      │ │  │
│  │  │                                                  │ │  │
│  │  │  ── POST LLM CALL → Rewind ──                  │ │  │
│  │  │  {PostToolUse, tool_name: "__llm_call__",       │ │  │
│  │  │   tool_response: {content_preview, tool_calls}} │ │  │
│  │  │                                                  │ │  │
│  │  │  ── PRE TOOL → Rewind ──                       │ │  │
│  │  │  {PreToolUse, tool_name: "get_cluster_pods",    │ │  │
│  │  │   tool_input: {cluster_name: "mulesoft"}}       │ │  │
│  │  │                                                  │ │  │
│  │  │  K8s API ← get_cluster_pods("mulesoft")         │ │  │
│  │  │  returns: [{name: "mulesoft-abc-head", ...}]    │ │  │
│  │  │                                                  │ │  │
│  │  │  ── POST TOOL → Rewind ──                      │ │  │
│  │  │  {PostToolUse, tool_name: "get_cluster_pods",   │ │  │
│  │  │   tool_response: "[{name: mulesoft-abc-head...}]│ │  │
│  │  │   elapsed_s: 0.234}                             │ │  │
│  │  │                                                  │ │  │
│  │  └──────────────────────────────────────────────────┘ │  │
│  │                                                      │  │
│  │  ┌─ Iteration 2 ──────────────────────────────────┐ │  │
│  │  │  (same pattern: pre_llm → LLM → post_llm       │ │  │
│  │  │   → pre_tool → tool → post_tool)                │ │  │
│  │  └──────────────────────────────────────────────────┘ │  │
│  │                                                      │  │
│  │  ┌─ Iteration N (final) ──────────────────────────┐ │  │
│  │  │  LLM responds with no tool_calls               │ │  │
│  │  │  → "Answer: Mulesoft cluster is healthy..."    │ │  │
│  │  └──────────────────────────────────────────────────┘ │  │
│  │                                                      │  │
│  │  ═══ SESSION END → Rewind ═══                       │  │
│  │  {event_type: "SessionEnd",                          │  │
│  │   payload: {session_id, iterations: 3,               │  │
│  │     tools_called: [get_cluster_pods, ...]}}          │  │
│  │                                                      │  │
│  └──────────────────────────────────────────────────────┘  │
│                                                            │
│  return QueryResponse(answer="Mulesoft cluster is...")     │
└────────────────────────────────────────────────────────────┘
```

## Data Flow: Alert Responder (Automated Triage)

```
Slack Channel (Moncloud/Argus alerts)
         │
         │  poll every 60s
         ▼
┌─ AlertResponder._poll_channels() ─────────────────────────┐
│                                                            │
│  Parse alert: "Low Availability: mulesoft-serve-svc"       │
│  → alert_type: "low_availability", service: "mulesoft"     │
│                                                            │
│  ┌─ _triage_alert() ───────────────────────────────────┐  │
│  │                                                      │  │
│  │  ═══ SESSION START → Rewind ═══                     │  │
│  │  {SessionStart, cwd: "/ray-agent/alerts/mulesoft",   │  │
│  │   tool_input: {alert_type, service}}                 │  │
│  │                                                      │  │
│  │  Run diagnostic strategy (6 tools for low_avail):    │  │
│  │                                                      │  │
│  │  ┌ query_splunk_logs ────────────────────────────┐  │  │
│  │  │ PRE TOOL → Rewind                             │  │  │
│  │  │ Splunk API ← search 5xx errors last 15m       │  │  │
│  │  │ POST TOOL → Rewind (results or error)         │  │  │
│  │  └───────────────────────────────────────────────┘  │  │
│  │  ┌ get_serve_error_logs ─────────────────────────┐  │  │
│  │  │ PRE/POST TOOL → Rewind                        │  │  │
│  │  └───────────────────────────────────────────────┘  │  │
│  │  ┌ analyze_head_health ──────────────────────────┐  │  │
│  │  │ PRE/POST TOOL → Rewind                        │  │  │
│  │  └───────────────────────────────────────────────┘  │  │
│  │  ┌ analyze_worker_health ────────────────────────┐  │  │
│  │  │ PRE/POST TOOL → Rewind                        │  │  │
│  │  └───────────────────────────────────────────────┘  │  │
│  │  ┌ check_worker_connectivity ────────────────────┐  │  │
│  │  │ PRE/POST TOOL → Rewind                        │  │  │
│  │  └───────────────────────────────────────────────┘  │  │
│  │  ┌ get_rollover_status ──────────────────────────┐  │  │
│  │  │ PRE/POST TOOL → Rewind                        │  │  │
│  │  └───────────────────────────────────────────────┘  │  │
│  │                                                      │  │
│  │  LLM ← TRIAGE_PROMPT + all diagnostic data           │  │
│  │  LLM → "Root cause: workers OOMKilled, ..."          │  │
│  │                                                      │  │
│  │  ═══ SESSION END → Rewind ═══                       │  │
│  │                                                      │  │
│  └──────────────────────────────────────────────────────┘  │
│                                                            │
│  Post triage report to Slack thread                        │
└────────────────────────────────────────────────────────────┘
```

## Hook Envelope Format

Every event POSTed to Rewind matches the `HookEventEnvelope` → `HookPayload` structs in `crates/rewind-web/src/hooks.rs`.

### Outer Envelope

```json
{
  "source": "ray-agent",
  "event_type": "PreToolUse | PostToolUse | PostToolUseFailure | SessionStart | SessionEnd",
  "timestamp": "2026-04-15T10:30:00.123Z",
  "payload": { ... }
}
```

### Inner Payload (matches `HookPayload`)

```json
{
  "session_id": "abc-123-def-456",
  "tool_name": "get_cluster_pods | __llm_call__ | session_start | ...",
  "tool_input": { "cluster_name": "mulesoft" },
  "tool_response": "{\"pods\": [{\"name\": \"mulesoft-abc-head\", ...}]}",
  "tool_use_id": "call-789",
  "cwd": "/ray-agent/mulesoft"
}
```

**Critical field mapping** (from Santa Method review):
- Use `tool_response` (NOT `tool_output`) — matches the Rust struct
- Use `cwd` for session naming — the handler derives session names from this path
- `tool_response` should be a string (gets blob-stored as-is)
- `name` and `metadata` fields are NOT in the struct — they're silently ignored

## Ray Agent Internal Architecture

```
                    POST /query
                        │
                        ▼
              ┌──────────────────┐
              │  AgentIngressBase │
              │  (FastAPI)        │
              └────────┬─────────┘
                       │
          ┌────────────┼────────────┐
          ▼            ▼            ▼
   ┌────────────┐ ┌─────────┐ ┌──────────────┐
   │ ReactAgent │ │ LLM     │ │ Alert        │
   │            │ │ Gateway  │ │ Responder    │
   │ ReAct loop │ │ Client   │ │ (background) │
   │ max 8 iter │ │ (mTLS)   │ │ Slack poll   │
   └──────┬─────┘ └─────────┘ └──────────────┘
          │
          ▼
   ┌──────────────┐
   │ ToolExecutor  │
   │ 30+ tools     │
   └──┬────┬────┬──┘
      │    │    │
      ▼    ▼    ▼
   ┌────┐┌────┐┌────┐
   │K8s ││Ray ││Splk│
   │API ││Dash││API │
   └────┘└────┘└────┘

   Tool Calling Modes (auto-detected):
   ┌─────────────────────────────────────────┐
   │ Native: LLM returns tool_invocations    │
   │   tc["function"]["name"]                │
   │   tc["function"]["arguments"]           │
   │                                         │
   │ Prompt: LLM returns text with patterns  │
   │   "Action: tool_name({...})"            │
   │   tc["name"]                            │
   │   tc["arguments"]                       │
   └─────────────────────────────────────────┘
```

## What Rewind Records Per Session

| Step # | Event Type | tool_name | What's Captured |
|--------|-----------|-----------|-----------------|
| 1 | SessionStart | session_start | Question, cluster hint |
| 2 | PreToolUse | __llm_call__ | Message count, last user message |
| 3 | PostToolUse | __llm_call__ | LLM content, tool calls decided, latency |
| 4 | PreToolUse | get_cluster_pods | Arguments: {cluster_name: "mulesoft"} |
| 5 | PostToolUse | get_cluster_pods | K8s API response (up to 4KB), latency |
| 6 | PreToolUse | __llm_call__ | Updated message count (includes tool results) |
| 7 | PostToolUse | __llm_call__ | Next reasoning step, more tool calls or answer |
| ... | ... | ... | Repeats up to 8 iterations |
| N | SessionEnd | session_end | Total iterations, tools called, error if any |

## Rewind Dashboard Capabilities

Once sessions are recorded, the dashboard at `:4800` provides:

1. **Session list**: All diagnostic runs, filterable by session name (cluster)
2. **Step timeline**: Click-through of every LLM decision and tool execution
3. **Request/Response inspector**: See the exact data the LLM received
4. **Fork**: Branch from any step, try a different tool, compare outcomes
5. **Replay**: Re-run with cached LLM responses, modify one variable
6. **Diff**: Compare two sessions side-by-side (e.g., same question, different model)
7. **Assertions**: Create baselines from known-good runs, detect regressions
