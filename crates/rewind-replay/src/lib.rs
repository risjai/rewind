use anyhow::{bail, Context, Result};
use rewind_store::{Span, Step, Store, Timeline};

/// Truncate an id to 8 chars for error messages — char-boundary safe (no
/// panic on multi-byte input).
fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Diff result between two timelines
#[derive(Debug, serde::Serialize)]
pub struct TimelineDiff {
    pub diverge_at_step: Option<u32>,
    pub left_label: String,
    pub right_label: String,
    pub step_diffs: Vec<StepDiff>,
}

#[derive(Debug, serde::Serialize)]
pub struct StepDiff {
    pub step_number: u32,
    pub diff_type: DiffType,
    pub left: Option<StepSummary>,
    pub right: Option<StepSummary>,
}

#[derive(Debug, serde::Serialize)]
pub struct StepSummary {
    pub step_type: String,
    pub status: String,
    pub model: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub duration_ms: u64,
    pub response_preview: String,
}

#[derive(Debug, PartialEq, serde::Serialize)]
pub enum DiffType {
    Same,
    Modified,
    LeftOnly,
    RightOnly,
}

/// Replay engine: hermetic replay from recorded data, fork-and-execute, timeline diff
pub struct ReplayEngine<'a> {
    store: &'a Store,
}

impl<'a> ReplayEngine<'a> {
    pub fn new(store: &'a Store) -> Self {
        ReplayEngine { store }
    }

    /// Get all steps for a timeline, including inherited steps from parent (for forks)
    pub fn get_full_timeline_steps(&self, timeline_id: &str, session_id: &str) -> Result<Vec<Step>> {
        let timelines = self.store.get_timelines(session_id)?;
        let timeline = timelines.iter().find(|t| t.id == timeline_id)
            .context("Timeline not found")?;

        if let (Some(parent_id), Some(fork_at)) = (&timeline.parent_timeline_id, timeline.fork_at_step) {
            // This is a forked timeline — get parent steps up to fork point, then own steps
            let parent_steps = self.store.get_steps(parent_id)?;
            let own_steps = self.store.get_steps(timeline_id)?;

            let mut combined: Vec<Step> = parent_steps.into_iter()
                .filter(|s| s.step_number <= fork_at)
                .collect();
            combined.extend(own_steps);
            combined.sort_by_key(|s| s.step_number);
            Ok(combined)
        } else {
            self.store.get_steps(timeline_id)
        }
    }

    /// Get all spans for a timeline, including inherited spans from parent (for forks)
    pub fn get_full_timeline_spans(&self, timeline_id: &str, session_id: &str) -> Result<Vec<Span>> {
        let timelines = self.store.get_timelines(session_id)?;
        let timeline = timelines.iter().find(|t| t.id == timeline_id)
            .context("Timeline not found")?;

        if let (Some(parent_id), Some(fork_at)) = (&timeline.parent_timeline_id, timeline.fork_at_step) {
            let parent_spans = self.store.get_spans_by_timeline(parent_id)?;
            let own_spans = self.store.get_spans_by_timeline(timeline_id)?;

            let parent_steps = self.store.get_steps(parent_id)?;
            let mut inherited: Vec<Span> = parent_spans.into_iter().filter(|span| {
                let span_steps: Vec<&Step> = parent_steps.iter()
                    .filter(|s| s.span_id.as_deref() == Some(&span.id))
                    .collect();
                if span_steps.is_empty() {
                    true
                } else {
                    span_steps.iter().all(|s| s.step_number <= fork_at)
                }
            }).collect();

            inherited.extend(own_spans);
            inherited.sort_by_key(|a| a.started_at);
            Ok(inherited)
        } else {
            self.store.get_spans_by_timeline(timeline_id)
        }
    }

