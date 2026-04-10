use anyhow::{bail, Result};
use rewind_store::{Dataset, DatasetExample, Store};
use serde_json::Value;
use std::io::{BufRead, Write};
use std::path::Path;

/// Manages datasets: CRUD, versioning, import/export, session extraction.
pub struct DatasetManager<'a> {
    store: &'a Store,
}

impl<'a> DatasetManager<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    /// Create a new empty dataset (version 1).
    pub fn create(&self, name: &str, description: &str) -> Result<Dataset> {
        if self.store.get_dataset_by_name(name)?.is_some() {
            bail!("Dataset '{}' already exists", name);
        }
        let dataset = Dataset::new(name, description);
        self.store.create_dataset(&dataset)?;
        Ok(dataset)
    }

    /// Add a single example to a dataset. Creates a new version with all previous examples + the new one.
    pub fn add_example(
        &self,
        dataset_name: &str,
        input: Value,
        expected: Value,
        metadata: Value,
    ) -> Result<DatasetExample> {
        let current = self
            .store
            .get_dataset_by_name(dataset_name)?
            .ok_or_else(|| anyhow::anyhow!("Dataset '{}' not found", dataset_name))?;

        // Create new version
        let mut new_ds = current.new_version();
        new_ds.example_count = current.example_count + 1;
        self.store.create_dataset(&new_ds)?;

        // Copy existing examples to new version
        self.store.copy_dataset_examples(&current.id, &new_ds.id)?;

        // Add new example
        let input_blob = self.store.blobs.put_json(&input)?;
        let expected_blob = self.store.blobs.put_json(&expected)?;
        let mut example = DatasetExample::new(
            &new_ds.id,
            current.example_count + 1, // 1-based ordinal
            &input_blob,
            &expected_blob,
        );
        example.metadata = metadata;
        self.store.create_dataset_example(&example)?;
        Ok(example)
    }

    /// Bulk add examples. Creates a single new version with all examples.
    pub fn add_examples_bulk(
        &self,
        dataset_name: &str,
        examples: Vec<(Value, Value, Value)>, // (input, expected, metadata)
    ) -> Result<Dataset> {
        if examples.is_empty() {
            bail!("No examples provided");
        }

        let current = self
            .store
            .get_dataset_by_name(dataset_name)?
            .ok_or_else(|| anyhow::anyhow!("Dataset '{}' not found", dataset_name))?;

        let mut new_ds = current.new_version();
        new_ds.example_count = current.example_count + examples.len() as u32;
        self.store.create_dataset(&new_ds)?;

        // Copy existing
        self.store.copy_dataset_examples(&current.id, &new_ds.id)?;

        // Add new examples
        for (i, (input, expected, metadata)) in examples.into_iter().enumerate() {
            let input_blob = self.store.blobs.put_json(&input)?;
            let expected_blob = self.store.blobs.put_json(&expected)?;
            let ordinal = current.example_count + 1 + i as u32;
            let mut example = DatasetExample::new(&new_ds.id, ordinal, &input_blob, &expected_blob);
            example.metadata = metadata;
            self.store.create_dataset_example(&example)?;
        }

        Ok(new_ds)
    }

    /// Import examples from a JSONL file. Each line: {"input": ..., "expected": ..., "metadata"?: ...}
    pub fn import_from_jsonl(&self, dataset_name: &str, path: &Path) -> Result<Dataset> {
        let file = std::fs::File::open(path)?;
        let reader = std::io::BufReader::new(file);

        let mut examples = Vec::new();
        for (line_num, line) in reader.lines().enumerate() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let parsed: Value = serde_json::from_str(trimmed)
                .map_err(|e| anyhow::anyhow!("Invalid JSON on line {}: {}", line_num + 1, e))?;
            let input = parsed
                .get("input")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Missing 'input' field on line {}", line_num + 1))?;
            let expected = parsed.get("expected").cloned().unwrap_or(Value::Null);
            let metadata = parsed.get("metadata").cloned().unwrap_or(serde_json::json!({}));
            examples.push((input, expected, metadata));
        }

        if examples.is_empty() {
            bail!("JSONL file is empty or has no valid lines");
        }

        // If the dataset doesn't exist yet, create it
        if self.store.get_dataset_by_name(dataset_name)?.is_none() {
            self.create(dataset_name, "")?;
        }

        self.add_examples_bulk(dataset_name, examples)
    }

    /// Export a dataset to JSONL format.
    pub fn export_jsonl(
        &self,
        dataset_name: &str,
        version: Option<u32>,
        writer: &mut dyn Write,
    ) -> Result<()> {
        let dataset = match version {
            Some(v) => self.store.get_dataset_by_name_version(dataset_name, v)?,
            None => self.store.get_dataset_by_name(dataset_name)?,
        };
        let dataset = dataset.ok_or_else(|| anyhow::anyhow!("Dataset '{}' not found", dataset_name))?;
        let examples = self.store.get_dataset_examples(&dataset.id)?;

        for ex in examples {
            let input: Value = self.store.blobs.get_json(&ex.input_blob)?;
            let expected: Value = if ex.expected_blob.is_empty() {
                Value::Null
            } else {
                self.store.blobs.get_json(&ex.expected_blob)?
            };
            let mut line = serde_json::json!({"input": input, "expected": expected});
            if ex.metadata != serde_json::json!({}) {
                line["metadata"] = ex.metadata;
            }
            writeln!(writer, "{}", serde_json::to_string(&line)?)?;
        }
        Ok(())
    }

    /// Extract an example from a recorded session step.
    pub fn import_from_session(
        &self,
        dataset_name: &str,
        session_ref: &str,
        input_step: u32,
        expected_step: Option<u32>,
    ) -> Result<DatasetExample> {
        // Resolve session
        let session = resolve_session(self.store, session_ref)?;
        let timeline = self
            .store
            .get_root_timeline(&session.id)?
            .ok_or_else(|| anyhow::anyhow!("Session has no root timeline"))?;
        let steps = self.store.get_steps(&timeline.id)?;

        // Get input from request blob of input_step
        let input_step_data = steps
            .iter()
            .find(|s| s.step_number == input_step)
            .ok_or_else(|| anyhow::anyhow!("Step {} not found in session", input_step))?;
        let input: Value = self.store.blobs.get_json(&input_step_data.request_blob)?;

        // Get expected from response blob of expected_step (or input_step if not specified)
        let expected_step_num = expected_step.unwrap_or(input_step);
        let expected_step_data = steps
            .iter()
            .find(|s| s.step_number == expected_step_num)
            .ok_or_else(|| anyhow::anyhow!("Step {} not found in session", expected_step_num))?;
        let expected: Value = self.store.blobs.get_json(&expected_step_data.response_blob)?;

        // If dataset doesn't exist yet, create it
        if self.store.get_dataset_by_name(dataset_name)?.is_none() {
            self.create(dataset_name, &format!("From session {}", session_ref))?;
        }

        let mut example = self.add_example(
            dataset_name,
            input,
            expected,
            serde_json::json!({"source": "session", "session_id": session.id, "input_step": input_step, "expected_step": expected_step_num}),
        )?;
        example.source_session_id = Some(session.id.clone());
        example.source_step_id = Some(input_step_data.id.clone());
        Ok(example)
    }

    /// Get the latest version of a dataset by name.
    pub fn get(&self, name: &str, version: Option<u32>) -> Result<Option<Dataset>> {
        match version {
            Some(v) => self.store.get_dataset_by_name_version(name, v),
            None => self.store.get_dataset_by_name(name),
        }
    }

    /// Get all examples for a dataset.
    pub fn get_examples(&self, dataset_id: &str) -> Result<Vec<DatasetExample>> {
        self.store.get_dataset_examples(dataset_id)
    }

    /// List all datasets (latest version of each).
    pub fn list(&self) -> Result<Vec<Dataset>> {
        self.store.list_datasets()
    }

    /// Delete a dataset and all its versions.
    pub fn delete(&self, name: &str) -> Result<()> {
        if self.store.get_dataset_by_name(name)?.is_none() {
            bail!("Dataset '{}' not found", name);
        }
        self.store.delete_dataset_by_name(name)
    }

    /// Resolve input/expected blobs for an example.
    pub fn resolve_example(&self, example: &DatasetExample) -> Result<(Value, Value)> {
        let input: Value = self.store.blobs.get_json(&example.input_blob)?;
        let expected: Value = if example.expected_blob.is_empty() {
            Value::Null
        } else {
            self.store.blobs.get_json(&example.expected_blob)?
        };
        Ok((input, expected))
    }
}

/// Resolve a session reference: "latest", exact ID, or prefix.
fn resolve_session(store: &Store, session_ref: &str) -> Result<rewind_store::Session> {
    if session_ref == "latest" {
        store
            .get_latest_session()?
            .ok_or_else(|| anyhow::anyhow!("No sessions recorded"))
    } else {
        // Try exact ID
        if let Some(s) = store.get_session(session_ref)? {
            return Ok(s);
        }
        // Try prefix match
        let sessions = store.list_sessions()?;
        sessions
            .into_iter()
            .find(|s| s.id.starts_with(session_ref))
            .ok_or_else(|| anyhow::anyhow!("Session '{}' not found", session_ref))
    }
}

/// Parse a dataset reference like "name" or "name@version".
pub fn parse_dataset_ref(reference: &str) -> (&str, Option<u32>) {
    if let Some(idx) = reference.rfind('@') {
        let name = &reference[..idx];
        let version_str = &reference[idx + 1..];
        if let Ok(v) = version_str.parse::<u32>() {
            return (name, Some(v));
        }
    }
    (reference, None)
}
