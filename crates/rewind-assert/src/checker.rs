use anyhow::Result;
use chrono::Utc;
use rewind_store::{BaselineStep, Step, Store};
use serde::{Deserialize, Serialize};

use crate::extract::{extract_response_fingerprint, extract_tool_name};
use crate::tolerance::Tolerance;

// ── Result types ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssertionResult {
    pub baseline_id: String,
    pub baseline_name: String,
    pub checked_session_id: String,
    pub checked_timeline_id: String,
    pub checked_at: String,
    pub passed: bool,
    pub summary: AssertionSummary,
    pub step_results: Vec<StepAssertionResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssertionSummary {
    pub total_checks: u32,
    pub passed_checks: u32,
    pub failed_checks: u32,
    pub warnings: u32,
    pub structural_match: bool,
    pub model_match: bool,
    pub status_match: bool,
    pub token_budget_ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepAssertionResult {
    pub step_number: u32,
    pub verdict: StepVerdict,
    pub checks: Vec<CheckResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StepVerdict {
    Pass,
    Warn,
    Fail,
    Missing,
    Extra,
}

impl StepVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            StepVerdict::Pass => "pass",
            StepVerdict::Warn => "warn",
            StepVerdict::Fail => "fail",
            StepVerdict::Missing => "missing",
            StepVerdict::Extra => "extra",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            StepVerdict::Pass => "✓",
            StepVerdict::Warn => "⚠",
            StepVerdict::Fail => "✗",
            StepVerdict::Missing => "∅",
            StepVerdict::Extra => "+",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub check_type: CheckType,
    pub passed: bool,
    pub expected: String,
    pub actual: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CheckType {
    StepType,
    Model,
    Status,
    ToolName,
    TokensIn,
    TokensOut,
    HasError,
    ResponseContent,
}

impl CheckType {
    pub fn as_str(&self) -> &'static str {
        match self {
            CheckType::StepType => "step_type",
            CheckType::Model => "model",
            CheckType::Status => "status",
            CheckType::ToolName => "tool_name",
            CheckType::TokensIn => "tokens_in",
            CheckType::TokensOut => "tokens_out",
            CheckType::HasError => "has_error",
            CheckType::ResponseContent => "response_content",
        }
    }
}

// ── Engine ────────────────────────────────────────────────

pub struct AssertionEngine<'a> {
    store: &'a Store,
    tolerance: Tolerance,
}

impl<'a> AssertionEngine<'a> {
    pub fn new(store: &'a Store, tolerance: Tolerance) -> Self {
        AssertionEngine { store, tolerance }
    }

