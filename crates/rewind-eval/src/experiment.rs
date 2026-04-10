use anyhow::{bail, Result};
use rewind_store::{Experiment, ExperimentResult, ExperimentScore, ExperimentStatus, Store};
use serde_json::Value;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Instant;

use crate::dataset::DatasetManager;
use crate::evaluator::EvaluatorRegistry;

/// Configuration for running an experiment.
pub struct RunConfig {
    pub dataset_name: String,
    pub dataset_version: Option<u32>,
    pub evaluator_names: Vec<String>,
    pub command: String,
    pub name: Option<String>,
    pub fail_below: Option<f64>,
    pub timeout_per_example_secs: u64,
    pub metadata: serde_json::Value,
}

/// Runs experiments: execute target command per example, score, aggregate.
pub struct ExperimentRunner<'a> {
    store: &'a Store,
}

impl<'a> ExperimentRunner<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    pub fn run(&self, config: RunConfig) -> Result<Experiment> {
        let dataset_mgr = DatasetManager::new(self.store);
        let eval_registry = EvaluatorRegistry::new(self.store);

        // Resolve dataset
        let dataset = dataset_mgr
            .get(&config.dataset_name, config.dataset_version)?
            .ok_or_else(|| {
                anyhow::anyhow!("Dataset '{}' not found", config.dataset_name)
            })?;
        let examples = dataset_mgr.get_examples(&dataset.id)?;
        if examples.is_empty() {
            bail!("Dataset '{}' has no examples", config.dataset_name);
        }

        // Validate evaluators exist
        for name in &config.evaluator_names {
            self.store
                .get_evaluator_by_name(name)?
                .ok_or_else(|| anyhow::anyhow!("Evaluator '{}' not found", name))?;
        }

        // Create experiment
        let exp_name = config.name.unwrap_or_else(|| {
            format!(
                "{}-{}",
                config.dataset_name,
                chrono::Utc::now().format("%Y%m%d-%H%M%S")
            )
        });
        let config_json = serde_json::json!({
            "schema_version": 1,
            "command": config.command,
            "evaluators": config.evaluator_names,
            "timeout_secs": config.timeout_per_example_secs,
        });
        let config_blob = self.store.blobs.put_json(&config_json)?;
        let mut experiment = Experiment::new(
            &exp_name,
            &dataset.id,
            dataset.version,
            examples.len() as u32,
            &config_blob,
        );
        experiment.metadata = config.metadata;
        self.store.create_experiment(&experiment)?;

        // Update status to running
        self.store
            .update_experiment_status(&experiment.id, ExperimentStatus::Running)?;

        // Run each example
        let mut all_scores: Vec<f64> = Vec::new();
        let mut total_passed: u32 = 0;
        let mut total_duration_ms: u64 = 0;
        let total_tokens: u64 = 0;
        let mut completed: u32 = 0;

        for example in &examples {
            let (input, expected) = dataset_mgr.resolve_example(example)?;
            let input_json = serde_json::to_string(&input)?;

            // Execute target command
            let start = Instant::now();
            let (output_value, exec_status, exec_error) =
                execute_command(&config.command, &input_json, config.timeout_per_example_secs);
            let duration_ms = start.elapsed().as_millis() as u64;

            // Store output in blob
            let output_blob = match &output_value {
                Some(v) => self.store.blobs.put_json(v)?,
                None => String::new(),
            };

            // Create result record
            let mut result = ExperimentResult::new(&experiment.id, &example.id, example.ordinal);
            result.output_blob = output_blob;
            result.duration_ms = duration_ms;
            result.status = exec_status.clone();
            result.error = exec_error.clone();
            self.store.create_experiment_result(&result)?;

            total_duration_ms += duration_ms;

            // Score with each evaluator (only if execution succeeded)
            if exec_status == "success" {
                let output = output_value.as_ref().unwrap_or(&Value::Null);
                let mut example_scores = Vec::new();

                for evaluator_name in &config.evaluator_names {
                    match eval_registry.score(evaluator_name, &input, output, &expected) {
                        Ok((evaluator_id, score_result)) => {
                            let score = ExperimentScore::new(
                                &result.id,
                                &evaluator_id,
                                score_result.score,
                                score_result.passed,
                                &score_result.reasoning,
                            );
                            self.store.create_experiment_score(&score)?;
                            example_scores.push(score_result.score);
                            if score_result.passed {
                                total_passed += 1;
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Evaluator '{}' failed: {}", evaluator_name, e);
                            // Record a zero score for failed evaluator
                            let evaluator = self.store.get_evaluator_by_name(evaluator_name)?;
                            if let Some(ev) = evaluator {
                                let score = ExperimentScore::new(
                                    &result.id,
                                    &ev.id,
                                    0.0,
                                    false,
                                    &format!("Evaluator error: {}", e),
                                );
                                self.store.create_experiment_score(&score)?;
                            }
                        }
                    }
                }

                if !example_scores.is_empty() {
                    let avg = example_scores.iter().sum::<f64>() / example_scores.len() as f64;
                    all_scores.push(avg);
                }
            }

            completed += 1;
            self.store
                .update_experiment_progress(&experiment.id, completed)?;
        }

        // Compute aggregates
        let (avg_score, min_score, max_score) = if all_scores.is_empty() {
            (0.0, 0.0, 0.0)
        } else {
            let avg = all_scores.iter().sum::<f64>() / all_scores.len() as f64;
            let min = all_scores.iter().cloned().fold(f64::INFINITY, f64::min);
            let max = all_scores
                .iter()
                .cloned()
                .fold(f64::NEG_INFINITY, f64::max);
            (avg, min, max)
        };
        let total_scores = examples.len() as u32 * config.evaluator_names.len() as u32;
        let pass_rate = if total_scores > 0 {
            total_passed as f64 / total_scores as f64
        } else {
            0.0
        };

        self.store.update_experiment_aggregates(
            &experiment.id,
            avg_score,
            min_score,
            max_score,
            pass_rate,
            total_duration_ms,
            total_tokens,
        )?;
        self.store
            .update_experiment_status(&experiment.id, ExperimentStatus::Completed)?;

        // Re-read to get updated fields
        let experiment = self
            .store
            .get_experiment(&experiment.id)?
            .unwrap_or(experiment);
        Ok(experiment)
    }
}

