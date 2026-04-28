use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub status: SessionStatus,
    pub source: SessionSource,
    pub total_steps: u32,
    pub total_tokens: u64,
    pub metadata: serde_json::Value,
    pub thread_id: Option<String>,
    pub thread_ordinal: Option<u32>,
    /// Caller-supplied stable key for idempotent session creation.
    ///
    /// When set, two `/sessions/start` requests carrying the same key
    /// resolve to the SAME session — the server returns the existing
    /// row instead of inserting a duplicate. Closes the multi-replica
    /// race where each runner-process keeps its own in-process
    /// `_session_cache` and creates a fresh session on cache miss.
    ///
    /// Database invariant: `UNIQUE WHERE client_session_key IS NOT
    /// NULL` so concurrent inserts collapse via the constraint.
    pub client_session_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionSource {
    Proxy,
    Direct,
    Hooks,
    OtelImport,
    Api,
}

impl SessionSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionSource::Proxy => "proxy",
            SessionSource::Direct => "direct",
            SessionSource::Hooks => "hooks",
            SessionSource::OtelImport => "otel_import",
            SessionSource::Api => "api",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "proxy" => SessionSource::Proxy,
            "direct" => SessionSource::Direct,
            "hooks" => SessionSource::Hooks,
            "otel_import" => SessionSource::OtelImport,
            "api" => SessionSource::Api,
            _ => SessionSource::Proxy,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SessionStatus {
    Recording,
    Completed,
    Failed,
    Forked,
}

impl SessionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionStatus::Recording => "recording",
            SessionStatus::Completed => "completed",
            SessionStatus::Failed => "failed",
            SessionStatus::Forked => "forked",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "recording" => SessionStatus::Recording,
            "completed" => SessionStatus::Completed,
            "failed" => SessionStatus::Failed,
            "forked" => SessionStatus::Forked,
            _ => SessionStatus::Recording,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timeline {
    pub id: String,
    pub session_id: String,
    pub parent_timeline_id: Option<String>,
    pub fork_at_step: Option<u32>,
    pub created_at: DateTime<Utc>,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    pub timeline_id: String,
    pub session_id: String,
    pub step_number: u32,
    pub step_type: StepType,
    pub status: StepStatus,
    pub created_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub model: String,
    pub request_blob: String,  // SHA-256 hash -> blob store (content-addressed)
    pub response_blob: String, // SHA-256 hash -> blob store (content-addressed)
    pub error: Option<String>,
    pub span_id: Option<String>,
    pub tool_name: Option<String>,
    /// Step 0.1: post-redaction canonical hash of the request body.
    ///
    /// Distinct from `request_blob`: the blob hash is computed without
    /// guarantees about redaction passes (the explicit-API record path on
    /// `crates/rewind-web/src/api.rs:955` does not redact, while the proxy
    /// path on `crates/rewind-proxy/src/lib.rs:289` does). For replay-cache
    /// content validation we need a single canonical hash that's identical
    /// between record and lookup, regardless of which record path was used.
    /// `None` for pre-migration rows — the lookup treats `None` as "match
    /// anything" to preserve backwards-compatible behavior.
    pub request_hash: Option<String>,
    /// Step 0.3: discriminator for the `response_blob` payload format.
    ///
    /// **`0` = `FORMAT_NAKED_LEGACY`** — naked JSON body. Used by:
    ///
    /// - Pre-migration rows (column DEFAULT 0); back-compat with v0.12.x
    ///   data written before the envelope existed.
    /// - Explicit-API record paths (`record_llm_call`, `record_tool_call`
    ///   in `crates/rewind-web/src/api.rs`): the SDK caller hands us a
    ///   parsed JSON `Value`, so there's no HTTP status/headers to wrap —
    ///   persisting naked is the natural fit.
    /// - Hooks ingest path (`crates/rewind-otel/src/ingest.rs`): OTel
    ///   spans don't carry HTTP envelopes either.
    /// - Transcript import (`crates/rewind-web/src/transcript.rs`):
    ///   synthesizes steps from Claude Code transcripts, no HTTP source.
    ///
    /// On read, `ResponseEnvelope::from_blob_bytes` decodes a format-0
    /// blob as `{status: 200, headers: [], body: <raw bytes>}` so the
    /// downstream code sees the same envelope shape as a real HTTP capture.
    ///
    /// **`1` = `FORMAT_ENVELOPE_V1`** — `ResponseEnvelope` (status code +
    /// scrubbed headers + body). Used **only** by the proxy record path
    /// (`handle_buffered_response` / `handle_streaming_response` in
    /// `crates/rewind-proxy/src/lib.rs`), which has the full HTTP wire
    /// response to preserve. Replayed steps that inherit from a cache
    /// hit copy this format from the originating cached step.
    ///
    /// Unknown values fall back to `0` parsing for forward-compat.
    pub response_blob_format: u8,
}

