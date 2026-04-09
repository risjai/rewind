use serde::{Deserialize, Serialize};

/// Configurable tolerances for assertion checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tolerance {
    /// Allow step count to differ by this many steps (default: 0)
    pub step_count_delta: u32,
    /// Token tolerance as a fraction (default: 0.20 = 20%)
    pub token_percentage: f64,
    /// Whether model changes are warnings vs failures (default: fail)
    pub model_change_is_warning: bool,
    /// Whether extra steps in the new session are warnings vs failures (default: warn)
    pub extra_steps_is_warning: bool,
}

impl Default for Tolerance {
    fn default() -> Self {
        Tolerance {
            step_count_delta: 0,
            token_percentage: 0.20,
            model_change_is_warning: false,
            extra_steps_is_warning: true,
        }
    }
}

impl Tolerance {
    /// Create with a specific token tolerance percentage (0-100 → fraction)
    pub fn with_token_pct(mut self, pct: u32) -> Self {
        self.token_percentage = pct as f64 / 100.0;
        self
    }

    /// Check whether a value is within tolerance of the expected value.
    pub fn tokens_within(&self, expected: u64, actual: u64) -> bool {
        if expected == 0 {
            return actual == 0;
        }
        let diff = (actual as f64 - expected as f64).abs();
        diff / expected as f64 <= self.token_percentage
    }
}
