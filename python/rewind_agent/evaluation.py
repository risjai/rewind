"""
Rewind Evaluation SDK — datasets, evaluators, and experiment execution.

Provides a Python-native evaluation framework that stores results in the same
SQLite database as the Rust CLI. Dataset writes go through the CLI subprocess
to avoid dual-store drift; reads use the Store directly for performance.
The evaluate() function runs entirely in-process (no CLI shelling).

Zero external dependencies — uses only Python stdlib.

Usage:
    from rewind_agent.evaluation import (
        Dataset, evaluate, exact_match, EvalScore, ExperimentResult,
    )

    ds = Dataset("my-tests")
    ds.add(input={"prompt": "hello"}, expected={"reply": "hi"})

    result = evaluate(
        dataset=ds,
        target_fn=my_agent,
        evaluators=[exact_match],
        fail_below=0.8,
    )
    print(f"Score: {result.avg_score:.1%}")
"""

import json
import os
import re
import shutil
import subprocess
import tempfile
import time
import uuid
from dataclasses import dataclass, field
from typing import Optional

from .store import Store


# ── Helpers ────────────────────────────────────────────────────

def _new_id() -> str:
    return str(uuid.uuid4())


def _find_rewind_binary() -> str:
    """Locate the rewind CLI binary using the same logic as _cli.py."""
    # Check PATH first
    which = shutil.which("rewind")
    if which:
        return which

    # Check cached binary
    cache_dir = os.path.join(os.path.expanduser("~"), ".rewind", "bin")
    if os.path.isdir(cache_dir):
        # Find the latest versioned binary
        candidates = sorted(
            (f for f in os.listdir(cache_dir) if f.startswith("rewind-")),
            reverse=True,
        )
        for c in candidates:
            path = os.path.join(cache_dir, c)
            if os.path.isfile(path) and os.access(path, os.X_OK):
                return path

    # Check local dev build
    pkg_dir = os.path.dirname(os.path.abspath(__file__))
    dev_path = os.path.join(pkg_dir, "..", "..", "target", "release", "rewind")
    if os.path.isfile(dev_path) and os.access(dev_path, os.X_OK):
        return os.path.normpath(dev_path)

    raise FileNotFoundError(
        "Rewind CLI binary not found. Install with: pip install rewind-agent"
    )


def _run_cli(*args: str, check: bool = True) -> subprocess.CompletedProcess:
    """Run a rewind CLI command, raising on failure if check=True."""
    binary = _find_rewind_binary()
    result = subprocess.run(
        [binary] + list(args),
        capture_output=True,
        text=True,
    )
    if check and result.returncode != 0:
        stderr = result.stderr.strip()
        raise RuntimeError(
            f"rewind CLI failed (exit {result.returncode}): {stderr or result.stdout.strip()}"
        )
    return result


# ── EvalScore ──────────────────────────────────────────────────

@dataclass
class EvalScore:
    """Result from a single evaluator on a single example."""
    score: float  # 0.0 - 1.0
    passed: bool
    reasoning: str = ""


# ── EvalFailedError ────────────────────────────────────────────

class EvalFailedError(Exception):
    """Raised when evaluate() detects that avg_score < fail_below threshold."""

    def __init__(self, result: "ExperimentResult"):
        self.result = result
        super().__init__(
            f"Evaluation failed: avg_score {result.avg_score:.3f} "
            f"below threshold (pass_rate={result.pass_rate:.1%}, "
            f"experiment={result.experiment_id!r})"
        )


# ── ExperimentResult ───────────────────────────────────────────

@dataclass
class ExampleResult:
    """Per-example result within an experiment."""
    example_id: str
    ordinal: int
    input: dict
    expected: Optional[dict]
    output: Optional[dict]
    scores: list  # list of dicts: {"evaluator": str, "score": EvalScore}
    duration_ms: int
    error: Optional[str] = None


@dataclass
class ExperimentResult:
    """Aggregate result from evaluate()."""
    experiment_id: str
    name: str
    avg_score: float
    min_score: float
    max_score: float
    pass_rate: float
    total_examples: int
    total_duration_ms: int
    results: list = field(default_factory=list)  # list of ExampleResult
    passed: bool = True


# ── ComparisonResult ───────────────────────────────────────────