/// Step 0.1: row shape of the `replay_contexts` table.
///
/// Replaces the 5-tuple return type from `Store::get_replay_context` that
/// clippy flagged as `type_complexity`. Named fields also make the call
/// site self-documenting.
#[derive(Debug, Clone)]
pub struct ReplayContextRow {
    pub session_id: String,
    pub timeline_id: String,
    /// Step number the replay forks at. `current_step` advances within
    /// `[from_step, total_steps]`.
    pub from_step: u32,
    /// Number of replay-lookup calls served so far for this context.
    /// Advanced by `Store::advance_replay_context` on the success path;
    /// peeked by `Store::peek_next_replay_step` for pre-validation
    /// lookups so a strict-mode 409 doesn't consume an ordinal slot.
    pub current_step: u32,
    /// **Phase 3 commit 6 (review #154 F3):** the replay-job dispatch
    /// path uses `last_accessed_at` to refuse Shape-B reuse of contexts
    /// older than the TTL window so a stale context can't silently
    /// resume cache lookups against expired blobs.
    pub last_accessed_at: chrono::DateTime<chrono::Utc>,
    /// `false` (default) → warn-on-divergence: cache hit returns the
    /// recorded step plus an `X-Rewind-Cache-Divergent: true` header.
    /// `true` → divergence escalates to HTTP 409 and the cursor stays put
    /// so the caller can retry without consuming an ordinal slot.
    pub strict_match: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StepType {
    LlmCall,
    ToolCall,
    ToolResult,
    UserPrompt,
    HookEvent,
}

impl StepType {
    pub fn as_str(&self) -> &'static str {
        match self {
            StepType::LlmCall => "llm_call",
            StepType::ToolCall => "tool_call",
            StepType::ToolResult => "tool_result",
            StepType::UserPrompt => "user_prompt",
            StepType::HookEvent => "hook_event",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "llm_call" => StepType::LlmCall,
            "tool_call" => StepType::ToolCall,
            "tool_result" => StepType::ToolResult,
            "user_prompt" => StepType::UserPrompt,
            "hook_event" => StepType::HookEvent,
            _ => StepType::LlmCall,
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            StepType::LlmCall => "🧠",
            StepType::ToolCall => "🔧",
            StepType::ToolResult => "📋",
            StepType::UserPrompt => "💬",
            StepType::HookEvent => "🪝",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            StepType::LlmCall => "LLM Call",
            StepType::ToolCall => "Tool Call",
            StepType::ToolResult => "Tool Result",
            StepType::UserPrompt => "User Prompt",
            StepType::HookEvent => "Hook Event",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StepStatus {
    Success,
    Error,
    Pending,
}

impl StepStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            StepStatus::Success => "success",
            StepStatus::Error => "error",
            StepStatus::Pending => "pending",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "success" => StepStatus::Success,
            "error" => StepStatus::Error,
            "pending" => StepStatus::Pending,
            _ => StepStatus::Pending,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    pub id: String,
    pub session_id: String,
    pub timeline_id: String,
    pub parent_span_id: Option<String>,
    pub span_type: SpanType,
    pub name: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub duration_ms: u64,
    pub metadata: serde_json::Value,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SpanType {
    Agent,
    Tool,
    Handoff,
    Custom,
}

impl SpanType {
    pub fn as_str(&self) -> &'static str {
        match self {
            SpanType::Agent => "agent",
            SpanType::Tool => "tool",
            SpanType::Handoff => "handoff",
            SpanType::Custom => "custom",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "agent" => SpanType::Agent,
            "tool" => SpanType::Tool,
            "handoff" => SpanType::Handoff,
            "custom" => SpanType::Custom,
            _ => SpanType::Custom,
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            SpanType::Agent => "🤖",
            SpanType::Tool => "🔧",
            SpanType::Handoff => "🔀",
            SpanType::Custom => "📦",
        }
    }
}

impl Span {
    pub fn new(session_id: &str, timeline_id: &str, span_type: SpanType, name: &str) -> Self {
        Span {
            id: Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            timeline_id: timeline_id.to_string(),
            parent_span_id: None,
            span_type,
            name: name.to_string(),
            status: "running".to_string(),
            started_at: Utc::now(),
            ended_at: None,
            duration_ms: 0,
            metadata: serde_json::json!({}),
            error: None,
        }
    }