/// Execute a command with input on stdin, capture stdout as JSON.
/// Returns (output_value, status, error).
fn execute_command(
    command: &str,
    input_json: &str,
    timeout_secs: u64,
) -> (Option<Value>, String, Option<String>) {
    // Parse command — support shell-style "python script.py" or just "cat"
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return (
            None,
            "error".to_string(),
            Some("Empty command".to_string()),
        );
    }

    let mut cmd = Command::new(parts[0]);
    if parts.len() > 1 {
        cmd.args(&parts[1..]);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("REWIND_EVAL_RUNNING", "1");

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return (
                None,
                "error".to_string(),
                Some(format!("Failed to spawn command '{}': {}", command, e)),
            );
        }
    };

    // Write input to stdin
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(input_json.as_bytes());
        // stdin is dropped here, closing the pipe
    }

    // Wait with timeout
    let result = if timeout_secs > 0 {
        match child.wait_timeout(std::time::Duration::from_secs(timeout_secs)) {
            Ok(Some(status)) => Ok(status),
            Ok(None) => {
                // Timed out — kill process
                let _ = child.kill();
                let _ = child.wait();
                return (
                    None,
                    "error".to_string(),
                    Some(format!("Timeout after {}s", timeout_secs)),
                );
            }
            Err(e) => Err(e),
        }
    } else {
        child.wait()
    };

    match result {
        Ok(status) => {
            if !status.success() {
                let mut stderr_str = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = stderr.read_to_string(&mut stderr_str);
                }
                // Truncate stderr to 1MB
                stderr_str.truncate(1_000_000);
                return (
                    None,
                    "error".to_string(),
                    Some(format!(
                        "Command exited with status {}. stderr: {}",
                        status,
                        stderr_str.trim()
                    )),
                );
            }

            // Read stdout
            let mut stdout_str = String::new();
            if let Some(mut stdout) = child.stdout.take() {
                let _ = stdout.read_to_string(&mut stdout_str);
            }
            // Truncate stdout to 10MB
            stdout_str.truncate(10_000_000);

            // Parse as JSON
            match serde_json::from_str::<Value>(stdout_str.trim()) {
                Ok(v) => (Some(v), "success".to_string(), None),
                Err(_) => {
                    // If not valid JSON, wrap the raw string as a JSON string value
                    let v = Value::String(stdout_str.trim().to_string());
                    (Some(v), "success".to_string(), None)
                }
            }
        }
        Err(e) => (
            None,
            "error".to_string(),
            Some(format!("Failed to wait on command: {}", e)),
        ),
    }
}

// wait_timeout is not in std — we need a simple implementation
trait WaitTimeout {
    fn wait_timeout(
        &mut self,
        dur: std::time::Duration,
    ) -> std::io::Result<Option<std::process::ExitStatus>>;
}

impl WaitTimeout for std::process::Child {
    fn wait_timeout(
        &mut self,
        dur: std::time::Duration,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        let start = Instant::now();
        loop {
            match self.try_wait()? {
                Some(status) => return Ok(Some(status)),
                None => {
                    if start.elapsed() >= dur {
                        return Ok(None);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
    }
}
