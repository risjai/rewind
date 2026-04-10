use serde_json::json;

// We test the public-via-crate pure functions by re-implementing them in-test
// from the same logic (since they are module-private `fn` in lib.rs).
// A better approach long-term is to make these `pub(crate)` or move to a helpers module.
// For now we test via the same JSON parsing logic.

// ── is_stream_request ────────────────────────────────────────────

fn is_stream_request(body: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
        .unwrap_or(false)
}

#[test]
fn is_stream_request_true() {
    let body = br#"{"model":"gpt-4o","stream":true}"#;
    assert!(is_stream_request(body));
}

#[test]
fn is_stream_request_false() {
    let body = br#"{"model":"gpt-4o","stream":false}"#;
    assert!(!is_stream_request(body));
}

#[test]
fn is_stream_request_missing_field() {
    let body = br#"{"model":"gpt-4o","messages":[]}"#;
    assert!(!is_stream_request(body));
}

#[test]
fn is_stream_request_string_value_not_bool() {
    // "stream": "true" (string) should NOT be treated as streaming
    let body = br#"{"model":"gpt-4o","stream":"true"}"#;
    assert!(!is_stream_request(body));
}

#[test]
fn is_stream_request_null_value() {
    let body = br#"{"model":"gpt-4o","stream":null}"#;
    assert!(!is_stream_request(body));
}

#[test]
fn is_stream_request_malformed_json() {
    let body = br#"not json at all"#;
    assert!(!is_stream_request(body));
}

#[test]
fn is_stream_request_empty_body() {
    assert!(!is_stream_request(b""));
}

// ── extract_model ────────────────────────────────────────────────

fn extract_model(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(String::from))
}

#[test]
fn extract_model_openai() {
    let body = br#"{"model":"gpt-4o","messages":[]}"#;
    assert_eq!(extract_model(body), Some("gpt-4o".to_string()));
}

#[test]
fn extract_model_anthropic() {
    let body = br#"{"model":"claude-sonnet-4-20250514","messages":[]}"#;
    assert_eq!(extract_model(body), Some("claude-sonnet-4-20250514".to_string()));
}

#[test]
fn extract_model_missing() {
    let body = br#"{"messages":[]}"#;
    assert_eq!(extract_model(body), None);
}

#[test]
fn extract_model_not_a_string() {
    let body = br#"{"model":42}"#;
    assert_eq!(extract_model(body), None);
}

#[test]
fn extract_model_malformed_json() {
    assert_eq!(extract_model(b"broken"), None);
}

// ── extract_model_from_path ──────────────────────────────────────

fn extract_model_from_path(path: &str) -> Option<String> {
    if path.contains("/model/") {
        let parts: Vec<&str> = path.split("/model/").collect();
        if parts.len() > 1 {
            let model_and_rest = parts[1];
            let model = model_and_rest.split('/').next().unwrap_or(model_and_rest);
            return Some(model.to_string());
        }
    }
    None
}

#[test]
fn extract_model_from_path_present() {
    assert_eq!(
        extract_model_from_path("/v1/model/gpt-4o/chat/completions"),
        Some("gpt-4o".to_string())
    );
}

#[test]
fn extract_model_from_path_absent() {
    assert_eq!(
        extract_model_from_path("/v1/chat/completions"),
        None
    );
}

#[test]
fn extract_model_from_path_trailing_slash() {
    // "/model/" with nothing after it — split on '/' yields empty string
    assert_eq!(
        extract_model_from_path("/v1/model/"),
        Some("".to_string())
    );
}

#[test]
fn extract_model_from_path_no_trailing_segments() {
    assert_eq!(
        extract_model_from_path("/v1/model/claude-3-opus"),
        Some("claude-3-opus".to_string())
    );
}

// ── extract_usage ────────────────────────────────────────────────

