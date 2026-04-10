use anyhow::{bail, Result};
use rewind_store::Store;
use serde_json::Value;

use crate::models::{DiffDirection, ExampleDiff, ExperimentComparison};

/// Compare two experiments side-by-side.
/// By default, requires experiments to use the same dataset and version.
pub fn compare_experiments(
    store: &Store,
    left_id: &str,
    right_id: &str,
    force: bool,
) -> Result<ExperimentComparison> {
    let left = store
        .get_experiment(left_id)?
        .ok_or_else(|| anyhow::anyhow!("Left experiment '{}' not found", left_id))?;
    let right = store
        .get_experiment(right_id)?
        .ok_or_else(|| anyhow::anyhow!("Right experiment '{}' not found", right_id))?;

    // Enforce same dataset + version unless forced
    if !force && (left.dataset_id != right.dataset_id || left.dataset_version != right.dataset_version) {
        bail!(
            "Experiments use different datasets or versions (left: v{}, right: v{}). Use --force to compare anyway.",
            left.dataset_version,
            right.dataset_version
        );
    }

    // Get results and scores for both
    let left_results = store.get_experiment_results(&left.id)?;
    let right_results = store.get_experiment_results(&right.id)?;

    // Build per-ordinal average scores
    let left_scores = build_ordinal_scores(store, &left_results)?;
    let right_scores = build_ordinal_scores(store, &right_results)?;

    // Build example diffs by matching on ordinal
    let max_ordinal = left_scores
        .keys()
        .chain(right_scores.keys())
        .cloned()
        .max()
        .unwrap_or(0);

    let mut diffs = Vec::new();
    let mut regressions = 0u32;
    let mut improvements = 0u32;
    let mut unchanged = 0u32;

    for ordinal in 1..=max_ordinal {
        let l_score = left_scores.get(&ordinal).cloned().unwrap_or(0.0);
        let r_score = right_scores.get(&ordinal).cloned().unwrap_or(0.0);
        let delta = r_score - l_score;
        let direction = if delta < -0.01 {
            regressions += 1;
            DiffDirection::Regression
        } else if delta > 0.01 {
            improvements += 1;
            DiffDirection::Improvement
        } else {
            unchanged += 1;
            DiffDirection::Unchanged
        };

        // Get input preview from left results
        let input_preview = left_results
            .iter()
            .find(|r| r.ordinal == ordinal)
            .and_then(|r| {
                // Try to get input from the example's blob
                let examples = store.get_dataset_examples(&left.dataset_id).ok()?;
                let ex = examples.iter().find(|e| e.id == r.example_id)?;
                let input: Value = store.blobs.get_json(&ex.input_blob).ok()?;
                let preview = serde_json::to_string(&input).unwrap_or_default();
                Some(truncate_str(&preview, 100))
            })
            .unwrap_or_default();

        diffs.push(ExampleDiff {
            ordinal,
            input_preview,
            left_score: l_score,
            right_score: r_score,
            delta,
            direction,
        });
    }

    let overall_delta = right.avg_score.unwrap_or(0.0) - left.avg_score.unwrap_or(0.0);

    Ok(ExperimentComparison {
        left_id: left.id,
        left_name: left.name,
        left_avg_score: left.avg_score.unwrap_or(0.0),
        left_pass_rate: left.pass_rate.unwrap_or(0.0),
        right_id: right.id,
        right_name: right.name,
        right_avg_score: right.avg_score.unwrap_or(0.0),
        right_pass_rate: right.pass_rate.unwrap_or(0.0),
        overall_delta,
        regressions,
        improvements,
        unchanged,
        example_diffs: diffs,
    })
}

/// Build a map of ordinal → average score across all evaluators.
fn build_ordinal_scores(
    store: &Store,
    results: &[rewind_store::ExperimentResult],
) -> Result<std::collections::HashMap<u32, f64>> {
    let mut map = std::collections::HashMap::new();
    for result in results {
        let scores = store.get_experiment_scores(&result.id)?;
        if scores.is_empty() {
            map.insert(result.ordinal, 0.0);
        } else {
            let avg = scores.iter().map(|s| s.score).sum::<f64>() / scores.len() as f64;
            map.insert(result.ordinal, avg);
        }
    }
    Ok(map)
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}
