//! Session serializer for sharing.
//!
//! Serializes a session + timelines + steps + spans + scores into a single
//! JSON blob. Two modes:
//!   - **Metadata only** (default): step types, models, tokens, timing, span
//!     tree, scores. No request/response blobs.
//!   - **With content**: includes blob values inline (request/response bodies).

use anyhow::{Context, Result};
use serde::Serialize;

use crate::{Session, Span, Step, Store, Timeline, TimelineScore};

/// A fully serialized session, ready for upload or file export.
#[derive(Debug, Clone, Serialize)]
pub struct ExportedSession {
    pub session: Session,
    pub timelines: Vec<ExportedTimeline>,
    pub spans: Vec<Span>,
    pub scores: Vec<TimelineScore>,
    pub include_content: bool,
    pub export_version: u32,
}

/// A timeline with its steps (and optionally resolved blob content).
#[derive(Debug, Clone, Serialize)]
pub struct ExportedTimeline {
    pub timeline: Timeline,
    pub steps: Vec<ExportedStep>,
}

/// A step, optionally with inline request/response content.
#[derive(Debug, Clone, Serialize)]
pub struct ExportedStep {
    #[serde(flatten)]
    pub step: Step,
    /// Inline request body (only when `include_content = true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_content: Option<serde_json::Value>,
    /// Inline response body (only when `include_content = true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_content: Option<serde_json::Value>,
}

/// Serialize a session into a self-contained JSON structure.
///
/// When `include_content` is false, only metadata is included (step types,
/// models, tokens, timing, tool names). When true, request/response blobs
/// are resolved from the blob store and included inline.
pub fn serialize_session(
    store: &Store,
    session_id: &str,
    include_content: bool,
) -> Result<ExportedSession> {
    let session = store
        .get_session(session_id)?
        .context("Session not found")?;

    let timelines = store.get_timelines(session_id)?;
    let spans = store.get_spans_by_session(session_id)?;
    let scores = store.get_timeline_scores(session_id)?;

    let mut exported_timelines = Vec::with_capacity(timelines.len());
    for tl in &timelines {
        let steps = store.get_steps(&tl.id)?;
        let exported_steps: Vec<ExportedStep> = steps
            .into_iter()
            .map(|step| export_step(store, step, include_content))
            .collect();

        exported_timelines.push(ExportedTimeline {
            timeline: tl.clone(),
            steps: exported_steps,
        });
    }

    Ok(ExportedSession {
        session,
        timelines: exported_timelines,
        spans,
        scores,
        include_content,
        export_version: 1,
    })
}

fn export_step(store: &Store, step: Step, include_content: bool) -> ExportedStep {
    let (request_content, response_content) = if include_content {
        (
            resolve_blob(store, &step.request_blob),
            resolve_blob(store, &step.response_blob),
        )
    } else {
        (None, None)
    };

    ExportedStep {
        step,
        request_content,
        response_content,
    }
}

fn resolve_blob(store: &Store, hash: &str) -> Option<serde_json::Value> {
    if hash.is_empty() {
        return None;
    }
    store
        .blobs
        .get(hash)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SessionSource, SessionStatus, StepStatus, StepType};
    use chrono::Utc;

    fn setup_store() -> (Store, String, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();

        let session = Session {
            id: "sess-1".into(),
            name: "test-session".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            status: SessionStatus::Completed,
            source: SessionSource::Direct,
            total_steps: 2,
            total_tokens: 300,
            metadata: serde_json::json!({}),
            thread_id: None,
            thread_ordinal: None,
        };
        store.create_session(&session).unwrap();

        let tl = Timeline::new_root(&session.id);
        store.create_timeline(&tl).unwrap();

        // Step 1 with blob content
        let req_hash = store.blobs.put(
            br#"{"messages":[{"role":"user","content":"hello"}]}"#,
        ).unwrap();
        let resp_hash = store.blobs.put(
            br#"{"choices":[{"message":{"content":"hi there"}}]}"#,
        ).unwrap();
        let step1 = Step {
            id: "step-1".into(),
            timeline_id: tl.id.clone(),
            session_id: session.id.clone(),
            step_number: 1,
            step_type: StepType::LlmCall,
            status: StepStatus::Success,
            created_at: Utc::now(),
            duration_ms: 500,
            tokens_in: 100,
            tokens_out: 50,
            model: "gpt-4o".into(),
            request_blob: req_hash,
            response_blob: resp_hash,
            error: None,
            span_id: None,
            tool_name: None,
        };
        store.create_step(&step1).unwrap();

        // Step 2 — tool call, no blobs
        let step2 = Step {
            id: "step-2".into(),
            timeline_id: tl.id.clone(),
            session_id: session.id.clone(),
            step_number: 2,
            step_type: StepType::ToolCall,
            status: StepStatus::Success,
            created_at: Utc::now(),
            duration_ms: 200,
            tokens_in: 100,
            tokens_out: 50,
            model: String::new(),
            request_blob: String::new(),
            response_blob: String::new(),
            error: None,
            span_id: None,
            tool_name: Some("search".into()),
        };
        store.create_step(&step2).unwrap();

        (store, session.id, dir)
    }

    #[test]
    fn metadata_only_excludes_content() {
        let (store, sid, _dir) = setup_store();
        let exported = serialize_session(&store, &sid, false).unwrap();

        assert_eq!(exported.session.id, "sess-1");
        assert_eq!(exported.timelines.len(), 1);
        assert_eq!(exported.timelines[0].steps.len(), 2);
        assert!(!exported.include_content);
        assert_eq!(exported.export_version, 1);

        // No content in metadata-only mode
        for step in &exported.timelines[0].steps {
            assert!(step.request_content.is_none());
            assert!(step.response_content.is_none());
        }
    }

    #[test]
    fn with_content_includes_blobs() {
        let (store, sid, _dir) = setup_store();
        let exported = serialize_session(&store, &sid, true).unwrap();

        assert!(exported.include_content);

        let step1 = &exported.timelines[0].steps[0];
        assert!(step1.request_content.is_some());
        assert!(step1.response_content.is_some());

        // Step 2 has empty blob hashes — content should be None
        let step2 = &exported.timelines[0].steps[1];
        assert!(step2.request_content.is_none());
        assert!(step2.response_content.is_none());
    }

    #[test]
    fn serializes_to_json() {
        let (store, sid, _dir) = setup_store();
        let exported = serialize_session(&store, &sid, false).unwrap();

        let json = serde_json::to_string_pretty(&exported).unwrap();
        assert!(json.contains("test-session"));
        assert!(json.contains("gpt-4o"));
        // request_content/response_content not present (skip_serializing_if)
        assert!(!json.contains("request_content"));
    }

    #[test]
    fn with_content_serializes_blobs_inline() {
        let (store, sid, _dir) = setup_store();
        let exported = serialize_session(&store, &sid, true).unwrap();

        let json = serde_json::to_string(&exported).unwrap();
        assert!(json.contains("hello")); // from request blob
        assert!(json.contains("hi there")); // from response blob
    }

    #[test]
    fn missing_session_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let result = serialize_session(&store, "nonexistent", false);
        assert!(result.is_err());
    }
}
