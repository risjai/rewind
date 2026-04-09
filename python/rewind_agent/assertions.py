"""
Rewind Assertions — query baselines and run regression checks from Python.

This module reads from the same SQLite database that the Rust CLI writes to.
Baseline creation is done via `rewind assert baseline` CLI or MCP tool.
Checks can be run from Python for CI integration.

Usage:
    from rewind_agent import Assertions

    assertions = Assertions()
    result = assertions.check("booking-happy-path", "latest")
    assert result.passed, f"Regression: {result.failed_checks} checks failed"
"""

from dataclasses import dataclass, field
from typing import Optional

from .store import Store


@dataclass
class Baseline:
    id: str
    name: str
    source_session_id: str
    source_timeline_id: str
    step_count: int
    total_tokens: int
    created_at: str
    description: str = ""


@dataclass
class BaselineStep:
    step_number: int
    step_type: str
    expected_status: str
    expected_model: str
    tokens_in: int
    tokens_out: int
    tool_name: Optional[str]
    has_error: bool


@dataclass
class CheckResult:
    check_type: str
    passed: bool
    expected: str
    actual: str
    message: str


@dataclass
class StepResult:
    step_number: int
    verdict: str  # "pass", "warn", "fail", "missing", "extra"
    checks: list = field(default_factory=list)


@dataclass
class AssertionResult:
    baseline_name: str
    session_id: str
    passed: bool
    total_checks: int
    passed_checks: int
    failed_checks: int
    warnings: int
    step_results: list = field(default_factory=list)


