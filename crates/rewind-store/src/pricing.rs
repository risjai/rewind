//! Model price table and replay savings calculator.
//!
//! Hardcoded prices for common LLM models. Update quarterly.
//! Used to show cost/time savings after fork-and-execute replays.

use crate::Step;
use serde::Serialize;

/// Per-model pricing: (input_cost_per_million, output_cost_per_million) in USD.
fn model_price(model: &str) -> (f64, f64) {
    let normalized = model.to_lowercase();
    match normalized.as_str() {
        s if s.contains("gpt-4o-mini") => (0.15, 0.60),
        s if s.contains("gpt-4o") => (2.50, 10.00),
        s if s.contains("gpt-4.1-mini") => (0.40, 1.60),
        s if s.contains("gpt-4.1-nano") => (0.10, 0.40),
        s if s.contains("gpt-4.1") => (2.00, 8.00),
        s if s.contains("o1-mini") => (1.10, 4.40),
        s if s.contains("o1") => (15.00, 60.00),
        s if s.contains("claude") && s.contains("opus") => (15.00, 75.00),
        s if s.contains("claude") && s.contains("haiku") => (0.25, 1.25),
        s if s.contains("claude") && s.contains("sonnet") => (3.00, 15.00),
        _ => (1.00, 3.00), // default fallback
    }
}

/// Estimate cost in USD for a single LLM call.
pub fn estimate_cost(model: &str, tokens_in: u64, tokens_out: u64) -> f64 {
    let (price_in, price_out) = model_price(model);
    (tokens_in as f64 * price_in + tokens_out as f64 * price_out) / 1_000_000.0
}

/// Summary of tokens, cost, and time saved by serving cached steps during replay.
#[derive(Debug, Clone, Serialize)]
pub struct ReplaySavings {
    pub steps_total: u32,
    pub steps_cached: u32,
    pub steps_live: u32,
    pub tokens_saved: u64,
    pub cost_saved_usd: f64,
    pub time_saved_ms: u64,
}

/// Compute savings from a fork-and-execute replay.
///
/// `cached_steps` are the parent-timeline steps that were served from cache
/// (steps 1..=fork_at_step). `live_steps` are the new steps recorded after
/// the fork point.
pub fn compute_savings(cached_steps: &[Step], live_steps: &[Step]) -> ReplaySavings {
    let mut tokens_saved: u64 = 0;
    let mut cost_saved: f64 = 0.0;
    let mut time_saved: u64 = 0;

    for step in cached_steps {
        tokens_saved += step.tokens_in + step.tokens_out;
        cost_saved += estimate_cost(&step.model, step.tokens_in, step.tokens_out);
        time_saved += step.duration_ms;
    }

    ReplaySavings {
        steps_total: (cached_steps.len() + live_steps.len()) as u32,
        steps_cached: cached_steps.len() as u32,
        steps_live: live_steps.len() as u32,
        tokens_saved,
        cost_saved_usd: (cost_saved * 100.0).round() / 100.0, // round to cents
        time_saved_ms: time_saved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use crate::{StepStatus, StepType};

    fn make_step(model: &str, tokens_in: u64, tokens_out: u64, duration_ms: u64) -> Step {
        Step {
            id: "test".into(),
            timeline_id: "tl".into(),
            session_id: "sess".into(),
            step_number: 1,
            step_type: StepType::LlmCall,
            status: StepStatus::Success,
            created_at: Utc::now(),
            duration_ms,
            tokens_in,
            tokens_out,
            model: model.into(),
            request_blob: String::new(),
            response_blob: String::new(),
            error: None,
            span_id: None,
            tool_name: None,
        }
    }

    // ── Price lookup tests ───────────────────────────────────────

    #[test]
    fn known_model_gpt4o() {
        let cost = estimate_cost("gpt-4o", 1_000_000, 1_000_000);
        assert!((cost - 12.50).abs() < 0.01); // 2.50 + 10.00
    }

    #[test]
    fn known_model_gpt4o_mini() {
        let cost = estimate_cost("gpt-4o-mini", 1_000_000, 1_000_000);
        assert!((cost - 0.75).abs() < 0.01); // 0.15 + 0.60
    }

    #[test]
    fn known_model_claude_sonnet() {
        let cost = estimate_cost("claude-sonnet-4-20250514", 1_000_000, 1_000_000);
        assert!((cost - 18.00).abs() < 0.01); // 3.00 + 15.00
    }

    #[test]
    fn known_model_claude_haiku() {
        let cost = estimate_cost("claude-haiku-4-5", 1_000_000, 1_000_000);
        assert!((cost - 1.50).abs() < 0.01); // 0.25 + 1.25
    }

    #[test]
    fn known_model_claude_opus() {
        let cost = estimate_cost("claude-opus-4-6", 1_000_000, 1_000_000);
        assert!((cost - 90.00).abs() < 0.01); // 15.00 + 75.00
    }

    #[test]
    fn unknown_model_uses_default() {
        let cost = estimate_cost("some-random-model", 1_000_000, 1_000_000);
        assert!((cost - 4.00).abs() < 0.01); // 1.00 + 3.00
    }

    #[test]
    fn case_insensitive_lookup() {
        let cost = estimate_cost("GPT-4o", 1_000_000, 0);
        assert!((cost - 2.50).abs() < 0.01);
    }

    #[test]
    fn zero_tokens_zero_cost() {
        assert_eq!(estimate_cost("gpt-4o", 0, 0), 0.0);
    }

    // ── compute_savings tests ────────────────────────────────────

    #[test]
    fn savings_from_cached_steps() {
        let cached = vec![
            make_step("gpt-4o", 500, 200, 1500),
            make_step("gpt-4o", 300, 100, 800),
        ];
        let live = vec![make_step("gpt-4o", 400, 150, 1200)];

        let savings = compute_savings(&cached, &live);

        assert_eq!(savings.steps_total, 3);
        assert_eq!(savings.steps_cached, 2);
        assert_eq!(savings.steps_live, 1);
        assert_eq!(savings.tokens_saved, 500 + 200 + 300 + 100); // 1100
        assert_eq!(savings.time_saved_ms, 1500 + 800); // 2300
        assert!(savings.cost_saved_usd > 0.0);
    }

    #[test]
    fn no_cached_steps_no_savings() {
        let savings = compute_savings(&[], &[make_step("gpt-4o", 100, 50, 500)]);

        assert_eq!(savings.steps_cached, 0);
        assert_eq!(savings.tokens_saved, 0);
        assert_eq!(savings.cost_saved_usd, 0.0);
        assert_eq!(savings.time_saved_ms, 0);
    }

    #[test]
    fn mixed_models_cost_correct() {
        let cached = vec![
            make_step("gpt-4o", 1_000_000, 0, 1000),     // $2.50
            make_step("claude-sonnet-4", 1_000_000, 0, 1000), // $3.00
        ];
        let savings = compute_savings(&cached, &[]);

        assert!((savings.cost_saved_usd - 5.50).abs() < 0.01);
    }

    #[test]
    fn cost_rounded_to_cents() {
        let cached = vec![make_step("gpt-4o", 333, 111, 100)];
        let savings = compute_savings(&cached, &[]);

        // Verify rounding: 333*2.50/1M + 111*10.00/1M = 0.001943 → rounds to 0.00
        assert_eq!(savings.cost_saved_usd, (savings.cost_saved_usd * 100.0).round() / 100.0);
    }
}