    pub fn with_parent(mut self, parent_span_id: &str) -> Self {
        self.parent_span_id = Some(parent_span_id.to_string());
        self
    }
}

impl Session {
    pub fn new(name: &str) -> Self {
        let now = Utc::now();
        Session {
            id: Uuid::new_v4().to_string(),
            name: name.to_string(),
            created_at: now,
            updated_at: now,
            status: SessionStatus::Recording,
            source: SessionSource::Proxy,
            total_steps: 0,
            total_tokens: 0,
            metadata: serde_json::json!({}),
            thread_id: None,
            thread_ordinal: None,
            client_session_key: None,
        }
    }
}

impl Timeline {
    pub fn new_root(session_id: &str) -> Self {
        Timeline {
            id: Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            parent_timeline_id: None,
            fork_at_step: None,
            created_at: Utc::now(),
            label: "main".to_string(),
        }
    }

    pub fn new_fork(session_id: &str, parent_timeline_id: &str, fork_at_step: u32, label: &str) -> Self {
        Timeline {
            id: Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            parent_timeline_id: Some(parent_timeline_id.to_string()),
            fork_at_step: Some(fork_at_step),
            created_at: Utc::now(),
            label: label.to_string(),
        }
    }
}

// ── Instant Replay Cache ──────────────────────────────────

#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub request_hash: String,
    pub response_blob: String,
    pub model: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub hit_count: u64,
    /// Step 0.3: format discriminator inherited from the originating step.
    /// Pre-v0.13 cache rows decode as 0 (legacy naked body) via the column
    /// DEFAULT, preserving back-compat. New `cache_put` calls forward the
    /// originating step's format so cache hits can be unwrapped consistently
    /// without sniff heuristics.
    pub response_blob_format: u8,
}

#[derive(Debug, Clone, Serialize)]
pub struct CacheStats {
    pub entries: u64,
    pub total_hits: u64,
    pub total_tokens_saved: u64,
}

// ── Snapshots ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub session_id: Option<String>,
    pub label: String,
    pub directory: String,
    pub blob_hash: String,
    pub file_count: u32,
    pub size_bytes: u64,
    pub created_at: DateTime<Utc>,
}

impl Snapshot {
    pub fn new(label: &str, directory: &str, blob_hash: &str, file_count: u32, size_bytes: u64) -> Self {
        Snapshot {
            id: Uuid::new_v4().to_string(),
            session_id: None,
            label: label.to_string(),
            directory: directory.to_string(),
            blob_hash: blob_hash.to_string(),
            file_count,
            size_bytes,
            created_at: Utc::now(),
        }
    }
}

impl Step {
    pub fn new_llm_call(
        timeline_id: &str,
        session_id: &str,
        step_number: u32,
        model: &str,
    ) -> Self {
        Step {
            id: Uuid::new_v4().to_string(),
            timeline_id: timeline_id.to_string(),
            session_id: session_id.to_string(),
            step_number,
            step_type: StepType::LlmCall,
            status: StepStatus::Pending,
            created_at: Utc::now(),
            duration_ms: 0,
            tokens_in: 0,
            tokens_out: 0,
            model: model.to_string(),
            request_blob: String::new(),
            response_blob: String::new(),
            error: None,
            span_id: None,
            tool_name: None,
            request_hash: None,
            response_blob_format: 0,
        }
    }
}

