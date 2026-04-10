use crate::models::ScoreResult;
use serde_json::Value;

/// Exact match: output == expected → 1.0, else 0.0
pub fn exact_match(output: &Value, expected: &Value, _config: &Value) -> ScoreResult {
    let matched = output == expected;
    ScoreResult {
        score: if matched { 1.0 } else { 0.0 },
        passed: matched,
        reasoning: if matched {
            "Output exactly matches expected".to_string()
        } else {
            "Output does not match expected".to_string()
        },
    }
}

/// Contains: check if a substring exists in the serialized output.
/// Config: {"substring": "..."}
pub fn contains(output: &Value, _expected: &Value, config: &Value) -> ScoreResult {
    let substring = config
        .get("substring")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let output_str = match output.as_str() {
        Some(s) => s.to_string(),
        None => serde_json::to_string(output).unwrap_or_default(),
    };
    let found = output_str.contains(substring);
    ScoreResult {
        score: if found { 1.0 } else { 0.0 },
        passed: found,
        reasoning: if found {
            format!("Output contains '{}'", substring)
        } else {
            format!("Output does not contain '{}'", substring)
        },
    }
}

/// Regex: match a pattern against the serialized output.
/// Config: {"pattern": "..."}
pub fn regex_match(output: &Value, _expected: &Value, config: &Value) -> ScoreResult {
    let pattern = config
        .get("pattern")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let output_str = match output.as_str() {
        Some(s) => s.to_string(),
        None => serde_json::to_string(output).unwrap_or_default(),
    };
    match regex::Regex::new(pattern) {
        Ok(re) => {
            let matched = re.is_match(&output_str);
            ScoreResult {
                score: if matched { 1.0 } else { 0.0 },
                passed: matched,
                reasoning: if matched {
                    format!("Output matches pattern '{}'", pattern)
                } else {
                    format!("Output does not match pattern '{}'", pattern)
                },
            }
        }
        Err(e) => ScoreResult {
            score: 0.0,
            passed: false,
            reasoning: format!("Invalid regex pattern '{}': {}", pattern, e),
        },
    }
}

/// JSON Schema validation: check if output conforms to a schema.
/// Config: {"schema": {...}} — the JSON schema object.
/// Simple structural validation: checks required keys and types.
pub fn json_schema(output: &Value, _expected: &Value, config: &Value) -> ScoreResult {
    let schema = match config.get("schema") {
        Some(s) => s,
        None => {
            return ScoreResult {
                score: 0.0,
                passed: false,
                reasoning: "No schema provided in config".to_string(),
            };
        }
    };

    // Simple validation: check required fields exist
    if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
        for field in required {
            if let Some(field_name) = field.as_str()
                && output.get(field_name).is_none()
            {
                return ScoreResult {
                    score: 0.0,
                    passed: false,
                    reasoning: format!("Missing required field '{}'", field_name),
                };
            }
        }
    }

    // Check properties exist and have correct types if "properties" is specified
    if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
        for (key, prop_schema) in props {
            if let Some(val) = output.get(key)
                && let Some(expected_type) = prop_schema.get("type").and_then(|t| t.as_str())
            {
                let type_ok = match expected_type {
                    "string" => val.is_string(),
                    "number" | "integer" => val.is_number(),
                    "boolean" => val.is_boolean(),
                    "array" => val.is_array(),
                    "object" => val.is_object(),
                    "null" => val.is_null(),
                    _ => true,
                };
                if !type_ok {
                    return ScoreResult {
                        score: 0.0,
                        passed: false,
                        reasoning: format!(
                            "Field '{}' has wrong type: expected {}, got {}",
                            key,
                            expected_type,
                            value_type_name(val)
                        ),
                    };
                }
            }
        }
    }

    ScoreResult {
        score: 1.0,
        passed: true,
        reasoning: "Output conforms to schema".to_string(),
    }
}

/// Tool use match: compare tool call names between output and expected.
/// Gives partial credit: score = matched_tools / expected_tools.
pub fn tool_use_match(output: &Value, expected: &Value, _config: &Value) -> ScoreResult {
    let expected_tools = extract_tool_names(expected);
    let output_tools = extract_tool_names(output);

    if expected_tools.is_empty() {
        return ScoreResult {
            score: if output_tools.is_empty() { 1.0 } else { 0.0 },
            passed: output_tools.is_empty(),
            reasoning: if output_tools.is_empty() {
                "No tools expected, none used".to_string()
            } else {
                format!("No tools expected, but found: {:?}", output_tools)
            },
        };
    }

    let matched = expected_tools
        .iter()
        .filter(|t| output_tools.contains(t))
        .count();
    let score = matched as f64 / expected_tools.len() as f64;
    let passed = (score - 1.0).abs() < f64::EPSILON;

    ScoreResult {
        score,
        passed,
        reasoning: format!(
            "{}/{} expected tools matched. Expected: {:?}, Got: {:?}",
            matched,
            expected_tools.len(),
            expected_tools,
            output_tools
        ),
    }
}

