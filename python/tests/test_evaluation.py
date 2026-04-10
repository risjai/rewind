"""Tests for the evaluation module — evaluators, datasets, evaluate(), compare()."""

import json
import os
import tempfile
import uuid
from datetime import datetime, timezone
from unittest import mock

import pytest

from rewind_agent import (
    Dataset,
    EvalScore,
    EvalFailedError,
    ExperimentResult,
    ExampleResult,
    ComparisonResult,
    evaluate,
    compare,
    evaluator,
    exact_match,
    contains_match,
    regex_match,
    tool_use_match,
)
from rewind_agent.evaluation import (
    _resolve_evaluator,
    _get_evaluator_name,
    _CUSTOM_EVALUATORS,
    get_evaluator,
    list_evaluators,
)
from rewind_agent.store import Store


# ── Helpers ────────────────────────────────────────────────────


def _now_rfc3339() -> str:
    return datetime.now(timezone.utc).isoformat()


def _new_id() -> str:
    return str(uuid.uuid4())


def _make_store(tmpdir: str) -> Store:
    """Create a fresh Store in a temp directory."""
    return Store(root=tmpdir)


def _seed_dataset(store: Store, name: str, examples: list[dict]) -> str:
    """
    Seed a dataset directly in the database, bypassing the CLI.

    Each item in `examples` should have 'input' and optionally 'expected'
    and 'metadata' keys.

    Returns the dataset ID.
    """
    dataset_id = _new_id()
    now = _now_rfc3339()

    store._conn.execute(
        "INSERT INTO datasets (id, name, description, created_at, updated_at, "
        "version, example_count, metadata) VALUES (?, ?, '', ?, ?, 1, ?, '{}')",
        (dataset_id, name, now, now, len(examples)),
    )

    for i, ex in enumerate(examples, start=1):
        example_id = _new_id()
        input_blob = store.blobs.put_json(ex["input"])
        expected_blob = ""
        if "expected" in ex and ex["expected"] is not None:
            expected_blob = store.blobs.put_json(ex["expected"])
        metadata = json.dumps(ex.get("metadata", {}), separators=(",", ":"))

        store._conn.execute(
            "INSERT INTO dataset_examples (id, dataset_id, ordinal, input_blob, "
            "expected_blob, metadata, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
            (example_id, dataset_id, i, input_blob, expected_blob, metadata, now),
        )

    store._conn.commit()
    return dataset_id


@pytest.fixture
def tmp_store():
    """Provide a fresh Store in a temp directory; close it after the test."""
    tmpdir = tempfile.mkdtemp()
    store = _make_store(tmpdir)
    yield store
    store.close()


# ══════════════════════════════════════════════════════════════
# Built-in Evaluators (pure function tests, no Store needed)
# ══════════════════════════════════════════════════════════════


class TestExactMatch:
    def test_pass_when_equal(self):
        score = exact_match({"q": "hi"}, {"answer": "yes"}, {"answer": "yes"})
        assert score.score == 1.0
        assert score.passed is True

    def test_fail_when_different(self):
        score = exact_match({"q": "hi"}, {"answer": "no"}, {"answer": "yes"})
        assert score.score == 0.0
        assert score.passed is False

    def test_handles_nested_dicts(self):
        nested = {"a": {"b": [1, 2, 3], "c": True}}
        score = exact_match({}, nested, nested)
        assert score.score == 1.0
        assert score.passed is True

    def test_nested_dicts_differ(self):
        out = {"a": {"b": [1, 2, 3]}}
        exp = {"a": {"b": [1, 2, 4]}}
        score = exact_match({}, out, exp)
        assert score.score == 0.0
        assert score.passed is False

    def test_no_expected(self):
        score = exact_match({}, {"answer": "yes"}, None)
        assert score.score == 0.0
        assert score.passed is False
        assert "No expected" in score.reasoning


