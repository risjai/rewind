//! Extract session timeline outputs for LLM-as-judge scoring.

use anyhow::{bail, Result};
use rewind_store::{Step, StepType, Store};
use serde_json::Value;

/// Extract (input, output) from a timeline's steps for evaluation.
///
/// - `input`: The request blob of the FIRST LlmCall step (initial prompt).
/// - `output`: The response blob of the LAST LlmCall step (final response).
///
/// Returns (Value::Null, Value::Null) if no LlmCall steps exist.
pub fn extract_timeline_output(
    store: &Store,
    timeline_id: &str,
) -> Result<(Value, Value)> {
    let steps = store.get_steps(timeline_id)?;

    let llm_steps: Vec<&Step> = steps
        .iter()
        .filter(|s| s.step_type == StepType::LlmCall)
        .collect();

    if llm_steps.is_empty() {
        return Ok((Value::Null, Value::Null));
    }

    let first = llm_steps.first().unwrap();
    let last = llm_steps.last().unwrap();

    let input = if first.request_blob.is_empty() {
        Value::Null
    } else {
        store
            .blobs
            .get_json::<Value>(&first.request_blob)
            .unwrap_or(Value::Null)
    };

    let output = if last.response_blob.is_empty() {
        Value::Null
    } else {
        store
            .blobs
            .get_json::<Value>(&last.response_blob)
            .unwrap_or(Value::Null)
    };

    Ok((input, output))
}

/// Validate that a session exists and has timelines.
pub fn validate_session_for_scoring(store: &Store, session_id: &str) -> Result<()> {
    let session = store
        .get_session(session_id)?
        .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

    let timelines = store.get_timelines(&session.id)?;
    if timelines.is_empty() {
        bail!(
            "Session '{}' has no timelines — nothing to score",
            session.name
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rewind_store::{Session, Step, StepType, Store, Timeline};
    use tempfile::TempDir;

    fn make_store() -> (Store, TempDir) {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (store, dir)
    }

    fn make_step(timeline_id: &str, session_id: &str, step_number: u32, step_type: StepType) -> Step {
        let mut step = Step::new_llm_call(timeline_id, session_id, step_number, "gpt-4o");
        step.step_type = step_type;
        step
    }

    fn seed_session_with_steps(store: &Store) -> (String, String) {
        let session = Session::new("test-session");
        let session_id = session.id.clone();
        store.create_session(&session).unwrap();

        let timeline = Timeline::new_root(&session_id);
        let timeline_id = timeline.id.clone();
        store.create_timeline(&timeline).unwrap();

        // Store request and response blobs
        let req_blob = store
            .blobs
            .put_json(&serde_json::json!({"messages": [{"role": "user", "content": "What is 2+2?"}]}))
            .unwrap();
        let resp_blob = store
            .blobs
            .put_json(&serde_json::json!({"choices": [{"message": {"content": "4"}}]}))
            .unwrap();

        let mut step = Step::new_llm_call(&timeline_id, &session_id, 1, "gpt-4o");
        step.request_blob = req_blob;
        step.response_blob = resp_blob;
        step.tokens_in = 10;
        step.tokens_out = 5;
        step.duration_ms = 100;
        store.create_step(&step).unwrap();

        (session_id, timeline_id)
    }

    #[test]
    fn test_extract_timeline_output_with_llm_steps() {
        let (store, _dir) = make_store();
        let (_, timeline_id) = seed_session_with_steps(&store);

        let (input, output) = extract_timeline_output(&store, &timeline_id).unwrap();

        assert!(input.is_object());
        assert!(input.get("messages").is_some());
        assert!(output.is_object());
        assert!(output.get("choices").is_some());
    }

    #[test]
    fn test_extract_timeline_output_no_steps() {
        let (store, _dir) = make_store();

        let session = Session::new("empty");
        let session_id = session.id.clone();
        store.create_session(&session).unwrap();

        let timeline = Timeline::new_root(&session_id);
        let timeline_id = timeline.id.clone();
        store.create_timeline(&timeline).unwrap();

        let (input, output) = extract_timeline_output(&store, &timeline_id).unwrap();
        assert!(input.is_null());
        assert!(output.is_null());
    }

    #[test]
    fn test_extract_skips_tool_steps() {
        let (store, _dir) = make_store();

        let session = Session::new("tool-test");
        let session_id = session.id.clone();
        store.create_session(&session).unwrap();

        let timeline = Timeline::new_root(&session_id);
        let timeline_id = timeline.id.clone();
        store.create_timeline(&timeline).unwrap();

        // LlmCall step
        let req_blob = store.blobs.put_json(&serde_json::json!({"prompt": "hello"})).unwrap();
        let resp_blob = store.blobs.put_json(&serde_json::json!({"answer": "hi"})).unwrap();
        let mut step1 = make_step(&timeline_id, &session_id, 1, StepType::LlmCall);
        step1.request_blob = req_blob;
        step1.response_blob = resp_blob;
        store.create_step(&step1).unwrap();

        // ToolCall step (should be skipped)
        let tool_blob = store.blobs.put_json(&serde_json::json!({"tool": "search"})).unwrap();
        let mut step2 = make_step(&timeline_id, &session_id, 2, StepType::ToolCall);
        step2.request_blob = tool_blob.clone();
        step2.response_blob = tool_blob;
        store.create_step(&step2).unwrap();

        // ToolResult step (should be skipped)
        let result_blob = store.blobs.put_json(&serde_json::json!({"result": "found"})).unwrap();
        let mut step3 = make_step(&timeline_id, &session_id, 3, StepType::ToolResult);
        step3.response_blob = result_blob;
        store.create_step(&step3).unwrap();

        // Second LlmCall (this should be the "output")
        let resp2_blob = store.blobs.put_json(&serde_json::json!({"final": "response"})).unwrap();
        let mut step4 = make_step(&timeline_id, &session_id, 4, StepType::LlmCall);
        step4.response_blob = resp2_blob;
        store.create_step(&step4).unwrap();

        let (input, output) = extract_timeline_output(&store, &timeline_id).unwrap();

        // Input = first LlmCall's request
        assert_eq!(input, serde_json::json!({"prompt": "hello"}));
        // Output = last LlmCall's response (step 4, not step 2/3)
        assert_eq!(output, serde_json::json!({"final": "response"}));
    }

    #[test]
    fn test_validate_session_for_scoring_ok() {
        let (store, _dir) = make_store();
        let (session_id, _) = seed_session_with_steps(&store);
        assert!(validate_session_for_scoring(&store, &session_id).is_ok());
    }

    #[test]
    fn test_validate_session_not_found() {
        let (store, _dir) = make_store();
        assert!(validate_session_for_scoring(&store, "nonexistent").is_err());
    }
}
