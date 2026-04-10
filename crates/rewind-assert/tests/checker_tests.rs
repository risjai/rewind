use rewind_assert::{AssertionEngine, Tolerance};
use rewind_assert::checker::{CheckType, StepVerdict};
use rewind_store::{BaselineStep, Session, Step, StepStatus, StepType, Store, Timeline};
use tempfile::TempDir;

/// Create a temporary Store for testing.
fn setup() -> (TempDir, Store) {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    (tmp, store)
}

/// Create a seed session with timeline so blobs can be stored.
fn seed_session(store: &Store) -> (String, String) {
    let session = Session::new("test-session");
    let timeline = Timeline::new_root(&session.id);
    store.create_session(&session).unwrap();
    store.create_timeline(&timeline).unwrap();
    (session.id, timeline.id)
}

/// Helper to create a Step with all fields specified.
fn make_step(
    timeline_id: &str,
    session_id: &str,
    step_number: u32,
    model: &str,
    status: StepStatus,
    step_type: StepType,
    tokens_in: u64,
    tokens_out: u64,
    response_blob: &str,
    error: Option<String>,
) -> Step {
    let mut step = Step::new_llm_call(timeline_id, session_id, step_number, model);
    step.status = status;
    step.step_type = step_type;
    step.tokens_in = tokens_in;
    step.tokens_out = tokens_out;
    step.response_blob = response_blob.to_string();
    step.error = error;
    step
}

/// Helper to create a BaselineStep.
fn make_baseline_step(
    baseline_id: &str,
    step_number: u32,
    step_type: &str,
    expected_status: &str,
    expected_model: &str,
    tokens_in: u64,
    tokens_out: u64,
    tool_name: Option<String>,
    response_blob: &str,
    has_error: bool,
) -> BaselineStep {
    BaselineStep {
        id: uuid::Uuid::new_v4().to_string(),
        baseline_id: baseline_id.to_string(),
        step_number,
        step_type: step_type.to_string(),
        expected_status: expected_status.to_string(),
        expected_model: expected_model.to_string(),
        tokens_in,
        tokens_out,
        tool_name,
        response_blob: response_blob.to_string(),
        request_blob: String::new(),
        has_error,
    }
}

// ── Tolerance Tests ──────────────────────────────────────────────

#[test]
fn tolerance_tokens_within_exact_match() {
    let t = Tolerance::default(); // 20%
    assert!(t.tokens_within(100, 100));
}

#[test]
fn tolerance_tokens_within_at_boundary() {
    let t = Tolerance::default(); // 20%
    assert!(t.tokens_within(100, 120)); // exactly 20% over
    assert!(t.tokens_within(100, 80));  // exactly 20% under
}

#[test]
fn tolerance_tokens_within_just_outside() {
    let t = Tolerance::default(); // 20%
    assert!(!t.tokens_within(100, 121)); // 21% over
    assert!(!t.tokens_within(100, 79));  // 21% under
}

#[test]
fn tolerance_tokens_within_zero_expected_zero_actual() {
    let t = Tolerance::default();
    assert!(t.tokens_within(0, 0));
}

#[test]
fn tolerance_tokens_within_zero_expected_nonzero_actual() {
    let t = Tolerance::default();
    assert!(!t.tokens_within(0, 1));
}

#[test]
fn tolerance_tokens_within_nonzero_expected_zero_actual() {
    let t = Tolerance::default(); // 20%
    // 100% difference — should fail
    assert!(!t.tokens_within(100, 0));
}

#[test]
fn tolerance_with_custom_pct() {
    let t = Tolerance::default().with_token_pct(50); // 50%
    assert!(t.tokens_within(100, 150)); // exactly 50%
    assert!(!t.tokens_within(100, 151)); // 51%
}

#[test]
fn tolerance_with_zero_pct() {
    let t = Tolerance::default().with_token_pct(0); // 0% — must be exact
    assert!(t.tokens_within(100, 100));
    assert!(!t.tokens_within(100, 101));
}

// ── Checker: Identical Steps ─────────────────────────────────────