class TestContainsMatch:
    def test_pass_when_substring_found(self):
        score = contains_match({}, {"text": "hello world"}, {}, substring="hello")
        assert score.score == 1.0
        assert score.passed is True

    def test_fail_when_substring_missing(self):
        score = contains_match({}, {"text": "goodbye"}, {}, substring="hello")
        assert score.score == 0.0
        assert score.passed is False

    def test_works_on_json_serialized_output(self):
        """The evaluator JSON-serializes the output, so keys are also searchable."""
        score = contains_match({}, {"my_key": "val"}, {}, substring="my_key")
        assert score.score == 1.0
        assert score.passed is True

    def test_substring_in_nested_value(self):
        score = contains_match({}, {"data": {"nested": "foobar"}}, {}, substring="foobar")
        assert score.score == 1.0

    def test_empty_substring_always_matches(self):
        score = contains_match({}, {"a": 1}, {}, substring="")
        assert score.score == 1.0


class TestRegexMatch:
    def test_pass_on_match(self):
        score = regex_match({}, {"status": "success_200"}, {}, pattern=r"success_\d+")
        assert score.score == 1.0
        assert score.passed is True

    def test_fail_on_no_match(self):
        score = regex_match({}, {"status": "error"}, {}, pattern=r"success_\d+")
        assert score.score == 0.0
        assert score.passed is False

    def test_handles_invalid_pattern_gracefully(self):
        """Invalid regex should raise (the evaluator doesn't catch it)."""
        # The regex_match function uses re.search which raises re.error for invalid patterns.
        # In evaluate(), this is caught and wrapped. Here we test the raw function.
        with pytest.raises(Exception):
            regex_match({}, {"status": "ok"}, {}, pattern=r"[invalid")

    def test_partial_match(self):
        """Regex searches anywhere in the JSON string, not just full match."""
        score = regex_match({}, {"msg": "abc123def"}, {}, pattern=r"\d{3}")
        assert score.score == 1.0


class TestToolUseMatch:
    def test_full_match_score_1(self):
        expected = {"tools_used": ["search", "calculate"]}
        output = {"tools_used": ["search", "calculate"]}
        score = tool_use_match({}, output, expected)
        assert score.score == 1.0
        assert score.passed is True

    def test_partial_match_fractional_score(self):
        expected = {"tools_used": ["search", "calculate"]}
        output = {"tools_used": ["search"]}
        score = tool_use_match({}, output, expected)
        # intersection={search}, union={search, calculate} => 1/2 = 0.5
        assert score.score == pytest.approx(0.5)
        assert score.passed is False

    def test_no_tools_expected_none_got(self):
        expected = {"some_key": "value"}  # no tool-related keys
        output = {"result": "done"}
        score = tool_use_match({}, output, expected)
        assert score.score == 1.0
        assert score.passed is True

    def test_no_tools_expected_but_some_used(self):
        expected = {"some_key": "value"}
        output = {"tools_used": ["search"]}
        score = tool_use_match({}, output, expected)
        assert score.score == 0.0
        assert score.passed is False

    def test_no_expected(self):
        score = tool_use_match({}, {"tools_used": ["search"]}, None)
        assert score.score == 0.0
        assert score.passed is False

    def test_tool_calls_with_dict_items(self):
        """Tests extraction from tool_calls with {name: ...} dicts."""
        expected = {"tool_calls": [{"name": "get_weather"}]}
        output = {"tool_calls": [{"name": "get_weather"}]}
        score = tool_use_match({}, output, expected)
        assert score.score == 1.0

    def test_tool_calls_with_function_name(self):
        """Tests extraction from tool_calls with {function: {name: ...}} dicts (OpenAI style)."""
        expected = {"tool_calls": [{"function": {"name": "search"}}]}
        output = {"tool_calls": [{"function": {"name": "search"}}]}
        score = tool_use_match({}, output, expected)
        assert score.score == 1.0

    def test_tool_name_key(self):
        """Tests extraction from the 'tool_name' key."""
        expected = {"tool_name": "calculate"}
        output = {"tool_name": "calculate"}
        score = tool_use_match({}, output, expected)
        assert score.score == 1.0

    def test_extra_tools_reduce_score(self):
        expected = {"tools_used": ["search"]}
        output = {"tools_used": ["search", "calculate", "fetch"]}
        score = tool_use_match({}, output, expected)
        # intersection={search}, union={search, calculate, fetch} => 1/3
        assert score.score == pytest.approx(1.0 / 3.0)