    /// Check a session's steps against a baseline.
    pub fn check(
        &self,
        baseline_id: &str,
        baseline_name: &str,
        baseline_steps: &[BaselineStep],
        actual_steps: &[Step],
        session_id: &str,
        timeline_id: &str,
    ) -> Result<AssertionResult> {
        let max_steps = baseline_steps.len().max(actual_steps.len());
        let mut step_results = Vec::new();
        let mut total_checks: u32 = 0;
        let mut passed_checks: u32 = 0;
        let mut failed_checks: u32 = 0;
        let mut warnings: u32 = 0;
        let mut all_models_match = true;
        let mut all_status_match = true;
        let mut all_tokens_ok = true;

        for i in 0..max_steps {
            let step_number = (i + 1) as u32;
            let expected = baseline_steps.iter().find(|s| s.step_number == step_number);
            let actual = actual_steps.iter().find(|s| s.step_number == step_number);

            match (expected, actual) {
                (Some(_), None) => {
                    // Step in baseline but not in actual → Missing (Fail)
                    step_results.push(StepAssertionResult {
                        step_number,
                        verdict: StepVerdict::Missing,
                        checks: vec![],
                    });
                    failed_checks += 1;
                    total_checks += 1;
                }
                (None, Some(_)) => {
                    // Step in actual but not in baseline → Extra
                    let verdict = if self.tolerance.extra_steps_is_warning {
                        warnings += 1;
                        StepVerdict::Extra
                    } else {
                        failed_checks += 1;
                        StepVerdict::Extra
                    };
                    step_results.push(StepAssertionResult {
                        step_number,
                        verdict,
                        checks: vec![],
                    });
                    total_checks += 1;
                }
                (Some(exp), Some(act)) => {
                    let mut checks = Vec::new();

                    // 1. StepType check
                    let type_match = exp.step_type == act.step_type.as_str();
                    checks.push(CheckResult {
                        check_type: CheckType::StepType,
                        passed: type_match,
                        expected: exp.step_type.clone(),
                        actual: act.step_type.as_str().to_string(),
                        message: if type_match {
                            "match".to_string()
                        } else {
                            format!("expected {}, got {}", exp.step_type, act.step_type.as_str())
                        },
                    });

                    // 2. Model check
                    let model_match = exp.expected_model == act.model || exp.expected_model.is_empty();
                    if !model_match {
                        all_models_match = false;
                    }
                    let model_is_warning = self.tolerance.model_change_is_warning;
                    checks.push(CheckResult {
                        check_type: CheckType::Model,
                        passed: model_match || model_is_warning,
                        expected: exp.expected_model.clone(),
                        actual: act.model.clone(),
                        message: if model_match {
                            "match".to_string()
                        } else if model_is_warning {
                            format!("model changed: {} → {} (warning)", exp.expected_model, act.model)
                        } else {
                            format!("model changed: {} → {}", exp.expected_model, act.model)
                        },
                    });

                    // 3. Status check (new errors = fail)
                    let status_ok = !(exp.expected_status != "error" && act.status.as_str() == "error");
                    if !status_ok {
                        all_status_match = false;
                    }
                    checks.push(CheckResult {
                        check_type: CheckType::Status,
                        passed: status_ok,
                        expected: exp.expected_status.clone(),
                        actual: act.status.as_str().to_string(),
                        message: if status_ok {
                            "ok".to_string()
                        } else {
                            "NEW ERROR in step".to_string()
                        },
                    });

                    // 4. HasError check
                    let error_ok = exp.has_error || act.error.is_none();
                    checks.push(CheckResult {
                        check_type: CheckType::HasError,
                        passed: error_ok,
                        expected: format!("has_error={}", exp.has_error),
                        actual: format!("has_error={}", act.error.is_some()),
                        message: if error_ok {
                            "ok".to_string()
                        } else {
                            format!(
                                "NEW ERROR: {}",
                                act.error.as_deref().unwrap_or("unknown")
                            )
                        },
                    });

                    // 5. ToolName check (only for tool-call-producing steps)
                    if exp.tool_name.is_some() {
                        let actual_tool = extract_tool_name(self.store, act);
                        let tool_match = exp.tool_name == actual_tool;
                        checks.push(CheckResult {
                            check_type: CheckType::ToolName,
                            passed: tool_match,
                            expected: exp.tool_name.clone().unwrap_or_default(),
                            actual: actual_tool.unwrap_or_else(|| "none".to_string()),
                            message: if tool_match {
                                "match".to_string()
                            } else {
                                "tool call changed".to_string()
                            },
                        });
                    }

                    // 6. TokensIn check (warn on mismatch, not fail)
                    let tokens_in_ok = self.tolerance.tokens_within(exp.tokens_in, act.tokens_in);
                    if !tokens_in_ok {
                        all_tokens_ok = false;
                    }
                    let pct_in = if exp.tokens_in > 0 {
                        (act.tokens_in as f64 - exp.tokens_in as f64) / exp.tokens_in as f64 * 100.0
                    } else {
                        0.0
                    };
                    checks.push(CheckResult {
                        check_type: CheckType::TokensIn,
                        passed: true, // tokens are always warnings, not failures
                        expected: exp.tokens_in.to_string(),
                        actual: act.tokens_in.to_string(),
                        message: if tokens_in_ok {
                            format!("{}→{} ({:+.1}%)", exp.tokens_in, act.tokens_in, pct_in)
                        } else {
                            format!(
                                "{}→{} ({:+.1}%) exceeds ±{:.0}% tolerance",
                                exp.tokens_in,
                                act.tokens_in,
                                pct_in,
                                self.tolerance.token_percentage * 100.0
                            )
                        },
                    });

                    // 7. TokensOut check
                    let tokens_out_ok = self.tolerance.tokens_within(exp.tokens_out, act.tokens_out);
                    if !tokens_out_ok {
                        all_tokens_ok = false;
                    }
                    let pct_out = if exp.tokens_out > 0 {
                        (act.tokens_out as f64 - exp.tokens_out as f64) / exp.tokens_out as f64 * 100.0
                    } else {
                        0.0
                    };
                    checks.push(CheckResult {
                        check_type: CheckType::TokensOut,
                        passed: true, // tokens are always warnings
                        expected: exp.tokens_out.to_string(),
                        actual: act.tokens_out.to_string(),
                        message: if tokens_out_ok {
                            format!("{}→{} ({:+.1}%)", exp.tokens_out, act.tokens_out, pct_out)
                        } else {
                            format!(
                                "{}→{} ({:+.1}%) exceeds ±{:.0}% tolerance",
                                exp.tokens_out,
                                act.tokens_out,
                                pct_out,
                                self.tolerance.token_percentage * 100.0
                            )
                        },
                    });

                    // 8. ResponseContent check (shallow fingerprint)
                    let exp_fp = extract_response_fingerprint(self.store, &exp.response_blob);
                    let act_fp = extract_response_fingerprint(self.store, &act.response_blob);
                    let length_ratio = if exp_fp.content_length > 0 {
                        act_fp.content_length as f64 / exp_fp.content_length as f64
                    } else {
                        1.0
                    };
                    let tool_names_match = exp_fp.tool_call_names == act_fp.tool_call_names;
                    let content_ok = (0.5..=2.0).contains(&length_ratio) && tool_names_match;
                    checks.push(CheckResult {
                        check_type: CheckType::ResponseContent,
                        passed: true, // content is always a warning
                        expected: format!("len={} tools={:?}", exp_fp.content_length, exp_fp.tool_call_names),
                        actual: format!("len={} tools={:?}", act_fp.content_length, act_fp.tool_call_names),
                        message: if content_ok {
                            "structural match".to_string()
                        } else if !tool_names_match {
                            "tool call names differ".to_string()
                        } else {
                            format!("response length ratio: {:.2}x", length_ratio)
                        },
                    });

                    // Compute step verdict
                    let has_fail = checks.iter().any(|c| {
                        !c.passed
                            && matches!(
                                c.check_type,
                                CheckType::StepType
                                    | CheckType::Status
                                    | CheckType::HasError
                                    | CheckType::ToolName
                                    | CheckType::Model
                            )
                    });
                    let has_warn = !tokens_in_ok || !tokens_out_ok || !content_ok;

                    let verdict = if has_fail {
                        StepVerdict::Fail
                    } else if has_warn {
                        StepVerdict::Warn
                    } else {
                        StepVerdict::Pass
                    };

                    for check in &checks {
                        total_checks += 1;
                        if check.passed {
                            passed_checks += 1;
                        } else {
                            failed_checks += 1;
                        }
                    }
                    if has_warn && !has_fail {
                        warnings += 1;
                    }

                    step_results.push(StepAssertionResult {
                        step_number,
                        verdict,
                        checks,
                    });
                }
                (None, None) => unreachable!(),
            }
        }

        let structural_match = baseline_steps.len() == actual_steps.len();
        let extra_is_failure = !self.tolerance.extra_steps_is_warning;
        let passed = !step_results.iter().any(|s| match s.verdict {
            StepVerdict::Fail | StepVerdict::Missing => true,
            StepVerdict::Extra => extra_is_failure,
            _ => false,
        });

        Ok(AssertionResult {
            baseline_id: baseline_id.to_string(),
            baseline_name: baseline_name.to_string(),
            checked_session_id: session_id.to_string(),
            checked_timeline_id: timeline_id.to_string(),
            checked_at: Utc::now().to_rfc3339(),
            passed,
            summary: AssertionSummary {
                total_checks,
                passed_checks,
                failed_checks,
                warnings,
                structural_match,
                model_match: all_models_match,
                status_match: all_status_match,
                token_budget_ok: all_tokens_ok,
            },
            step_results,
        })
    }
}