#[test]
fn check_identical_steps_all_pass() {
    let (_tmp, store) = setup();
    let (sid, tid) = seed_session(&store);

    // Store a response blob for fingerprinting
    let resp_blob = store.blobs.put_json(&serde_json::json!({"choices": [{"message": {"content": "hello"}}]})).unwrap();

    let baseline_steps = vec![
        make_baseline_step("b1", 1, "llm_call", "success", "gpt-4o", 100, 50, None, &resp_blob, false),
    ];
    let actual_steps = vec![
        make_step(&tid, &sid, 1, "gpt-4o", StepStatus::Success, StepType::LlmCall, 100, 50, &resp_blob, None),
    ];

    let engine = AssertionEngine::new(&store, Tolerance::default());
    let result = engine.check("b1", "test-baseline", &baseline_steps, &actual_steps, &sid, &tid).unwrap();

    assert!(result.passed);
    assert_eq!(result.step_results.len(), 1);
    assert_eq!(result.step_results[0].verdict, StepVerdict::Pass);
    assert!(result.summary.structural_match);
    assert!(result.summary.model_match);
    assert!(result.summary.status_match);
}

// ── Checker: Missing Step ────────────────────────────────────────

#[test]
fn check_missing_step_is_fail() {
    let (_tmp, store) = setup();
    let (sid, tid) = seed_session(&store);
    let resp_blob = store.blobs.put_json(&serde_json::json!({})).unwrap();

    let baseline_steps = vec![
        make_baseline_step("b1", 1, "llm_call", "success", "gpt-4o", 100, 50, None, &resp_blob, false),
        make_baseline_step("b1", 2, "llm_call", "success", "gpt-4o", 100, 50, None, &resp_blob, false),
    ];
    let actual_steps = vec![
        make_step(&tid, &sid, 1, "gpt-4o", StepStatus::Success, StepType::LlmCall, 100, 50, &resp_blob, None),
        // Step 2 is missing
    ];

    let engine = AssertionEngine::new(&store, Tolerance::default());
    let result = engine.check("b1", "test", &baseline_steps, &actual_steps, &sid, &tid).unwrap();

    assert!(!result.passed);
    assert_eq!(result.step_results[1].verdict, StepVerdict::Missing);
    assert!(!result.summary.structural_match);
}

// ── Checker: Extra Step ──────────────────────────────────────────

#[test]
fn check_extra_step_is_warning_by_default() {
    let (_tmp, store) = setup();
    let (sid, tid) = seed_session(&store);
    let resp_blob = store.blobs.put_json(&serde_json::json!({})).unwrap();

    let baseline_steps = vec![
        make_baseline_step("b1", 1, "llm_call", "success", "gpt-4o", 100, 50, None, &resp_blob, false),
    ];
    let actual_steps = vec![
        make_step(&tid, &sid, 1, "gpt-4o", StepStatus::Success, StepType::LlmCall, 100, 50, &resp_blob, None),
        make_step(&tid, &sid, 2, "gpt-4o", StepStatus::Success, StepType::LlmCall, 100, 50, &resp_blob, None),
    ];

    let tolerance = Tolerance {
        extra_steps_is_warning: true,
        ..Default::default()
    };
    let engine = AssertionEngine::new(&store, tolerance);
    let result = engine.check("b1", "test", &baseline_steps, &actual_steps, &sid, &tid).unwrap();

    // Extra step should be a warning, not a failure — so overall passes
    assert!(result.passed);
    assert_eq!(result.step_results[1].verdict, StepVerdict::Extra);
    assert!(result.summary.warnings > 0);
}

#[test]
fn check_extra_step_is_fail_when_configured() {
    let (_tmp, store) = setup();
    let (sid, tid) = seed_session(&store);
    let resp_blob = store.blobs.put_json(&serde_json::json!({})).unwrap();

    let baseline_steps = vec![
        make_baseline_step("b1", 1, "llm_call", "success", "gpt-4o", 100, 50, None, &resp_blob, false),
    ];
    let actual_steps = vec![
        make_step(&tid, &sid, 1, "gpt-4o", StepStatus::Success, StepType::LlmCall, 100, 50, &resp_blob, None),
        make_step(&tid, &sid, 2, "gpt-4o", StepStatus::Success, StepType::LlmCall, 100, 50, &resp_blob, None),
    ];

    let tolerance = Tolerance {
        extra_steps_is_warning: false,
        ..Default::default()
    };
    let engine = AssertionEngine::new(&store, tolerance);
    let result = engine.check("b1", "test", &baseline_steps, &actual_steps, &sid, &tid).unwrap();

    assert_eq!(result.step_results[1].verdict, StepVerdict::Extra);
    assert_eq!(result.summary.failed_checks, 1);

    // Extra step configured as failure should cause the overall assertion to fail
    assert!(!result.passed);
}

