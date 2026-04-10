use std::sync::{Arc, Mutex};

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router,
};
use rewind_assert::{AssertionEngine, BaselineManager, Tolerance};
use rewind_eval::{compare_experiments, DatasetManager};
use rewind_replay::ReplayEngine;
use rewind_store::{Experiment, Store};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ── Parameter types ──────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SessionParam {
    /// Session ID, ID prefix, or "latest"
    pub session: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetStepDetailParams {
    /// Step ID (UUID)
    pub step_id: String,
    /// Include the full request body (can be large). Default: false
    #[serde(default)]
    pub include_request: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiffTimelinesParams {
    /// Session ID, prefix, or "latest"
    pub session: String,
    /// Left timeline: ID, prefix, or label (e.g. "main")
    pub left: String,
    /// Right timeline: ID, prefix, or label (e.g. "fixed")
    pub right: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ForkTimelineParams {
    /// Session ID, prefix, or "latest"
    pub session: String,
    /// Step number to fork at
    pub at_step: u32,
    /// Label for the new fork (default: "fork")
    #[serde(default = "default_fork_label")]
    pub label: String,
}

fn default_fork_label() -> String {
    "fork".to_string()
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReplaySessionParams {
    /// Session ID, prefix, or "latest"
    pub session: String,
    /// Step number to replay from (steps 1..from_step served from cache, from_step+1 onward live)
    pub from_step: u32,
    /// Upstream LLM API base URL (default: https://api.openai.com)
    #[serde(default = "default_upstream")]
    pub upstream: String,
    /// Proxy listen port (default: 8443)
    #[serde(default = "default_port")]
    pub port: u16,
    /// Label for the forked timeline
    #[serde(default = "default_replay_label")]
    pub label: String,
}

fn default_upstream() -> String {
    "https://api.openai.com".to_string()
}

fn default_port() -> u16 {
    8443
}

fn default_replay_label() -> String {
    "replayed".to_string()
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateBaselineParams {
    /// Session ID, prefix, or "latest"
    pub session: String,
    /// Unique name for this baseline
    pub name: String,
    /// Optional description
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CheckBaselineParams {
    /// Session ID, prefix, or "latest" — the session to check
    pub session: String,
    /// Baseline name to check against
    pub against: String,
    /// Token tolerance percentage (default: 20)
    #[serde(default = "default_token_tolerance")]
    pub token_tolerance: u32,
}

fn default_token_tolerance() -> u32 {
    20
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BaselineNameParam {
    /// Baseline name
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DatasetNameParam {
    /// Dataset name
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListExperimentsParam {
    /// Filter by dataset name (optional)
    #[serde(default)]
    pub dataset: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExperimentRefParam {
    /// Experiment name or ID
    pub experiment: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CompareExperimentsParam {
    /// Left experiment name or ID
    pub left: String,
    /// Right experiment name or ID
    pub right: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateEvalDatasetParams {
    /// Name for the new dataset
    pub name: String,
    /// Optional description of the dataset
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddEvalExampleParams {
    /// Dataset name to add the example to
    pub dataset: String,
    /// Input value (the request/prompt to test)
    pub input: serde_json::Value,
    /// Expected output value (the ideal response)
    pub expected: serde_json::Value,
    /// Optional metadata for this example
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DatasetFromSessionParams {
    /// Dataset name to add the example to
    pub dataset: String,
    /// Session ID, prefix, or "latest"
    pub session: String,
    /// Step number to use as the input (request blob)
    pub input_step: u32,
    /// Step number to use as the expected output (response blob). Defaults to input_step.
    #[serde(default)]
    pub expected_step: Option<u32>,
}

// ── Response types ───────────────────────────────────────────

#[derive(Serialize)]
struct SessionSummary {
    id: String,
    name: String,
    status: String,
    total_steps: u32,
    total_tokens: u64,
    created_at: String,
    timelines: usize,
}

#[derive(Serialize)]
struct StepSummaryResponse {
    step_number: u32,
    step_type: String,
    status: String,
    model: String,
    duration_ms: u64,
    tokens_in: u64,
    tokens_out: u64,
    error: Option<String>,
    response_preview: String,
}

#[derive(Serialize)]
struct TimelineSummary {
    id: String,
    label: String,
    parent_timeline_id: Option<String>,
    fork_at_step: Option<u32>,
}

// ── Server ───────────────────────────────────────────────────

#[derive(Clone)]
pub struct RewindMcp {
    store: Arc<Mutex<Store>>,
    tool_router: ToolRouter<Self>,
}

impl RewindMcp {
    pub fn new(store: Store) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
            tool_router: Self::tool_router(),
        }
    }

    fn lock_store(&self) -> Result<std::sync::MutexGuard<'_, Store>, McpError> {
        self.store.lock().map_err(|e| {
            McpError::internal_error(format!("Store lock poisoned: {e}"), None)
        })
    }
}

#[tool_router]
impl RewindMcp {
    #[tool(
        name = "list_sessions",
        description = "List all recorded agent sessions with summary stats (name, steps, tokens, status, timeline count)"
    )]
    async fn list_sessions(&self) -> Result<CallToolResult, McpError> {
        let store = self.lock_store()?;
        let sessions = store.list_sessions().map_err(|e| mcp_err(&e))?;

        let summaries: Vec<SessionSummary> = sessions
            .iter()
            .map(|s| {
                let timeline_count = store.get_timelines(&s.id).map(|t| t.len()).unwrap_or(0);
                SessionSummary {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    status: s.status.as_str().to_string(),
                    total_steps: s.total_steps,
                    total_tokens: s.total_tokens,
                    created_at: s.created_at.to_rfc3339(),
                    timelines: timeline_count,
                }
            })
            .collect();

        let json = serde_json::json!({
            "sessions": summaries,
            "count": summaries.len(),
        });
        ok_json(&json)
    }

    #[tool(
        name = "show_session",
        description = "Show the step-by-step trace for an agent session. Returns the timeline with step types, models, token counts, durations, errors, and response previews. Pass session ID, prefix, or 'latest'."
    )]
    async fn show_session(
        &self,
        params: Parameters<SessionParam>,
    ) -> Result<CallToolResult, McpError> {
        let store = self.lock_store()?;
        let sess = resolve_session(&store, &params.0.session)?;
        let timelines = store.get_timelines(&sess.id).map_err(|e| mcp_err(&e))?;
        let root = timelines
            .iter()
            .find(|t| t.parent_timeline_id.is_none())
            .ok_or_else(|| mcp_err_str("No root timeline found"))?;

        let engine = ReplayEngine::new(&store);
        let steps = engine
            .get_full_timeline_steps(&root.id, &sess.id)
            .map_err(|e| mcp_err(&e))?;

        let step_summaries: Vec<StepSummaryResponse> = steps
            .iter()
            .map(|s| {
                let preview = extract_response_preview(&store, &s.response_blob);
                StepSummaryResponse {
                    step_number: s.step_number,
                    step_type: s.step_type.as_str().to_string(),
                    status: s.status.as_str().to_string(),
                    model: s.model.clone(),
                    duration_ms: s.duration_ms,
                    tokens_in: s.tokens_in,
                    tokens_out: s.tokens_out,
                    error: s.error.clone(),
                    response_preview: preview,
                }
            })
            .collect();

        let timeline_summaries: Vec<TimelineSummary> = timelines
            .iter()
            .map(|t| TimelineSummary {
                id: t.id.clone(),
                label: t.label.clone(),
                parent_timeline_id: t.parent_timeline_id.clone(),
                fork_at_step: t.fork_at_step,
            })
            .collect();

        let json = serde_json::json!({
            "session": {
                "id": sess.id,
                "name": sess.name,
                "status": sess.status.as_str(),
                "total_steps": sess.total_steps,
                "total_tokens": sess.total_tokens,
            },
            "timeline": { "id": root.id, "label": root.label },
            "steps": step_summaries,
            "timelines": timeline_summaries,
        });
        ok_json(&json)
    }

    #[tool(
        name = "get_step_detail",
        description = "Get the full request and response content for a specific step, reading from the content-addressed blob store. Set include_request=true to also get the (potentially large) request body."
    )]
    async fn get_step_detail(
        &self,
        params: Parameters<GetStepDetailParams>,
    ) -> Result<CallToolResult, McpError> {
        let params = &params.0;
        let store = self.lock_store()?;
        let step = store
            .get_step(&params.step_id)
            .map_err(|e| mcp_err(&e))?
            .ok_or_else(|| mcp_err_str(&format!("Step not found: {}", params.step_id)))?;

        let response: serde_json::Value = store
            .blobs
            .get_json(&step.response_blob)
            .unwrap_or(serde_json::json!({"error": "blob not found"}));

        let mut result = serde_json::json!({
            "step": {
                "id": step.id,
                "step_number": step.step_number,
                "step_type": step.step_type.as_str(),
                "status": step.status.as_str(),
                "model": step.model,
                "duration_ms": step.duration_ms,
                "tokens_in": step.tokens_in,
                "tokens_out": step.tokens_out,
                "error": step.error,
            },
            "response": response,
        });

        if params.include_request {
            let request: serde_json::Value = store
                .blobs
                .get_json(&step.request_blob)
                .unwrap_or(serde_json::json!({"error": "blob not found"}));
            result["request"] = request;
        }

        ok_json(&result)
    }

    #[tool(
        name = "diff_timelines",
        description = "Compare two timelines within a session side by side. Shows where they diverge and how each step differs. Use timeline IDs, prefixes, or labels (e.g. 'main', 'fixed')."
    )]
    async fn diff_timelines(
        &self,
        params: Parameters<DiffTimelinesParams>,
    ) -> Result<CallToolResult, McpError> {
        let params = &params.0;
        let store = self.lock_store()?;
        let sess = resolve_session(&store, &params.session)?;
        let timelines = store.get_timelines(&sess.id).map_err(|e| mcp_err(&e))?;

        let left_id = resolve_timeline_ref(&timelines, &params.left)?;
        let right_id = resolve_timeline_ref(&timelines, &params.right)?;

        let engine = ReplayEngine::new(&store);
        let diff = engine
            .diff_timelines(&sess.id, &left_id, &right_id)
            .map_err(|e| mcp_err(&e))?;

        let json = serde_json::to_value(&diff).map_err(|e| mcp_err(&e))?;
        ok_json(&json)
    }

    #[tool(
        name = "list_snapshots",
        description = "List all workspace snapshots with labels, file counts, sizes, and creation times."
    )]
    async fn list_snapshots(&self) -> Result<CallToolResult, McpError> {
        let store = self.lock_store()?;
        let snapshots = store.list_snapshots().map_err(|e| mcp_err(&e))?;

        let items: Vec<serde_json::Value> = snapshots
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "label": s.label,
                    "directory": s.directory,
                    "file_count": s.file_count,
                    "size_bytes": s.size_bytes,
                    "size_human": format_bytes(s.size_bytes),
                    "created_at": s.created_at.to_rfc3339(),
                })
            })
            .collect();

        let json = serde_json::json!({
            "snapshots": items,
            "count": items.len(),
        });
        ok_json(&json)
    }

    #[tool(
        name = "cache_stats",
        description = "Show Instant Replay cache statistics: number of cached responses, total cache hits, and total tokens saved."
    )]
    async fn cache_stats(&self) -> Result<CallToolResult, McpError> {
        let store = self.lock_store()?;
        let stats = store.cache_stats().map_err(|e| mcp_err(&e))?;
        let json = serde_json::to_value(&stats).map_err(|e| mcp_err(&e))?;
        ok_json(&json)
    }

    #[tool(
        name = "fork_timeline",
        description = "Create a timeline fork at a specific step, allowing exploration of alternative agent execution paths. Steps before the fork point are shared with the parent (zero re-execution)."
    )]
    async fn fork_timeline(
        &self,
        params: Parameters<ForkTimelineParams>,
    ) -> Result<CallToolResult, McpError> {
        let params = &params.0;
        let store = self.lock_store()?;
        let sess = resolve_session(&store, &params.session)?;
        let root = store
            .get_root_timeline(&sess.id)
            .map_err(|e| mcp_err(&e))?
            .ok_or_else(|| mcp_err_str("No root timeline found"))?;

        let engine = ReplayEngine::new(&store);
        let fork = engine
            .fork(&sess.id, &root.id, params.at_step, &params.label)
            .map_err(|e| mcp_err(&e))?;

        let json = serde_json::json!({
            "fork": {
                "id": fork.id,
                "label": fork.label,
                "parent_timeline_id": fork.parent_timeline_id,
                "fork_at_step": fork.fork_at_step,
                "session_id": fork.session_id,
            },
            "message": format!(
                "Fork created. Steps 1-{} are shared with parent timeline.",
                params.at_step
            ),
        });
        ok_json(&json)
    }

    #[tool(
        name = "replay_session",
        description = "Set up a fork-and-execute replay: creates a forked timeline where steps 1..from_step \
            are served from the parent's cached responses (0ms, 0 tokens), and steps after from_step \
            are forwarded to the real LLM. Returns connection info for the replay proxy. \
            Point your agent at the returned proxy URL to run the replay."
    )]
    async fn replay_session(
        &self,
        params: Parameters<ReplaySessionParams>,
    ) -> Result<CallToolResult, McpError> {
        let params = &params.0;
        let store = self.lock_store()?;
        let sess = resolve_session(&store, &params.session)?;
        let root = store
            .get_root_timeline(&sess.id)
            .map_err(|e| mcp_err(&e))?
            .ok_or_else(|| mcp_err_str("No root timeline found"))?;

        let engine = ReplayEngine::new(&store);
        let parent_steps = engine
            .get_full_timeline_steps(&root.id, &sess.id)
            .map_err(|e| mcp_err(&e))?;

        if params.from_step == 0 || params.from_step as usize > parent_steps.len() {
            return Err(mcp_err_str(&format!(
                "Invalid from_step {}. Session has {} steps (use 1-{}).",
                params.from_step, parent_steps.len(), parent_steps.len()
            )));
        }

        // Create the forked timeline
        let fork = engine
            .fork(&sess.id, &root.id, params.from_step, &params.label)
            .map_err(|e| mcp_err(&e))?;

        let cached_steps: Vec<serde_json::Value> = parent_steps.iter()
            .filter(|s| s.step_number <= params.from_step)
            .map(|s| serde_json::json!({
                "step_number": s.step_number,
                "model": s.model,
                "tokens_in": s.tokens_in,
                "tokens_out": s.tokens_out,
            }))
            .collect();

        let json = serde_json::json!({
            "replay": {
                "session_id": sess.id,
                "session_name": sess.name,
                "fork_id": fork.id,
                "fork_label": fork.label,
                "from_step": params.from_step,
                "total_parent_steps": parent_steps.len(),
                "cached_steps": cached_steps,
                "port": params.port,
                "upstream": params.upstream,
            },
            "instructions": format!(
                "Fork created. To start the replay proxy, run:\n  rewind replay {} --from {} --port {} --upstream {}",
                &sess.id[..8.min(sess.id.len())], params.from_step, params.port, params.upstream
            ),
            "message": format!(
                "Replay set up: steps 1-{} will be served from cache (0ms, 0 tokens). Steps {}+ will hit {}.",
                params.from_step, params.from_step + 1, params.upstream
            ),
        });
        ok_json(&json)
    }

    // ── Assertion / Baseline tools ──────────────────────────

    #[tool(
        name = "create_baseline",
        description = "Create an assertion baseline from a recorded session. \
            Extracts step signatures (types, models, tool names, token counts) \
            for regression comparison. The baseline name must be unique."
    )]
    async fn create_baseline(
        &self,
        params: Parameters<CreateBaselineParams>,
    ) -> Result<CallToolResult, McpError> {
        let params = &params.0;
        let store = self.lock_store()?;
        let sess = resolve_session(&store, &params.session)?;
        let root = store
            .get_root_timeline(&sess.id)
            .map_err(|e| mcp_err(&e))?
            .ok_or_else(|| mcp_err_str("No root timeline found"))?;

        let manager = BaselineManager::new(&store);
        let baseline = manager
            .create_baseline(&sess.id, &root.id, &params.name, &params.description)
            .map_err(|e| mcp_err(&e))?;

        let json = serde_json::json!({
            "baseline": {
                "id": baseline.id,
                "name": baseline.name,
                "source_session_id": baseline.source_session_id,
                "step_count": baseline.step_count,
                "total_tokens": baseline.total_tokens,
            },
            "message": format!(
                "Baseline '{}' created with {} steps. Check with: check_baseline(session, against='{}')",
                baseline.name, baseline.step_count, baseline.name
            ),
        });
        ok_json(&json)
    }

    #[tool(
        name = "check_baseline",
        description = "Check a session against a baseline for regressions. \
            Compares step types, models, error status, tool names, and token usage. \
            Returns per-step pass/warn/fail verdicts and an overall result."
    )]
    async fn check_baseline(
        &self,
        params: Parameters<CheckBaselineParams>,
    ) -> Result<CallToolResult, McpError> {
        let params = &params.0;
        let store = self.lock_store()?;
        let sess = resolve_session(&store, &params.session)?;
        let root = store
            .get_root_timeline(&sess.id)
            .map_err(|e| mcp_err(&e))?
            .ok_or_else(|| mcp_err_str("No root timeline found"))?;

        let manager = BaselineManager::new(&store);
        let baseline = manager
            .get_baseline(&params.against)
            .map_err(|e| mcp_err(&e))?
            .ok_or_else(|| mcp_err_str(&format!("Baseline '{}' not found", params.against)))?;
        let baseline_steps = manager
            .get_baseline_steps(&baseline.id)
            .map_err(|e| mcp_err(&e))?;

        let engine = ReplayEngine::new(&store);
        let actual_steps = engine
            .get_full_timeline_steps(&root.id, &sess.id)
            .map_err(|e| mcp_err(&e))?;

        let tolerance = Tolerance::default().with_token_pct(params.token_tolerance);
        let checker = AssertionEngine::new(&store, tolerance);
        let result = checker
            .check(
                &baseline.id,
                &baseline.name,
                &baseline_steps,
                &actual_steps,
                &sess.id,
                &root.id,
            )
            .map_err(|e| mcp_err(&e))?;

        let json = serde_json::to_value(&result).map_err(|e| mcp_err(&e))?;
        ok_json(&json)
    }

    #[tool(
        name = "list_baselines",
        description = "List all assertion baselines with names, source sessions, \
            step counts, and creation times."
    )]
    async fn list_baselines(&self) -> Result<CallToolResult, McpError> {
        let store = self.lock_store()?;
        let manager = BaselineManager::new(&store);
        let baselines = manager.list_baselines().map_err(|e| mcp_err(&e))?;

        let items: Vec<serde_json::Value> = baselines
            .iter()
            .map(|b| {
                serde_json::json!({
                    "name": b.name,
                    "source_session_id": b.source_session_id,
                    "step_count": b.step_count,
                    "total_tokens": b.total_tokens,
                    "description": b.description,
                    "created_at": b.created_at.to_rfc3339(),
                })
            })
            .collect();

        let json = serde_json::json!({
            "baselines": items,
            "count": items.len(),
        });
        ok_json(&json)
    }

    #[tool(
        name = "show_baseline",
        description = "Show detailed baseline information including all expected \
            step signatures (types, models, tool names, token expectations)."
    )]
    async fn show_baseline(
        &self,
        params: Parameters<BaselineNameParam>,
    ) -> Result<CallToolResult, McpError> {
        let store = self.lock_store()?;
        let manager = BaselineManager::new(&store);
        let baseline = manager
            .get_baseline(&params.0.name)
            .map_err(|e| mcp_err(&e))?
            .ok_or_else(|| mcp_err_str(&format!("Baseline '{}' not found", params.0.name)))?;
        let steps = manager
            .get_baseline_steps(&baseline.id)
            .map_err(|e| mcp_err(&e))?;

        let step_items: Vec<serde_json::Value> = steps
            .iter()
            .map(|s| {
                serde_json::json!({
                    "step_number": s.step_number,
                    "step_type": s.step_type,
                    "expected_status": s.expected_status,
                    "expected_model": s.expected_model,
                    "tokens_in": s.tokens_in,
                    "tokens_out": s.tokens_out,
                    "tool_name": s.tool_name,
                    "has_error": s.has_error,
                })
            })
            .collect();

        let json = serde_json::json!({
            "baseline": {
                "id": baseline.id,
                "name": baseline.name,
                "source_session_id": baseline.source_session_id,
                "source_timeline_id": baseline.source_timeline_id,
                "step_count": baseline.step_count,
                "total_tokens": baseline.total_tokens,
                "description": baseline.description,
                "created_at": baseline.created_at.to_rfc3339(),
            },
            "expected_steps": step_items,
        });
        ok_json(&json)
    }

    #[tool(
        name = "delete_baseline",
        description = "Delete an assertion baseline by name."
    )]
    async fn delete_baseline(
        &self,
        params: Parameters<BaselineNameParam>,
    ) -> Result<CallToolResult, McpError> {
        let store = self.lock_store()?;
        let manager = BaselineManager::new(&store);
        manager
            .delete_baseline(&params.0.name)
            .map_err(|e| mcp_err(&e))?;

        let json = serde_json::json!({
            "deleted": params.0.name,
            "message": format!("Baseline '{}' deleted.", params.0.name),
        });
        ok_json(&json)
    }

    // ── Evaluation tools ──────────────────────────────────────

    #[tool(
        name = "list_eval_datasets",
        description = "List all evaluation datasets with example counts and versions"
    )]
    async fn list_eval_datasets(&self) -> Result<CallToolResult, McpError> {
        let store = self.lock_store()?;
        let datasets = store.list_datasets().map_err(|e| mcp_err(&e))?;

        let items: Vec<serde_json::Value> = datasets
            .iter()
            .map(|d| {
                serde_json::json!({
                    "id": d.id,
                    "name": d.name,
                    "description": d.description,
                    "version": d.version,
                    "example_count": d.example_count,
                    "created_at": d.created_at.to_rfc3339(),
                    "updated_at": d.updated_at.to_rfc3339(),
                })
            })
            .collect();

        let json = serde_json::json!({
            "datasets": items,
            "count": items.len(),
        });
        ok_json(&json)
    }

    #[tool(
        name = "show_eval_dataset",
        description = "Show evaluation dataset details including example previews"
    )]
    async fn show_eval_dataset(
        &self,
        params: Parameters<DatasetNameParam>,
    ) -> Result<CallToolResult, McpError> {
        let store = self.lock_store()?;
        let dataset = store
            .get_dataset_by_name(&params.0.name)
            .map_err(|e| mcp_err(&e))?
            .ok_or_else(|| mcp_err_str(&format!("Dataset '{}' not found", params.0.name)))?;

        let examples = store
            .get_dataset_examples(&dataset.id)
            .map_err(|e| mcp_err(&e))?;

        let example_previews: Vec<serde_json::Value> = examples
            .iter()
            .take(10)
            .map(|ex| {
                let input_preview = store
                    .blobs
                    .get_json::<serde_json::Value>(&ex.input_blob)
                    .ok()
                    .map(|v| truncate_json_preview(&v, 200))
                    .unwrap_or_else(|| "(blob not found)".to_string());

                let expected_preview = store
                    .blobs
                    .get_json::<serde_json::Value>(&ex.expected_blob)
                    .ok()
                    .map(|v| truncate_json_preview(&v, 200))
                    .unwrap_or_else(|| "(blob not found)".to_string());

                serde_json::json!({
                    "id": ex.id,
                    "ordinal": ex.ordinal,
                    "input_preview": input_preview,
                    "expected_preview": expected_preview,
                    "source_session_id": ex.source_session_id,
                })
            })
            .collect();

        let json = serde_json::json!({
            "dataset": {
                "id": dataset.id,
                "name": dataset.name,
                "description": dataset.description,
                "version": dataset.version,
                "example_count": dataset.example_count,
                "created_at": dataset.created_at.to_rfc3339(),
                "updated_at": dataset.updated_at.to_rfc3339(),
                "metadata": dataset.metadata,
            },
            "examples": example_previews,
            "showing": example_previews.len(),
            "total": examples.len(),
        });
        ok_json(&json)
    }

    #[tool(
        name = "list_eval_experiments",
        description = "List evaluation experiments, optionally filtered by dataset name"
    )]
    async fn list_eval_experiments(
        &self,
        params: Parameters<ListExperimentsParam>,
    ) -> Result<CallToolResult, McpError> {
        let store = self.lock_store()?;
        let experiments = if let Some(ref dataset_name) = params.0.dataset {
            store
                .list_experiments_by_dataset(dataset_name)
                .map_err(|e| mcp_err(&e))?
        } else {
            store.list_experiments().map_err(|e| mcp_err(&e))?
        };

        let items: Vec<serde_json::Value> = experiments
            .iter()
            .map(|exp| {
                serde_json::json!({
                    "id": exp.id,
                    "name": exp.name,
                    "dataset_id": exp.dataset_id,
                    "dataset_version": exp.dataset_version,
                    "status": exp.status.as_str(),
                    "total_examples": exp.total_examples,
                    "completed_examples": exp.completed_examples,
                    "avg_score": exp.avg_score,
                    "pass_rate": exp.pass_rate,
                    "total_duration_ms": exp.total_duration_ms,
                    "total_tokens": exp.total_tokens,
                    "created_at": exp.created_at.to_rfc3339(),
                    "completed_at": exp.completed_at.map(|dt| dt.to_rfc3339()),
                })
            })
            .collect();

        let json = serde_json::json!({
            "experiments": items,
            "count": items.len(),
        });
        ok_json(&json)
    }

    #[tool(
        name = "show_eval_experiment",
        description = "Show experiment results with per-example scores and evaluator details"
    )]
    async fn show_eval_experiment(
        &self,
        params: Parameters<ExperimentRefParam>,
    ) -> Result<CallToolResult, McpError> {
        let store = self.lock_store()?;
        let exp = resolve_experiment_ref(&store, &params.0.experiment)?;

        let results = store
            .get_experiment_results(&exp.id)
            .map_err(|e| mcp_err(&e))?;

        // Build evaluator ID → name lookup
        let evaluators = store.list_evaluators().map_err(|e| mcp_err(&e))?;
        let evaluator_names: std::collections::HashMap<String, String> = evaluators
            .into_iter()
            .map(|ev| (ev.id.clone(), ev.name))
            .collect();

        let result_items: Vec<serde_json::Value> = results
            .iter()
            .map(|r| {
                let scores = store
                    .get_experiment_scores(&r.id)
                    .unwrap_or_default();

                let score_items: Vec<serde_json::Value> = scores
                    .iter()
                    .map(|s| {
                        let evaluator_name = evaluator_names
                            .get(&s.evaluator_id)
                            .cloned()
                            .unwrap_or_else(|| s.evaluator_id.clone());
                        serde_json::json!({
                            "evaluator": evaluator_name,
                            "score": s.score,
                            "passed": s.passed,
                            "reasoning": s.reasoning,
                        })
                    })
                    .collect();

                serde_json::json!({
                    "ordinal": r.ordinal,
                    "example_id": r.example_id,
                    "status": r.status,
                    "duration_ms": r.duration_ms,
                    "tokens_in": r.tokens_in,
                    "tokens_out": r.tokens_out,
                    "error": r.error,
                    "scores": score_items,
                })
            })
            .collect();

        let json = serde_json::json!({
            "experiment": {
                "id": exp.id,
                "name": exp.name,
                "dataset_id": exp.dataset_id,
                "dataset_version": exp.dataset_version,
                "status": exp.status.as_str(),
                "total_examples": exp.total_examples,
                "completed_examples": exp.completed_examples,
                "avg_score": exp.avg_score,
                "min_score": exp.min_score,
                "max_score": exp.max_score,
                "pass_rate": exp.pass_rate,
                "total_duration_ms": exp.total_duration_ms,
                "total_tokens": exp.total_tokens,
                "created_at": exp.created_at.to_rfc3339(),
                "completed_at": exp.completed_at.map(|dt| dt.to_rfc3339()),
            },
            "results": result_items,
        });
        ok_json(&json)
    }

    #[tool(
        name = "compare_eval_experiments",
        description = "Compare two experiments side-by-side showing regressions and improvements"
    )]
    async fn compare_eval_experiments(
        &self,
        params: Parameters<CompareExperimentsParam>,
    ) -> Result<CallToolResult, McpError> {
        let store = self.lock_store()?;
        let left = resolve_experiment_ref(&store, &params.0.left)?;
        let right = resolve_experiment_ref(&store, &params.0.right)?;

        let comparison =
            compare_experiments(&store, &left.id, &right.id, false).map_err(|e| mcp_err(&e))?;

        let json = serde_json::to_value(&comparison).map_err(|e| mcp_err(&e))?;
        ok_json(&json)
    }

    // ── Evaluation write tools ───────────────────────────────

    #[tool(
        name = "create_eval_dataset",
        description = "Create a new evaluation dataset for testing agent behavior"
    )]
    async fn create_eval_dataset(
        &self,
        params: Parameters<CreateEvalDatasetParams>,
    ) -> Result<CallToolResult, McpError> {
        let params = &params.0;
        let store = self.lock_store()?;
        let manager = DatasetManager::new(&store);
        let dataset = manager
            .create(&params.name, params.description.as_deref().unwrap_or(""))
            .map_err(|e| mcp_err(&e))?;

        let json = serde_json::json!({
            "dataset": {
                "id": dataset.id,
                "name": dataset.name,
                "description": dataset.description,
                "version": dataset.version,
                "example_count": dataset.example_count,
                "created_at": dataset.created_at.to_rfc3339(),
            },
            "message": format!("Dataset '{}' created (v1)", dataset.name),
        });
        ok_json(&json)
    }

    #[tool(
        name = "add_eval_example",
        description = "Add an input/expected pair to an evaluation dataset"
    )]
    async fn add_eval_example(
        &self,
        params: Parameters<AddEvalExampleParams>,
    ) -> Result<CallToolResult, McpError> {
        let params = &params.0;
        let store = self.lock_store()?;
        let manager = DatasetManager::new(&store);
        let example = manager
            .add_example(
                &params.dataset,
                params.input.clone(),
                params.expected.clone(),
                params.metadata.clone().unwrap_or(serde_json::json!({})),
            )
            .map_err(|e| mcp_err(&e))?;

        // Get the updated dataset to show version and count
        let updated = store
            .get_dataset_by_name(&params.dataset)
            .map_err(|e| mcp_err(&e))?
            .ok_or_else(|| mcp_err_str(&format!("Dataset '{}' not found after add", params.dataset)))?;

        let json = serde_json::json!({
            "example": {
                "ordinal": example.ordinal,
            },
            "message": format!(
                "Example added to dataset '{}' (now v{}, {} examples)",
                params.dataset, updated.version, updated.example_count
            ),
        });
        ok_json(&json)
    }

    #[tool(
        name = "dataset_from_session",
        description = "Extract an evaluation example from a recorded session's request/response"
    )]
    async fn dataset_from_session(
        &self,
        params: Parameters<DatasetFromSessionParams>,
    ) -> Result<CallToolResult, McpError> {
        let params = &params.0;
        let store = self.lock_store()?;
        let manager = DatasetManager::new(&store);
        let expected_step_num = params.expected_step.unwrap_or(params.input_step);
        let example = manager
            .import_from_session(
                &params.dataset,
                &params.session,
                params.input_step,
                params.expected_step,
            )
            .map_err(|e| mcp_err(&e))?;

        let json = serde_json::json!({
            "example": {
                "ordinal": example.ordinal,
                "source_session": example.source_session_id,
                "input_step": params.input_step,
                "expected_step": expected_step_num,
            },
            "message": format!(
                "Example extracted from session (step {} -> {}) and added to dataset '{}'",
                params.input_step, expected_step_num, params.dataset
            ),
        });
        ok_json(&json)
    }
}