@dataclass
class ComparisonResult:
    """Result of comparing two experiments."""
    left_id: str
    right_id: str
    left_name: str
    right_name: str
    score_delta: float  # right.avg - left.avg
    pass_rate_delta: float
    improved: bool
    regressed: bool
    left_avg: float
    right_avg: float
    left_pass_rate: float
    right_pass_rate: float
    per_example: list = field(default_factory=list)


# ── Dataset ────────────────────────────────────────────────────

class Dataset:
    """
    A versioned collection of input/expected pairs for evaluation.

    Writes go through the rewind CLI subprocess to stay in sync with the
    Rust store. Reads use the Python Store directly for performance.
    """

    def __init__(self, name: str, store: Store = None):
        self.name = name
        self._store = store or Store()
        self._dataset = self._store.get_dataset_by_name(name)

    def _ensure_created(self):
        """Create the dataset via CLI if it doesn't exist yet."""
        if self._dataset is None:
            _run_cli("eval", "dataset", "create", self.name)
            self._dataset = self._store.get_dataset_by_name(self.name)
            if self._dataset is None:
                raise RuntimeError(f"Failed to create dataset '{self.name}'")

    def _refresh(self):
        """Refresh the cached dataset metadata from the store."""
        self._dataset = self._store.get_dataset_by_name(self.name)

    def add(self, input: dict, expected: dict = None, metadata: dict = None) -> "Dataset":
        """
        Add a single example to the dataset via CLI.

        Uses JSONL import with a single-line temp file since the CLI
        doesn't have an atomic single-example add command with JSON args.

        Returns self for chaining.
        """
        self._ensure_created()

        example = {"input": input}
        if expected is not None:
            example["expected"] = expected
        if metadata is not None:
            example["metadata"] = metadata

        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".jsonl", delete=False
        ) as f:
            f.write(json.dumps(example, separators=(",", ":"), default=str))
            f.write("\n")
            tmp_path = f.name

        try:
            _run_cli("eval", "dataset", "import", self.name, tmp_path)
        finally:
            os.unlink(tmp_path)

        self._refresh()
        return self

    def add_many(self, examples: list) -> "Dataset":
        """
        Add multiple examples at once. Each item should be a dict with
        'input' and optionally 'expected' and 'metadata' keys.

        Writes a temp JSONL file and imports via CLI.
        Returns self for chaining.
        """
        if not examples:
            return self

        self._ensure_created()

        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".jsonl", delete=False
        ) as f:
            for ex in examples:
                line = {}
                line["input"] = ex.get("input", ex) if isinstance(ex, dict) else ex
                if isinstance(ex, dict):
                    if "expected" in ex:
                        line["expected"] = ex["expected"]
                    if "metadata" in ex:
                        line["metadata"] = ex["metadata"]
                f.write(json.dumps(line, separators=(",", ":"), default=str))
                f.write("\n")
            tmp_path = f.name

        try:
            _run_cli("eval", "dataset", "import", self.name, tmp_path)
        finally:
            os.unlink(tmp_path)

        self._refresh()
        return self

    @classmethod
    def from_session(cls, name: str, session_ref: str = "latest",
                     input_step: int = 1, expected_step: int = None,
                     store: Store = None) -> "Dataset":
        """
        Create a dataset with an example extracted from a recorded session.

        Calls `rewind eval dataset add-from-session`.
        """
        args = [
            "eval", "dataset", "add-from-session", name, session_ref,
            "--input-step", str(input_step),
        ]
        if expected_step is not None:
            args.extend(["--expected-step", str(expected_step)])

        _run_cli(*args)

        ds = cls(name, store=store)
        ds._refresh()
        return ds

    @classmethod
    def from_jsonl(cls, name: str, path: str, store: Store = None) -> "Dataset":
        """
        Create or append to a dataset from a JSONL file.

        Each line should be a JSON object with 'input' and optionally
        'expected' and 'metadata' keys.
        """
        _run_cli("eval", "dataset", "import", name, path)
        ds = cls(name, store=store)
        ds._refresh()
        return ds

    def export_jsonl(self, path: str):
        """Export the dataset to a JSONL file."""
        self._ensure_created()
        _run_cli("eval", "dataset", "export", self.name, path)

    def examples(self) -> list:
        """
        Read all examples from the store. Returns a list of dicts, each with
        'input', 'expected', and 'metadata' keys (resolved from blob store).
        """
        if self._dataset is None:
            return []

        raw_examples = self._store.get_dataset_examples(self._dataset["id"])
        result = []
        for ex in raw_examples:
            input_data = self._store.blobs.get_json(ex["input_blob"]) or {}
            expected_data = self._store.blobs.get_json(ex["expected_blob"])
            meta_str = ex.get("metadata", "{}")
            if isinstance(meta_str, str):
                try:
                    meta = json.loads(meta_str)
                except (json.JSONDecodeError, TypeError):
                    meta = {}
            else:
                meta = meta_str

            result.append({
                "id": ex["id"],
                "ordinal": ex["ordinal"],
                "input": input_data,
                "expected": expected_data,
                "metadata": meta,
            })
        return result

    @property
    def version(self) -> int:
        """Current dataset version."""
        if self._dataset is None:
            return 0
        return self._dataset.get("version", 0)

    @property
    def count(self) -> int:
        """Number of examples in the dataset."""
        if self._dataset is None:
            return 0
        return self._dataset.get("example_count", 0)

    def __repr__(self) -> str:
        return f"Dataset(name={self.name!r}, version={self.version}, count={self.count})"