    /// Create a fork: new timeline branching from a specific step
    pub fn fork(&self, session_id: &str, source_timeline_id: &str, at_step: u32, label: &str) -> Result<Timeline> {
        let steps = self.get_full_timeline_steps(source_timeline_id, session_id)?;
        let total = u32::try_from(steps.len()).unwrap_or(u32::MAX);
        if at_step == 0 || at_step > total {
            bail!("Invalid fork step {}. Session has {} steps (use 1-{}).", at_step, steps.len(), steps.len());
        }

        let fork = Timeline::new_fork(session_id, source_timeline_id, at_step, label);
        self.store.create_timeline(&fork)?;
        tracing::info!(
            fork_id = %fork.id,
            source = %source_timeline_id,
            at_step = at_step,
            "Created fork: {}",
            label,
        );
        Ok(fork)
    }

    /// Delete a fork and every step/span/replay-context/score that belongs
    /// to it. Enforces these invariants up front and refuses the delete
    /// rather than silently destroying data (issue #143):
    ///
    /// * The timeline must exist and belong to the given session.
    /// * It must not be the root (`parent_timeline_id` is `None`).
    /// * It must have no child forks — users must delete descendants first.
    /// * No baseline may reference it as `source_timeline_id` — deleting a
    ///   baselined fork would silently invalidate saved regression tests.
    pub fn delete_fork(&self, session_id: &str, timeline_id: &str) -> Result<()> {
        let timelines = self.store.get_timelines(session_id)?;

        let target = timelines.iter()
            .find(|t| t.id == timeline_id)
            .with_context(|| format!(
                "Timeline {} not found in session {}",
                short(timeline_id), short(session_id),
            ))?;

        if target.parent_timeline_id.is_none() {
            bail!("Cannot delete the root timeline of a session.");
        }

        let children: Vec<&Timeline> = timelines.iter()
            .filter(|t| t.parent_timeline_id.as_deref() == Some(timeline_id))
            .collect();
        if !children.is_empty() {
            let labels: Vec<String> = children.iter().map(|t| format!("'{}'", t.label)).collect();
            bail!(
                "Cannot delete fork '{}' while it has {} child fork(s): {}. Delete the children first.",
                target.label, children.len(), labels.join(", "),
            );
        }

        let baseline_refs = self.store.count_baselines_referencing_timeline(timeline_id)?;
        if baseline_refs > 0 {
            bail!(
                "Cannot delete fork '{}' — {} baseline(s) reference it. Delete the baselines first \
                 or pick a different fork.",
                target.label, baseline_refs,
            );
        }

        let deleted = self.store.delete_timeline(timeline_id)?;
        if !deleted {
            // Another caller raced us — the existence check above passed but
            // the row is now gone. Surface the mismatch rather than silently
            // returning Ok.
            bail!("Timeline {} was concurrently removed.", short(timeline_id));
        }

        tracing::info!(
            fork_id = %timeline_id,
            session_id = %session_id,
            "Deleted fork: {}",
            target.label,
        );
        Ok(())
    }

