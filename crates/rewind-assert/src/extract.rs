use rewind_store::{Step, Store, StepType};
use serde::Serialize;

/// Fingerprint of a response for shallow comparison.
#[derive(Debug, Clone, Serialize)]
pub struct ResponseFingerprint {
    pub content_length: usize,
    pub has_tool_calls: bool,
    pub tool_call_names: Vec<String>,
    pub text_preview: String,
}

/// Extract tool name from a step's response blob.
/// Handles both OpenAI and Anthropic JSON formats.
pub fn extract_tool_name(store: &Store, step: &Step) -> Option<String> {
    if step.step_type != StepType::LlmCall {
        return None;
    }

    let response: serde_json::Value = store.blobs.get_json(&step.response_blob).ok()?;

    // OpenAI format: choices[0].message.tool_calls[0].function.name
    if let Some(choices) = response.get("choices").and_then(|c| c.as_array())
        && let Some(first) = choices.first()
            && let Some(tool_calls) = first
                .get("message")
                .and_then(|m| m.get("tool_calls"))
                .and_then(|tc| tc.as_array())
                && let Some(first_tc) = tool_calls.first()
                    && let Some(name) = first_tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                    {
                        return Some(name.to_string());
                    }

    // Anthropic format: content[*].type == "tool_use" → .name
    if let Some(content) = response.get("content").and_then(|c| c.as_array()) {
        for block in content {
            if block.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                && let Some(name) = block.get("name").and_then(|n| n.as_str()) {
                    return Some(name.to_string());
                }
        }
    }

    None
}

/// Extract all tool call names from a response blob.
fn extract_all_tool_names(response: &serde_json::Value) -> Vec<String> {
    let mut names = Vec::new();

    // OpenAI
    if let Some(choices) = response.get("choices").and_then(|c| c.as_array())
        && let Some(first) = choices.first()
            && let Some(tool_calls) = first
                .get("message")
                .and_then(|m| m.get("tool_calls"))
                .and_then(|tc| tc.as_array())
            {
                for tc in tool_calls {
                    if let Some(name) = tc.get("function").and_then(|f| f.get("name")).and_then(|n| n.as_str()) {
                        names.push(name.to_string());
                    }
                }
            }

    // Anthropic
    if let Some(content) = response.get("content").and_then(|c| c.as_array()) {
        for block in content {
            if block.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                && let Some(name) = block.get("name").and_then(|n| n.as_str()) {
                    names.push(name.to_string());
                }
        }
    }

    names
}

/// Extract a shallow fingerprint from a response blob for comparison.
pub fn extract_response_fingerprint(store: &Store, response_blob: &str) -> ResponseFingerprint {
    let response: serde_json::Value = match store.blobs.get_json(response_blob) {
        Ok(v) => v,
        Err(_) => {
            return ResponseFingerprint {
                content_length: 0,
                has_tool_calls: false,
                tool_call_names: vec![],
                text_preview: String::new(),
            };
        }
    };

    let serialized = serde_json::to_string(&response).unwrap_or_default();
    let tool_call_names = extract_all_tool_names(&response);
    let has_tool_calls = !tool_call_names.is_empty();

    // Extract text preview from the response content
    let text_preview = extract_text_preview(&response);

    ResponseFingerprint {
        content_length: serialized.len(),
        has_tool_calls,
        tool_call_names,
        text_preview,
    }
}

fn extract_text_preview(response: &serde_json::Value) -> String {
    // OpenAI: choices[0].message.content
    if let Some(content) = response
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
    {
        return content.chars().take(200).collect();
    }

    // Anthropic: content[0].text
    if let Some(content) = response.get("content").and_then(|c| c.as_array()) {
        for block in content {
            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                return text.chars().take(200).collect();
            }
        }
    }

    String::new()
}
