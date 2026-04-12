use opentelemetry::KeyValue;
use rewind_store::models::{Step, StepType};
use serde_json::Value;

/// OTel span kind classification per StepType.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OtelSpanKind {
    /// LLM calls are CLIENT spans (remote model invocation).
    Client,
    /// Tool calls, user prompts, hook events are INTERNAL spans.
    Internal,
}

/// Derive the OTel span name from a step.
///
/// - LlmCall → "gen_ai.chat {model}"
/// - ToolCall → "tool.execute {tool_name}"
/// - ToolResult → "tool.result {tool_name}"
/// - UserPrompt → "user.prompt"
/// - HookEvent → "hook.event"
pub fn span_name(step: &Step) -> String {
    match step.step_type {
        StepType::LlmCall => {
            let model = if step.model.is_empty() { "unknown" } else { &step.model };
            format!("gen_ai.chat {}", model)
        }
        StepType::ToolCall => {
            let name = step.tool_name.as_deref().unwrap_or("unknown");
            format!("tool.execute {}", name)
        }
        StepType::ToolResult => {
            let name = step.tool_name.as_deref().unwrap_or("unknown");
            format!("tool.result {}", name)
        }
        StepType::UserPrompt => "user.prompt".to_string(),
        StepType::HookEvent => "hook.event".to_string(),
    }
}

/// Derive OTel SpanKind from StepType.
pub fn span_kind(step: &Step) -> OtelSpanKind {
    match step.step_type {
        StepType::LlmCall => OtelSpanKind::Client,
        _ => OtelSpanKind::Internal,
    }
}

/// Infer the LLM provider from the model name.
pub fn infer_provider(model: &str) -> &'static str {
    let m = model.to_lowercase();
    // Strip common routing prefixes (e.g. "openai/gpt-4o-mini", "anthropic/claude-3")
    let stripped = m.rsplit('/').next().unwrap_or(&m);
    if stripped.starts_with("gpt-") || stripped.starts_with("o1") || stripped.starts_with("o3") || stripped.starts_with("o4") || stripped.contains("davinci") || stripped.contains("turbo") {
        "openai"
    } else if stripped.starts_with("claude") {
        "anthropic"
    } else if stripped.starts_with("gemini") {
        "google"
    } else if stripped.starts_with("mistral") || stripped.starts_with("mixtral") {
        "mistral"
    } else if stripped.starts_with("llama") || stripped.starts_with("meta-llama") {
        "meta"
    } else if m.starts_with("openai/") {
        "openai"
    } else if m.starts_with("anthropic/") {
        "anthropic"
    } else if m.starts_with("google/") || m.starts_with("models/gemini") {
        "google"
    } else {
        "unknown"
    }
}

