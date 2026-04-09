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
    pub total_steps: u32,
    pub total_tokens: u64,
    pub metadata: serde_json::Value,
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
    pub request_blob: String,  // SHA-256 hash -> blob store
    pub response_blob: String, // SHA-256 hash -> blob store
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StepType {
    LlmCall,
    ToolCall,
    ToolResult,
}

impl StepType {
    pub fn as_str(&self) -> &'static str {
        match self {
            StepType::LlmCall => "llm_call",
            StepType::ToolCall => "tool_call",
            StepType::ToolResult => "tool_result",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "llm_call" => StepType::LlmCall,
            "tool_call" => StepType::ToolCall,
            "tool_result" => StepType::ToolResult,
            _ => StepType::LlmCall,
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            StepType::LlmCall => "🧠",
            StepType::ToolCall => "🔧",
            StepType::ToolResult => "📋",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            StepType::LlmCall => "LLM Call",
            StepType::ToolCall => "Tool Call",
            StepType::ToolResult => "Tool Result",
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

impl Session {
    pub fn new(name: &str) -> Self {
        let now = Utc::now();
        Session {
            id: Uuid::new_v4().to_string(),
            name: name.to_string(),
            created_at: now,
            updated_at: now,
            status: SessionStatus::Recording,
            total_steps: 0,
            total_tokens: 0,
            metadata: serde_json::json!({}),
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
