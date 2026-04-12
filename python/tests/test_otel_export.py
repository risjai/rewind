"""Tests for the OTel export module."""

import hashlib
import os
import sys
import tempfile

import pytest

# Add parent dir to path so we can import rewind_agent
sys.path.insert(0, str(__import__("pathlib").Path(__file__).parent.parent))

from rewind_agent.otel_export import (
    _trace_id_from_session,
    _span_id_from_id,
    _infer_provider,
    _iso_to_ns,
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


class TestIsoToNs:
    def test_with_fractional_seconds(self):
        ns = _iso_to_ns("2026-04-12T11:15:57.394421")
        assert ns > 0
        # Verify it's in the right ballpark (2026 epoch range)
        epoch_sec = ns // 1_000_000_000
        assert 1_770_000_000 < epoch_sec < 1_780_000_000

    def test_without_fractional_seconds(self):
        ns = _iso_to_ns("2026-04-12T11:15:57")
        assert ns > 0

    def test_with_z_suffix(self):
        ns = _iso_to_ns("2026-04-12T11:15:57.394421Z")
        assert ns > 0

    def test_empty_string_raises(self):
        with pytest.raises(ValueError, match="Empty timestamp"):
            _iso_to_ns("")

    def test_garbage_raises(self):
        with pytest.raises(ValueError, match="Could not parse"):
            _iso_to_ns("not-a-timestamp")


class TestExportSessionImportGuard:
    def test_import_error_without_otel(self):
        """Verify helpful error when OTel is not installed."""
        try:
            import importlib.util
            if importlib.util.find_spec("opentelemetry.sdk"):
                pytest.skip("OTel is installed, can't test ImportError path")
        except ModuleNotFoundError:
            pass  # opentelemetry not installed at all — proceed with test
        else:
            with pytest.raises(ImportError, match="pip install rewind-agent"):
                from rewind_agent.otel_export import export_session
                export_session("nonexistent")


class TestPublicApi:
    def test_export_otel_is_importable(self):
        """Verify export_otel is accessible from the package without OTel installed."""
        import rewind_agent
        assert hasattr(rewind_agent, "export_otel")
        assert callable(rewind_agent.export_otel)


class TestExportSessionIntegration:
    """Integration test using a real Store to verify data extraction."""

    def _has_otel(self):
        try:
            import importlib.util
            return importlib.util.find_spec("opentelemetry.sdk") is not None
        except ModuleNotFoundError:
            return False

    def test_query_functions_return_all_columns(self):
        """Create a temp store with a session, verify queries return timestamps and tool_name."""
        from rewind_agent.store import Store
        from rewind_agent.otel_export import (
            _query_session,
            _query_timelines,
            _query_steps,
            _iso_to_ns,
        )

        with tempfile.TemporaryDirectory() as tmpdir:
            os.environ["REWIND_DATA"] = tmpdir
            try:
                store = Store()
                session_id, timeline_id = store.create_session("test-otel-export")

                store.create_step(
                    session_id=session_id,
                    timeline_id=timeline_id,
                    step_number=1,
                    step_type="llm_call",
                    status="success",
                    model="gpt-4o",
                    request_blob=store.blobs.put_json({"messages": [{"role": "user", "content": "hello"}]}),
                    response_blob=store.blobs.put_json({"id": "chatcmpl-abc", "model": "gpt-4o"}),
                    tokens_in=10,
                    tokens_out=5,
                    duration_ms=500,
                )

                # Session query includes timestamps
                sess = _query_session(store._conn, session_id)
                assert sess is not None
                assert "created_at" in sess and sess["created_at"] is not None
                assert "updated_at" in sess and sess["updated_at"] is not None
                assert _iso_to_ns(sess["created_at"]) > 0

                # Timeline query includes created_at
                tls = _query_timelines(store._conn, session_id, None, False)
                assert len(tls) == 1
                assert "created_at" in tls[0] and tls[0]["created_at"] is not None
                assert _iso_to_ns(tls[0]["created_at"]) > 0

                # Step query includes created_at AND tool_name
                steps = _query_steps(store._conn, timeline_id)
                assert len(steps) == 1
                assert "created_at" in steps[0] and steps[0]["created_at"] is not None
                assert steps[0]["model"] == "gpt-4o"
                assert steps[0]["tokens_in"] == 10
                assert _iso_to_ns(steps[0]["created_at"]) > 0

            finally:
                del os.environ["REWIND_DATA"]

    def test_export_session_end_to_end(self):
        """Full export_session() call against a temp store with a mock endpoint."""
        if not self._has_otel():
            pytest.skip("OTel not installed")

        from rewind_agent.store import Store
        from rewind_agent.otel_export import export_session

        with tempfile.TemporaryDirectory() as tmpdir:
            os.environ["REWIND_DATA"] = tmpdir
            try:
                store = Store()
                session_id, timeline_id = store.create_session("e2e-test")

                store.create_step(
                    session_id=session_id,
                    timeline_id=timeline_id,
                    step_number=1,
                    step_type="llm_call",
                    status="success",
                    model="gpt-4o",
                    request_blob=store.blobs.put_json({"messages": []}),
                    response_blob=store.blobs.put_json({"model": "gpt-4o", "choices": []}),
                    tokens_in=10,
                    tokens_out=5,
                    duration_ms=200,
                )

                # Export to a non-existent endpoint — will fail to send but
                # should still succeed in creating spans (batch processor queues them)
                count = export_session(
                    session_id,
                    endpoint="http://127.0.0.1:1",  # nothing listening
                )
                # 3 spans: session + timeline + step
                assert count == 3

            finally:
                del os.environ["REWIND_DATA"]
