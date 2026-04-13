"""Tests for the LLM-as-judge evaluator module."""

import json
from unittest import mock

import pytest

from rewind_agent.llm_judge import (
    CRITERIA_PRESETS,
    _render_template,
    _is_retryable,
    run_judge,
    main,
)
from rewind_agent.evaluation import (
    _resolve_evaluator,
    _BUILTIN_EVALUATORS,
)


# ══════════════════════════════════════════════════════════════
# Criteria Presets
# ══════════════════════════════════════════════════════════════


class TestCriteriaPresets:
    def test_all_five_presets_exist(self):
        expected = {"correctness", "coherence", "relevance", "safety", "task_completion"}
        assert set(CRITERIA_PRESETS.keys()) == expected

    def test_each_preset_has_required_fields(self):
        for name, preset in CRITERIA_PRESETS.items():
            assert "template" in preset, f"{name} missing template"
            assert "choice_scores" in preset, f"{name} missing choice_scores"
            assert "requires_expected" in preset, f"{name} missing requires_expected"
            assert isinstance(preset["choice_scores"], dict), f"{name} choice_scores not dict"
            assert len(preset["choice_scores"]) > 0, f"{name} has empty choice_scores"

    def test_correctness_requires_expected(self):
        assert CRITERIA_PRESETS["correctness"]["requires_expected"] is True

    def test_safety_does_not_require_expected(self):
        assert CRITERIA_PRESETS["safety"]["requires_expected"] is False

    def test_coherence_does_not_require_expected(self):
        assert CRITERIA_PRESETS["coherence"]["requires_expected"] is False

    def test_correctness_choice_scores(self):
        scores = CRITERIA_PRESETS["correctness"]["choice_scores"]
        assert scores["C"] == 1.0  # identical = perfect
        assert scores["D"] == 0.0  # disagrees = zero
        assert scores["E"] == 1.0  # immaterial diff = perfect

    def test_safety_choice_scores(self):
        scores = CRITERIA_PRESETS["safety"]["choice_scores"]
        assert scores["Safe"] == 1.0
        assert scores["Unsafe"] == 0.0
        assert scores["Unsure"] == 0.5

    def test_all_scores_between_0_and_1(self):
        for name, preset in CRITERIA_PRESETS.items():
            for choice, score in preset["choice_scores"].items():
                assert 0.0 <= score <= 1.0, f"{name}[{choice}] = {score} out of range"


# ══════════════════════════════════════════════════════════════
# Template Rendering
# ══════════════════════════════════════════════════════════════


class TestTemplateRendering:
    def test_replaces_all_placeholders(self):
        template = "Input: {{input}} Output: {{output}} Expected: {{expected}}"
        result = _render_template(template, "my input", "my output", "my expected")
        assert result == "Input: my input Output: my output Expected: my expected"

    def test_handles_dict_values(self):
        template = "Data: {{output}}"
        result = _render_template(template, None, {"key": "value"}, None)
        assert '"key": "value"' in result

    def test_handles_none_as_empty_string(self):
        template = "Expected: {{expected}}"
        result = _render_template(template, None, None, None)
        assert result == "Expected: "

    def test_handles_list_values(self):
        template = "Input: {{input}}"
        result = _render_template(template, [1, 2, 3], None, None)
        assert "1" in result and "2" in result and "3" in result


# ══════════════════════════════════════════════════════════════
# Expected Validation
# ══════════════════════════════════════════════════════════════


class TestExpectedValidation:
    def test_correctness_without_expected_raises(self):
        """Correctness requires expected; should raise ValueError."""
        with pytest.raises(ValueError, match="requires an expected value"):
            run_judge("question", "answer", None, criteria="correctness")

    def test_correctness_with_empty_string_expected_raises(self):
        with pytest.raises(ValueError, match="requires an expected value"):
            run_judge("question", "answer", "", criteria="correctness")

    def test_safety_without_expected_does_not_raise(self):
        """Safety doesn't need expected — should NOT raise ValueError.
        It will fail on the API call, but that's mocked separately."""
        # We mock the client so it doesn't actually call the API
        mock_response = _make_mock_response("Safe", "Content is safe")
        with mock.patch("rewind_agent.llm_judge._get_openai_client") as mock_client:
            mock_client.return_value.chat.completions.create.return_value = mock_response
            result = run_judge("query", "safe content", None, criteria="safety")
            assert result["score"] == 1.0

    def test_coherence_without_expected_does_not_raise(self):
        mock_response = _make_mock_response("Y", "Well structured")
        with mock.patch("rewind_agent.llm_judge._get_openai_client") as mock_client:
            mock_client.return_value.chat.completions.create.return_value = mock_response
            result = run_judge("query", "coherent text", None, criteria="coherence")
            assert result["score"] == 1.0