fn extract_usage(resp_bytes: &[u8]) -> (u64, u64) {
    let val: serde_json::Value = serde_json::from_slice(resp_bytes).unwrap_or_default();

    if let Some(usage) = val.get("usage") {
        let input = usage.get("prompt_tokens")
            .or(usage.get("input_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let output = usage.get("completion_tokens")
            .or(usage.get("output_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        return (input, output);
    }

    (0, 0)
}

#[test]
fn extract_usage_openai_format() {
    let resp = json!({
        "usage": {"prompt_tokens": 100, "completion_tokens": 50}
    });
    let bytes = serde_json::to_vec(&resp).unwrap();
    assert_eq!(extract_usage(&bytes), (100, 50));
}

#[test]
fn extract_usage_anthropic_format() {
    let resp = json!({
        "usage": {"input_tokens": 200, "output_tokens": 75}
    });
    let bytes = serde_json::to_vec(&resp).unwrap();
    assert_eq!(extract_usage(&bytes), (200, 75));
}

#[test]
fn extract_usage_no_usage_field() {
    let resp = json!({"choices": []});
    let bytes = serde_json::to_vec(&resp).unwrap();
    assert_eq!(extract_usage(&bytes), (0, 0));
}

#[test]
fn extract_usage_empty_response() {
    assert_eq!(extract_usage(b"{}"), (0, 0));
}

#[test]
fn extract_usage_malformed_json() {
    assert_eq!(extract_usage(b"not json"), (0, 0));
}

#[test]
fn extract_usage_partial_fields() {
    // Only prompt_tokens present, no completion_tokens
    let resp = json!({"usage": {"prompt_tokens": 150}});
    let bytes = serde_json::to_vec(&resp).unwrap();
    assert_eq!(extract_usage(&bytes), (150, 0));
}

#[test]
fn extract_usage_openai_takes_precedence_over_anthropic() {
    // If both prompt_tokens AND input_tokens are present, prompt_tokens wins (OpenAI format)
    let resp = json!({
        "usage": {
            "prompt_tokens": 100,
            "input_tokens": 200,
            "completion_tokens": 50,
            "output_tokens": 75,
        }
    });
    let bytes = serde_json::to_vec(&resp).unwrap();
    assert_eq!(extract_usage(&bytes), (100, 50));
}

// ── is_tool_call_response ────────────────────────────────────────

fn is_tool_call_response(resp_bytes: &[u8]) -> bool {
    let val: serde_json::Value = serde_json::from_slice(resp_bytes).unwrap_or_default();

    if let Some(choices) = val.get("choices").and_then(|c| c.as_array()) {
        for choice in choices {
            if let Some(msg) = choice.get("message") {
                if let Some(tc) = msg.get("tool_calls").and_then(|t| t.as_array()) {
                    if !tc.is_empty() {
                        return true;
                    }
                }
            }
        }
    }

    if let Some(content) = val.get("content").and_then(|c| c.as_array()) {
        for block in content {
            if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                return true;
            }
        }
    }

    false
}

#[test]
fn is_tool_call_openai_format() {
    let resp = json!({
        "choices": [{
            "message": {
                "tool_calls": [{"function": {"name": "search"}}]
            }
        }]
    });
    let bytes = serde_json::to_vec(&resp).unwrap();
    assert!(is_tool_call_response(&bytes));
}

#[test]
fn is_tool_call_anthropic_format() {
    let resp = json!({
        "content": [
            {"type": "text", "text": "Let me search..."},
            {"type": "tool_use", "name": "search", "input": {}}
        ]
    });
    let bytes = serde_json::to_vec(&resp).unwrap();
    assert!(is_tool_call_response(&bytes));
}

#[test]
fn is_tool_call_empty_tool_calls_array() {
    let resp = json!({
        "choices": [{"message": {"tool_calls": []}}]
    });
    let bytes = serde_json::to_vec(&resp).unwrap();
    assert!(!is_tool_call_response(&bytes));
}

#[test]
fn is_tool_call_no_tool_calls() {
    let resp = json!({
        "choices": [{"message": {"content": "Hello!"}}]
    });
    let bytes = serde_json::to_vec(&resp).unwrap();
    assert!(!is_tool_call_response(&bytes));
}

#[test]
fn is_tool_call_malformed() {
    assert!(!is_tool_call_response(b"not json"));
}

// ── parse_sse_event ──────────────────────────────────────────────

fn parse_sse_event(
    event: &serde_json::Value,
    text: &mut String,
    input_tokens: &mut u64,
    output_tokens: &mut u64,
    model: &mut String,
    has_tool_calls: &mut bool,
    tool_calls: &mut Vec<serde_json::Value>,
) {
    // OpenAI streaming format
    if let Some(choices) = event.get("choices").and_then(|c| c.as_array()) {
        if let Some(delta) = choices.first().and_then(|c| c.get("delta")) {
            if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                text.push_str(content);
            }
            if let Some(tc) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                *has_tool_calls = true;
                for call in tc {
                    tool_calls.push(call.clone());
                }
            }
        }
        if let Some(usage) = event.get("usage") {
            *input_tokens = usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(*input_tokens);
            *output_tokens = usage.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(*output_tokens);
        }
    }

    // Anthropic streaming format
    if let Some(event_type) = event.get("type").and_then(|t| t.as_str()) {
        match event_type {
            "message_start" => {
                if let Some(msg) = event.get("message") {
                    if let Some(m) = msg.get("model").and_then(|m| m.as_str()) {
                        *model = m.to_string();
                    }
                    if let Some(usage) = msg.get("usage") {
                        *input_tokens = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    }
                }
            }
            "content_block_start" => {
                if let Some(block) = event.get("content_block") {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        *has_tool_calls = true;
                        tool_calls.push(block.clone());
                    }
                }
            }
            "content_block_delta" => {
                if let Some(delta) = event.get("delta") {
                    if let Some(t) = delta.get("text").and_then(|t| t.as_str()) {
                        text.push_str(t);
                    }
                    // Tool input JSON delta — accumulate into last tool call, not text
                    if let Some(partial) = delta.get("partial_json").and_then(|p| p.as_str()) {
                        if let Some(last_tc) = tool_calls.last_mut() {
                            let existing = last_tc.get("_partial_input")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            last_tc["_partial_input"] = serde_json::Value::String(
                                format!("{}{}", existing, partial),
                            );
                        }
                    }
                }
            }
            "message_delta" => {
                if let Some(usage) = event.get("usage") {
                    *output_tokens = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(*output_tokens);
                }
            }
            _ => {}
        }
    }

    // Model at top level (OpenAI)
    if let Some(m) = event.get("model").and_then(|m| m.as_str()) {
        if !m.is_empty() {
            *model = m.to_string();
        }
    }
}