// ── Evaluation: Datasets ─────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dataset {
    pub id: String,
    pub name: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub version: u32,
    pub example_count: u32,
    pub metadata: serde_json::Value,
}

impl Dataset {
    pub fn new(name: &str, description: &str) -> Self {
        let now = Utc::now();
        Dataset {
            id: Uuid::new_v4().to_string(),
            name: name.to_string(),
            description: description.to_string(),
            created_at: now,
            updated_at: now,
            version: 1,
            example_count: 0,
            metadata: serde_json::json!({}),
        }
    }

    pub fn new_version(&self) -> Self {
        let now = Utc::now();
        Dataset {
            id: Uuid::new_v4().to_string(),
            name: self.name.clone(),
            description: self.description.clone(),
            created_at: self.created_at,
            updated_at: now,
            version: self.version + 1,
            example_count: self.example_count,
            metadata: self.metadata.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetExample {
    pub id: String,
    pub dataset_id: String,
    pub ordinal: u32,
    pub input_blob: String,
    pub expected_blob: String,
    pub metadata: serde_json::Value,
    pub source_session_id: Option<String>,
    pub source_step_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl DatasetExample {
    pub fn new(dataset_id: &str, ordinal: u32, input_blob: &str, expected_blob: &str) -> Self {
        DatasetExample {
            id: Uuid::new_v4().to_string(),
            dataset_id: dataset_id.to_string(),
            ordinal,
            input_blob: input_blob.to_string(),
            expected_blob: expected_blob.to_string(),
            metadata: serde_json::json!({}),
            source_session_id: None,
            source_step_id: None,
            created_at: Utc::now(),
        }
    }
}

// ── Evaluation: Evaluators ───────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evaluator {
    pub id: String,
    pub name: String,
    pub evaluator_type: String,
    pub config_blob: String,
    pub created_at: DateTime<Utc>,
    pub description: String,
}

impl Evaluator {
    pub fn new(name: &str, evaluator_type: &str, config_blob: &str, description: &str) -> Self {
        Evaluator {
            id: Uuid::new_v4().to_string(),
            name: name.to_string(),
            evaluator_type: evaluator_type.to_string(),
            config_blob: config_blob.to_string(),
            created_at: Utc::now(),
            description: description.to_string(),
        }
    }
}

// ── Evaluation: Experiments ──────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Experiment {
    pub id: String,
    pub name: String,
    pub dataset_id: String,
    pub dataset_version: u32,
    pub status: ExperimentStatus,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub total_examples: u32,
    pub completed_examples: u32,
    pub avg_score: Option<f64>,
    pub min_score: Option<f64>,
    pub max_score: Option<f64>,
    pub pass_rate: Option<f64>,
    pub total_duration_ms: u64,
    pub total_tokens: u64,
    pub config_blob: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ExperimentStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl ExperimentStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExperimentStatus::Pending => "pending",
            ExperimentStatus::Running => "running",
            ExperimentStatus::Completed => "completed",
            ExperimentStatus::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "running" => ExperimentStatus::Running,
            "completed" => ExperimentStatus::Completed,
            "failed" => ExperimentStatus::Failed,
            _ => ExperimentStatus::Pending,
        }
    }
}