# ══════════════════════════════════════════════════════════════
# Dataset class
# ══════════════════════════════════════════════════════════════


class TestDataset:
    def test_create_and_read_examples(self, tmp_store):
        """Create a dataset with seeded examples and verify examples() returns correct data."""
        _seed_dataset(tmp_store, "test-ds", [
            {"input": {"q": "hello"}, "expected": {"a": "world"}},
            {"input": {"q": "foo"}, "expected": {"a": "bar"}},
        ])

        ds = Dataset("test-ds", store=tmp_store)
        examples = ds.examples()
        assert len(examples) == 2
        assert examples[0]["input"] == {"q": "hello"}
        assert examples[0]["expected"] == {"a": "world"}
        assert examples[1]["input"] == {"q": "foo"}
        assert examples[1]["expected"] == {"a": "bar"}

    def test_count_and_version(self, tmp_store):
        _seed_dataset(tmp_store, "count-ds", [
            {"input": {"x": 1}, "expected": {"y": 2}},
            {"input": {"x": 3}, "expected": {"y": 4}},
            {"input": {"x": 5}, "expected": {"y": 6}},
        ])

        ds = Dataset("count-ds", store=tmp_store)
        assert ds.count == 3
        assert ds.version == 1

    def test_empty_dataset(self, tmp_store):
        """A dataset that doesn't exist should have count=0 and version=0."""
        ds = Dataset("nonexistent", store=tmp_store)
        assert ds.count == 0
        assert ds.version == 0
        assert ds.examples() == []

    def test_examples_resolves_blobs(self, tmp_store):
        """Verify that examples() resolves blob hashes to actual data."""
        _seed_dataset(tmp_store, "blob-ds", [
            {"input": {"complex": {"nested": [1, 2, 3]}}, "expected": {"result": True}},
        ])

        ds = Dataset("blob-ds", store=tmp_store)
        examples = ds.examples()
        assert examples[0]["input"] == {"complex": {"nested": [1, 2, 3]}}
        assert examples[0]["expected"] == {"result": True}

    def test_examples_with_no_expected(self, tmp_store):
        """Examples without expected data should have expected=None."""
        _seed_dataset(tmp_store, "no-exp-ds", [
            {"input": {"q": "test"}},
        ])

        ds = Dataset("no-exp-ds", store=tmp_store)
        examples = ds.examples()
        assert len(examples) == 1
        assert examples[0]["expected"] is None

    @mock.patch("rewind_agent.evaluation._run_cli")
    def test_add_calls_cli(self, mock_cli, tmp_store):
        """Dataset.add() should shell out to the CLI."""
        # Pre-seed the dataset so _ensure_created finds it
        _seed_dataset(tmp_store, "add-ds", [])

        ds = Dataset("add-ds", store=tmp_store)
        ds.add(input={"q": "hello"}, expected={"a": "world"})

        # Should have been called with "eval dataset import ..."
        assert mock_cli.called
        call_args = mock_cli.call_args[0]
        assert "eval" in call_args
        assert "dataset" in call_args
        assert "import" in call_args

    @mock.patch("rewind_agent.evaluation._run_cli")
    def test_add_many_calls_cli(self, mock_cli, tmp_store):
        """Dataset.add_many() should shell out to the CLI."""
        _seed_dataset(tmp_store, "many-ds", [])

        ds = Dataset("many-ds", store=tmp_store)
        ds.add_many([
            {"input": {"x": 1}, "expected": {"y": 2}},
            {"input": {"x": 3}, "expected": {"y": 4}},
        ])

        assert mock_cli.called
        call_args = mock_cli.call_args[0]
        assert "import" in call_args

    @mock.patch("rewind_agent.evaluation._run_cli")
    def test_add_many_empty_is_noop(self, mock_cli, tmp_store):
        """add_many with empty list should not call CLI."""
        _seed_dataset(tmp_store, "empty-many-ds", [])

        ds = Dataset("empty-many-ds", store=tmp_store)
        ds.add_many([])
        assert not mock_cli.called

    @mock.patch("rewind_agent.evaluation._run_cli")
    def test_from_jsonl_creates_dataset(self, mock_cli, tmp_store):
        """from_jsonl should create a dataset via CLI import."""
        # Pre-seed so the Dataset constructor can find it
        _seed_dataset(tmp_store, "jsonl-ds", [
            {"input": {"q": "hello"}, "expected": {"a": "world"}},
        ])

        tmpfile = tempfile.NamedTemporaryFile(
            mode="w", suffix=".jsonl", delete=False
        )
        tmpfile.write(json.dumps({"input": {"q": "hello"}, "expected": {"a": "world"}}) + "\n")
        tmpfile.close()

        try:
            Dataset.from_jsonl("jsonl-ds", tmpfile.name, store=tmp_store)
            assert mock_cli.called
            call_args = mock_cli.call_args[0]
            assert "import" in call_args
            assert tmpfile.name in call_args
        finally:
            os.unlink(tmpfile.name)

    def test_repr(self, tmp_store):
        _seed_dataset(tmp_store, "repr-ds", [{"input": {"x": 1}}])
        ds = Dataset("repr-ds", store=tmp_store)
        r = repr(ds)
        assert "repr-ds" in r
        assert "count=1" in r
        assert "version=1" in r

    def test_ordinals_are_sequential(self, tmp_store):
        """Seeded examples should have ordinals 1, 2, 3."""
        _seed_dataset(tmp_store, "ord-ds", [
            {"input": {"a": 1}},
            {"input": {"a": 2}},
            {"input": {"a": 3}},
        ])

        ds = Dataset("ord-ds", store=tmp_store)
        examples = ds.examples()
        ordinals = [e["ordinal"] for e in examples]
        assert ordinals == [1, 2, 3]