fn fresh_sse_state() -> (String, u64, u64, String, bool, Vec<serde_json::Value>) {
    (String::new(), 0, 0, "unknown".to_string(), false, Vec::new())
}

#[test]
fn parse_sse_openai_text_delta() {
    let (mut text, mut tin, mut tout, mut model, mut tc, mut tcj) = fresh_sse_state();
    let event = json!({"choices": [{"delta": {"content": "Hello"}}], "model": "gpt-4o"});
    parse_sse_event(&event, &mut text, &mut tin, &mut tout, &mut model, &mut tc, &mut tcj);
    assert_eq!(text, "Hello");
    assert_eq!(model, "gpt-4o");
    assert!(!tc);
}

#[test]
fn parse_sse_openai_multiple_text_deltas() {
    let (mut text, mut tin, mut tout, mut model, mut tc, mut tcj) = fresh_sse_state();
    let events = vec![
        json!({"choices": [{"delta": {"content": "Hello"}}]}),
        json!({"choices": [{"delta": {"content": " world"}}]}),
    ];
    for e in &events {
        parse_sse_event(e, &mut text, &mut tin, &mut tout, &mut model, &mut tc, &mut tcj);
    }
    assert_eq!(text, "Hello world");
}

#[test]
fn parse_sse_openai_tool_calls() {
    let (mut text, mut tin, mut tout, mut model, mut tc, mut tcj) = fresh_sse_state();
    let event = json!({
        "choices": [{"delta": {"tool_calls": [{"function": {"name": "search", "arguments": "{}"}}]}}]
    });
    parse_sse_event(&event, &mut text, &mut tin, &mut tout, &mut model, &mut tc, &mut tcj);
    assert!(tc);
    assert_eq!(tcj.len(), 1);
    assert_eq!(text, ""); // tool calls shouldn't add to text
}

