use serde::{Deserialize, Serialize};

/// Score result from an evaluator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreResult {
    pub score: f64,
    pub passed: bool,
    pub reasoning: String,
}

/// Comparison between two experiments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentComparison {
    pub left_id: String,
    pub left_name: String,
    pub left_avg_score: f64,
    pub left_pass_rate: f64,
    pub right_id: String,
    pub right_name: String,
    pub right_avg_score: f64,
    pub right_pass_rate: f64,
    pub overall_delta: f64,
    pub regressions: u32,
    pub improvements: u32,
    pub unchanged: u32,
    pub example_diffs: Vec<ExampleDiff>,
}

/// Per-example comparison between two experiments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExampleDiff {
    pub ordinal: u32,
    pub input_preview: String,
    pub left_score: f64,
    pub right_score: f64,
    pub delta: f64,
    pub direction: DiffDirection,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DiffDirection {
    Regression,
    Improvement,
    Unchanged,
}