# ── Built-in Evaluators ───────────────────────────────────────

def exact_match(input: dict, output: dict, expected: dict) -> EvalScore:
    """
    Exact match evaluator. Compares output to expected using deep equality.
    Returns score 1.0 if they match, 0.0 otherwise.
    """
    if expected is None:
        return EvalScore(score=0.0, passed=False, reasoning="No expected output to compare against")

    matched = output == expected
    return EvalScore(
        score=1.0 if matched else 0.0,
        passed=matched,
        reasoning="Exact match" if matched else f"Output differs from expected",
    )


def contains_match(input: dict, output: dict, expected: dict, *, substring: str) -> EvalScore:
    """
    Check if the substring appears anywhere in the JSON-serialized output.
    """
    output_str = json.dumps(output, separators=(",", ":"), default=str)
    found = substring in output_str
    return EvalScore(
        score=1.0 if found else 0.0,
        passed=found,
        reasoning=f"Substring {'found' if found else 'not found'}: {substring!r}",
    )


def regex_match(input: dict, output: dict, expected: dict, *, pattern: str) -> EvalScore:
    """
    Check if the regex pattern matches anywhere in the JSON-serialized output.
    """
    output_str = json.dumps(output, separators=(",", ":"), default=str)
    match = re.search(pattern, output_str)
    found = match is not None
    return EvalScore(
        score=1.0 if found else 0.0,
        passed=found,
        reasoning=f"Pattern {'matched' if found else 'not matched'}: {pattern!r}",
    )


def tool_use_match(input: dict, output: dict, expected: dict) -> EvalScore:
    """
    Check that the output used the same tools (by name) as expected.
    Looks for 'tool_calls', 'tools_used', or 'tool_name' keys in both
    output and expected.
    """
    if expected is None:
        return EvalScore(score=0.0, passed=False, reasoning="No expected output to compare against")

    def _extract_tools(d: dict) -> set:
        tools = set()
        if isinstance(d, dict):
            # Common patterns for tool usage
            for key in ("tool_calls", "tools_used", "tools"):
                val = d.get(key)
                if isinstance(val, list):
                    for item in val:
                        if isinstance(item, dict):
                            name = item.get("name") or item.get("function", {}).get("name", "")
                            if name:
                                tools.add(name)
                        elif isinstance(item, str):
                            tools.add(item)
            if "tool_name" in d:
                tools.add(d["tool_name"])
        return tools

    expected_tools = _extract_tools(expected)
    actual_tools = _extract_tools(output)

    if not expected_tools:
        return EvalScore(
            score=1.0 if not actual_tools else 0.0,
            passed=not actual_tools,
            reasoning="No tools expected" + (f", but found: {actual_tools}" if actual_tools else ""),
        )

    if expected_tools == actual_tools:
        return EvalScore(score=1.0, passed=True, reasoning=f"Tools match: {expected_tools}")

    # Partial credit: intersection over union
    intersection = expected_tools & actual_tools
    union = expected_tools | actual_tools
    score = len(intersection) / len(union) if union else 0.0
    passed = score >= 1.0

    missing = expected_tools - actual_tools
    extra = actual_tools - expected_tools
    parts = []
    if missing:
        parts.append(f"missing: {missing}")
    if extra:
        parts.append(f"extra: {extra}")
    reasoning = f"Tool overlap {score:.0%}" + (f" ({', '.join(parts)})" if parts else "")

    return EvalScore(score=score, passed=passed, reasoning=reasoning)