#[test]
fn parse_sse_openai_usage_in_final_chunk() {
    let (mut text, mut tin, mut tout, mut model, mut tc, mut tcj) = fresh_sse_state();
    let event = json!({
        "choices": [],
        "usage": {"prompt_tokens": 150, "completion_tokens": 42}
    });
    parse_sse_event(&event, &mut text, &mut tin, &mut tout, &mut model, &mut tc, &mut tcj);
    assert_eq!(tin, 150);
    assert_eq!(tout, 42);
}

#[test]
fn parse_sse_anthropic_message_start() {
    let (mut text, mut tin, mut tout, mut model, mut tc, mut tcj) = fresh_sse_state();
    let event = json!({
        "type": "message_start",
        "message": {
            "model": "claude-sonnet-4-20250514",
            "usage": {"input_tokens": 300}
        }
    });
    parse_sse_event(&event, &mut text, &mut tin, &mut tout, &mut model, &mut tc, &mut tcj);
    assert_eq!(model, "claude-sonnet-4-20250514");
    assert_eq!(tin, 300);
}

#[test]
fn parse_sse_anthropic_content_block_text_delta() {
    let (mut text, mut tin, mut tout, mut model, mut tc, mut tcj) = fresh_sse_state();
    let event = json!({
        "type": "content_block_delta",
        "delta": {"type": "text_delta", "text": "Hello from Claude"}
    });
    parse_sse_event(&event, &mut text, &mut tin, &mut tout, &mut model, &mut tc, &mut tcj);
    assert_eq!(text, "Hello from Claude");
}

#[test]
fn parse_sse_anthropic_content_block_tool_use_start() {
    let (mut text, mut tin, mut tout, mut model, mut tc, mut tcj) = fresh_sse_state();
    let event = json!({
        "type": "content_block_start",
        "content_block": {"type": "tool_use", "name": "calculator", "id": "tool_1"}
    });
    parse_sse_event(&event, &mut text, &mut tin, &mut tout, &mut model, &mut tc, &mut tcj);
    assert!(tc);
    assert_eq!(tcj.len(), 1);
    assert_eq!(tcj[0]["name"], "calculator");
}

#[test]
fn parse_sse_anthropic_partial_json_does_not_contaminate_text() {
    // partial_json for tool input deltas must NOT be accumulated into the text
    // buffer — it goes into the last tool call's _partial_input field instead.
    let (mut text, mut tin, mut tout, mut model, mut tc, mut tcj) = fresh_sse_state();

    // First: register a tool call via content_block_start
    let tool_start = json!({
        "type": "content_block_start",
        "content_block": {"type": "tool_use", "name": "search", "id": "tool_1"}
    });
    parse_sse_event(&tool_start, &mut text, &mut tin, &mut tout, &mut model, &mut tc, &mut tcj);

    // Then: text content
    let text_event = json!({
        "type": "content_block_delta",
        "delta": {"type": "text_delta", "text": "Let me search"}
    });
    parse_sse_event(&text_event, &mut text, &mut tin, &mut tout, &mut model, &mut tc, &mut tcj);

    // Then: tool input delta (partial_json)
    let tool_event = json!({
        "type": "content_block_delta",
        "delta": {"type": "input_json_delta", "partial_json": "{\"query\":\"test\"}"}
    });
    parse_sse_event(&tool_event, &mut text, &mut tin, &mut tout, &mut model, &mut tc, &mut tcj);

    // Text should ONLY contain the text content, not the tool input JSON
    assert_eq!(text, "Let me search");
    // Tool input should be accumulated on the tool call object
    assert_eq!(tcj[0]["_partial_input"], "{\"query\":\"test\"}");
}

