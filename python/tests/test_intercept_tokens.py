"""Tests for ``rewind_agent.intercept._tokens``.

Token + model extraction across the supported provider shapes.
Each shape is tested with realistic response bodies copied from
provider documentation.
"""

from __future__ import annotations

import unittest

from rewind_agent.intercept._tokens import extract_tokens_and_model


class TestOpenAIShape(unittest.TestCase):
    def test_chat_completions_response(self) -> None:
        # Minimal OpenAI chat.completion response shape.
        request = {"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}
        response = {
            "id": "chatcmpl-abc",
            "object": "chat.completion",
            "model": "gpt-4o-2024-11-20",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "hello"}}],
            "usage": {"prompt_tokens": 12, "completion_tokens": 3, "total_tokens": 15},
        }
        tokens_in, tokens_out, model = extract_tokens_and_model(request, response)
        self.assertEqual(tokens_in, 12)
        self.assertEqual(tokens_out, 3)
        # The response carries the resolved model alias — record THAT,
        # not the request's user-supplied alias, so the recorded step
        # is what actually billed.
        self.assertEqual(model, "gpt-4o-2024-11-20")

    def test_response_without_model_falls_back_to_request(self) -> None:
        request = {"model": "gpt-4o-mini"}
        response = {
            "usage": {"prompt_tokens": 5, "completion_tokens": 2},
        }
        _, _, model = extract_tokens_and_model(request, response)
        self.assertEqual(model, "gpt-4o-mini")


class TestAnthropicShape(unittest.TestCase):
    def test_messages_response(self) -> None:
        request = {"model": "claude-3-5-sonnet-20241022", "messages": []}
        response = {
            "id": "msg_abc",
            "type": "message",
            "model": "claude-3-5-sonnet-20241022",
            "content": [{"type": "text", "text": "hi"}],
            "usage": {"input_tokens": 20, "output_tokens": 7},
        }
        tokens_in, tokens_out, model = extract_tokens_and_model(request, response)
        self.assertEqual(tokens_in, 20)
        self.assertEqual(tokens_out, 7)
        self.assertEqual(model, "claude-3-5-sonnet-20241022")

    def test_anthropic_takes_priority_over_openai_when_both_keys_present(self) -> None:
        # Defensive: a response object that happens to have both
        # `prompt_tokens` and `input_tokens` (e.g. some gateways
        # add OpenAI-compat aliases) — try OpenAI first since it's
        # most common. This documents the order; flipping it would
        # be a silent behavior change.
        response = {
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "input_tokens": 999,
                "output_tokens": 999,
            }
        }
        tokens_in, _, _ = extract_tokens_and_model({}, response)
        self.assertEqual(tokens_in, 1, "OpenAI shape wins when both are present")


class TestGeminiShape(unittest.TestCase):
    def test_generate_content_response(self) -> None:
        request = {"contents": [{"parts": [{"text": "hi"}]}]}
        response = {
            "candidates": [{"content": {"parts": [{"text": "hello"}]}}],
            "usageMetadata": {
                "promptTokenCount": 4,
                "candidatesTokenCount": 1,
                "totalTokenCount": 5,
            },
            "modelVersion": "gemini-1.5-pro-002",
        }
        tokens_in, tokens_out, model = extract_tokens_and_model(request, response)
        self.assertEqual(tokens_in, 4)
        self.assertEqual(tokens_out, 1)
        self.assertEqual(model, "gemini-1.5-pro-002")


class TestCohereShape(unittest.TestCase):
    def test_v2_billed_units_input_tokens(self) -> None:
        response = {
            "model": "command-r-plus",
            "meta": {"billed_units": {"input_tokens": 30, "output_tokens": 12}},
        }
        tokens_in, tokens_out, model = extract_tokens_and_model({}, response)
        self.assertEqual(tokens_in, 30)
        self.assertEqual(tokens_out, 12)
        self.assertEqual(model, "command-r-plus")

    def test_v1_billed_units_input_units(self) -> None:
        # Older Cohere uses input_units/output_units. Both forms
        # supported per docstring.
        response = {
            "model": "command",
            "meta": {"billed_units": {"input_units": 50, "output_units": 25}},
        }
        tokens_in, tokens_out, _ = extract_tokens_and_model({}, response)
        self.assertEqual(tokens_in, 50)
        self.assertEqual(tokens_out, 25)


class TestFallback(unittest.TestCase):
    def test_unknown_shape_returns_zeros(self) -> None:
        # Hypothetical custom provider with a totally different shape.
        response = {"output": "hi", "metadata": {"input": 5, "output": 2}}
        tokens_in, tokens_out, model = extract_tokens_and_model({"model": "x"}, response)
        self.assertEqual(tokens_in, 0)
        self.assertEqual(tokens_out, 0)
        self.assertEqual(model, "x")

    def test_non_dict_response_returns_zeros(self) -> None:
        # Defensive: the JSON body wasn't a dict (rare but possible
        # with edge providers that wrap responses in arrays).
        for response in (None, "raw text", [1, 2, 3], 42):
            tokens_in, tokens_out, model = extract_tokens_and_model({}, response)
            self.assertEqual((tokens_in, tokens_out, model), (0, 0, ""))

    def test_model_falls_back_to_request_when_response_lacks_one(self) -> None:
        request = {"model": "fallback-model-name"}
        response = {"foo": "bar"}  # unknown shape, no model field
        _, _, model = extract_tokens_and_model(request, response)
        self.assertEqual(model, "fallback-model-name")

    def test_no_model_anywhere_returns_empty(self) -> None:
        _, _, model = extract_tokens_and_model({}, {"foo": "bar"})
        self.assertEqual(model, "")


if __name__ == "__main__":
    unittest.main()