# ══════════════════════════════════════════════════════════════
# LLM Call (mocked)
# ══════════════════════════════════════════════════════════════


def _make_mock_response(choice: str, reasons: str):
    """Build a mock OpenAI response with a function call."""
    tool_call = mock.MagicMock()
    tool_call.function.arguments = json.dumps({"choice": choice, "reasons": reasons})

    message = mock.MagicMock()
    message.tool_calls = [tool_call]
    message.content = None

    choice_obj = mock.MagicMock()
    choice_obj.message = message

    response = mock.MagicMock()
    response.choices = [choice_obj]
    return response


class TestRunJudge:
    def test_correctness_scores_c_as_1(self):
        """Choice C (identical) should score 1.0."""
        mock_response = _make_mock_response("C", "Answers are identical")
        with mock.patch("rewind_agent.llm_judge._get_openai_client") as mock_client:
            mock_client.return_value.chat.completions.create.return_value = mock_response
            result = run_judge("question", "answer", "answer", criteria="correctness")

        assert result["score"] == 1.0
        assert result["passed"] is True
        assert "[C]" in result["reasoning"]

    def test_correctness_scores_d_as_0(self):
        """Choice D (disagrees) should score 0.0."""
        mock_response = _make_mock_response("D", "Factual disagreement")
        with mock.patch("rewind_agent.llm_judge._get_openai_client") as mock_client:
            mock_client.return_value.chat.completions.create.return_value = mock_response
            result = run_judge("q", "wrong", "right", criteria="correctness")

        assert result["score"] == 0.0
        assert result["passed"] is False

    def test_safety_safe_scores_1(self):
        mock_response = _make_mock_response("Safe", "No harmful content")
        with mock.patch("rewind_agent.llm_judge._get_openai_client") as mock_client:
            mock_client.return_value.chat.completions.create.return_value = mock_response
            result = run_judge(None, "safe content", None, criteria="safety")

        assert result["score"] == 1.0
        assert result["passed"] is True

    def test_safety_unsafe_scores_0(self):
        mock_response = _make_mock_response("Unsafe", "Contains harmful content")
        with mock.patch("rewind_agent.llm_judge._get_openai_client") as mock_client:
            mock_client.return_value.chat.completions.create.return_value = mock_response
            result = run_judge(None, "harmful content", None, criteria="safety")

        assert result["score"] == 0.0
        assert result["passed"] is False

    def test_task_completion_partial_scores_half(self):
        mock_response = _make_mock_response("Partial", "Task half done")
        with mock.patch("rewind_agent.llm_judge._get_openai_client") as mock_client:
            mock_client.return_value.chat.completions.create.return_value = mock_response
            result = run_judge("do task", "partial result", None, criteria="task_completion")

        assert result["score"] == 0.5
        assert result["passed"] is True  # 0.5 >= 0.5

    def test_custom_template_and_scores(self):
        """Custom template with custom choice_scores should work."""
        mock_response = _make_mock_response("Good", "Looks good")
        with mock.patch("rewind_agent.llm_judge._get_openai_client") as mock_client:
            mock_client.return_value.chat.completions.create.return_value = mock_response
            result = run_judge(
                "input", "output", None,
                template="Rate this: {{output}}\n(Good) or (Bad)",
                choice_scores={"Good": 1.0, "Bad": 0.0},
            )

        assert result["score"] == 1.0
        assert result["passed"] is True

    def test_unknown_choice_scores_zero(self):
        """If LLM returns an unlisted choice, score should be 0.0."""
        mock_response = _make_mock_response("X", "Unknown choice")
        with mock.patch("rewind_agent.llm_judge._get_openai_client") as mock_client:
            mock_client.return_value.chat.completions.create.return_value = mock_response
            result = run_judge(None, "output", None, criteria="safety")

        assert result["score"] == 0.0
        assert result["passed"] is False

    def test_no_tool_call_in_response(self):
        """If LLM doesn't use function calling, should return score 0."""
        message = mock.MagicMock()
        message.tool_calls = []
        message.content = "I think it's safe"

        choice_obj = mock.MagicMock()
        choice_obj.message = message

        response = mock.MagicMock()
        response.choices = [choice_obj]

        with mock.patch("rewind_agent.llm_judge._get_openai_client") as mock_client:
            mock_client.return_value.chat.completions.create.return_value = response
            result = run_judge(None, "output", None, criteria="safety")

        assert result["score"] == 0.0
        assert "did not use function calling" in result["reasoning"]

    def test_api_error_returns_zero_with_reasoning(self):
        """Non-retryable API error should return score 0 with error message."""
        with mock.patch("rewind_agent.llm_judge._get_openai_client") as mock_client:
            mock_client.return_value.chat.completions.create.side_effect = \
                RuntimeError("Authentication failed")
            result = run_judge(None, "output", None, criteria="safety")

        assert result["score"] == 0.0
        assert result["passed"] is False
        assert "Authentication failed" in result["reasoning"]

    def test_uses_configured_model(self):
        """The model parameter should be passed to the API call."""
        mock_response = _make_mock_response("Y", "Relevant")
        with mock.patch("rewind_agent.llm_judge._get_openai_client") as mock_client:
            mock_client.return_value.chat.completions.create.return_value = mock_response
            run_judge("q", "a", None, criteria="relevance", model="gpt-4o")

            call_kwargs = mock_client.return_value.chat.completions.create.call_args[1]
            assert call_kwargs["model"] == "gpt-4o"

    def test_uses_temperature_zero_by_default(self):
        mock_response = _make_mock_response("Y", "ok")
        with mock.patch("rewind_agent.llm_judge._get_openai_client") as mock_client:
            mock_client.return_value.chat.completions.create.return_value = mock_response
            run_judge("q", "a", None, criteria="coherence")

            call_kwargs = mock_client.return_value.chat.completions.create.call_args[1]
            assert call_kwargs["temperature"] == 0

    def test_criteria_as_custom_text(self):
        """If criteria isn't a preset name, treat it as custom prompt text."""
        mock_response = _make_mock_response("Y", "Custom eval pass")
        with mock.patch("rewind_agent.llm_judge._get_openai_client") as mock_client:
            mock_client.return_value.chat.completions.create.return_value = mock_response
            result = run_judge(
                "input", "output", None,
                criteria="Is this output professional? (Y) Yes (N) No",
            )

        assert result["score"] == 1.0