fn extract_tool_names(value: &Value) -> Vec<String> {
    let mut names = Vec::new();
    // OpenAI format: choices[0].message.tool_calls[*].function.name
    if let Some(choices) = value.get("choices").and_then(|c| c.as_array())
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
    // Anthropic format: content[*] where type == "tool_use"
    if let Some(content) = value.get("content").and_then(|c| c.as_array()) {
        for block in content {
            if block.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                && let Some(name) = block.get("name").and_then(|n| n.as_str())
            {
                names.push(name.to_string());
            }
        }
    }
    // Simple format: tool_calls[*].name or tools[*]
    if let Some(tool_calls) = value.get("tool_calls").and_then(|tc| tc.as_array()) {
        for tc in tool_calls {
            if let Some(name) = tc.get("name").and_then(|n| n.as_str()) {
                names.push(name.to_string());
            }
            if let Some(name) = tc.get("function").and_then(|f| f.get("name")).and_then(|n| n.as_str()) {
                names.push(name.to_string());
            }
        }
    }
    if let Some(tools) = value.get("tools").and_then(|t| t.as_array()) {
        for t in tools {
            if let Some(name) = t.as_str() {
                names.push(name.to_string());
            }
        }
    }
    names
}

fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_exact_match_pass() {
        let r = exact_match(&json!({"a": 1}), &json!({"a": 1}), &json!({}));
        assert_eq!(r.score, 1.0);
        assert!(r.passed);
    }

    #[test]
    fn test_exact_match_fail() {
        let r = exact_match(&json!({"a": 1}), &json!({"a": 2}), &json!({}));
        assert_eq!(r.score, 0.0);
        assert!(!r.passed);
    }

    #[test]
    fn test_contains_pass() {
        let r = contains(&json!("hello world"), &json!(null), &json!({"substring": "world"}));
        assert_eq!(r.score, 1.0);
        assert!(r.passed);
    }

    #[test]
    fn test_contains_fail() {
        let r = contains(&json!("hello"), &json!(null), &json!({"substring": "world"}));
        assert_eq!(r.score, 0.0);
        assert!(!r.passed);
    }

    #[test]
    fn test_contains_json_object() {
        let r = contains(&json!({"msg": "booking_confirmed"}), &json!(null), &json!({"substring": "booking_confirmed"}));
        assert_eq!(r.score, 1.0);
        assert!(r.passed);
    }

    #[test]
    fn test_regex_match_pass() {
        let r = regex_match(&json!("error code: 404"), &json!(null), &json!({"pattern": "\\d{3}"}));
        assert_eq!(r.score, 1.0);
        assert!(r.passed);
    }

    #[test]
    fn test_regex_match_fail() {
        let r = regex_match(&json!("no numbers here"), &json!(null), &json!({"pattern": "\\d+"}));
        assert_eq!(r.score, 0.0);
        assert!(!r.passed);
    }

    #[test]
    fn test_regex_invalid_pattern() {
        let r = regex_match(&json!("test"), &json!(null), &json!({"pattern": "[invalid"}));
        assert_eq!(r.score, 0.0);
        assert!(!r.passed);
        assert!(r.reasoning.contains("Invalid regex"));
    }

    #[test]
    fn test_json_schema_pass() {
        let schema = json!({
            "required": ["name", "age"],
            "properties": {
                "name": {"type": "string"},
                "age": {"type": "number"}
            }
        });
        let output = json!({"name": "Alice", "age": 30});
        let r = json_schema(&output, &json!(null), &json!({"schema": schema}));
        assert_eq!(r.score, 1.0);
        assert!(r.passed);
    }

    #[test]
    fn test_json_schema_missing_field() {
        let schema = json!({"required": ["name", "age"]});
        let output = json!({"name": "Alice"});
        let r = json_schema(&output, &json!(null), &json!({"schema": schema}));
        assert_eq!(r.score, 0.0);
        assert!(!r.passed);
        assert!(r.reasoning.contains("Missing required field"));
    }

    #[test]
    fn test_json_schema_wrong_type() {
        let schema = json!({
            "properties": {"age": {"type": "number"}}
        });
        let output = json!({"age": "thirty"});
        let r = json_schema(&output, &json!(null), &json!({"schema": schema}));
        assert_eq!(r.score, 0.0);
        assert!(!r.passed);
    }

    #[test]
    fn test_tool_use_match_full() {
        let expected = json!({"tool_calls": [{"name": "search"}, {"name": "book"}]});
        let output = json!({"tool_calls": [{"name": "search"}, {"name": "book"}]});
        let r = tool_use_match(&output, &expected, &json!({}));
        assert_eq!(r.score, 1.0);
        assert!(r.passed);
    }

    #[test]
    fn test_tool_use_match_partial() {
        let expected = json!({"tool_calls": [{"name": "search"}, {"name": "book"}]});
        let output = json!({"tool_calls": [{"name": "search"}]});
        let r = tool_use_match(&output, &expected, &json!({}));
        assert_eq!(r.score, 0.5);
        assert!(!r.passed);
    }

    #[test]
    fn test_tool_use_match_none_expected_none_got() {
        let r = tool_use_match(&json!({}), &json!({}), &json!({}));
        assert_eq!(r.score, 1.0);
        assert!(r.passed);
    }
}