# ── Custom Evaluator Registry ─────────────────────────────────

_CUSTOM_EVALUATORS = {}


def evaluator(name=None):
    """
    Decorator to register a custom evaluator function.

    Usage:
        @evaluator("my_scorer")
        def my_scorer(input, output, expected):
            score = 1.0 if "success" in output.get("status", "") else 0.0
            return EvalScore(score=score, passed=score > 0.5)

    The function must accept (input, output, expected) and return an EvalScore.
    """
    def decorator(fn):
        eval_name = name or fn.__name__
        _CUSTOM_EVALUATORS[eval_name] = fn
        return fn
    return decorator


def get_evaluator(name: str):
    """Look up a registered evaluator by name. Returns None if not found."""
    return _CUSTOM_EVALUATORS.get(name)


def list_evaluators() -> dict:
    """Return a copy of the custom evaluator registry."""
    return dict(_CUSTOM_EVALUATORS)


# ── Built-in evaluator name map ───────────────────────────────

_BUILTIN_EVALUATORS = {
    "exact_match": exact_match,
    "contains_match": contains_match,
    "regex_match": regex_match,
    "tool_use_match": tool_use_match,
}


def _resolve_evaluator(e):
    """
    Resolve an evaluator from various input forms:
    - A callable is returned as-is
    - A string is looked up in custom evaluators, then built-ins
    """
    if callable(e):
        return e
    if isinstance(e, str):
        # Check custom evaluators first
        if e in _CUSTOM_EVALUATORS:
            return _CUSTOM_EVALUATORS[e]
        if e in _BUILTIN_EVALUATORS:
            return _BUILTIN_EVALUATORS[e]
        raise ValueError(
            f"Unknown evaluator '{e}'. "
            f"Built-in: {list(_BUILTIN_EVALUATORS.keys())}. "
            f"Custom: {list(_CUSTOM_EVALUATORS.keys())}."
        )
    raise TypeError(f"Evaluator must be a callable or string name, got {type(e).__name__}")


def _get_evaluator_name(e) -> str:
    """Get a human-readable name for an evaluator."""
    if isinstance(e, str):
        return e
    if hasattr(e, "__name__"):
        return e.__name__
    return repr(e)


# ── evaluate() ─────────────────────────────────────────────────