# ══════════════════════════════════════════════════════════════
# @evaluator decorator
# ══════════════════════════════════════════════════════════════


class TestEvaluatorDecorator:
    def test_register_custom_evaluator(self):
        @evaluator("test_custom_eval")
        def my_evaluator(input, output, expected):
            return EvalScore(score=1.0, passed=True, reasoning="always pass")

        assert "test_custom_eval" in _CUSTOM_EVALUATORS
        assert get_evaluator("test_custom_eval") is my_evaluator

    def test_evaluator_in_registry(self):
        @evaluator("registry_check_eval")
        def another_eval(input, output, expected):
            return EvalScore(score=0.5, passed=False)

        registry = list_evaluators()
        assert "registry_check_eval" in registry

    def test_call_custom_evaluator(self):
        @evaluator("callable_eval")
        def score_by_length(input, output, expected):
            length = len(json.dumps(output))
            return EvalScore(
                score=min(length / 100.0, 1.0),
                passed=length > 10,
                reasoning=f"Output length: {length}",
            )

        result = score_by_length({}, {"data": "some output text"}, {})
        assert isinstance(result, EvalScore)
        assert result.score > 0
        assert result.passed is True

    def test_evaluator_uses_function_name_when_no_name(self):
        @evaluator()
        def auto_named_eval(input, output, expected):
            return EvalScore(score=1.0, passed=True)

        assert "auto_named_eval" in _CUSTOM_EVALUATORS

    def test_resolve_evaluator_by_string(self):
        @evaluator("resolvable_eval")
        def _eval(input, output, expected):
            return EvalScore(score=1.0, passed=True)

        resolved = _resolve_evaluator("resolvable_eval")
        assert resolved is _eval

    def test_resolve_builtin_by_string(self):
        resolved = _resolve_evaluator("exact_match")
        assert resolved is exact_match

    def test_resolve_callable(self):
        def my_fn(input, output, expected):
            return EvalScore(score=1.0, passed=True)

        resolved = _resolve_evaluator(my_fn)
        assert resolved is my_fn

    def test_resolve_unknown_string_raises(self):
        with pytest.raises(ValueError, match="Unknown evaluator"):
            _resolve_evaluator("nonexistent_evaluator_xyz")

    def test_resolve_invalid_type_raises(self):
        with pytest.raises(TypeError):
            _resolve_evaluator(42)

    def test_get_evaluator_name_string(self):
        assert _get_evaluator_name("exact_match") == "exact_match"

    def test_get_evaluator_name_function(self):
        def my_func(i, o, e):
            pass
        assert _get_evaluator_name(my_func) == "my_func"