// ── Checker: Model Change ────────────────────────────────────────

#[test]
fn check_model_change_is_fail_by_default() {
    let (_tmp, store) = setup();
    let (sid, tid) = seed_session(&store);
    let resp_blob = store.blobs.put_json(&serde_json::json!({"choices": [{"message": {"content": "x"}}]})).unwrap();

    let baseline_steps = vec![
        make_baseline_step("b1", 1, "llm_call", "success", "gpt-4o", 100, 50, None, &resp_blob, false),
    ];
    let actual_steps = vec![
        make_step(&tid, &sid, 1, "gpt-4o-mini", StepStatus::Success, StepType::LlmCall, 100, 50, &resp_blob, None),
    ];

    let engine = AssertionEngine::new(&store, Tolerance::default());
    let result = engine.check("b1", "test", &baseline_steps, &actual_steps, &sid, &tid).unwrap();

    assert!(!result.passed);
    assert_eq!(result.step_results[0].verdict, StepVerdict::Fail);
    assert!(!result.summary.model_match);
}

#[test]
fn check_model_change_is_warning_when_configured() {
    let (_tmp, store) = setup();
    let (sid, tid) = seed_session(&store);
    let resp_blob = store.blobs.put_json(&serde_json::json!({"choices": [{"message": {"content": "x"}}]})).unwrap();

    let baseline_steps = vec![
        make_baseline_step("b1", 1, "llm_call", "success", "gpt-4o", 100, 50, None, &resp_blob, false),
    ];
    let actual_steps = vec![
        make_step(&tid, &sid, 1, "gpt-4o-mini", StepStatus::Success, StepType::LlmCall, 100, 50, &resp_blob, None),
    ];

    let tolerance = Tolerance {
        model_change_is_warning: true,
        ..Default::default()
    };
    let engine = AssertionEngine::new(&store, tolerance);
    let result = engine.check("b1", "test", &baseline_steps, &actual_steps, &sid, &tid).unwrap();

    // Model change is a warning, not failure — overall passes
    assert!(result.passed);
    assert!(!result.summary.model_match);
}

#[test]
fn check_empty_expected_model_always_passes() {
    let (_tmp, store) = setup();
    let (sid, tid) = seed_session(&store);
    let resp_blob = store.blobs.put_json(&serde_json::json!({})).unwrap();

    let baseline_steps = vec![
        make_baseline_step("b1", 1, "llm_call", "success", "", 100, 50, None, &resp_blob, false),
    ];
    let actual_steps = vec![
        make_step(&tid, &sid, 1, "any-model-at-all", StepStatus::Success, StepType::LlmCall, 100, 50, &resp_blob, None),
    ];

    let engine = AssertionEngine::new(&store, Tolerance::default());
    let result = engine.check("b1", "test", &baseline_steps, &actual_steps, &sid, &tid).unwrap();

    assert!(result.passed);
    assert!(result.summary.model_match); // empty expected = any model accepted
}

// ── Checker: Status Change ───────────────────────────────────────

#[test]
fn check_new_error_is_fail() {
    let (_tmp, store) = setup();
    let (sid, tid) = seed_session(&store);
    let resp_blob = store.blobs.put_json(&serde_json::json!({})).unwrap();

    let baseline_steps = vec![
        make_baseline_step("b1", 1, "llm_call", "success", "gpt-4o", 100, 50, None, &resp_blob, false),
    ];
    let actual_steps = vec![
        make_step(&tid, &sid, 1, "gpt-4o", StepStatus::Error, StepType::LlmCall, 100, 50, &resp_blob, Some("timeout".to_string())),
    ];

    let engine = AssertionEngine::new(&store, Tolerance::default());
    let result = engine.check("b1", "test", &baseline_steps, &actual_steps, &sid, &tid).unwrap();

    assert!(!result.passed);
    assert!(!result.summary.status_match);
}