# ══════════════════════════════════════════════════════════════
# Retry Logic
# ══════════════════════════════════════════════════════════════


class TestRetryLogic:
    def test_is_retryable_rate_limit(self):
        """openai.RateLimitError should be retryable."""
        try:
            import openai
            err = openai.RateLimitError(
                message="Rate limit exceeded",
                response=mock.MagicMock(status_code=429),
                body=None,
            )
            assert _is_retryable(err) is True
        except ImportError:
            pytest.skip("openai not installed")

    def test_is_retryable_server_error(self):
        """openai.APIStatusError with 500 should be retryable."""
        try:
            import openai
            err = openai.InternalServerError(
                message="Internal server error",
                response=mock.MagicMock(status_code=500),
                body=None,
            )
            assert _is_retryable(err) is True
        except ImportError:
            pytest.skip("openai not installed")

    def test_is_not_retryable_auth_error(self):
        """openai.AuthenticationError should NOT be retryable."""
        try:
            import openai
            err = openai.AuthenticationError(
                message="Invalid API key",
                response=mock.MagicMock(status_code=401),
                body=None,
            )
            assert _is_retryable(err) is False
        except ImportError:
            pytest.skip("openai not installed")

    def test_is_not_retryable_generic(self):
        """Generic ValueError should NOT be retryable."""
        assert _is_retryable(ValueError("bad value")) is False

    def test_connection_error_is_retryable(self):
        """ConnectionError should be retryable."""
        assert _is_retryable(ConnectionError("refused")) is True

    def test_retry_then_succeed(self):
        """Rate limit on first call, success on second — should return success score."""
        try:
            import openai
            rate_limit_err = openai.RateLimitError(
                message="Rate limit exceeded",
                response=mock.MagicMock(status_code=429),
                body=None,
            )
        except ImportError:
            pytest.skip("openai not installed")

        mock_response = _make_mock_response("Safe", "Content is safe")
        with mock.patch("rewind_agent.llm_judge._get_openai_client") as mock_client, \
             mock.patch("time.sleep"):  # skip actual sleep
            mock_client.return_value.chat.completions.create.side_effect = [
                rate_limit_err,
                mock_response,
            ]
            result = run_judge(None, "safe content", None, criteria="safety")

        assert result["score"] == 1.0
        assert result["passed"] is True
        # Should have been called twice (first fails, second succeeds)
        assert mock_client.return_value.chat.completions.create.call_count == 2

    def test_string_500_in_message_does_not_trigger_retry(self):
        """An error with '500' in the message but not a status error should NOT retry."""
        err = ValueError("Token limit 500 exceeded")
        assert _is_retryable(err) is False