// ── ServerHandler implementation ─────────────────────────────

#[tool_handler(router = self.tool_router)]
impl ServerHandler for RewindMcp {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Rewind is a time-travel debugger for AI agents. \
             Use these tools to inspect recorded agent sessions, \
             view step-by-step traces, examine full request/response content, \
             diff timelines, and create forks to explore alternative paths. \
             Use assertion baselines to create regression tests from known-good sessions \
             and check new sessions for regressions. \
             Use evaluation tools to browse datasets, inspect experiment results with \
             per-example scores, and compare experiments to find regressions and improvements. \
             Use evaluation write tools to create datasets, add examples, and extract examples from sessions."
                .to_string(),
        );
        info
    }
}

// ── Helpers ──────────────────────────────────────────────────

fn ok_json(value: &serde_json::Value) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(value).unwrap(),
    )]))
}

fn mcp_err(e: &dyn std::fmt::Display) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

fn mcp_err_str(msg: &str) -> McpError {
    McpError::internal_error(msg.to_string(), None)
}

fn resolve_experiment_ref(store: &Store, reference: &str) -> Result<Experiment, McpError> {
    store
        .get_experiment_by_name(reference)
        .ok()
        .flatten()
        .or_else(|| store.get_experiment(reference).ok().flatten())
        .ok_or_else(|| mcp_err_str(&format!("Experiment not found: {}", reference)))
}