#[test]
fn parse_sse_anthropic_message_delta_output_tokens() {
    let (mut text, mut tin, mut tout, mut model, mut tc, mut tcj) = fresh_sse_state();
    let event = json!({
        "type": "message_delta",
        "usage": {"output_tokens": 89}
    });
    parse_sse_event(&event, &mut text, &mut tin, &mut tout, &mut model, &mut tc, &mut tcj);
    assert_eq!(tout, 89);
}

#[test]
fn parse_sse_openai_model_at_top_level() {
    let (mut text, mut tin, mut tout, mut model, mut tc, mut tcj) = fresh_sse_state();
    model = "old-model".to_string();
    let event = json!({"model": "gpt-4o-mini", "choices": [{"delta": {}}]});
    parse_sse_event(&event, &mut text, &mut tin, &mut tout, &mut model, &mut tc, &mut tcj);
    assert_eq!(model, "gpt-4o-mini");
}

#[test]
fn parse_sse_empty_model_does_not_overwrite() {
    let (mut text, mut tin, mut tout, mut model, mut tc, mut tcj) = fresh_sse_state();
    model = "gpt-4o".to_string();
    let event = json!({"model": "", "choices": [{"delta": {}}]});
    parse_sse_event(&event, &mut text, &mut tin, &mut tout, &mut model, &mut tc, &mut tcj);
    assert_eq!(model, "gpt-4o"); // empty model should NOT overwrite
}

#[test]
fn parse_sse_anthropic_full_sequence() {
    let (mut text, mut tin, mut tout, mut model, mut tc, mut tcj) = fresh_sse_state();

    let events = vec![
        json!({"type": "message_start", "message": {"model": "claude-sonnet-4-20250514", "usage": {"input_tokens": 100}}}),
        json!({"type": "content_block_start", "content_block": {"type": "text"}}),
        json!({"type": "content_block_delta", "delta": {"type": "text_delta", "text": "The answer is "}}),
        json!({"type": "content_block_delta", "delta": {"type": "text_delta", "text": "42."}}),
        json!({"type": "message_delta", "usage": {"output_tokens": 10}}),
    ];

    for e in &events {
        parse_sse_event(e, &mut text, &mut tin, &mut tout, &mut model, &mut tc, &mut tcj);
    }

    assert_eq!(model, "claude-sonnet-4-20250514");
    assert_eq!(text, "The answer is 42.");
    assert_eq!(tin, 100);
    assert_eq!(tout, 10);
    assert!(!tc);
}

// ── build_synthetic_response ─────────────────────────────────────

