"""Tests for the OTel export module."""

import hashlib
import sys
import pytest

# Add parent dir to path so we can import rewind_agent
sys.path.insert(0, str(__import__("pathlib").Path(__file__).parent.parent))

from rewind_agent.otel_export import (
    _trace_id_from_session,
    _span_id_from_id,
    _infer_provider,
)


class TestTraceIdGeneration:
    def test_deterministic(self):
        id1 = _trace_id_from_session("session-abc-123")
        id2 = _trace_id_from_session("session-abc-123")
        assert id1 == id2

    def test_different_sessions(self):
        id1 = _trace_id_from_session("session-1")
        id2 = _trace_id_from_session("session-2")
        assert id1 != id2

    def test_matches_rust_algorithm(self):
        """Verify Python produces same trace ID as Rust: SHA-256[0..16] as big-endian int."""
        session_id = "test-session"
        h = hashlib.sha256(session_id.encode()).digest()
        expected = int.from_bytes(h[:16], "big")
        assert _trace_id_from_session(session_id) == expected

    def test_nonzero(self):
        assert _trace_id_from_session("any-id") != 0


class TestSpanIdGeneration:
    def test_deterministic(self):
        id1 = _span_id_from_id("step-abc")
        id2 = _span_id_from_id("step-abc")
        assert id1 == id2

    def test_different_ids(self):
        id1 = _span_id_from_id("step-1")
        id2 = _span_id_from_id("step-2")
        assert id1 != id2


class TestInferProvider:
    def test_openai_models(self):
        assert _infer_provider("gpt-4o") == "openai"
        assert _infer_provider("gpt-4-turbo") == "openai"
        assert _infer_provider("o1-preview") == "openai"
        assert _infer_provider("o3-mini") == "openai"

    def test_anthropic_models(self):
        assert _infer_provider("claude-sonnet-4-5-20250514") == "anthropic"
        assert _infer_provider("claude-3-haiku-20240307") == "anthropic"

    def test_google_models(self):
        assert _infer_provider("gemini-pro") == "google"

    def test_prefixed_models(self):
        assert _infer_provider("openai/gpt-4o-mini") == "openai"
        assert _infer_provider("anthropic/claude-3-haiku") == "anthropic"
        assert _infer_provider("google/gemini-pro") == "google"

    def test_unknown_model(self):
        assert _infer_provider("my-custom-model") == "unknown"


class TestExportSessionImportGuard:
    def test_import_error_without_otel(self, monkeypatch):
        """Verify helpful error when OTel is not installed."""
        import importlib
        # This test only works if OTel is NOT installed
        try:
            import opentelemetry.sdk
            pytest.skip("OTel is installed, can't test ImportError path")
        except ImportError:
            with pytest.raises(ImportError, match="pip install rewind-agent"):
                from rewind_agent.otel_export import export_session
                export_session("nonexistent")


class TestPublicApi:
    def test_export_otel_is_importable(self):
        """Verify export_otel is accessible from the package without OTel installed."""
        import rewind_agent
        assert hasattr(rewind_agent, "export_otel")
        assert callable(rewind_agent.export_otel)