# ══════════════════════════════════════════════════════════════
# evaluate() function
# ══════════════════════════════════════════════════════════════


class TestEvaluate:
    def test_run_with_builtin_evaluator_name(self, tmp_store):
        """evaluate() should accept a built-in evaluator by string name."""
        _seed_dataset(tmp_store, "eval-str-ds", [
            {"input": {"q": "hello"}, "expected": {"echo": {"q": "hello"}}},
        ])

        ds = Dataset("eval-str-ds", store=tmp_store)
        result = evaluate(
            dataset=ds,
            target_fn=lambda x: {"echo": x},
            evaluators=["exact_match"],
            store=tmp_store,
        )

        assert isinstance(result, ExperimentResult)
        assert result.avg_score == 1.0
        assert result.passed is True

    def test_run_with_custom_evaluator_function(self, tmp_store):
        """evaluate() should accept a callable evaluator directly."""
        _seed_dataset(tmp_store, "eval-fn-ds", [
            {"input": {"q": "test"}, "expected": {"q": "test"}},
        ])

        def always_pass(input, output, expected):
            return EvalScore(score=1.0, passed=True, reasoning="always pass")

        ds = Dataset("eval-fn-ds", store=tmp_store)
        result = evaluate(
            dataset=ds,
            target_fn=lambda x: x,
            evaluators=[always_pass],
            store=tmp_store,
        )

        assert result.avg_score == 1.0
        assert result.pass_rate == 1.0

    def test_multiple_evaluators_on_same_dataset(self, tmp_store):
        """Running multiple evaluators should produce scores from each."""
        _seed_dataset(tmp_store, "multi-eval-ds", [
            {"input": {"q": "hello"}, "expected": {"echo": {"q": "hello"}}},
        ])

        def always_half(input, output, expected):
            return EvalScore(score=0.5, passed=False, reasoning="half score")

        ds = Dataset("multi-eval-ds", store=tmp_store)
        result = evaluate(
            dataset=ds,
            target_fn=lambda x: {"echo": x},
            evaluators=[exact_match, always_half],
            store=tmp_store,
        )

        # 2 evaluators * 1 example = 2 scores. Avg = (1.0 + 0.5) / 2 = 0.75
        assert result.avg_score == pytest.approx(0.75)
        assert len(result.results) == 1
        assert len(result.results[0].scores) == 2

    def test_fail_below_raises_eval_failed_error(self, tmp_store):
        """When avg_score < fail_below, EvalFailedError should be raised."""
        _seed_dataset(tmp_store, "fail-ds", [
            {"input": {"q": "hello"}, "expected": {"different": "value"}},
        ])

        ds = Dataset("fail-ds", store=tmp_store)
        with pytest.raises(EvalFailedError) as exc_info:
            evaluate(
                dataset=ds,
                target_fn=lambda x: {"echo": x},
                evaluators=[exact_match],
                fail_below=0.5,
                store=tmp_store,
            )

        assert exc_info.value.result.avg_score == 0.0
        assert exc_info.value.result.passed is False

    def test_fail_below_passes_when_score_above_threshold(self, tmp_store):
        """When avg_score >= fail_below, no error should be raised."""
        _seed_dataset(tmp_store, "pass-ds", [
            {"input": {"q": "hello"}, "expected": {"echo": {"q": "hello"}}},
        ])

        ds = Dataset("pass-ds", store=tmp_store)
        result = evaluate(
            dataset=ds,
            target_fn=lambda x: {"echo": x},
            evaluators=[exact_match],
            fail_below=0.5,
            store=tmp_store,
        )

        assert result.avg_score == 1.0
        assert result.passed is True

    def test_empty_dataset_raises_error(self, tmp_store):
        """evaluate() should raise ValueError on an empty dataset."""
        _seed_dataset(tmp_store, "empty-ds", [])

        ds = Dataset("empty-ds", store=tmp_store)
        with pytest.raises(ValueError, match="no examples"):
            evaluate(
                dataset=ds,
                target_fn=lambda x: x,
                evaluators=[exact_match],
                store=tmp_store,
            )

    def test_experiment_result_aggregates(self, tmp_store):
        """ExperimentResult should have correct avg, min, max, pass_rate."""
        _seed_dataset(tmp_store, "agg-ds", [
            {"input": {"q": "a"}, "expected": {"echo": {"q": "a"}}},
            {"input": {"q": "b"}, "expected": {"wrong": "value"}},
            {"input": {"q": "c"}, "expected": {"echo": {"q": "c"}}},
        ])

        ds = Dataset("agg-ds", store=tmp_store)
        result = evaluate(
            dataset=ds,
            target_fn=lambda x: {"echo": x},
            evaluators=[exact_match],
            store=tmp_store,
        )

        # 3 examples: 1.0, 0.0, 1.0
        assert result.avg_score == pytest.approx(2.0 / 3.0)
        assert result.min_score == 0.0
        assert result.max_score == 1.0
        # pass_rate = count(score >= 1.0) / total = 2/3
        assert result.pass_rate == pytest.approx(2.0 / 3.0)
        assert result.total_examples == 3

    def test_target_fn_error_scores_zero(self, tmp_store):
        """If target_fn raises, all evaluators should score 0."""
        _seed_dataset(tmp_store, "error-ds", [
            {"input": {"q": "boom"}, "expected": {"a": "ok"}},
        ])

        def failing_fn(x):
            raise RuntimeError("boom!")

        ds = Dataset("error-ds", store=tmp_store)
        result = evaluate(
            dataset=ds,
            target_fn=failing_fn,
            evaluators=[exact_match],
            store=tmp_store,
        )

        assert result.avg_score == 0.0
        assert result.results[0].error is not None
        assert "RuntimeError" in result.results[0].error

    def test_experiment_result_has_example_results(self, tmp_store):
        """Each example should produce an ExampleResult in results."""
        _seed_dataset(tmp_store, "detail-ds", [
            {"input": {"q": "a"}, "expected": {"echo": {"q": "a"}}},
            {"input": {"q": "b"}, "expected": {"echo": {"q": "b"}}},
        ])

        ds = Dataset("detail-ds", store=tmp_store)
        result = evaluate(
            dataset=ds,
            target_fn=lambda x: {"echo": x},
            evaluators=[exact_match],
            store=tmp_store,
        )

        assert len(result.results) == 2
        for er in result.results:
            assert isinstance(er, ExampleResult)
            assert er.output == {"echo": er.input}
            assert er.error is None
            assert er.duration_ms >= 0

    def test_evaluate_with_dataset_name_string(self, tmp_store):
        """evaluate() should accept a dataset name string."""
        _seed_dataset(tmp_store, "str-ds", [
            {"input": {"q": "x"}, "expected": {"echo": {"q": "x"}}},
        ])

        result = evaluate(
            dataset="str-ds",
            target_fn=lambda x: {"echo": x},
            evaluators=[exact_match],
            store=tmp_store,
        )

        assert result.avg_score == 1.0

    def test_evaluate_default_evaluator_with_expected(self, tmp_store):
        """If no evaluators specified but expected data exists, defaults to exact_match."""
        _seed_dataset(tmp_store, "default-eval-ds", [
            {"input": {"q": "hello"}, "expected": {"echo": {"q": "hello"}}},
        ])

        ds = Dataset("default-eval-ds", store=tmp_store)
        result = evaluate(
            dataset=ds,
            target_fn=lambda x: {"echo": x},
            store=tmp_store,
        )

        assert result.avg_score == 1.0

    def test_evaluate_no_evaluators_no_expected_raises(self, tmp_store):
        """If no evaluators and no expected data, should raise ValueError."""
        _seed_dataset(tmp_store, "no-eval-ds", [
            {"input": {"q": "hello"}},
        ])

        ds = Dataset("no-eval-ds", store=tmp_store)
        with pytest.raises(ValueError, match="At least one evaluator"):
            evaluate(
                dataset=ds,
                target_fn=lambda x: x,
                store=tmp_store,
            )

    def test_experiment_persisted_in_store(self, tmp_store):
        """The experiment should be persisted in the store after evaluate()."""
        _seed_dataset(tmp_store, "persist-ds", [
            {"input": {"q": "x"}, "expected": {"echo": {"q": "x"}}},
        ])

        ds = Dataset("persist-ds", store=tmp_store)
        result = evaluate(
            dataset=ds,
            target_fn=lambda x: {"echo": x},
            evaluators=[exact_match],
            store=tmp_store,
        )

        exp = tmp_store.get_experiment(result.experiment_id)
        assert exp is not None
        assert exp["status"] == "completed"
        assert exp["avg_score"] == pytest.approx(1.0)

    def test_evaluator_error_scores_zero(self, tmp_store):
        """If an evaluator raises, it should score 0 with error reasoning."""
        _seed_dataset(tmp_store, "eval-err-ds", [
            {"input": {"q": "test"}, "expected": {"a": "ok"}},
        ])

        def bad_evaluator(input, output, expected):
            raise ValueError("evaluator broke")

        ds = Dataset("eval-err-ds", store=tmp_store)
        result = evaluate(
            dataset=ds,
            target_fn=lambda x: {"echo": x},
            evaluators=[bad_evaluator],
            store=tmp_store,
        )

        assert result.avg_score == 0.0
        score_info = result.results[0].scores[0]["score"]
        assert "evaluator broke" in score_info.reasoning

    def test_custom_experiment_name(self, tmp_store):
        """evaluate() should use the provided experiment name."""
        _seed_dataset(tmp_store, "named-ds", [
            {"input": {"q": "x"}, "expected": {"echo": {"q": "x"}}},
        ])

        ds = Dataset("named-ds", store=tmp_store)
        result = evaluate(
            dataset=ds,
            target_fn=lambda x: {"echo": x},
            evaluators=[exact_match],
            name="my-experiment",
            store=tmp_store,
        )

        assert result.name == "my-experiment"