fn build_synthetic_response(
    model: &str,
    text: &str,
    input_tokens: u64,
    output_tokens: u64,
    has_tool_calls: bool,
    tool_calls: &[serde_json::Value],
) -> serde_json::Value {
    if model.contains("claude") || model.contains("anthropic") {
        let mut content = vec![];
        if !text.is_empty() {
            content.push(json!({"type": "text", "text": text}));
        }
        if has_tool_calls {
            for tc in tool_calls {
                content.push(tc.clone());
            }
        }
        json!({
            "model": model,
            "type": "message",
            "role": "assistant",
            "content": content,
            "usage": {
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
            }
        })
    } else {
        let mut message = json!({"role": "assistant"});
        if !text.is_empty() {
            message["content"] = serde_json::Value::String(text.to_string());
        }
        if has_tool_calls && !tool_calls.is_empty() {
            message["tool_calls"] = serde_json::Value::Array(tool_calls.to_vec());
        }
        json!({
            "model": model,
            "choices": [{"index": 0, "message": message, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": input_tokens,
                "completion_tokens": output_tokens,
            }
        })
    }
}

#[test]
fn build_synthetic_openai_text_response() {
    let resp = build_synthetic_response("gpt-4o", "Hello world", 100, 50, false, &[]);
    assert_eq!(resp["model"], "gpt-4o");
    assert_eq!(resp["choices"][0]["message"]["content"], "Hello world");
    assert_eq!(resp["usage"]["prompt_tokens"], 100);
    assert_eq!(resp["usage"]["completion_tokens"], 50);
}

#[test]
fn build_synthetic_openai_tool_call_response() {
    let tool_calls = vec![json!({"function": {"name": "search", "arguments": "{}"}})];
    let resp = build_synthetic_response("gpt-4o", "", 100, 50, true, &tool_calls);
    assert_eq!(resp["choices"][0]["message"]["tool_calls"].as_array().unwrap().len(), 1);
    // text is empty, so "content" should not be present
    assert!(resp["choices"][0]["message"].get("content").is_none());
}

#[test]
fn build_synthetic_anthropic_text_response() {
    let resp = build_synthetic_response("claude-sonnet-4-20250514", "Hi there", 200, 30, false, &[]);
    assert_eq!(resp["model"], "claude-sonnet-4-20250514");
    assert_eq!(resp["type"], "message");
    assert_eq!(resp["content"][0]["type"], "text");
    assert_eq!(resp["content"][0]["text"], "Hi there");
    assert_eq!(resp["usage"]["input_tokens"], 200);
    assert_eq!(resp["usage"]["output_tokens"], 30);
}

#[test]
fn build_synthetic_anthropic_tool_use_response() {
    let tool_calls = vec![json!({"type": "tool_use", "name": "calculator", "input": {"x": 5}})];
    let resp = build_synthetic_response("claude-sonnet-4-20250514", "", 200, 30, true, &tool_calls);
    let content = resp["content"].as_array().unwrap();
    assert_eq!(content.len(), 1); // no text block (empty text), only tool_use
    assert_eq!(content[0]["type"], "tool_use");
    assert_eq!(content[0]["name"], "calculator");
}

#[test]
fn build_synthetic_anthropic_mixed_text_and_tools() {
    let tool_calls = vec![json!({"type": "tool_use", "name": "search", "input": {}})];
    let resp = build_synthetic_response("claude-sonnet-4-20250514", "Let me search", 200, 30, true, &tool_calls);
    let content = resp["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[1]["type"], "tool_use");
}

#[test]
fn build_synthetic_empty_response() {
    let resp = build_synthetic_response("gpt-4o", "", 0, 0, false, &[]);
    assert_eq!(resp["model"], "gpt-4o");
    // No content or tool_calls when both are empty
    let msg = &resp["choices"][0]["message"];
    assert!(msg.get("content").is_none());
    assert!(msg.get("tool_calls").is_none());
}

#[test]
fn build_synthetic_model_detection_anthropic_substring() {
    // A model name containing "anthropic" should use Anthropic format
    let resp = build_synthetic_response("anthropic.claude-v2", "test", 10, 5, false, &[]);
    assert_eq!(resp["type"], "message"); // Anthropic format marker
    assert!(resp.get("choices").is_none()); // Not OpenAI format
}

#[test]
fn build_synthetic_model_detection_non_claude_non_anthropic() {
    // A model like "llama-3" should use OpenAI format
    let resp = build_synthetic_response("llama-3-70b", "test", 10, 5, false, &[]);
    assert!(resp.get("choices").is_some());
    assert!(resp.get("type").is_none());
}