    /// Diff two timelines step by step
    pub fn diff_timelines(&self, session_id: &str, left_timeline_id: &str, right_timeline_id: &str) -> Result<TimelineDiff> {
        let left_steps = self.get_full_timeline_steps(left_timeline_id, session_id)?;
        let right_steps = self.get_full_timeline_steps(right_timeline_id, session_id)?;

        let timelines = self.store.get_timelines(session_id)?;
        let left_label = timelines.iter().find(|t| t.id == left_timeline_id)
            .map(|t| t.label.clone()).unwrap_or_else(|| "left".into());
        let right_label = timelines.iter().find(|t| t.id == right_timeline_id)
            .map(|t| t.label.clone()).unwrap_or_else(|| "right".into());

        let max_steps = left_steps.len().max(right_steps.len());
        let mut step_diffs = Vec::new();
        let mut diverge_at_step = None;

        for i in 0..max_steps {
            let left = left_steps.get(i);
            let right = right_steps.get(i);
            let step_num = (i + 1) as u32;

            let diff_type = match (left, right) {
                (Some(l), Some(r)) => {
                    if l.response_blob == r.response_blob && l.status == r.status {
                        DiffType::Same
                    } else {
                        if diverge_at_step.is_none() {
                            diverge_at_step = Some(step_num);
                        }
                        DiffType::Modified
                    }
                }
                (Some(_), None) => {
                    if diverge_at_step.is_none() {
                        diverge_at_step = Some(step_num);
                    }
                    DiffType::LeftOnly
                }
                (None, Some(_)) => {
                    if diverge_at_step.is_none() {
                        diverge_at_step = Some(step_num);
                    }
                    DiffType::RightOnly
                }
                (None, None) => continue,
            };

            step_diffs.push(StepDiff {
                step_number: step_num,
                diff_type,
                left: left.map(|s| self.step_summary(s)),
                right: right.map(|s| self.step_summary(s)),
            });
        }

        Ok(TimelineDiff {
            diverge_at_step,
            left_label,
            right_label,
            step_diffs,
        })
    }

    fn step_summary(&self, step: &Step) -> StepSummary {
        let response_preview = self.store.blobs.get(&step.response_blob)
            .ok()
            .and_then(|data| String::from_utf8(data).ok())
            .and_then(|json_str| {
                let val: serde_json::Value = serde_json::from_str(&json_str).ok()?;
                // OpenAI format
                if let Some(choices) = val.get("choices").and_then(|c| c.as_array())
                    && let Some(msg) = choices.first()
                        .and_then(|c| c.get("message"))
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_str())
                    {
                        return Some(msg.chars().take(150).collect());
                    }
                // Anthropic format
                if let Some(content) = val.get("content").and_then(|c| c.as_array())
                    && let Some(text) = content.first()
                        .and_then(|b| b.get("text"))
                        .and_then(|t| t.as_str())
                    {
                        return Some(text.chars().take(150).collect());
                    }
                Some(json_str.chars().take(150).collect())
            })
            .unwrap_or_else(|| "(no response)".to_string());