fn truncate_json_preview(value: &serde_json::Value, max_len: usize) -> String {
    let s = serde_json::to_string(value).unwrap_or_default();
    if s.len() <= max_len {
        s
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}

fn resolve_session(
    store: &Store,
    session_ref: &str,
) -> Result<rewind_store::Session, McpError> {
    if session_ref == "latest" {
        store
            .get_latest_session()
            .map_err(|e| mcp_err(&e))?
            .ok_or_else(|| mcp_err_str("No sessions found. Run 'rewind demo' to create demo data."))
    } else {
        if let Some(session) = store.get_session(session_ref).map_err(|e| mcp_err(&e))? {
            return Ok(session);
        }
        let sessions = store.list_sessions().map_err(|e| mcp_err(&e))?;
        sessions
            .into_iter()
            .find(|s| s.id.starts_with(session_ref))
            .ok_or_else(|| mcp_err_str(&format!("Session not found: {}", session_ref)))
    }
}

fn resolve_timeline_ref(
    timelines: &[rewind_store::Timeline],
    reference: &str,
) -> Result<String, McpError> {
    if let Some(t) = timelines.iter().find(|t| t.id == reference) {
        return Ok(t.id.clone());
    }
    if let Some(t) = timelines.iter().find(|t| t.id.starts_with(reference)) {
        return Ok(t.id.clone());
    }
    if let Some(t) = timelines.iter().find(|t| t.label == reference) {
        return Ok(t.id.clone());
    }
    Err(mcp_err_str(&format!("Timeline not found: {}", reference)))
}

fn extract_response_preview(store: &Store, response_blob: &str) -> String {
    store
        .blobs
        .get(response_blob)
        .ok()
        .and_then(|data| String::from_utf8(data).ok())
        .and_then(|json_str| {
            let val: serde_json::Value = serde_json::from_str(&json_str).ok()?;
            if let Some(content) = val
                .pointer("/choices/0/message/content")
                .and_then(|c| c.as_str())
            {
                return Some(content.replace('\n', " ").chars().take(150).collect());
            }
            if let Some(calls) = val
                .pointer("/choices/0/message/tool_calls")
                .and_then(|c| c.as_array())
            {
                let names: Vec<&str> = calls
                    .iter()
                    .filter_map(|c| c.pointer("/function/name").and_then(|n| n.as_str()))
                    .collect();
                return Some(format!("tool_calls: {}", names.join(", ")));
            }
            if let Some(content) = val.get("content").and_then(|c| c.as_array())
                && let Some(text) = content
                    .first()
                    .and_then(|b| b.get("text"))
                    .and_then(|t| t.as_str())
            {
                return Some(text.replace('\n', " ").chars().take(150).collect());
            }
            Some(json_str.chars().take(150).collect())
        })
        .unwrap_or_else(|| "(no response)".to_string())
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