# ══════════════════════════════════════════════════════════════
# SDK Integration
# ══════════════════════════════════════════════════════════════


class TestSDKIntegration:
    def test_llm_judge_in_builtin_evaluators(self):
        assert "llm_judge" in _BUILTIN_EVALUATORS

    def test_resolve_evaluator_by_string(self):
        resolved = _resolve_evaluator("llm_judge")
        assert callable(resolved)

    def test_llm_judge_evaluator_factory(self):
        from rewind_agent import llm_judge_evaluator
        judge = llm_judge_evaluator(criteria="safety", model="gpt-4o")
        assert callable(judge)
        assert "safety" in judge.__name__


# ══════════════════════════════════════════════════════════════
# Subprocess Entry Point
# ══════════════════════════════════════════════════════════════


class TestSubprocessEntryPoint:
    def test_invalid_json_stdin(self, capsys):
        """Invalid JSON on stdin should output error JSON and exit 1."""
        with mock.patch("sys.stdin") as mock_stdin:
            mock_stdin.read.return_value = "not json"
            with pytest.raises(SystemExit) as exc_info:
                main()
            assert exc_info.value.code == 1

        captured = capsys.readouterr()
        result = json.loads(captured.out)
        assert result["score"] == 0.0
        assert "Invalid stdin JSON" in result["reasoning"]

    def test_valid_stdin_calls_run_judge(self, capsys):
        """Valid JSON payload should call run_judge and output result."""
        payload = {
            "input": "What is 2+2?",
            "output": "4",
            "expected": "4",
            "config": {"criteria": "correctness", "model": "gpt-4o-mini"},
        }

        mock_response = _make_mock_response("C", "Identical")
        with mock.patch("sys.stdin") as mock_stdin, \
             mock.patch("rewind_agent.llm_judge._get_openai_client") as mock_client:
            mock_stdin.read.return_value = json.dumps(payload)
            mock_client.return_value.chat.completions.create.return_value = mock_response
            main()

        captured = capsys.readouterr()
        result = json.loads(captured.out)
        assert result["score"] == 1.0
        assert result["passed"] is True

    def test_value_error_exits_nonzero(self, capsys):
        """ValueError (e.g., missing expected) should exit with code 1."""
        payload = {
            "input": "question",
            "output": "answer",
            "expected": None,
            "config": {"criteria": "correctness"},
        }

        with mock.patch("sys.stdin") as mock_stdin:
            mock_stdin.read.return_value = json.dumps(payload)
            with pytest.raises(SystemExit) as exc_info:
                main()
            assert exc_info.value.code == 1

        captured = capsys.readouterr()
        result = json.loads(captured.out)
        assert result["score"] == 0.0
        assert "config error" in result["reasoning"]
