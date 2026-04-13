use anyhow::{bail, Context, Result};
use rewind_replay::ReplayEngine;
use rewind_store::{Baseline, BaselineStep, Store};

use crate::extract::extract_tool_name;

pub struct BaselineManager<'a> {
    store: &'a Store,
}

impl<'a> BaselineManager<'a> {
    pub fn new(store: &'a Store) -> Self {
        BaselineManager { store }
    }

    /// Create a baseline from an existing session's timeline.
    /// Extracts step signatures and stores them in the baselines/baseline_steps tables.
    pub fn create_baseline(
        &self,
        session_id: &str,
        timeline_id: &str,
        name: &str,
        description: &str,
    ) -> Result<Baseline> {
        // Validate name format
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
            bail!("Invalid baseline name '{}'. Use only letters, numbers, hyphens, underscores, and dots.", name);
        }

        // Check name uniqueness
        if self.store.get_baseline_by_name(name)?.is_some() {
            bail!("Baseline '{}' already exists. Choose a different name.", name);
        }

        let engine = ReplayEngine::new(self.store);
        let steps = engine
            .get_full_timeline_steps(timeline_id, session_id)
            .context("Failed to get timeline steps")?;

        let total_tokens: u64 = steps.iter().map(|s| s.tokens_in + s.tokens_out).sum();

        let baseline = Baseline::new(
            name,
            session_id,
            timeline_id,
            description,
            steps.len() as u32,
            total_tokens,
        );

        self.store
            .create_baseline(&baseline)
            .context("Failed to create baseline")?;

        // Extract and store step signatures
        for step in &steps {
            let tool_name = extract_tool_name(self.store, step);
            let bs = BaselineStep::from_step(&baseline.id, step, tool_name);
            self.store
                .create_baseline_step(&bs)
                .context("Failed to create baseline step")?;
        }

        tracing::info!(
            baseline_id = %baseline.id,
            name = %baseline.name,
            steps = baseline.step_count,
            "Created baseline"
        );

        Ok(baseline)
    }

    /// List all baselines.
    pub fn list_baselines(&self) -> Result<Vec<Baseline>> {
        self.store.list_baselines()
    }

    /// Get a baseline by name.
    pub fn get_baseline(&self, name: &str) -> Result<Option<Baseline>> {
        self.store.get_baseline_by_name(name)
    }

    /// Get the expected steps for a baseline.
    pub fn get_baseline_steps(&self, baseline_id: &str) -> Result<Vec<BaselineStep>> {
        self.store.get_baseline_steps(baseline_id)
    }

    /// Delete a baseline and its steps (CASCADE).
    pub fn delete_baseline(&self, name: &str) -> Result<()> {
        let baseline = self
            .store
            .get_baseline_by_name(name)?
            .context(format!("Baseline '{}' not found", name))?;
        self.store.delete_baseline(&baseline.id)
    }
}
