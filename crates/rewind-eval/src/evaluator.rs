use anyhow::{bail, Result};
use rewind_store::Store;
use serde_json::Value;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Instant;

use crate::models::ScoreResult;
use crate::scoring;

/// Registry that resolves evaluator names to scoring functions.
pub struct EvaluatorRegistry<'a> {
    store: &'a Store,
}

impl<'a> EvaluatorRegistry<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    /// Score an output using the named evaluator.
    pub fn score(
        &self,
        evaluator_name: &str,
        input: &Value,
        output: &Value,
        expected: &Value,
    ) -> Result<(String, ScoreResult)> {
        let evaluator = self
            .store
            .get_evaluator_by_name(evaluator_name)?
            .ok_or_else(|| anyhow::anyhow!("Evaluator '{}' not found", evaluator_name))?;

        let config: Value = if evaluator.config_blob.is_empty() {
            Value::Object(serde_json::Map::new())
        } else {
            // Try blob store first, then parse as raw JSON
            match self.store.blobs.get_json::<Value>(&evaluator.config_blob) {
                Ok(v) => v,
                Err(_) => serde_json::from_str(&evaluator.config_blob).unwrap_or_default(),
            }
        };

        let result = match evaluator.evaluator_type.as_str() {
            "exact_match" => scoring::exact_match(output, expected, &config),
            "contains" => scoring::contains(output, expected, &config),
            "regex" => scoring::regex_match(output, expected, &config),
            "json_schema" => scoring::json_schema(output, expected, &config),
            "tool_use_match" => scoring::tool_use_match(output, expected, &config),
            "custom" => run_custom_evaluator(&config, input, output, expected)?,
            "llm_judge" => run_llm_judge_evaluator(&config, input, output, expected)?,
            other => bail!("Unsupported evaluator type: '{}'. Supported: exact_match, contains, regex, json_schema, tool_use_match, custom, llm_judge", other),
        };

        Ok((evaluator.id, result))
    }

    /// List valid built-in evaluator type names.
    pub fn builtin_types() -> &'static [&'static str] {
        &["exact_match", "contains", "regex", "json_schema", "tool_use_match", "custom", "llm_judge"]
    }

    /// Validate that an evaluator type is known.
    pub fn is_valid_type(evaluator_type: &str) -> bool {
        Self::builtin_types().contains(&evaluator_type)
    }
}

/// Redact API keys from error output to prevent leaking secrets into stored reasoning.
fn sanitize_secrets(input: &str) -> String {
    // Redact OpenAI-style keys: sk-..., sk-proj-..., etc.
    let re = regex::Regex::new(r"sk-[a-zA-Z0-9_-]{10,}").unwrap();
    re.replace_all(input, "sk-***REDACTED***").to_string()
}