impl Experiment {
    pub fn new(name: &str, dataset_id: &str, dataset_version: u32, total_examples: u32, config_blob: &str) -> Self {
        Experiment {
            id: Uuid::new_v4().to_string(),
            name: name.to_string(),
            dataset_id: dataset_id.to_string(),
            dataset_version,
            status: ExperimentStatus::Pending,
            created_at: Utc::now(),
            completed_at: None,
            total_examples,
            completed_examples: 0,
            avg_score: None,
            min_score: None,
            max_score: None,
            pass_rate: None,
            total_duration_ms: 0,
            total_tokens: 0,
            config_blob: config_blob.to_string(),
            metadata: serde_json::json!({}),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentResult {
    pub id: String,
    pub experiment_id: String,
    pub example_id: String,
    pub ordinal: u32,
    pub output_blob: String,
    pub trace_session_id: Option<String>,
    pub trace_timeline_id: Option<String>,
    pub duration_ms: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub status: String,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl ExperimentResult {
    pub fn new(experiment_id: &str, example_id: &str, ordinal: u32) -> Self {
        ExperimentResult {
            id: Uuid::new_v4().to_string(),
            experiment_id: experiment_id.to_string(),
            example_id: example_id.to_string(),
            ordinal,
            output_blob: String::new(),
            trace_session_id: None,
            trace_timeline_id: None,
            duration_ms: 0,
            tokens_in: 0,
            tokens_out: 0,
            status: "pending".to_string(),
            error: None,
            created_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentScore {
    pub id: String,
    pub result_id: String,
    pub evaluator_id: String,
    pub score: f64,
    pub passed: bool,
    pub reasoning: String,
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

impl ExperimentScore {
    pub fn new(result_id: &str, evaluator_id: &str, score: f64, passed: bool, reasoning: &str) -> Self {
        ExperimentScore {
            id: Uuid::new_v4().to_string(),
            result_id: result_id.to_string(),
            evaluator_id: evaluator_id.to_string(),
            score,
            passed,
            reasoning: reasoning.to_string(),
            metadata: serde_json::json!({}),
            created_at: Utc::now(),
        }
    }
}

// ── Timeline Scores (LLM-as-Judge) ─────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineScore {
    pub id: String,
    pub session_id: String,
    pub timeline_id: String,
    pub evaluator_id: String,
    pub score: f64,
    pub passed: bool,
    pub reasoning: String,
    pub input_blob: String,
    pub output_blob: String,
    pub created_at: DateTime<Utc>,
}

impl TimelineScore {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: &str,
        timeline_id: &str,
        evaluator_id: &str,
        score: f64,
        passed: bool,
        reasoning: &str,
        input_blob: &str,
        output_blob: &str,
    ) -> Self {
        TimelineScore {
            id: Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            timeline_id: timeline_id.to_string(),
            evaluator_id: evaluator_id.to_string(),
            score,
            passed,
            reasoning: reasoning.to_string(),
            input_blob: input_blob.to_string(),
            output_blob: output_blob.to_string(),
            created_at: Utc::now(),
        }
    }
}

// ── Assertion Baselines ──────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baseline {
    pub id: String,
    pub name: String,
    pub source_session_id: String,
    pub source_timeline_id: String,
    pub created_at: DateTime<Utc>,
    pub description: String,
    pub step_count: u32,
    pub total_tokens: u64,
    pub metadata: serde_json::Value,
}

impl Baseline {
    pub fn new(name: &str, source_session_id: &str, source_timeline_id: &str, description: &str, step_count: u32, total_tokens: u64) -> Self {
        Baseline {
            id: Uuid::new_v4().to_string(),
            name: name.to_string(),
            source_session_id: source_session_id.to_string(),
            source_timeline_id: source_timeline_id.to_string(),
            created_at: Utc::now(),
            description: description.to_string(),
            step_count,
            total_tokens,
            metadata: serde_json::json!({}),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineStep {
    pub id: String,
    pub baseline_id: String,
    pub step_number: u32,
    pub step_type: String,
    pub expected_status: String,
    pub expected_model: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub tool_name: Option<String>,
    pub response_blob: String,
    pub request_blob: String,
    pub has_error: bool,
}

impl BaselineStep {
    pub fn from_step(baseline_id: &str, step: &Step, tool_name: Option<String>) -> Self {
        BaselineStep {
            id: Uuid::new_v4().to_string(),
            baseline_id: baseline_id.to_string(),
            step_number: step.step_number,
            step_type: step.step_type.as_str().to_string(),
            expected_status: step.status.as_str().to_string(),
            expected_model: step.model.clone(),
            tokens_in: step.tokens_in,
            tokens_out: step.tokens_out,
            tool_name,
            response_blob: step.response_blob.clone(),
            request_blob: step.request_blob.clone(),
            has_error: step.error.is_some(),
        }
    }
}
