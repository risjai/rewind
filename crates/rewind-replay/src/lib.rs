use anyhow::{bail, Context, Result};
use rewind_store::{Span, Step, Store, Timeline};
use std::collections::HashMap;

/// Truncate an id to 8 chars for error messages — char-boundary safe (no
/// panic on multi-byte input).
fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Cap label length in error messages so a pathological (user-controlled)
/// label can't balloon the HTTP response. 64 chars is well above anything a
/// human would type; the CLI's `LABEL_REGEX` tolerates up to this length in
/// practice.
fn truncate_label(label: &str) -> String {
    let mut out: String = label.chars().take(64).collect();
    if label.chars().count() > 64 {
        out.push('…');
    }
    out
}

/// Typed error returned from [`ReplayEngine::delete_fork`]. The HTTP layer
/// maps each variant to a specific status code instead of scraping the
/// error message — see santa-review Important-4 on PR #146.
#[derive(Debug, thiserror::Error)]
pub enum DeleteForkError {
    #[error("Timeline {short} not found in session {session}", short = short(.0), session = short(.1))]
    NotFound(String, String),

    #[error("Cannot delete the root timeline of a session.")]
    IsRoot,

    #[error("Cannot delete fork '{parent}' while it has {count} child fork(s): {children}. Delete the children first.", children = children.join(", "))]
    HasChildren { parent: String, count: usize, children: Vec<String> },

    #[error("Cannot delete fork '{parent}' — {count} baseline(s) reference it. Delete the baselines first or pick a different fork.")]
    HasBaselines { parent: String, count: u32 },

    #[error("Cannot delete fork '{parent}' while an active replay context exists. Stop the replay proxy first.")]
    HasActiveReplayContext { parent: String },

    /// Wrapped underlying I/O / DB failure. Maps to HTTP 500.
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
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

