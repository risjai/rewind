"""Tests for ``rewind_agent.intercept._predicates``.

The default predicates are the routing decision every intercepted
request goes through. False negatives are silent recording bugs;
false positives are silent privacy bugs. Both deserve tight tests.
"""

from __future__ import annotations

import unittest

from rewind_agent.intercept._predicates import (
    DEFAULT_LLM_HOSTS,
    DefaultPredicates,
    default_is_llm_call,
    default_is_tool_call,
)
from rewind_agent.intercept._request import RewindRequest


def _req(url: str) -> RewindRequest:
    return RewindRequest(url=url, method="POST", headers={}, body=b"")


class TestDefaultLLMHosts(unittest.TestCase):
    def test_lists_known_providers(self) -> None:
        # Locked in so a refactor that drops a host without thinking
        # about the consequences gets a failing test.
        for host in (
            "api.openai.com",
            "api.anthropic.com",
            "generativelanguage.googleapis.com",
            "api.cohere.ai",
            "api.together.xyz",
            "api.groq.com",
            "api.deepseek.com",
            "api.mistral.ai",
        ):
            self.assertIn(host, DEFAULT_LLM_HOSTS)


class TestDefaultIsLLMCall(unittest.TestCase):
    def test_openai_chat_completions(self) -> None:
        self.assertTrue(
            default_is_llm_call(_req("https://api.openai.com/v1/chat/completions"))
        )

    def test_anthropic_messages(self) -> None:
        self.assertTrue(
            default_is_llm_call(_req("https://api.anthropic.com/v1/messages"))
        )

    def test_gemini_generate(self) -> None:
        self.assertTrue(
            default_is_llm_call(
                _req(
                    "https://generativelanguage.googleapis.com/v1/models/gemini-1.5-pro:generateContent"
                )
            )
        )

    def test_uppercase_host_normalized(self) -> None:
        # Hosts are case-insensitive per RFC 3986; users typing
        # "API.OPENAI.COM" should still match.
        self.assertTrue(default_is_llm_call(_req("https://API.OPENAI.COM/v1/chat")))

    def test_strips_userinfo_prefix(self) -> None:
        # If a credential leaked into the URL (proxy auth scheme),
        # we should still match the underlying host. Otherwise the
        # secret causes a privacy bug AND a recording miss.
        self.assertTrue(
            default_is_llm_call(_req("https://user:secret@api.openai.com/v1/chat"))
        )

    def test_strips_port_suffix(self) -> None:
        # Custom-port deployments (rare for the public APIs but
        # common for self-hosted gateways) should still match if
        # the hostname matches.
        self.assertTrue(default_is_llm_call(_req("https://api.openai.com:443/v1/chat")))

    def test_strips_userinfo_and_port_together(self) -> None:
        self.assertTrue(
            default_is_llm_call(
                _req("https://user:secret@api.openai.com:443/v1/chat")
            )
        )

    # ── Strict-by-default: subdomains and similar hosts do NOT match ──

    def test_subdomain_does_not_match(self) -> None:
        # Per the strict-default policy: corporate gateways like
        # llm-gateway.openai.com OR proxy.api.openai.com are NOT
        # auto-recorded. Operators wanting them in scope pass a
        # custom predicate.
        self.assertFalse(
            default_is_llm_call(_req("https://proxy.api.openai.com/v1/chat"))
        )

    def test_typosquat_does_not_match(self) -> None:
        # api.openai.con (typosquat) shouldn't match even though
        # it's "close" — exact-match policy.
        self.assertFalse(
            default_is_llm_call(_req("https://api.openai.con/v1/chat"))
        )

    def test_unrelated_host_does_not_match(self) -> None:
        self.assertFalse(default_is_llm_call(_req("https://example.com/anything")))

    def test_localhost_does_not_match(self) -> None:
        # Self-hosted dev work shouldn't auto-record. Operator
        # opts in via custom predicate.
        self.assertFalse(default_is_llm_call(_req("http://localhost:8000/v1/chat")))

    def test_ip_does_not_match(self) -> None:
        self.assertFalse(default_is_llm_call(_req("http://10.0.0.5/v1/chat")))


class TestDefaultIsToolCall(unittest.TestCase):
    def test_returns_false_for_everything(self) -> None:
        # The default tool-call predicate is a stub. HTTP-based tool
        # routing is operator-specific; the default should never
        # surprise-record an internal endpoint.
        for url in (
            "https://api.openai.com/v1/chat/completions",
            "https://internal-tool.corp.example/lookup_user",
            "http://localhost:8000/tools/search",
        ):
            self.assertFalse(default_is_tool_call(_req(url)))


class TestDefaultPredicates(unittest.TestCase):
    """The class form bundles the default functions for ``install()``."""

    def test_class_returns_function_results(self) -> None:
        preds = DefaultPredicates()
        # is_llm_call mirrors default_is_llm_call
        self.assertTrue(preds.is_llm_call(_req("https://api.openai.com/v1/chat")))
        self.assertFalse(preds.is_llm_call(_req("https://example.com/")))
        # is_tool_call is unconditionally False
        self.assertFalse(preds.is_tool_call(_req("https://anything.com/")))

    def test_subclass_compose_with_default(self) -> None:
        """Operators extend the default with extra hosts via subclassing.

        This is the documented escape hatch for custom gateways.
        """

        class ExtendedPredicates(DefaultPredicates):
            def is_llm_call(self, req: RewindRequest) -> bool:
                if req.url_parts.netloc.endswith(".my-corp.example"):
                    return True
                return super().is_llm_call(req)

        preds = ExtendedPredicates()
        # Default still works
        self.assertTrue(preds.is_llm_call(_req("https://api.openai.com/v1/chat")))
        # Extension works
        self.assertTrue(preds.is_llm_call(_req("https://llm.my-corp.example/v1/chat")))
        # Unrelated host stays out of scope
        self.assertFalse(preds.is_llm_call(_req("https://example.com/")))


if __name__ == "__main__":
    unittest.main()