/// Execute the built-in LLM-as-judge evaluator via a Python subprocess.
///
/// Contract:
///   - Calls `python3 -m rewind_agent.llm_judge`
///   - Stdin receives: `{"input": ..., "output": ..., "expected": ..., "config": ...}`
///     (NOTE: unlike `run_custom_evaluator`, this passes `config` on stdin)
///   - Stdout must return: `{"score": 0.0-1.0, "passed": bool, "reasoning": "..."}`
///   - Timeout: 120 seconds (LLM calls can be slow)
///   - Non-zero exit code = score 0.0 with error reasoning
fn run_llm_judge_evaluator(
    config: &Value,
    input: &Value,
    output: &Value,
    expected: &Value,
) -> Result<ScoreResult> {
    let payload = serde_json::json!({
        "input": input,
        "output": output,
        "expected": expected,
        "config": config,
    });
    let payload_str = serde_json::to_string(&payload)?;

    let mut cmd = Command::new("python3");
    cmd.args(["-m", "rewind_agent.llm_judge"]);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn LLM judge (python3 -m rewind_agent.llm_judge): {}", e))?;

    // Write payload to stdin
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(payload_str.as_bytes());
    }

    // Wait with 120s timeout (LLM calls can be slow)
    let timeout = std::time::Duration::from_secs(120);
    loop {
        match child.try_wait()? {
            Some(status) => {
                let duration = start.elapsed();
                if !status.success() {
                    let mut stderr_str = String::new();
                    if let Some(mut stderr) = child.stderr.take() {
                        let _ = stderr.read_to_string(&mut stderr_str);
                    }
                    stderr_str.truncate(1000);
                    let sanitized = sanitize_secrets(stderr_str.trim());
                    return Ok(ScoreResult {
                        score: 0.0,
                        passed: false,
                        reasoning: format!(
                            "LLM judge exited with {} ({}ms). stderr: {}",
                            status,
                            duration.as_millis(),
                            sanitized
                        ),
                    });
                }

                // Read stdout
                let mut stdout_str = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    stdout.read_to_string(&mut stdout_str)?;
                }
                stdout_str.truncate(100_000);

                // Parse result
                let result: Value = serde_json::from_str(stdout_str.trim()).map_err(|e| {
                    anyhow::anyhow!(
                        "LLM judge stdout is not valid JSON: {}. Got: '{}'",
                        e,
                        stdout_str.chars().take(200).collect::<String>()
                    )
                })?;

                let score = result
                    .get("score")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0);
                let passed = result
                    .get("passed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(score >= 0.5);
                let reasoning = result
                    .get("reasoning")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                return Ok(ScoreResult {
                    score,
                    passed,
                    reasoning,
                });
            }
            None => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(ScoreResult {
                        score: 0.0,
                        passed: false,
                        reasoning: format!("LLM judge timed out after {}s", timeout.as_secs()),
                    });
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

/// Execute a custom evaluator as a subprocess.
///
/// Contract:
///   - Config must contain `{"command": "path/to/evaluator"}`
///   - Stdin receives: `{"input": ..., "output": ..., "expected": ...}`
///   - Stdout must return: `{"score": 0.0-1.0, "passed": bool, "reasoning": "..."}`
///   - Timeout: 30 seconds per evaluation
///   - Non-zero exit code = score 0.0 with error reasoning
fn run_custom_evaluator(
    config: &Value,
    input: &Value,
    output: &Value,
    expected: &Value,
) -> Result<ScoreResult> {
    let command = config
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Custom evaluator config must contain a 'command' field, e.g. {{\"command\": \"python judge.py\"}}"
            )
        })?;

    let payload = serde_json::json!({
        "input": input,
        "output": output,
        "expected": expected,
    });
    let payload_str = serde_json::to_string(&payload)?;

    // Parse command into program + args
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        bail!("Custom evaluator command is empty");
    }

    let mut cmd = Command::new(parts[0]);
    if parts.len() > 1 {
        cmd.args(&parts[1..]);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn custom evaluator '{}': {}", command, e))?;

    // Write payload to stdin
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(payload_str.as_bytes());
    }

    // Wait with 30s timeout
    let timeout = std::time::Duration::from_secs(30);
    loop {
        match child.try_wait()? {
            Some(status) => {
                let duration = start.elapsed();
                if !status.success() {
                    let mut stderr_str = String::new();
                    if let Some(mut stderr) = child.stderr.take() {
                        let _ = stderr.read_to_string(&mut stderr_str);
                    }
                    stderr_str.truncate(1000);
                    return Ok(ScoreResult {
                        score: 0.0,
                        passed: false,
                        reasoning: format!(
                            "Custom evaluator exited with {} ({}ms). stderr: {}",
                            status,
                            duration.as_millis(),
                            stderr_str.trim()
                        ),
                    });
                }

                // Read stdout
                let mut stdout_str = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    stdout.read_to_string(&mut stdout_str)?;
                }
                stdout_str.truncate(100_000); // 100KB max

                // Parse result
                let result: Value = serde_json::from_str(stdout_str.trim()).map_err(|e| {
                    anyhow::anyhow!(
                        "Custom evaluator stdout is not valid JSON: {}. Got: '{}'",
                        e,
                        stdout_str.chars().take(200).collect::<String>()
                    )
                })?;

                let score = result
                    .get("score")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0);
                let passed = result
                    .get("passed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(score >= 0.5);
                let reasoning = result
                    .get("reasoning")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                return Ok(ScoreResult {
                    score,
                    passed,
                    reasoning,
                });
            }
            None => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(ScoreResult {
                        score: 0.0,
                        passed: false,
                        reasoning: format!("Custom evaluator timed out after {}s", timeout.as_secs()),
                    });
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_types_includes_llm_judge() {
        let types = EvaluatorRegistry::builtin_types();
        assert!(
            types.contains(&"llm_judge"),
            "builtin_types() should include 'llm_judge', got: {:?}",
            types
        );
    }

    #[test]
    fn test_is_valid_type_llm_judge() {
        assert!(EvaluatorRegistry::is_valid_type("llm_judge"));
    }

    #[test]
    fn test_is_valid_type_rejects_unknown() {
        assert!(!EvaluatorRegistry::is_valid_type("not_a_real_type"));
    }

    #[test]
    fn test_builtin_types_count() {
        // 7 types: exact_match, contains, regex, json_schema, tool_use_match, custom, llm_judge
        assert_eq!(EvaluatorRegistry::builtin_types().len(), 7);
    }

    #[test]
    fn test_sanitize_secrets_redacts_api_key() {
        let input = "Error: Incorrect API key provided: sk-proj-abc123def456ghi789jkl0";
        let sanitized = sanitize_secrets(input);
        assert!(!sanitized.contains("sk-proj-abc123def456ghi789jkl0"));
        assert!(sanitized.contains("sk-***REDACTED***"));
    }

    #[test]
    fn test_sanitize_secrets_preserves_clean_text() {
        let input = "LLM judge failed: connection refused";
        assert_eq!(sanitize_secrets(input), input);
    }

    #[test]
    fn test_sanitize_secrets_redacts_multiple_keys() {
        let input = "key1: sk-abcdefghijklmnop key2: sk-1234567890abcdef";
        let sanitized = sanitize_secrets(input);
        assert!(!sanitized.contains("sk-abcdefghijklmnop"));
        assert!(!sanitized.contains("sk-1234567890abcdef"));
    }
}
