"""Tests for the Langfuse trace import module."""

import json
import urllib.error
from unittest import mock

import pytest

from rewind_agent.langfuse_import import (
    _build_attributes,
    _convert_to_otlp,
    _infer_span_name,
    _iso_to_nanos,
    _observation_to_span,
    _stable_id,
    import_from_langfuse,
    main,
)


# ══════════════════════════════════════════════════════════════
# Helper builders
# ══════════════════════════════════════════════════════════════

def _make_observation(
    obs_id="obs-1",
    obs_type="GENERATION",
    name="gpt-4o",
    model="gpt-4o",
    parent_id=None,
    input_data=None,
    output_data=None,
    tokens_in=100,
    tokens_out=50,
    start_time="2026-04-13T10:00:00.000Z",
    end_time="2026-04-13T10:00:01.500Z",
    level="DEFAULT",
    status_message=None,
):
    obs = {
        "id": obs_id,
        "type": obs_type,
        "name": name,
        "model": model,
        "startTime": start_time,
        "endTime": end_time,
        "level": level,
        "usageDetails": {"input": tokens_in, "output": tokens_out},
        "input": input_data,
        "output": output_data,
    }
    if parent_id:
        obs["parentObservationId"] = parent_id
    if status_message:
        obs["statusMessage"] = status_message
    return obs


def _make_trace(trace_id="trace-abc", name="test-agent", observations=None):
    return {
        "id": trace_id,
        "name": name,
        "observations": observations or [],
    }


# ══════════════════════════════════════════════════════════════
# Unit tests: helpers
# ══════════════════════════════════════════════════════════════


class TestStableId:
    def test_returns_hex_string(self):
        result = _stable_id("test", 8)
        assert len(result) == 16  # 8 bytes = 16 hex chars

    def test_deterministic(self):
        assert _stable_id("abc", 8) == _stable_id("abc", 8)

    def test_different_inputs_differ(self):
        assert _stable_id("abc", 8) != _stable_id("def", 8)

    def test_trace_id_length(self):
        result = _stable_id("trace-123", 16)
        assert len(result) == 32  # 16 bytes = 32 hex chars


class TestIsoToNanos:
    def test_valid_timestamp(self):
        nanos = _iso_to_nanos("2026-04-13T10:00:00.000Z")
        assert nanos > 0

    def test_empty_string_returns_zero(self):
        assert _iso_to_nanos("") == 0

    def test_none_returns_zero(self):
        assert _iso_to_nanos(None) == 0

    def test_invalid_returns_zero(self):
        assert _iso_to_nanos("not-a-date") == 0


class TestInferSpanName:
    def test_generation_with_model(self):
        obs = {"type": "GENERATION", "model": "gpt-4o", "name": "chat"}
        assert _infer_span_name(obs) == "gen_ai.chat gpt-4o"

    def test_generation_without_model(self):
        obs = {"type": "GENERATION", "name": "chat"}
        assert _infer_span_name(obs) == "gen_ai.chat unknown"

    def test_tool_type(self):
        obs = {"type": "TOOL", "name": "search_web"}
        assert _infer_span_name(obs) == "tool.execute search_web"

    def test_span_type_uses_name(self):
        obs = {"type": "SPAN", "name": "planning-phase"}
        assert _infer_span_name(obs) == "planning-phase"

    def test_span_without_name(self):
        obs = {"type": "SPAN"}
        assert _infer_span_name(obs) == "langfuse.span"


class TestBuildAttributes:
    def test_generation_has_model_and_operation(self):
        obs = _make_observation()
        attrs = _build_attributes(obs)
        keys = [a["key"] for a in attrs]
        assert "gen_ai.request.model" in keys
        assert "gen_ai.operation.name" in keys

    def test_includes_tokens(self):
        obs = _make_observation(tokens_in=200, tokens_out=100)
        attrs = _build_attributes(obs)
        token_attrs = {a["key"]: a["value"] for a in attrs if "tokens" in a["key"]}
        assert token_attrs["gen_ai.usage.input_tokens"]["intValue"] == "200"
        assert token_attrs["gen_ai.usage.output_tokens"]["intValue"] == "100"

    def test_includes_content_blobs(self):
        obs = _make_observation(
            input_data=[{"role": "user", "content": "hello"}],
            output_data=[{"role": "assistant", "content": "hi"}],
        )
        attrs = _build_attributes(obs)
        keys = [a["key"] for a in attrs]
        assert "gen_ai.input.messages" in keys
        assert "gen_ai.output.messages" in keys

    def test_no_content_when_none(self):
        obs = _make_observation(input_data=None, output_data=None)
        attrs = _build_attributes(obs)
        keys = [a["key"] for a in attrs]
        assert "gen_ai.input.messages" not in keys
        assert "gen_ai.output.messages" not in keys

    def test_tool_has_tool_name(self):
        obs = _make_observation(obs_type="TOOL", name="calculator", model=None)
        attrs = _build_attributes(obs)
        keys = [a["key"] for a in attrs]
        assert "gen_ai.tool.name" in keys