/// Build OTel attributes for an LlmCall step.
/// `request_blob` and `response_blob` are the parsed JSON from the blob store.
pub fn llm_call_attributes(
    step: &Step,
    request_blob: Option<&Value>,
    response_blob: Option<&Value>,
    include_content: bool,
) -> Vec<KeyValue> {
    let mut attrs = Vec::with_capacity(16);

    // Required
    attrs.push(KeyValue::new("gen_ai.operation.name", "chat"));
    attrs.push(KeyValue::new("gen_ai.system", infer_provider(&step.model)));
    if !step.model.is_empty() {
        attrs.push(KeyValue::new("gen_ai.request.model", step.model.clone()));
    }

    // Token usage
    if step.tokens_in > 0 {
        attrs.push(KeyValue::new("gen_ai.usage.input_tokens", step.tokens_in as i64));
    }
    if step.tokens_out > 0 {
        attrs.push(KeyValue::new("gen_ai.usage.output_tokens", step.tokens_out as i64));
    }

    // Duration
    attrs.push(KeyValue::new("rewind.duration_ms", step.duration_ms as i64));

    // Error
    if let Some(ref err) = step.error {
        attrs.push(KeyValue::new("error.type", err.clone()));
    }

    // Extract from request blob
    if let Some(req) = request_blob {
        if let Some(temp) = req.get("temperature").and_then(|v| v.as_f64()) {
            attrs.push(KeyValue::new("gen_ai.request.temperature", temp));
        }
        if let Some(max_tok) = req.get("max_tokens").and_then(|v| v.as_i64()) {
            attrs.push(KeyValue::new("gen_ai.request.max_tokens", max_tok));
        }
        // Content (opt-in)
        if include_content {
            if let Some(messages) = req.get("messages") {
                attrs.push(KeyValue::new("gen_ai.input.messages", messages.to_string()));
            }
        }
    }

    // Extract from response blob
    if let Some(resp) = response_blob {
        if let Some(model) = resp.get("model").and_then(|v| v.as_str()) {
            attrs.push(KeyValue::new("gen_ai.response.model", model.to_string()));
        }
        if let Some(id) = resp.get("id").and_then(|v| v.as_str()) {
            attrs.push(KeyValue::new("gen_ai.response.id", id.to_string()));
        }
        // finish_reasons — extract from choices array
        if let Some(choices) = resp.get("choices").and_then(|v| v.as_array()) {
            let reasons: Vec<String> = choices
                .iter()
                .filter_map(|c| c.get("finish_reason").and_then(|v| v.as_str()))
                .map(|s| s.to_string())
                .collect();
            if !reasons.is_empty() {
                attrs.push(KeyValue::new("gen_ai.response.finish_reasons", format!("[{}]", reasons.join(","))));
            }
        }
        // Anthropic format: stop_reason at top level
        if let Some(stop) = resp.get("stop_reason").and_then(|v| v.as_str()) {
            attrs.push(KeyValue::new("gen_ai.response.finish_reasons", format!("[{}]", stop)));
        }
        // Content (opt-in)
        if include_content {
            if let Some(choices) = resp.get("choices") {
                attrs.push(KeyValue::new("gen_ai.output.messages", choices.to_string()));
            }
            // Anthropic format
            if let Some(content) = resp.get("content") {
                attrs.push(KeyValue::new("gen_ai.output.messages", content.to_string()));
            }
        }
    }

    attrs
}

/// Build OTel attributes for a ToolCall step.
pub fn tool_call_attributes(step: &Step, request_blob: Option<&Value>) -> Vec<KeyValue> {
    let mut attrs = Vec::with_capacity(8);

    if let Some(name) = &step.tool_name {
        attrs.push(KeyValue::new("gen_ai.tool.name", name.clone()));
    }
    attrs.push(KeyValue::new("gen_ai.tool.type", "function"));
    attrs.push(KeyValue::new("rewind.duration_ms", step.duration_ms as i64));

    if let Some(ref err) = step.error {
        attrs.push(KeyValue::new("error.type", err.clone()));
    }

    // Extract tool call ID and arguments from request blob
    if let Some(req) = request_blob {
        if let Some(id) = req.get("tool_call_id").and_then(|v| v.as_str()) {
            attrs.push(KeyValue::new("gen_ai.tool.call.id", id.to_string()));
        }
    }

    attrs
}

/// Build OTel attributes for a ToolResult step.
pub fn tool_result_attributes(step: &Step) -> Vec<KeyValue> {
    let mut attrs = Vec::with_capacity(6);

    if let Some(name) = &step.tool_name {
        attrs.push(KeyValue::new("gen_ai.tool.name", name.clone()));
    }
    attrs.push(KeyValue::new("gen_ai.tool.type", "function"));
    attrs.push(KeyValue::new("rewind.duration_ms", step.duration_ms as i64));

    if let Some(ref err) = step.error {
        attrs.push(KeyValue::new("error.type", err.clone()));
    }

    attrs
}

/// Build OTel attributes for UserPrompt / HookEvent steps.
pub fn misc_step_attributes(step: &Step) -> Vec<KeyValue> {
    let mut attrs = Vec::with_capacity(4);

    attrs.push(KeyValue::new("rewind.step.type", step.step_type.as_str().to_string()));
    attrs.push(KeyValue::new("rewind.duration_ms", step.duration_ms as i64));

    if let Some(ref err) = step.error {
        attrs.push(KeyValue::new("error.type", err.clone()));
    }

    attrs
}