#[test]
fn check_previously_errored_step_still_errors_is_ok() {
    let (_tmp, store) = setup();
    let (sid, tid) = seed_session(&store);
    let resp_blob = store.blobs.put_json(&serde_json::json!({})).unwrap();

    let baseline_steps = vec![
        make_baseline_step("b1", 1, "llm_call", "error", "gpt-4o", 100, 50, None, &resp_blob, true),
    ];
    let actual_steps = vec![
        make_step(&tid, &sid, 1, "gpt-4o", StepStatus::Error, StepType::LlmCall, 100, 50, &resp_blob, Some("still broken".to_string())),
    ];

    let engine = AssertionEngine::new(&store, Tolerance::default());
    let result = engine.check("b1", "test", &baseline_steps, &actual_steps, &sid, &tid).unwrap();

    // Both errored — no NEW error, so this passes
    assert!(result.passed);
}

#[test]
fn check_error_to_success_is_ok() {
    // When baseline had an error but actual is now success — that's an improvement, not a regression
    let (_tmp, store) = setup();
    let (sid, tid) = seed_session(&store);
    let resp_blob = store.blobs.put_json(&serde_json::json!({})).unwrap();

    let baseline_steps = vec![
        make_baseline_step("b1", 1, "llm_call", "error", "gpt-4o", 100, 50, None, &resp_blob, true),
    ];
    let actual_steps = vec![
        make_step(&tid, &sid, 1, "gpt-4o", StepStatus::Success, StepType::LlmCall, 100, 50, &resp_blob, None),
    ];

    let engine = AssertionEngine::new(&store, Tolerance::default());
    let result = engine.check("b1", "test", &baseline_steps, &actual_steps, &sid, &tid).unwrap();

    assert!(result.passed);
}

// ── Checker: Token Tolerance ─────────────────────────────────────

#[test]
fn check_token_outside_tolerance_warns() {
    let (_tmp, store) = setup();
    let (sid, tid) = seed_session(&store);
    let resp_blob = store.blobs.put_json(&serde_json::json!({"choices": [{"message": {"content": "x"}}]})).unwrap();

    let baseline_steps = vec![
        make_baseline_step("b1", 1, "llm_call", "success", "gpt-4o", 100, 50, None, &resp_blob, false),
    ];
    let actual_steps = vec![
        // tokens_in went from 100 to 200 — 100% increase, way outside 20% tolerance
        make_step(&tid, &sid, 1, "gpt-4o", StepStatus::Success, StepType::LlmCall, 200, 50, &resp_blob, None),
    ];

    let engine = AssertionEngine::new(&store, Tolerance::default());
    let result = engine.check("b1", "test", &baseline_steps, &actual_steps, &sid, &tid).unwrap();

    // Token changes are warnings, not failures — overall still passes
    assert!(result.passed);
    assert_eq!(result.step_results[0].verdict, StepVerdict::Warn);
    assert!(!result.summary.token_budget_ok);
}

// ── Checker: Step Type Mismatch ──────────────────────────────────

#[test]
fn check_step_type_mismatch_is_fail() {
    let (_tmp, store) = setup();
    let (sid, tid) = seed_session(&store);
    let resp_blob = store.blobs.put_json(&serde_json::json!({})).unwrap();

    let baseline_steps = vec![
        make_baseline_step("b1", 1, "llm_call", "success", "gpt-4o", 100, 50, None, &resp_blob, false),
    ];
    let actual_steps = vec![
        make_step(&tid, &sid, 1, "gpt-4o", StepStatus::Success, StepType::ToolCall, 100, 50, &resp_blob, None),
    ];

    let engine = AssertionEngine::new(&store, Tolerance::default());
    let result = engine.check("b1", "test", &baseline_steps, &actual_steps, &sid, &tid).unwrap();

    assert!(!result.passed);
    assert_eq!(result.step_results[0].verdict, StepVerdict::Fail);
}

// ── Checker: Tool Name ───────────────────────────────────────────

#[test]
fn check_tool_name_mismatch_is_fail() {
    let (_tmp, store) = setup();
    let (sid, tid) = seed_session(&store);

    // Store two different responses: one with tool "search", one with tool "calculate"
    let resp_search = store.blobs.put_json(&serde_json::json!({
        "choices": [{"message": {"tool_calls": [{"function": {"name": "search"}}]}}]
    })).unwrap();
    let resp_calc = store.blobs.put_json(&serde_json::json!({
        "choices": [{"message": {"tool_calls": [{"function": {"name": "calculate"}}]}}]
    })).unwrap();

    let baseline_steps = vec![
        make_baseline_step("b1", 1, "llm_call", "success", "gpt-4o", 100, 50, Some("search".to_string()), &resp_search, false),
    ];
    let actual_steps = vec![
        make_step(&tid, &sid, 1, "gpt-4o", StepStatus::Success, StepType::LlmCall, 100, 50, &resp_calc, None),
    ];

    let engine = AssertionEngine::new(&store, Tolerance::default());
    let result = engine.check("b1", "test", &baseline_steps, &actual_steps, &sid, &tid).unwrap();

    assert!(!result.passed);
    // Find the ToolName check
    let tool_check = result.step_results[0].checks.iter().find(|c| c.check_type == CheckType::ToolName).unwrap();
    assert!(!tool_check.passed);
}

