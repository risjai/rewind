# Ray Agent Integration: Remaining Work

Items that need proper implementation to replace the current manual/hack setup.

## Must-Do (Blocking production use)

### 1. Auto-start Rewind server on pod startup
**Problem:** The Ray operator overrides `ENTRYPOINT` and `CMD` with `ray start`, so our `entrypoint.sh` never runs. We manually `kubectl exec` to start the server.
**Fix options:**
- Add a lifecycle `postStart` hook in the RayService pod spec to run `rewind web --host 0.0.0.0 &`
- Use a sidecar container instead of an in-process background server (add to `rayservice.yaml` template)
- Add `rewind web` as a Ray runtime_env setup command

### 2. Shared Rewind server instead of per-pod instances
**Problem:** Each pod runs its own Rewind server with its own SQLite. Sessions are randomly split across head and worker pods. You need to port-forward each pod separately to see all sessions.
**Fix:** Deploy Rewind as a standalone K8s Deployment + ClusterIP Service (the `rewind-deployment.yaml` spec already exists). Point `REWIND_URL` at `http://rewind.ids.svc.cluster.local:4800` instead of `http://127.0.0.1:4800`. All replicas POST to one server, one dashboard shows everything.

### 3. Download Rewind binary from Artifactory instead of git
**Problem:** The 20MB static binary is committed to the ray-agent git repo (`bin/rewind-linux-x86_64`). Not sustainable.
**Fix:** Upload the musl binary to `rpm.repo.local.sfdc.net/artifactory/strata-blobs/hawking/byom/libraries/` and `curl` it during Docker build (same pattern as the triton binaries). Remove the binary from git.

## Should-Do (Quality / correctness)

### ~~4. Rename `claude_session_id` to `external_session_id` in hooks.rs~~ DONE
Renamed metadata key to `external_session_id` with backward-compat fallback for old databases. Variables and parameters renamed across `hooks.rs`, `transcript.rs`, and tests.

### ~~5. Rename `ClaudeCodeHookPayload` to `HookPayload`~~ DONE
Renamed in `hooks.rs`. All 8 occurrences updated.

### 6. Multi-turn conversation grouping
**Problem:** Each follow-up question in a multi-turn conversation creates a separate Rewind session. The dashboard doesn't show them as related.
**Fix options:**
- Use Rewind's `thread_id` concept — derive a stable thread ID from the conversation (e.g., hash of first message) and set it on all turns
- Reuse the same `session_id` across turns when `conversation_history` is non-empty, and use SessionStart revival to append steps
- Add the conversation_history to the session metadata so the dashboard can show context

### 7. Full LLM message capture
**Problem:** By default, `RewindHook` only captures message count and a 300-char preview of the last user message. This is enough for tracing but not for replay.
**Fix:** Set `REWIND_FULL_CAPTURE=true` in the RayService spec to capture the full messages array. Monitor storage impact first — each ReAct session can have 8+ LLM calls with growing context windows.

## Nice-to-Have (Future enhancements)

### 8. LLM-as-judge scoring for diagnostic quality
Use Rewind's eval framework to auto-score ray-agent's diagnostic answers. Create a dataset of known diagnostic questions with expected answers, run evaluations across model changes.

### 9. Regression baselines
Snapshot known-good diagnostic sessions as baselines. After model or tool changes, re-run the same queries and use Rewind's assertion system to detect regressions in tool selection or answer quality.

### 10. OTel export to Splunk/Argus
Export Rewind sessions as OpenTelemetry traces, feed into Salesforce's existing Splunk/Argus observability stack. Enables correlation between agent sessions and infrastructure metrics.

### 11. Alert responder triage audit dashboard
Build a view that shows all automated Slack triage sessions — was the diagnosis correct? Did the agent pick the right tools? Use this to tune diagnostic strategies.