def evaluate(
    dataset,
    target_fn,
    evaluators=None,
    name=None,
    fail_below=None,
    metadata=None,
    store=None,
) -> ExperimentResult:
    """
    Run an evaluation experiment.

    For each example in the dataset:
      1. Call target_fn(example_input) -> output dict
      2. Run each evaluator(input, output, expected) -> EvalScore
      3. Record results in the store

    Args:
        dataset: A Dataset object or a dataset name string.
        target_fn: Callable that takes an input dict and returns an output dict.
        evaluators: List of evaluator callables or built-in name strings.
                    Defaults to [exact_match] if expected data is present.
        name: Experiment name. Auto-generated if omitted.
        fail_below: If set, raise EvalFailedError when avg_score < threshold.
        metadata: Optional dict of metadata tags.
        store: Optional Store instance (shared with dataset if possible).

    Returns:
        ExperimentResult with aggregate scores and per-example detail.

    Raises:
        EvalFailedError: If fail_below is set and avg_score is below threshold.
    """
    # Resolve dataset
    if isinstance(dataset, str):
        _store = store or Store()
        dataset = Dataset(dataset, store=_store)
    else:
        _store = store or dataset._store

    examples = dataset.examples()
    if not examples:
        raise ValueError(f"Dataset '{dataset.name}' has no examples")

    # Resolve evaluators
    if evaluators is None:
        has_expected = any(ex.get("expected") is not None for ex in examples)
        evaluators = [exact_match] if has_expected else []

    if not evaluators:
        raise ValueError("At least one evaluator is required")

    resolved_evaluators = [_resolve_evaluator(e) for e in evaluators]
    evaluator_names = [_get_evaluator_name(e) for e in evaluators]

    # Auto-generate experiment name
    experiment_name = name or f"{dataset.name}-{_new_id()[:8]}"
    experiment_id = _new_id()

    # Ensure evaluator records exist in the store (for foreign key references)
    evaluator_ids = {}
    for eval_name, eval_fn in zip(evaluator_names, resolved_evaluators):
        existing = _store.get_evaluator_by_name(eval_name)
        if existing:
            evaluator_ids[eval_name] = existing["id"]
        else:
            eid = _new_id()
            eval_type = "builtin" if eval_name in _BUILTIN_EVALUATORS else "custom"
            _store.create_evaluator(eid, eval_name, eval_type)
            evaluator_ids[eval_name] = eid

    # Build config blob
    config = {
        "evaluators": evaluator_names,
        "target_fn": getattr(target_fn, "__name__", str(target_fn)),
    }
    config_blob = _store.blobs.put_json(config)

    metadata_str = json.dumps(metadata or {}, separators=(",", ":"), default=str)

    # Create experiment record
    _store.create_experiment(
        experiment_id=experiment_id,
        name=experiment_name,
        dataset_id=dataset._dataset["id"],
        dataset_version=dataset.version,
        total_examples=len(examples),
        config_blob=config_blob,
        metadata=metadata_str,
    )

    # Run evaluation
    all_example_results = []
    all_scores = []
    total_duration_ms = 0

    for idx, example in enumerate(examples):
        ex_input = example["input"]
        ex_expected = example.get("expected")
        example_id = example["id"]
        ordinal = example.get("ordinal", idx + 1)

        # Run target function
        result_id = _new_id()
        error_msg = None
        output = None
        duration_ms = 0

        start = time.monotonic()
        try:
            output = target_fn(ex_input)
        except Exception as exc:
            error_msg = f"{type(exc).__name__}: {exc}"
        duration_ms = int((time.monotonic() - start) * 1000)
        total_duration_ms += duration_ms

        # Store output in blob store
        output_blob = ""
        if output is not None:
            output_blob = _store.blobs.put_json(output)

        # Determine result status
        result_status = "error" if error_msg else "success"

        # Persist experiment result
        _store.create_experiment_result(
            result_id=result_id,
            experiment_id=experiment_id,
            example_id=example_id,
            ordinal=ordinal,
            output_blob=output_blob,
            duration_ms=duration_ms,
            status=result_status,
            error=error_msg,
        )

        # Run evaluators and score
        example_scores = []
        for eval_name, eval_fn in zip(evaluator_names, resolved_evaluators):
            if error_msg:
                # Target function errored — score is 0
                eval_score = EvalScore(
                    score=0.0,
                    passed=False,
                    reasoning=f"Target function error: {error_msg}",
                )
            else:
                try:
                    eval_score = eval_fn(ex_input, output, ex_expected)
                except Exception as exc:
                    eval_score = EvalScore(
                        score=0.0,
                        passed=False,
                        reasoning=f"Evaluator error: {type(exc).__name__}: {exc}",
                    )

            # Persist score
            score_id = _new_id()
            _store.create_experiment_score(
                score_id=score_id,
                result_id=result_id,
                evaluator_id=evaluator_ids[eval_name],
                score=eval_score.score,
                passed=eval_score.passed,
                reasoning=eval_score.reasoning,
            )

            example_scores.append({
                "evaluator": eval_name,
                "score": eval_score,
            })
            all_scores.append(eval_score.score)

        all_example_results.append(ExampleResult(
            example_id=example_id,
            ordinal=ordinal,
            input=ex_input,
            expected=ex_expected,
            output=output,
            scores=example_scores,
            duration_ms=duration_ms,
            error=error_msg,
        ))

        # Update progress
        _store.update_experiment_progress(experiment_id, idx + 1)

    # Compute aggregates
    if all_scores:
        avg_score = sum(all_scores) / len(all_scores)
        min_score = min(all_scores)
        max_score = max(all_scores)
    else:
        avg_score = 0.0
        min_score = 0.0
        max_score = 0.0

    passed_count = sum(1 for s in all_scores if s >= 1.0)
    pass_rate = passed_count / len(all_scores) if all_scores else 0.0

    # Persist aggregates
    _store.update_experiment_aggregates(
        experiment_id=experiment_id,
        avg_score=avg_score,
        min_score=min_score,
        max_score=max_score,
        pass_rate=pass_rate,
        total_duration_ms=total_duration_ms,
    )
    _store.update_experiment_status(experiment_id, "completed")

    # Determine if overall experiment passed
    overall_passed = True
    if fail_below is not None and avg_score < fail_below:
        overall_passed = False

    result = ExperimentResult(
        experiment_id=experiment_id,
        name=experiment_name,
        avg_score=avg_score,
        min_score=min_score,
        max_score=max_score,
        pass_rate=pass_rate,
        total_examples=len(examples),
        total_duration_ms=total_duration_ms,
        results=all_example_results,
        passed=overall_passed,
    )

    if not overall_passed:
        raise EvalFailedError(result)

    return result