    /// Get all steps visible on `timeline_id`, including inherited steps
    /// from the parent up to the fork point. When the same `step_number`
    /// exists on both the parent (inherited) and on the timeline itself
    /// (owned, e.g. via `upsert_step_on_timeline_and_cascade` after a
    /// promote-and-mutate edit), the **owned** row wins — the inherited
    /// row is omitted so the dashboard's step picker doesn't show two
    /// rows at the same step_number masking the user's edit.
    pub fn get_full_timeline_steps(&self, timeline_id: &str, session_id: &str) -> Result<Vec<Step>> {
        let timelines = self.store.get_timelines(session_id)?;
        let timeline = timelines.iter().find(|t| t.id == timeline_id)
            .context("Timeline not found")?;

        if let (Some(parent_id), Some(fork_at)) = (&timeline.parent_timeline_id, timeline.fork_at_step) {
            let parent_steps = self.store.get_steps(parent_id)?;
            let own_steps = self.store.get_steps(timeline_id)?;

            // Insert parent (inherited) first, then own — HashMap::insert
            // returning the previous value gives us "owned overrides
            // inherited at the same step_number" for free. Pre-size the
            // map to avoid rehash on long sessions (review #162 S3).
            let cap = (fork_at as usize).saturating_add(own_steps.len());
            let mut by_step_number: HashMap<u32, Step> = HashMap::with_capacity(cap);
            for s in parent_steps.into_iter().filter(|s| s.step_number <= fork_at) {
                by_step_number.insert(s.step_number, s);
            }
            for s in own_steps {
                by_step_number.insert(s.step_number, s);
            }

            let mut combined: Vec<Step> = by_step_number.into_values().collect();
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

    /// Delete a fork and every step/span/replay-context/score/step-counter
    /// that belongs to it. Enforces these invariants up front and refuses
    /// the delete rather than silently destroying data (issue #143):
    ///
    /// * The timeline must exist and belong to the given session
    ///   → [`DeleteForkError::NotFound`]
    /// * It must not be the root (`parent_timeline_id` is `None`)
    ///   → [`DeleteForkError::IsRoot`]
    /// * It must have no child forks — users must delete descendants first
    ///   → [`DeleteForkError::HasChildren`]
    /// * No baseline may reference it as `source_timeline_id` — deleting a
    ///   baselined fork would silently invalidate saved regression tests
    ///   → [`DeleteForkError::HasBaselines`]
    /// * No active replay context may reference it — deleting mid-replay
    ///   would FK-violate the proxy's next `create_step`
    ///   → [`DeleteForkError::HasActiveReplayContext`]
    ///
    /// The check-then-delete sequence is safe by virtue of the caller's
    /// `Arc<Mutex<Store>>` — only one delete runs at a time. The wrapped
    /// SQLite transaction covers atomicity of the cascade itself, not the
    /// invariant check.
    pub fn delete_fork(&self, session_id: &str, timeline_id: &str) -> Result<(), DeleteForkError> {
        let timelines = self.store.get_timelines(session_id)?;

        let target = timelines.iter()
            .find(|t| t.id == timeline_id)
            .ok_or_else(|| DeleteForkError::NotFound(timeline_id.to_string(), session_id.to_string()))?;

        if target.parent_timeline_id.is_none() {
            return Err(DeleteForkError::IsRoot);
        }

        let children: Vec<&Timeline> = timelines.iter()
            .filter(|t| t.parent_timeline_id.as_deref() == Some(timeline_id))
            .collect();
        if !children.is_empty() {
            return Err(DeleteForkError::HasChildren {
                parent: truncate_label(&target.label),
                count: children.len(),
                children: children.iter()
                    .map(|t| format!("'{}'", truncate_label(&t.label)))
                    .collect(),
            });
        }

        let baseline_refs = self.store.count_baselines_referencing_timeline(timeline_id)?;
        if baseline_refs > 0 {
            return Err(DeleteForkError::HasBaselines {
                parent: truncate_label(&target.label),
                count: baseline_refs,
            });
        }

        let active_contexts = self.store.count_active_replay_contexts_for_timeline(timeline_id)?;
        if active_contexts > 0 {
            return Err(DeleteForkError::HasActiveReplayContext {
                parent: truncate_label(&target.label),
            });
        }

        let deleted = self.store.delete_timeline(timeline_id)?;
        if !deleted {
            // Another caller raced us — the existence check above passed but
            // the row is now gone. Surface the mismatch rather than silently
            // returning Ok. (In practice the store lock serializes deletes,
            // but make the invariant explicit.)
            return Err(DeleteForkError::Internal(anyhow::anyhow!(
                "Timeline {} was concurrently removed", short(timeline_id),
            )));
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
        // Step 0.3 (Phase 0 follow-up): envelope-aware unwrap before
        // preview extraction. Without this, fork-diff views would surface
        // {status, headers, body} wrapper text in the response_preview
        // field for v0.13+ proxy-recorded steps. Pre-migration format=0
        // round-trips unchanged.
        let response_preview = match self.store.read_step_response_body(step) {
            Some(body) => match String::from_utf8(body) {
                Ok(json_str) => {
                    let parsed = serde_json::from_str::<serde_json::Value>(&json_str).ok();
                    let derived = parsed.as_ref().and_then(|val| {
                        if let Some(choices) = val.get("choices").and_then(|c| c.as_array())
                            && let Some(msg) = choices.first()
                                .and_then(|c| c.get("message"))
                                .and_then(|m| m.get("content"))
                                .and_then(|c| c.as_str())
                        {
                            return Some(msg.chars().take(150).collect());
                        }
                        if let Some(content) = val.get("content").and_then(|c| c.as_array())
                            && let Some(text) = content.first()
                                .and_then(|b| b.get("text"))
                                .and_then(|t| t.as_str())
                        {
                            return Some(text.chars().take(150).collect());
                        }
                        None
                    });
                    derived.unwrap_or_else(|| json_str.chars().take(150).collect::<String>())
                }
                Err(_) => "(binary data)".to_string(),
            },
            None => "(no response)".to_string(),
        };

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
    fn get_full_timeline_steps_dedupes_owned_over_inherited() {
        // Regression: when a fork OWNS a step at the same step_number
        // as an inherited (parent) step — e.g. after a promote-and-mutate
        // PATCH /steps/{id}/edit?target_timeline_id=fork — the union view
        // must return ONE row (the owned one), not both. Without this
        // the dashboard's step picker shows two #N entries on the fork
        // and the inherited one masks the user's edit (visible bug in
        // dev1 with session ray-agent-30053072 on 2026-04-29).
        let (_tmp, store) = setup();
        let (sid, root_tid) = seed_session_with_steps(&store, 3);

        let engine = ReplayEngine::new(&store);
        let fork = engine.fork(&sid, &root_tid, 2, "dedup-test").unwrap();

        // Manually insert an owned step on the fork at step_number=2
        // (this is what upsert_step_on_timeline_and_cascade does after
        // promote-and-mutate).
        let mut owned = Step::new_llm_call(&fork.id, &sid, 2, "edited-model");
        owned.id = "fork-owned-2".to_string();
        store.create_step(&owned).unwrap();

        let view = engine.get_full_timeline_steps(&fork.id, &sid).unwrap();

        // Two distinct step_numbers visible: 1 (inherited) + 2 (owned).
        // Before the dedup fix this returned 3 rows: 1 inherited, 2
        // inherited from main, and 2 owned by the fork.
        assert_eq!(view.len(), 2, "expected 2 rows, got {:?}",
            view.iter().map(|s| (s.step_number, &s.timeline_id, &s.id)).collect::<Vec<_>>());

        let at_two: Vec<&Step> = view.iter().filter(|s| s.step_number == 2).collect();
        assert_eq!(at_two.len(), 1, "expected exactly one row at step #2");
        assert_eq!(at_two[0].timeline_id, fork.id,
            "the surviving row must be the OWNED one (timeline=fork), not the inherited one");
        assert_eq!(at_two[0].id, "fork-owned-2");
        assert_eq!(at_two[0].model, "edited-model");

        // Step #1 is still inherited from main, untouched by the dedup.
        let at_one: Vec<&Step> = view.iter().filter(|s| s.step_number == 1).collect();
        assert_eq!(at_one.len(), 1);
        assert_eq!(at_one[0].timeline_id, root_tid);
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
        assert!(matches!(err, DeleteForkError::IsRoot), "got: {err:?}");
    }

    #[test]
    fn delete_fork_refuses_when_children_exist() {
        let (_tmp, store) = setup();
        let (sid, root) = seed_session_with_steps(&store, 3);
        let engine = ReplayEngine::new(&store);
        let parent_fork = engine.fork(&sid, &root, 2, "parent-fork").unwrap();
        let step = Step::new_llm_call(&parent_fork.id, &sid, 3, "gpt-4o");
        store.create_step(&step).unwrap();
        let _child = engine.fork(&sid, &parent_fork.id, 2, "child-fork").unwrap();

        let err = engine.delete_fork(&sid, &parent_fork.id).unwrap_err();
        match &err {
            DeleteForkError::HasChildren { parent, count, children } => {
                assert_eq!(parent, "parent-fork");
                assert_eq!(*count, 1);
                assert!(children.iter().any(|c| c.contains("child-fork")), "got: {children:?}");
            }
            other => panic!("expected HasChildren, got {other:?}"),
        }
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
        assert!(matches!(err, DeleteForkError::HasBaselines { count: 1, .. }), "got: {err:?}");

        // Fork still present.
        let timelines = store.get_timelines(&sid).unwrap();
        assert!(timelines.iter().any(|t| t.id == fork.id));
    }

    #[test]
    fn delete_fork_refuses_when_an_active_replay_context_exists() {
        // santa review Important-6 on PR #146: a mid-flight proxy would
        // FK-violate on its next create_step if we deleted the timeline
        // out from under it.
        let (_tmp, store) = setup();
        let (sid, root) = seed_session_with_steps(&store, 3);
        let engine = ReplayEngine::new(&store);
        let fork = engine.fork(&sid, &root, 2, "in-use").unwrap();
        let ctx_id = "test-replay-ctx";
        store.create_replay_context(ctx_id, &sid, &fork.id, 2).unwrap();

        let err = engine.delete_fork(&sid, &fork.id).unwrap_err();
        assert!(matches!(err, DeleteForkError::HasActiveReplayContext { .. }), "got: {err:?}");

        // Releasing the context unblocks the delete.
        store.delete_replay_context(ctx_id).unwrap();
        engine.delete_fork(&sid, &fork.id).unwrap();
    }

    #[test]
    fn delete_fork_errors_when_timeline_id_does_not_exist() {
        let (_tmp, store) = setup();
        let (sid, _tid) = seed_session_with_steps(&store, 3);
        let engine = ReplayEngine::new(&store);
        let err = engine.delete_fork(&sid, "nonexistent-id").unwrap_err();
        assert!(matches!(err, DeleteForkError::NotFound(_, _)), "got: {err:?}");
    }

    #[test]
    fn delete_fork_full_cascade_clears_all_dependent_tables() {
        // santa review suggestion: assert every dependent table is empty
        // after a delete, not just `steps`.
        let (_tmp, store) = setup();
        let (sid, root) = seed_session_with_steps(&store, 3);
        let engine = ReplayEngine::new(&store);
        let fork = engine.fork(&sid, &root, 2, "full-cascade").unwrap();

        // Seed every dependent row type.
        let step = Step::new_llm_call(&fork.id, &sid, 3, "gpt-4o");
        store.create_step(&step).unwrap();
        let span = rewind_store::Span::new(
            &sid, &fork.id, rewind_store::SpanType::Tool, "a-span",
        );
        store.create_span(&span).unwrap();
        let ctx_id = "cascade-ctx";
        store.create_replay_context(ctx_id, &sid, &fork.id, 2).unwrap();
        // step_counters entry — created lazily by `next_step_number`.
        let _ = store.next_step_number(&sid, &fork.id).unwrap();

        // Release the replay context so delete isn't blocked.
        store.delete_replay_context(ctx_id).unwrap();

        engine.delete_fork(&sid, &fork.id).unwrap();

        // Every dependent table is empty for this timeline id.
        assert!(store.get_steps(&fork.id).unwrap().is_empty(), "steps not cleared");
        assert!(store.get_spans_by_timeline(&fork.id).unwrap().is_empty(), "spans not cleared");
        assert_eq!(store.count_active_replay_contexts_for_timeline(&fork.id).unwrap(), 0);
        // step_counters PK is (session_id, timeline_id). Verify the row is gone
        // via a direct count query.
        let sc_count = store.count_step_counters_for_timeline_in_session(&sid, &fork.id).unwrap();
        assert_eq!(sc_count, 0, "step_counters row should be gone");
    }

    #[test]
    fn short_id_does_not_panic_on_multibyte_input() {
        // Mirrors santa review Important-2 on PR #145 — truncation in error
        // messages must be char-boundary safe.
        let weird = "π_🦀_timeline_id";
        let s = short(weird);
        assert!(s.chars().count() <= 8);
        assert!(weird.starts_with(&s));
    }

    #[test]
    fn truncate_label_caps_long_user_labels_and_marks_truncation() {
        let long_label = "x".repeat(200);
        let truncated = truncate_label(&long_label);
        // 64 chars + the truncation marker '…'.
        assert_eq!(truncated.chars().count(), 65);
        assert!(truncated.ends_with('…'));
    }
}
