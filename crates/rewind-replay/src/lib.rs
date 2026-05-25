use anyhow::{bail, Result};
use rewind_store::{Span, Step, Store, Timeline};
use std::collections::{HashMap, HashSet};

/// Maximum supported depth of the timeline ancestry chain. Any session
/// that nests forks 64 levels deep is almost certainly a cycle the FK
/// graph let through, so refuse rather than spin.
const MAX_ANCESTRY_DEPTH: usize = 64;

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

    /// Walk the timeline's ancestry from `timeline_id` up to the root,
    /// returning a vector of `(timeline_id, visible_upper_bound)` pairs
    /// in *child-first* order (the leaf is index 0; the root, if
    /// reachable, is the last entry).
    ///
    /// `visible_upper_bound` is the cumulative `min(fork_at_step)` of
    /// every fork-edge traversed *below* the current node. For the leaf
    /// it is `u32::MAX` (the leaf can see all of its own owned steps).
    /// For a parent reached via `child.fork_at_step = K`, it is `K`. For
    /// a grandparent reached via `child.fork_at_step = J` and
    /// `parent.fork_at_step = K`, it is `min(J, K)` — i.e. each
    /// ancestor's contribution is clamped to the narrowest fork
    /// boundary on the path.
    ///
    /// Cycle defense: a `HashSet` of visited ids rejects any node that
    /// reappears. The ancestry chain depth is capped at
    /// [`MAX_ANCESTRY_DEPTH`] as a final guard against pathological
    /// graphs.
    fn ancestry_chain(
        timelines: &[Timeline],
        timeline_id: &str,
    ) -> Result<Vec<(String, u32)>> {
        let mut chain: Vec<(String, u32)> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut cursor: Option<&Timeline> = timelines.iter().find(|t| t.id == timeline_id);
        let mut clamp: u32 = u32::MAX;

        while let Some(t) = cursor {
            if !visited.insert(t.id.clone()) {
                bail!("Cycle detected in timeline ancestry at {}", short(&t.id));
            }
            if chain.len() >= MAX_ANCESTRY_DEPTH {
                bail!(
                    "Timeline ancestry exceeds depth limit {} at {}",
                    MAX_ANCESTRY_DEPTH, short(&t.id),
                );
            }
            chain.push((t.id.clone(), clamp));

            match (t.parent_timeline_id.as_deref(), t.fork_at_step) {
                (Some(parent_id), Some(fork_at)) => {
                    clamp = clamp.min(fork_at);
                    cursor = timelines.iter().find(|p| p.id == parent_id);
                }
                _ => break,
            }
        }
        Ok(chain)
    }

    /// Get all steps visible on `timeline_id`, including inherited steps
    /// from every ancestor up to the fork point. When the same
    /// `step_number` exists on multiple levels of the chain, the
    /// **lowest** (closest-to-leaf) row wins — so an owned edit on a
    /// fork shadows the inherited one. The parent's own owned edits in
    /// turn shadow the grandparent's, etc.
    pub fn get_full_timeline_steps(&self, timeline_id: &str, session_id: &str) -> Result<Vec<Step>> {
        let timelines = self.store.get_timelines(session_id)?;
        if !timelines.iter().any(|t| t.id == timeline_id) {
            return Err(anyhow::anyhow!("Timeline not found"));
        }

        let chain = Self::ancestry_chain(&timelines, timeline_id)?;

        // Walk root-first so that closer-to-leaf overrides land last
        // and win the HashMap insert. Each ancestor's contribution is
        // clamped by the cumulative min(fork_at_step) recorded in the
        // chain.
        let mut by_step_number: HashMap<u32, Step> = HashMap::new();
        for (tid, clamp) in chain.iter().rev() {
            let steps = self.store.get_steps(tid)?;
            for s in steps {
                if s.step_number <= *clamp {
                    by_step_number.insert(s.step_number, s);
                }
            }
        }

        let mut combined: Vec<Step> = by_step_number.into_values().collect();
        combined.sort_by_key(|s| s.step_number);
        Ok(combined)
    }

    /// Get all spans visible on `timeline_id`, including inherited
    /// spans from every ancestor. A span on an ancestor is included
    /// iff every step that references it has `step_number <=` that
    /// ancestor's contribution clamp.
    pub fn get_full_timeline_spans(&self, timeline_id: &str, session_id: &str) -> Result<Vec<Span>> {
        let timelines = self.store.get_timelines(session_id)?;
        if !timelines.iter().any(|t| t.id == timeline_id) {
            return Err(anyhow::anyhow!("Timeline not found"));
        }

        let chain = Self::ancestry_chain(&timelines, timeline_id)?;
        let mut combined: Vec<Span> = Vec::new();

        for (tid, clamp) in &chain {
            let spans = self.store.get_spans_by_timeline(tid)?;
            // Steps lookup only needed when filtering an ancestor — the
            // leaf (clamp == u32::MAX) admits every span unconditionally.
            if *clamp == u32::MAX {
                combined.extend(spans);
                continue;
            }
            let steps = self.store.get_steps(tid)?;
            for span in spans {
                let span_steps: Vec<&Step> = steps.iter()
                    .filter(|s| s.span_id.as_deref() == Some(&span.id))
                    .collect();
                let visible = span_steps.is_empty()
                    || span_steps.iter().all(|s| s.step_number <= *clamp);
                if visible {
                    combined.push(span);
                }
            }
        }

        combined.sort_by_key(|a| a.started_at);
        Ok(combined)
    }

    /// Create a fork branching at `at_step`. Seeds `step_counters` to
    /// `at_step` so the runner's next recorded step is `at_step + 1`,
    /// chronologically after the inherited prefix (`fork_seeds_step_counter…`
    /// regression test has the rationale).
    pub fn fork(&self, session_id: &str, source_timeline_id: &str, at_step: u32, label: &str) -> Result<Timeline> {
        let steps = self.get_full_timeline_steps(source_timeline_id, session_id)?;
        let total = u32::try_from(steps.len()).unwrap_or(u32::MAX);
        if at_step == 0 || at_step > total {
            bail!("Invalid fork step {}. Session has {} steps (use 1-{}).", at_step, steps.len(), steps.len());
        }

        let fork = Timeline::new_fork(session_id, source_timeline_id, at_step, label);
        self.store.create_timeline_with_seeded_counter(&fork, at_step)?;
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
    fn get_full_timeline_steps_walks_grandparent_inheritance() {
        // Regression: previously the union view only walked ONE parent
        // level. A 3-level chain (root -> mid-fork -> leaf-fork) caused
        // the leaf to lose visibility of inherited steps that originated
        // on the grandparent root timeline. Visible bug on dev1 with
        // session ray-agent-7bea73fa (2026-05-25): a replay timeline
        // forked from an edited-fork forked from main rendered as
        // 1 step in the dashboard instead of all 4 inherited.
        let (_tmp, store) = setup();
        let (sid, root_tid) = seed_session_with_steps(&store, 4);

        let engine = ReplayEngine::new(&store);
        // mid-fork branches from root at step 4 — inherits steps 1..=4.
        let mid = engine.fork(&sid, &root_tid, 4, "mid").unwrap();
        // leaf-fork branches from mid at step 4 — inherits the same prefix.
        let leaf = engine.fork(&sid, &mid.id, 4, "leaf").unwrap();

        let view = engine.get_full_timeline_steps(&leaf.id, &sid).unwrap();
        assert_eq!(
            view.len(), 4,
            "leaf must see all 4 grandparent steps; got {:?}",
            view.iter().map(|s| (s.step_number, s.timeline_id.clone())).collect::<Vec<_>>()
        );
        let nums: Vec<u32> = view.iter().map(|s| s.step_number).collect();
        assert_eq!(nums, vec![1, 2, 3, 4]);
        // All four rows trace back to the root timeline.
        assert!(view.iter().all(|s| s.timeline_id == root_tid));
    }

    #[test]
    fn get_full_timeline_steps_min_clamps_when_mid_fork_branches_earlier() {
        // The cumulative min-clamp invariant: when mid forks at 4 but
        // leaf forks at 2 from mid, leaf must only see steps 1..=2 of
        // root (clamped by leaf's fork_at_step), not 1..=4.
        let (_tmp, store) = setup();
        let (sid, root_tid) = seed_session_with_steps(&store, 4);

        let engine = ReplayEngine::new(&store);
        let mid = engine.fork(&sid, &root_tid, 4, "mid").unwrap();
        let leaf = engine.fork(&sid, &mid.id, 2, "leaf-clamped").unwrap();

        let view = engine.get_full_timeline_steps(&leaf.id, &sid).unwrap();
        let nums: Vec<u32> = view.iter().map(|s| s.step_number).collect();
        assert_eq!(
            nums, vec![1, 2],
            "leaf forked at 2 must only see steps 1..=2, not the full 1..=4 mid-fork inheritance"
        );
    }

    #[test]
    fn get_full_timeline_steps_min_clamps_when_leaf_forks_later_than_mid() {
        // Symmetric clamp: when mid forks at 2 (inheriting 1..=2 from
        // root) and leaf forks at 4 from mid, leaf still cannot see
        // beyond step 2 from root — even if it asks for step 4 — because
        // mid's own fork boundary is the upper bound on what root
        // contributes to leaf.
        let (_tmp, store) = setup();
        let (sid, root_tid) = seed_session_with_steps(&store, 4);

        let engine = ReplayEngine::new(&store);
        let mid = engine.fork(&sid, &root_tid, 2, "mid-narrow").unwrap();
        // mid owns no steps beyond 2; create steps 3..=4 on mid so leaf has something at those.
        for i in 3..=4 {
            let step = Step::new_llm_call(&mid.id, &sid, i, "gpt-4o");
            store.create_step(&step).unwrap();
        }
        let leaf = engine.fork(&sid, &mid.id, 4, "leaf-wide").unwrap();

        let view = engine.get_full_timeline_steps(&leaf.id, &sid).unwrap();
        // Steps 1..=2 come from root, 3..=4 from mid. Leaf sees 4 total.
        assert_eq!(view.len(), 4);
        let from_root = view.iter().filter(|s| s.timeline_id == root_tid).count();
        let from_mid = view.iter().filter(|s| s.timeline_id == mid.id).count();
        assert_eq!(from_root, 2, "only 1..=2 should come from root (mid's fork boundary)");
        assert_eq!(from_mid, 2, "3..=4 should come from mid (its own steps)");
    }

    #[test]
    fn get_full_timeline_spans_walks_grandparent_inheritance() {
        // Symmetric to the steps test above: spans must also flow
        // through every ancestor level, not just one.
        let (_tmp, store) = setup();
        let (sid, root_tid) = seed_session_with_steps(&store, 4);

        // Add a span to root.
        let root_span = rewind_store::Span::new(
            &sid, &root_tid, rewind_store::SpanType::Tool, "root-span",
        );
        store.create_span(&root_span).unwrap();

        let engine = ReplayEngine::new(&store);
        let mid = engine.fork(&sid, &root_tid, 4, "mid").unwrap();
        let leaf = engine.fork(&sid, &mid.id, 4, "leaf").unwrap();

        let spans = engine.get_full_timeline_spans(&leaf.id, &sid).unwrap();
        assert!(
            spans.iter().any(|s| s.id == root_span.id),
            "leaf must inherit the root-level span; got {:?}",
            spans.iter().map(|s| s.id.clone()).collect::<Vec<_>>()
        );
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

    #[test]
    fn fork_seeds_step_counter_so_replay_steps_continue_after_inherited_prefix() {
        // Regression: previously `engine.fork()` created the timeline
        // row but didn't seed step_counters, so the runner's first
        // recorded step on the fork landed at step_number=1. That
        // collided with the inherited prefix (1..=fork_at_step) and
        // — combined with the owned-over-inherited dedup —
        // shadowed the original turn-1..N steps with the agent's
        // *new* turn-(N+1) work. Visible bug on dev1 session
        // ray-agent-a18ac577 (2026-05-03): replay against an
        // edited-fork at step 6 produced owned steps 1..5 + an
        // inherited step 6, which sorted to put the user's edited
        // question AFTER the agent's response. Backwards.
        //
        // After the fix, fork(at_step=N) seeds the counter to N so
        // the runner's first step is N+1. Sort order matches
        // chronology again.
        let (_tmp, store) = setup();
        let (sid, tid) = seed_session_with_steps(&store, 6);

        let engine = ReplayEngine::new(&store);
        let fork = engine.fork(&sid, &tid, 6, "replay-fork").unwrap();

        // First runner-recorded step on the fork: should be 7, not 1.
        let next1 = store.next_step_number(&sid, &fork.id).unwrap();
        assert_eq!(
            next1, 7,
            "first recorded step on a fork@6 must be #7 to continue \
             after the inherited prefix (got #{next1})"
        );
        let next2 = store.next_step_number(&sid, &fork.id).unwrap();
        assert_eq!(next2, 8);
        let next3 = store.next_step_number(&sid, &fork.id).unwrap();
        assert_eq!(next3, 9);

        // step_counters row exists with the seeded value (so a
        // subsequent runner that re-attaches sees the right cursor).
        let count = store
            .count_step_counters_for_timeline_in_session(&sid, &fork.id)
            .unwrap();
        assert_eq!(count, 1, "exactly one step_counters row for the fork");
    }

    #[test]
    fn fork_at_step_1_seeds_counter_to_1_so_first_replay_step_is_2() {
        // Edge case: fork at the very first step. Inherited prefix
        // is just step 1; the next recorded step should be 2.
        let (_tmp, store) = setup();
        let (sid, tid) = seed_session_with_steps(&store, 3);
        let engine = ReplayEngine::new(&store);
        let fork = engine.fork(&sid, &tid, 1, "fork-at-1").unwrap();
        let next = store.next_step_number(&sid, &fork.id).unwrap();
        assert_eq!(next, 2);
    }

    #[test]
    fn fork_does_not_disturb_existing_step_counters_on_other_timelines() {
        // Defensive: seeding the new fork's row must not touch the
        // parent's counter. Parent root timeline keeps its existing
        // counter (3 in this fixture, since seed_session_with_steps
        // calls create_step which increments the counter).
        let (_tmp, store) = setup();
        let (sid, tid) = seed_session_with_steps(&store, 3);

        // Sync parent's counter to a known value so the assertion
        // doesn't depend on create_step's internal bookkeeping.
        store.sync_step_counter(&sid, &tid, 3).unwrap();

        let engine = ReplayEngine::new(&store);
        let _fork = engine.fork(&sid, &tid, 2, "side-effect-test").unwrap();

        // Parent's next step is still 4 (counter was 3 -> next = 4).
        let parent_next = store.next_step_number(&sid, &tid).unwrap();
        assert_eq!(parent_next, 4, "fork must not disturb parent counter");
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