// ── Checker: Overall Verdict Logic ───────────────────────────────

#[test]
fn check_overall_pass_requires_no_fail_no_missing() {
    let (_tmp, store) = setup();
    let (sid, tid) = seed_session(&store);
    let resp_blob = store.blobs.put_json(&serde_json::json!({})).unwrap();

    // 3 baseline steps, 3 actual steps, all matching
    let baseline_steps: Vec<_> = (1..=3).map(|i|
        make_baseline_step("b1", i, "llm_call", "success", "gpt-4o", 100, 50, None, &resp_blob, false)
    ).collect();
    let actual_steps: Vec<_> = (1..=3).map(|i|
        make_step(&tid, &sid, i, "gpt-4o", StepStatus::Success, StepType::LlmCall, 100, 50, &resp_blob, None)
    ).collect();

    let engine = AssertionEngine::new(&store, Tolerance::default());
    let result = engine.check("b1", "test", &baseline_steps, &actual_steps, &sid, &tid).unwrap();

    assert!(result.passed);
    assert!(result.summary.structural_match);
    assert_eq!(result.summary.failed_checks, 0);
    assert_eq!(result.step_results.len(), 3);
    assert!(result.step_results.iter().all(|s| s.verdict == StepVerdict::Pass));
}

// ── Checker: Response Fingerprint ────────────────────────────────

#[test]
fn check_response_length_ratio_boundary() {
    let (_tmp, store) = setup();
    let (sid, tid) = seed_session(&store);

    // Create responses of different sizes
    // "content": "x" (short)  vs  "content": "xxxxxxxxxxxxxxxxxx...x" (long)
    let short_resp = store.blobs.put_json(&serde_json::json!({
        "choices": [{"message": {"content": "x"}}]
    })).unwrap();
    let long_resp = store.blobs.put_json(&serde_json::json!({
        "choices": [{"message": {"content": "x".repeat(1000)}}]
    })).unwrap();

    let baseline_steps = vec![
        make_baseline_step("b1", 1, "llm_call", "success", "gpt-4o", 100, 50, None, &short_resp, false),
    ];
    let actual_steps = vec![
        make_step(&tid, &sid, 1, "gpt-4o", StepStatus::Success, StepType::LlmCall, 100, 50, &long_resp, None),
    ];

    let engine = AssertionEngine::new(&store, Tolerance::default());
    let result = engine.check("b1", "test", &baseline_steps, &actual_steps, &sid, &tid).unwrap();

    // Response content fingerprint differences are warnings, not failures
    assert!(result.passed);
    // Should be a Warn verdict due to content length ratio exceeding 2.0x
    assert_eq!(result.step_results[0].verdict, StepVerdict::Warn);
}

// ── Checker: has_error flag ──────────────────────────────────────

#[test]
fn check_new_error_detected_via_has_error_flag() {
    let (_tmp, store) = setup();
    let (sid, tid) = seed_session(&store);
    let resp_blob = store.blobs.put_json(&serde_json::json!({})).unwrap();

    // Baseline says no error
    let baseline_steps = vec![
        make_baseline_step("b1", 1, "llm_call", "success", "gpt-4o", 100, 50, None, &resp_blob, false),
    ];
    // Actual has an error
    let actual_steps = vec![
        make_step(&tid, &sid, 1, "gpt-4o", StepStatus::Success, StepType::LlmCall, 100, 50, &resp_blob, Some("rate limited".to_string())),
    ];

    let engine = AssertionEngine::new(&store, Tolerance::default());
    let result = engine.check("b1", "test", &baseline_steps, &actual_steps, &sid, &tid).unwrap();

    assert!(!result.passed);
    let error_check = result.step_results[0].checks.iter().find(|c| c.check_type == CheckType::HasError).unwrap();
    assert!(!error_check.passed);
}