class Assertions:
    """Query baselines and run assertion checks against recorded sessions."""

    def __init__(self, store: Store = None):
        if store is None:
            store = Store()
        self._store = store
        self._conn = store._conn

    def list_baselines(self) -> list:
        """List all baselines."""
        rows = self._conn.execute(
            "SELECT id, name, source_session_id, source_timeline_id, "
            "step_count, total_tokens, created_at, description "
            "FROM baselines ORDER BY created_at DESC"
        ).fetchall()
        return [
            Baseline(
                id=r[0], name=r[1], source_session_id=r[2],
                source_timeline_id=r[3], step_count=r[4],
                total_tokens=r[5], created_at=r[6], description=r[7],
            )
            for r in rows
        ]

    def get_baseline(self, name: str) -> Optional[Baseline]:
        """Get a baseline by name."""
        row = self._conn.execute(
            "SELECT id, name, source_session_id, source_timeline_id, "
            "step_count, total_tokens, created_at, description "
            "FROM baselines WHERE name = ?",
            (name,),
        ).fetchone()
        if row is None:
            return None
        return Baseline(
            id=row[0], name=row[1], source_session_id=row[2],
            source_timeline_id=row[3], step_count=row[4],
            total_tokens=row[5], created_at=row[6], description=row[7],
        )

    def get_baseline_steps(self, baseline_id: str) -> list:
        """Get expected steps for a baseline."""
        rows = self._conn.execute(
            "SELECT step_number, step_type, expected_status, expected_model, "
            "tokens_in, tokens_out, tool_name, has_error "
            "FROM baseline_steps WHERE baseline_id = ? ORDER BY step_number",
            (baseline_id,),
        ).fetchall()
        return [
            BaselineStep(
                step_number=r[0], step_type=r[1], expected_status=r[2],
                expected_model=r[3], tokens_in=r[4], tokens_out=r[5],
                tool_name=r[6], has_error=bool(r[7]),
            )
            for r in rows
        ]

    def check(
        self,
        baseline_name: str,
        session_id: str = "latest",
        token_tolerance: float = 0.20,
    ) -> AssertionResult:
        """
        Check a session against a baseline.

        For CI usage:
            result = assertions.check("booking-happy-path", "latest")
            assert result.passed, f"Regression: {result.failed_checks} checks failed"
        """
        baseline = self.get_baseline(baseline_name)
        if baseline is None:
            raise ValueError(f"Baseline '{baseline_name}' not found")

        expected_steps = self.get_baseline_steps(baseline.id)

        # Resolve session
        if session_id == "latest":
            row = self._conn.execute(
                "SELECT id FROM sessions ORDER BY created_at DESC LIMIT 1"
            ).fetchone()
            if row is None:
                raise ValueError("No sessions found")
            session_id = row[0]

        # Get root timeline
        row = self._conn.execute(
            "SELECT id FROM timelines WHERE session_id = ? AND parent_timeline_id IS NULL",
            (session_id,),
        ).fetchone()
        if row is None:
            raise ValueError(f"No root timeline for session {session_id}")
        timeline_id = row[0]

        # Get actual steps
        actual_rows = self._conn.execute(
            "SELECT step_number, step_type, status, model, tokens_in, tokens_out, error "
            "FROM steps WHERE timeline_id = ? ORDER BY step_number",
            (timeline_id,),
        ).fetchall()

        expected_map = {s.step_number: s for s in expected_steps}
        actual_map = {r[0]: r for r in actual_rows}
        max_step = max(
            max(expected_map.keys(), default=0),
            max(actual_map.keys(), default=0),
        )

        step_results = []
        total_checks = 0
        passed_checks = 0
        failed_checks = 0
        warnings = 0

        for step_num in range(1, max_step + 1):
            exp = expected_map.get(step_num)
            act = actual_map.get(step_num)

            if exp and not act:
                step_results.append(StepResult(step_num, "missing"))
                failed_checks += 1
                total_checks += 1
                continue

            if act and not exp:
                step_results.append(StepResult(step_num, "extra"))
                warnings += 1
                total_checks += 1
                continue

            # Both exist — run checks
            checks = []

            # StepType
            type_match = exp.step_type == act[1]
            checks.append(CheckResult("step_type", type_match, exp.step_type, act[1],
                                      "match" if type_match else f"expected {exp.step_type}, got {act[1]}"))

            # Model
            model_match = exp.expected_model == act[3] or exp.expected_model == ""
            checks.append(CheckResult("model", model_match, exp.expected_model, act[3],
                                      "match" if model_match else f"model changed: {exp.expected_model} → {act[3]}"))

            # Status (new errors = fail)
            status_ok = not (exp.expected_status != "error" and act[2] == "error")
            checks.append(CheckResult("status", status_ok, exp.expected_status, act[2],
                                      "ok" if status_ok else "NEW ERROR"))

            # HasError
            has_new_error = not exp.has_error and act[6] is not None
            checks.append(CheckResult("has_error", not has_new_error,
                                      str(exp.has_error), str(act[6] is not None),
                                      "ok" if not has_new_error else f"NEW ERROR: {act[6]}"))

            # TokensIn
            tokens_in_ok = _within_tolerance(exp.tokens_in, act[4], token_tolerance)
            checks.append(CheckResult("tokens_in", True, str(exp.tokens_in), str(act[4]),
                                      "ok" if tokens_in_ok else "token drift"))

            # TokensOut
            tokens_out_ok = _within_tolerance(exp.tokens_out, act[5], token_tolerance)
            checks.append(CheckResult("tokens_out", True, str(exp.tokens_out), str(act[5]),
                                      "ok" if tokens_out_ok else "token drift"))

            has_fail = any(not c.passed for c in checks)
            has_warn = not tokens_in_ok or not tokens_out_ok

            if has_fail:
                verdict = "fail"
            elif has_warn:
                verdict = "warn"
            else:
                verdict = "pass"

            for c in checks:
                total_checks += 1
                if c.passed:
                    passed_checks += 1
                else:
                    failed_checks += 1
            if has_warn and not has_fail:
                warnings += 1

            step_results.append(StepResult(step_num, verdict, checks))

        overall_passed = not any(
            s.verdict in ("fail", "missing") for s in step_results
        )

        return AssertionResult(
            baseline_name=baseline_name,
            session_id=session_id,
            passed=overall_passed,
            total_checks=total_checks,
            passed_checks=passed_checks,
            failed_checks=failed_checks,
            warnings=warnings,
            step_results=step_results,
        )


def _within_tolerance(expected: int, actual: int, tolerance: float) -> bool:
    if expected == 0:
        return actual == 0
    diff = abs(actual - expected)
    return diff / expected <= tolerance
