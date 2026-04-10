use anyhow::{bail, Result};
use rewind_store::Store;
use serde_json::Value;

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
            other => bail!("Unsupported evaluator type: '{}'. Supported: exact_match, contains, regex, json_schema, tool_use_match", other),
        };

        // Ignore `input` for now — built-in evaluators only compare output vs expected.
        // LLM-as-judge (Phase 2) will use input for context.
        let _ = input;

        Ok((evaluator.id, result))
    }

    /// List valid built-in evaluator type names.
    pub fn builtin_types() -> &'static [&'static str] {
        &["exact_match", "contains", "regex", "json_schema", "tool_use_match"]
    }

    /// Validate that an evaluator type is known.
    pub fn is_valid_type(evaluator_type: &str) -> bool {
        Self::builtin_types().contains(&evaluator_type) || evaluator_type == "llm_judge" || evaluator_type == "custom"
    }
}