# ── compare() ──────────────────────────────────────────────────

def compare(left, right, store: Store = None) -> ComparisonResult:
    """
    Compare two experiments side-by-side.

    Args:
        left: ExperimentResult object, experiment ID string, or experiment name string.
        right: ExperimentResult object, experiment ID string, or experiment name string.
        store: Optional Store instance for resolving string references.

    Returns:
        ComparisonResult with deltas and per-example comparison.
    """
    _store = store or Store()

    def _resolve(ref):
        if isinstance(ref, ExperimentResult):
            return {
                "id": ref.experiment_id,
                "name": ref.name,
                "avg_score": ref.avg_score,
                "pass_rate": ref.pass_rate,
                "results": ref.results,
            }
        if isinstance(ref, str):
            # Try by ID first, then by name
            exp = _store.get_experiment(ref)
            if exp is None:
                exp = _store.get_experiment_by_name(ref)
            if exp is None:
                raise ValueError(f"Experiment not found: {ref!r}")
            # Load per-example results
            raw_results = _store.get_experiment_results(exp["id"])
            return {
                "id": exp["id"],
                "name": exp["name"],
                "avg_score": exp.get("avg_score") or 0.0,
                "pass_rate": exp.get("pass_rate") or 0.0,
                "results": raw_results,
            }
        raise TypeError(f"Expected ExperimentResult or string, got {type(ref).__name__}")

    left_data = _resolve(left)
    right_data = _resolve(right)

    left_avg = left_data["avg_score"]
    right_avg = right_data["avg_score"]
    left_pr = left_data["pass_rate"]
    right_pr = right_data["pass_rate"]
    score_delta = right_avg - left_avg
    pass_rate_delta = right_pr - left_pr

    # Build per-example comparison by ordinal
    per_example = []
    left_by_ordinal = {}
    right_by_ordinal = {}

    for r in left_data["results"]:
        ordinal = r.ordinal if isinstance(r, ExampleResult) else r.get("ordinal", 0)
        left_by_ordinal[ordinal] = r

    for r in right_data["results"]:
        ordinal = r.ordinal if isinstance(r, ExampleResult) else r.get("ordinal", 0)
        right_by_ordinal[ordinal] = r

    all_ordinals = sorted(set(left_by_ordinal.keys()) | set(right_by_ordinal.keys()))

    def _avg_scores(result):
        if result is None:
            return None
        if isinstance(result, ExampleResult):
            scores = [s["score"].score for s in result.scores]
        elif isinstance(result, dict):
            # Load from store if needed
            scores_data = _store.get_experiment_scores(result["id"])
            scores = [s["score"] for s in scores_data]
        else:
            return None
        return sum(scores) / len(scores) if scores else 0.0

    for ordinal in all_ordinals:
        l = left_by_ordinal.get(ordinal)
        r = right_by_ordinal.get(ordinal)

        l_avg = _avg_scores(l)
        r_avg = _avg_scores(r)

        per_example.append({
            "ordinal": ordinal,
            "left_score": l_avg,
            "right_score": r_avg,
            "delta": (r_avg - l_avg) if (l_avg is not None and r_avg is not None) else None,
        })

    # A score_delta > 0.001 counts as improved; < -0.001 as regressed
    improved = score_delta > 0.001
    regressed = score_delta < -0.001

    return ComparisonResult(
        left_id=left_data["id"],
        right_id=right_data["id"],
        left_name=left_data["name"],
        right_name=right_data["name"],
        score_delta=score_delta,
        pass_rate_delta=pass_rate_delta,
        improved=improved,
        regressed=regressed,
        left_avg=left_avg,
        right_avg=right_avg,
        left_pass_rate=left_pr,
        right_pass_rate=right_pr,
        per_example=per_example,
    )