        StepSummary {
            step_type: step.step_type.label().to_string(),
            status: step.status.as_str().to_string(),
            model: step.model.clone(),
            tokens_in: step.tokens_in,
            tokens_out: step.tokens_out,
            duration_ms: step.duration_ms,
            response_preview,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rewind_store::{Baseline, Session, Step, Timeline};
    use tempfile::TempDir;

    fn setup() -> (TempDir, Store) {
        let tmp = TempDir::new().unwrap();
        let store = Store::open(tmp.path()).unwrap();
        (tmp, store)
    }

    fn seed_session_with_steps(store: &Store, step_count: u32) -> (String, String) {
        let session = Session::new("test-session");
        let timeline = Timeline::new_root(&session.id);
        store.create_session(&session).unwrap();
        store.create_timeline(&timeline).unwrap();
        for i in 1..=step_count {
            let step = Step::new_llm_call(&timeline.id, &session.id, i, "gpt-4o");
            store.create_step(&step).unwrap();
        }
        (session.id, timeline.id)
    }

    #[test]
    fn fork_at_step_zero_is_rejected() {
        let (_tmp, store) = setup();
        let (sid, tid) = seed_session_with_steps(&store, 3);
        let engine = ReplayEngine::new(&store);
        let err = engine.fork(&sid, &tid, 0, "bad-fork").unwrap_err();
        assert!(err.to_string().contains("Invalid fork step 0"));
    }

    #[test]
    fn fork_beyond_total_steps_is_rejected() {
        let (_tmp, store) = setup();
        let (sid, tid) = seed_session_with_steps(&store, 3);
        let engine = ReplayEngine::new(&store);
        let err = engine.fork(&sid, &tid, 99, "bad-fork").unwrap_err();
        assert!(err.to_string().contains("Invalid fork step 99"));
        assert!(err.to_string().contains("3 steps"));
    }

    #[test]
    fn fork_at_valid_step_succeeds() {
        let (_tmp, store) = setup();
        let (sid, tid) = seed_session_with_steps(&store, 3);
        let engine = ReplayEngine::new(&store);
        let fork = engine.fork(&sid, &tid, 2, "valid-fork");
        assert!(fork.is_ok());
    }

    // ── delete_fork tests (#143) ─────────────────────────────────

    #[test]
    fn delete_fork_removes_a_childless_fork_and_its_steps() {
        let (_tmp, store) = setup();
        let (sid, tid) = seed_session_with_steps(&store, 3);
        let engine = ReplayEngine::new(&store);
        let fork = engine.fork(&sid, &tid, 2, "throwaway").unwrap();
        // Add a step on the fork so we can assert the cascade.
        let fork_step = Step::new_llm_call(&fork.id, &sid, 3, "gpt-4o");
        store.create_step(&fork_step).unwrap();

        engine.delete_fork(&sid, &fork.id).unwrap();

        let timelines = store.get_timelines(&sid).unwrap();
        assert!(timelines.iter().all(|t| t.id != fork.id), "fork row should be gone");
        let remaining_steps = store.get_steps(&fork.id).unwrap();
        assert!(remaining_steps.is_empty(), "fork's steps should be gone");
    }

    #[test]
    fn delete_fork_refuses_to_delete_the_root_timeline() {
        let (_tmp, store) = setup();
        let (sid, tid) = seed_session_with_steps(&store, 3);
        let engine = ReplayEngine::new(&store);
        let err = engine.delete_fork(&sid, &tid).unwrap_err();
        assert!(err.to_string().contains("root timeline"), "got: {}", err);
    }

    #[test]
    fn delete_fork_refuses_when_children_exist() {
        let (_tmp, store) = setup();
        let (sid, root) = seed_session_with_steps(&store, 3);
        let engine = ReplayEngine::new(&store);
        let parent_fork = engine.fork(&sid, &root, 2, "parent-fork").unwrap();
        // Seed a step on the parent fork so the child fork is valid.
        let step = Step::new_llm_call(&parent_fork.id, &sid, 3, "gpt-4o");
        store.create_step(&step).unwrap();
        let _child = engine.fork(&sid, &parent_fork.id, 2, "child-fork").unwrap();

        let err = engine.delete_fork(&sid, &parent_fork.id).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("child fork"), "got: {msg}");
        assert!(msg.contains("'child-fork'"), "should list the blocking children: {msg}");

        // Parent still present.
        let timelines = store.get_timelines(&sid).unwrap();
        assert!(timelines.iter().any(|t| t.id == parent_fork.id));
    }

    #[test]
    fn delete_fork_refuses_when_a_baseline_references_the_fork() {
        let (_tmp, store) = setup();
        let (sid, root) = seed_session_with_steps(&store, 3);
        let engine = ReplayEngine::new(&store);
        let fork = engine.fork(&sid, &root, 2, "baselined").unwrap();
        let baseline = Baseline::new("golden", &sid, &fork.id, "", 2, 0);
        store.create_baseline(&baseline).unwrap();

        let err = engine.delete_fork(&sid, &fork.id).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("baseline"), "got: {msg}");

        // Fork still present.
        let timelines = store.get_timelines(&sid).unwrap();
        assert!(timelines.iter().any(|t| t.id == fork.id));
    }

    #[test]
    fn delete_fork_errors_when_timeline_id_does_not_exist() {
        let (_tmp, store) = setup();
        let (sid, _tid) = seed_session_with_steps(&store, 3);
        let engine = ReplayEngine::new(&store);
        let err = engine.delete_fork(&sid, "nonexistent-id").unwrap_err();
        assert!(err.to_string().contains("not found"), "got: {}", err);
    }
}