# ══════════════════════════════════════════════════════════════
# Unit tests: conversion
# ══════════════════════════════════════════════════════════════


class TestObservationToSpan:
    def test_basic_generation(self):
        obs = _make_observation()
        span = _observation_to_span(obs, "a" * 32)
        assert span is not None
        assert span["name"] == "gen_ai.chat gpt-4o"
        assert span["traceId"] == "a" * 32
        assert len(span["spanId"]) == 16
        assert span["parentSpanId"] == ""

    def test_with_parent(self):
        obs = _make_observation(parent_id="parent-obs")
        span = _observation_to_span(obs, "a" * 32)
        assert span["parentSpanId"] != ""

    def test_error_status(self):
        obs = _make_observation(level="ERROR", status_message="rate_limit")
        span = _observation_to_span(obs, "a" * 32)
        assert span["status"]["code"] == 2
        assert span["status"]["message"] == "rate_limit"

    def test_no_id_returns_none(self):
        obs = {"id": "", "type": "SPAN"}
        assert _observation_to_span(obs, "a" * 32) is None


class TestConvertToOtlp:
    def test_basic_trace(self):
        trace = _make_trace(observations=[
            _make_observation(obs_id="obs-1"),
            _make_observation(obs_id="obs-2", obs_type="TOOL", name="search", model=None),
        ])
        result = _convert_to_otlp(trace)
        spans = result["resourceSpans"][0]["scopeSpans"][0]["spans"]
        assert len(spans) == 2

    def test_empty_observations_raises(self):
        trace = _make_trace(observations=[])
        with pytest.raises(ValueError, match="no observations"):
            _convert_to_otlp(trace)

    def test_content_preserved(self):
        trace = _make_trace(observations=[
            _make_observation(
                input_data={"messages": [{"role": "user", "content": "hello"}]},
                output_data={"choices": [{"message": {"content": "hi"}}]},
            ),
        ])
        result = _convert_to_otlp(trace)
        span = result["resourceSpans"][0]["scopeSpans"][0]["spans"][0]
        attr_keys = [a["key"] for a in span["attributes"]]
        assert "gen_ai.input.messages" in attr_keys
        assert "gen_ai.output.messages" in attr_keys


# ══════════════════════════════════════════════════════════════
# Integration tests (mocked HTTP)
# ══════════════════════════════════════════════════════════════


class TestImportFromLangfuse:
    def test_missing_keys_raises(self):
        with mock.patch.dict("os.environ", {}, clear=True):
            with pytest.raises(RuntimeError, match="API keys required"):
                import_from_langfuse("trace-123")

    def test_trace_not_found(self):
        error = urllib.error.HTTPError(
            "http://test", 404, "Not Found", {}, None
        )
        with mock.patch("rewind_agent.langfuse_import.urllib.request.urlopen", side_effect=error):
            with pytest.raises(ValueError, match="Trace not found"):
                import_from_langfuse(
                    "trace-123",
                    public_key="pk-test",
                    secret_key="sk-test",
                )

    def test_auth_failure(self):
        error = urllib.error.HTTPError(
            "http://test", 401, "Unauthorized", {}, None
        )
        with mock.patch("rewind_agent.langfuse_import.urllib.request.urlopen", side_effect=error):
            with pytest.raises(RuntimeError, match="authentication failed"):
                import_from_langfuse(
                    "trace-123",
                    public_key="pk-test",
                    secret_key="sk-test",
                )


class TestSubprocessEntryPoint:
    def test_invalid_json(self, capsys):
        with mock.patch("sys.stdin") as mock_stdin:
            mock_stdin.read.return_value = "not json"
            with pytest.raises(SystemExit) as exc:
                main()
            assert exc.value.code == 1

        captured = capsys.readouterr()
        result = json.loads(captured.out)
        assert "error" in result

    def test_missing_trace_id(self, capsys):
        with mock.patch("sys.stdin") as mock_stdin:
            mock_stdin.read.return_value = json.dumps({"host": "http://test"})
            with pytest.raises((SystemExit, KeyError)):
                main()