# ══════════════════════════════════════════════════════════════
# compare() function
# ══════════════════════════════════════════════════════════════


class TestCompare:
    def test_compare_two_experiments(self, tmp_store):
        """compare() should compute delta between two ExperimentResults."""
        _seed_dataset(tmp_store, "cmp-ds", [
            {"input": {"q": "a"}, "expected": {"echo": {"q": "a"}}},
            {"input": {"q": "b"}, "expected": {"wrong": "value"}},
        ])

        ds = Dataset("cmp-ds", store=tmp_store)

        # Left experiment: target returns echo (1 pass, 1 fail)
        left = evaluate(
            dataset=ds,
            target_fn=lambda x: {"echo": x},
            evaluators=[exact_match],
            name="left-exp",
            store=tmp_store,
        )

        # Right experiment: always returns wrong output (all fail)
        right = evaluate(
            dataset=ds,
            target_fn=lambda x: {"wrong": "output"},
            evaluators=[exact_match],
            name="right-exp",
            store=tmp_store,
        )

        result = compare(left, right, store=tmp_store)

        assert isinstance(result, ComparisonResult)
        assert result.left_name == "left-exp"
        assert result.right_name == "right-exp"
        assert result.left_avg == pytest.approx(0.5)
        assert result.right_avg == 0.0
        assert result.score_delta == pytest.approx(-0.5)
        assert result.regressed is True
        assert result.improved is False

    def test_compare_improvement(self, tmp_store):
        """When right is better, improved should be True."""
        _seed_dataset(tmp_store, "imp-ds", [
            {"input": {"q": "a"}, "expected": {"wrong": "value"}},
        ])

        ds = Dataset("imp-ds", store=tmp_store)

        # Left: fails (wrong output)
        left = evaluate(
            dataset=ds,
            target_fn=lambda x: {"bad": "output"},
            evaluators=[exact_match],
            name="imp-left",
            store=tmp_store,
        )

        # Change dataset to match the right target's output
        _seed_dataset(tmp_store, "imp-ds-2", [
            {"input": {"q": "a"}, "expected": {"good": {"q": "a"}}},
        ])
        ds2 = Dataset("imp-ds-2", store=tmp_store)

        right = evaluate(
            dataset=ds2,
            target_fn=lambda x: {"good": x},
            evaluators=[exact_match],
            name="imp-right",
            store=tmp_store,
        )

        result = compare(left, right, store=tmp_store)
        assert result.score_delta > 0
        assert result.improved is True
        assert result.regressed is False

    def test_compare_unchanged(self, tmp_store):
        """When scores are equal, neither improved nor regressed."""
        _seed_dataset(tmp_store, "unch-ds", [
            {"input": {"q": "a"}, "expected": {"echo": {"q": "a"}}},
        ])

        ds = Dataset("unch-ds", store=tmp_store)

        left = evaluate(
            dataset=ds,
            target_fn=lambda x: {"echo": x},
            evaluators=[exact_match],
            name="unch-left",
            store=tmp_store,
        )

        right = evaluate(
            dataset=ds,
            target_fn=lambda x: {"echo": x},
            evaluators=[exact_match],
            name="unch-right",
            store=tmp_store,
        )

        result = compare(left, right, store=tmp_store)
        assert result.score_delta == pytest.approx(0.0)
        assert result.improved is False
        assert result.regressed is False

    def test_compare_per_example(self, tmp_store):
        """per_example should have entries for each ordinal."""
        _seed_dataset(tmp_store, "per-ds", [
            {"input": {"q": "a"}, "expected": {"echo": {"q": "a"}}},
            {"input": {"q": "b"}, "expected": {"echo": {"q": "b"}}},
        ])

        ds = Dataset("per-ds", store=tmp_store)

        left = evaluate(
            dataset=ds,
            target_fn=lambda x: {"echo": x},
            evaluators=[exact_match],
            name="per-left",
            store=tmp_store,
        )

        right = evaluate(
            dataset=ds,
            target_fn=lambda x: {"echo": x},
            evaluators=[exact_match],
            name="per-right",
            store=tmp_store,
        )

        result = compare(left, right, store=tmp_store)
        assert len(result.per_example) == 2
        for pe in result.per_example:
            assert pe["left_score"] is not None
            assert pe["right_score"] is not None
            assert pe["delta"] is not None

    def test_compare_pass_rate_delta(self, tmp_store):
        """pass_rate_delta should reflect the difference in pass rates."""
        _seed_dataset(tmp_store, "pr-ds", [
            {"input": {"q": "a"}, "expected": {"echo": {"q": "a"}}},
            {"input": {"q": "b"}, "expected": {"wrong": "value"}},
        ])

        ds = Dataset("pr-ds", store=tmp_store)

        # Left: 1 pass, 1 fail => pass_rate 0.5
        left = evaluate(
            dataset=ds,
            target_fn=lambda x: {"echo": x},
            evaluators=[exact_match],
            name="pr-left",
            store=tmp_store,
        )

        # Right: all pass
        _seed_dataset(tmp_store, "pr-ds-right", [
            {"input": {"q": "a"}, "expected": {"echo": {"q": "a"}}},
            {"input": {"q": "b"}, "expected": {"echo": {"q": "b"}}},
        ])
        ds_right = Dataset("pr-ds-right", store=tmp_store)

        right = evaluate(
            dataset=ds_right,
            target_fn=lambda x: {"echo": x},
            evaluators=[exact_match],
            name="pr-right",
            store=tmp_store,
        )

        result = compare(left, right, store=tmp_store)
        assert result.left_pass_rate == pytest.approx(0.5)
        assert result.right_pass_rate == pytest.approx(1.0)
        assert result.pass_rate_delta == pytest.approx(0.5)