/// Build the complete attribute set for any step type.
pub fn step_attributes(
    step: &Step,
    request_blob: Option<&Value>,
    response_blob: Option<&Value>,
    include_content: bool,
) -> Vec<KeyValue> {
    match step.step_type {
        StepType::LlmCall => llm_call_attributes(step, request_blob, response_blob, include_content),
        StepType::ToolCall => tool_call_attributes(step, request_blob),
        StepType::ToolResult => tool_result_attributes(step),
        StepType::UserPrompt | StepType::HookEvent => misc_step_attributes(step),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rewind_store::models::{StepStatus, StepType};
    use chrono::Utc;

    fn make_step(step_type: StepType, model: &str, tool_name: Option<&str>) -> Step {
        Step {
            id: "step-1".to_string(),
            timeline_id: "tl-1".to_string(),
            session_id: "sess-1".to_string(),
            step_number: 1,
            step_type,
            status: StepStatus::Success,
            created_at: Utc::now(),
            duration_ms: 1500,
            tokens_in: 100,
            tokens_out: 50,
            model: model.to_string(),
            request_blob: "abc123".to_string(),
            response_blob: "def456".to_string(),
            error: None,
            span_id: None,
            tool_name: tool_name.map(|s| s.to_string()),
        }
    }

    fn find_attr<'a>(attrs: &'a [KeyValue], key: &str) -> Option<&'a KeyValue> {
        attrs.iter().find(|kv| kv.key.as_str() == key)
    }

    // ── span_name tests ──

    #[test]
    fn test_span_name_llm_call() {
        let step = make_step(StepType::LlmCall, "gpt-4o", None);
        assert_eq!(span_name(&step), "gen_ai.chat gpt-4o");
    }

    #[test]
    fn test_span_name_llm_call_empty_model() {
        let step = make_step(StepType::LlmCall, "", None);
        assert_eq!(span_name(&step), "gen_ai.chat unknown");
    }

    #[test]
    fn test_span_name_tool_call() {
        let step = make_step(StepType::ToolCall, "", Some("search_web"));
        assert_eq!(span_name(&step), "tool.execute search_web");
    }

    #[test]
    fn test_span_name_tool_result() {
        let step = make_step(StepType::ToolResult, "", Some("search_web"));
        assert_eq!(span_name(&step), "tool.result search_web");
    }

    #[test]
    fn test_span_name_user_prompt() {
        let step = make_step(StepType::UserPrompt, "", None);
        assert_eq!(span_name(&step), "user.prompt");
    }

    #[test]
    fn test_span_name_hook_event() {
        let step = make_step(StepType::HookEvent, "", None);
        assert_eq!(span_name(&step), "hook.event");
    }

    // ── span_kind tests ──

    #[test]
    fn test_span_kind_llm_is_client() {
        let step = make_step(StepType::LlmCall, "gpt-4", None);
        assert_eq!(span_kind(&step), OtelSpanKind::Client);
    }

    #[test]
    fn test_span_kind_tool_is_internal() {
        let step = make_step(StepType::ToolCall, "", Some("tool"));
        assert_eq!(span_kind(&step), OtelSpanKind::Internal);
    }

    // ── infer_provider tests ──

    #[test]
    fn test_infer_provider_openai() {
        assert_eq!(infer_provider("gpt-4o"), "openai");
        assert_eq!(infer_provider("gpt-4-turbo"), "openai");
        assert_eq!(infer_provider("o1-preview"), "openai");
        assert_eq!(infer_provider("o3-mini"), "openai");
    }

    #[test]
    fn test_infer_provider_anthropic() {
        assert_eq!(infer_provider("claude-sonnet-4-5-20250514"), "anthropic");
        assert_eq!(infer_provider("claude-3-haiku-20240307"), "anthropic");
    }

    #[test]
    fn test_infer_provider_google() {
        assert_eq!(infer_provider("gemini-pro"), "google");
    }

    #[test]
    fn test_infer_provider_with_prefix() {
        assert_eq!(infer_provider("openai/gpt-4o-mini"), "openai");
        assert_eq!(infer_provider("anthropic/claude-3-haiku"), "anthropic");
        assert_eq!(infer_provider("google/gemini-pro"), "google");
    }

    #[test]
    fn test_infer_provider_unknown() {
        assert_eq!(infer_provider("my-custom-model"), "unknown");
    }

    // ── llm_call_attributes tests ──

    #[test]
    fn test_llm_call_basic_attributes() {
        let step = make_step(StepType::LlmCall, "gpt-4o", None);
        let attrs = llm_call_attributes(&step, None, None, false);

        assert!(find_attr(&attrs, "gen_ai.operation.name").is_some());
        assert!(find_attr(&attrs, "gen_ai.system").is_some());
        assert!(find_attr(&attrs, "gen_ai.request.model").is_some());
        assert!(find_attr(&attrs, "gen_ai.usage.input_tokens").is_some());
        assert!(find_attr(&attrs, "gen_ai.usage.output_tokens").is_some());
    }

    #[test]
    fn test_llm_call_with_request_blob() {
        let step = make_step(StepType::LlmCall, "gpt-4o", None);
        let req = serde_json::json!({
            "temperature": 0.7,
            "max_tokens": 4096,
            "messages": [{"role": "user", "content": "hello"}]
        });
        let attrs = llm_call_attributes(&step, Some(&req), None, false);

        let temp = find_attr(&attrs, "gen_ai.request.temperature").unwrap();
        assert_eq!(temp.value.to_string(), "0.7");

        let max_tok = find_attr(&attrs, "gen_ai.request.max_tokens").unwrap();
        assert_eq!(max_tok.value.to_string(), "4096");

        // Content NOT included when include_content=false
        assert!(find_attr(&attrs, "gen_ai.input.messages").is_none());
    }

    #[test]
    fn test_llm_call_with_content_opt_in() {
        let step = make_step(StepType::LlmCall, "gpt-4o", None);
        let req = serde_json::json!({
            "messages": [{"role": "user", "content": "hello"}]
        });
        let attrs = llm_call_attributes(&step, Some(&req), None, true);

        assert!(find_attr(&attrs, "gen_ai.input.messages").is_some());
    }

    #[test]
    fn test_llm_call_with_response_blob_openai() {
        let step = make_step(StepType::LlmCall, "gpt-4o", None);
        let resp = serde_json::json!({
            "id": "chatcmpl-abc",
            "model": "gpt-4o-2024-08-06",
            "choices": [{"finish_reason": "stop", "message": {"content": "hi"}}]
        });
        let attrs = llm_call_attributes(&step, None, Some(&resp), false);

        // OTel Value::to_string() wraps strings in quotes
        assert!(find_attr(&attrs, "gen_ai.response.model")
            .unwrap()
            .value
            .to_string()
            .contains("gpt-4o-2024-08-06"));
        assert!(find_attr(&attrs, "gen_ai.response.id")
            .unwrap()
            .value
            .to_string()
            .contains("chatcmpl-abc"));
        assert!(find_attr(&attrs, "gen_ai.response.finish_reasons").is_some());
    }

    #[test]
    fn test_llm_call_with_response_blob_anthropic() {
        let step = make_step(StepType::LlmCall, "claude-sonnet-4-5-20250514", None);
        let resp = serde_json::json!({
            "id": "msg_abc",
            "model": "claude-sonnet-4-5-20250514",
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "hello"}]
        });
        let attrs = llm_call_attributes(&step, None, Some(&resp), false);

        assert!(find_attr(&attrs, "gen_ai.response.finish_reasons").is_some());
    }

    #[test]
    fn test_llm_call_with_error() {
        let mut step = make_step(StepType::LlmCall, "gpt-4o", None);
        step.error = Some("rate_limit_exceeded".to_string());
        let attrs = llm_call_attributes(&step, None, None, false);

        assert!(find_attr(&attrs, "error.type")
            .unwrap()
            .value
            .to_string()
            .contains("rate_limit_exceeded"));
    }

    // ── tool_call_attributes tests ──

    #[test]
    fn test_tool_call_attributes() {
        let step = make_step(StepType::ToolCall, "", Some("search_web"));
        let attrs = tool_call_attributes(&step, None);

        assert!(find_attr(&attrs, "gen_ai.tool.name")
            .unwrap()
            .value
            .to_string()
            .contains("search_web"));
        assert!(find_attr(&attrs, "gen_ai.tool.type").is_some());
    }

    // ── step_attributes dispatch tests ──

    #[test]
    fn test_step_attributes_dispatches_correctly() {
        let llm = make_step(StepType::LlmCall, "gpt-4o", None);
        let tool = make_step(StepType::ToolCall, "", Some("read_file"));
        let prompt = make_step(StepType::UserPrompt, "", None);

        let llm_attrs = step_attributes(&llm, None, None, false);
        assert!(find_attr(&llm_attrs, "gen_ai.operation.name").is_some());

        let tool_attrs = step_attributes(&tool, None, None, false);
        assert!(find_attr(&tool_attrs, "gen_ai.tool.name").is_some());

        let prompt_attrs = step_attributes(&prompt, None, None, false);
        assert!(find_attr(&prompt_attrs, "rewind.step.type").is_some());
    }
}
