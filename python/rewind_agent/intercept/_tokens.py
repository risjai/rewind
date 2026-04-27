"""Provider-agnostic token + model extraction from LLM response bodies.

Used by :mod:`._flow` to populate ``record_llm_call(tokens_in=…, tokens_out=…, model=…)``
on cache miss. The recording is best-effort: when extraction fails (custom
provider, non-standard response shape, malformed JSON), we record zeros
rather than failing the whole flow — observability beats perfection here.

Supported response shapes:

- **OpenAI / OpenAI-compatible** (Together, Groq, DeepSeek, Mistral, ...):
  ``{"usage": {"prompt_tokens": int, "completion_tokens": int},
     "model": str}``
- **Anthropic Messages API**:
  ``{"usage": {"input_tokens": int, "output_tokens": int},
     "model": str}``
- **Google Gemini**:
  ``{"usageMetadata": {"promptTokenCount": int, "candidatesTokenCount": int},
     "modelVersion": str}``  (Gemini stamps the resolved version, not the
  user-supplied alias, in the response — we record what came back.)
- **Cohere chat v2**:
  ``{"meta": {"billed_units": {"input_tokens": int, "output_tokens": int}},
     "model": str}``  (older Cohere shapes use ``billed_units.input_units`` —
  we accept both.)

When none of these shapes match, we return ``(0, 0, model_from_request)``
where ``model_from_request`` falls back to the request body's ``model``
field if present, or the empty string. The dashboard's "tokens saved"
math will under-count for unknown providers; the cure is to add a shape
to this module rather than to make the extractor stricter.
"""

from __future__ import annotations

from typing import Any


def extract_tokens_and_model(
    request_body: Any,
    response_body: Any,
) -> tuple[int, int, str]:
    """Return ``(tokens_in, tokens_out, model)`` for an LLM call.

    Both arguments are JSON-decoded values (typically ``dict``). Any
    non-dict input (e.g. parse failure left them as ``None``) collapses
    to zeros and an empty model string — no exception, no log spam.
    """
    if not isinstance(response_body, dict):
        return 0, 0, _model_from_request(request_body)

    # Try shapes in order of how often they appear in the wild. Each
    # extractor returns None to signal "this isn't my shape" so the
    # next can try.
    for extractor in (
        _try_openai_shape,
        _try_anthropic_shape,
        _try_gemini_shape,
        _try_cohere_shape,
    ):
        result = extractor(response_body)
        if result is not None:
            tokens_in, tokens_out, model = result
            # If the response didn't carry a model field, fall back to
            # the request's. Common for non-canonical gateways.
            if not model:
                model = _model_from_request(request_body)
            return tokens_in, tokens_out, model

    # Unknown shape — surface the request's model so the step is at
    # least attributable, but token counts unknown.
    return 0, 0, _model_from_request(request_body)


def _model_from_request(request_body: Any) -> str:
    """Fallback model extraction from the request side.

    Most LLM APIs accept ``{"model": "<name>", ...}`` in the request body
    even when the response doesn't echo it. Used both as the tiebreaker
    when response shape lacks a model field and as the only signal when
    extraction outright fails.
    """
    if isinstance(request_body, dict):
        model = request_body.get("model")
        if isinstance(model, str):
            return model
    return ""


def _try_openai_shape(resp: dict) -> tuple[int, int, str] | None:
    usage = resp.get("usage")
    if not isinstance(usage, dict):
        return None
    prompt = usage.get("prompt_tokens")
    completion = usage.get("completion_tokens")
    if not isinstance(prompt, int) or not isinstance(completion, int):
        return None
    model = resp.get("model")
    return prompt, completion, model if isinstance(model, str) else ""


def _try_anthropic_shape(resp: dict) -> tuple[int, int, str] | None:
    usage = resp.get("usage")
    if not isinstance(usage, dict):
        return None
    inp = usage.get("input_tokens")
    out = usage.get("output_tokens")
    if not isinstance(inp, int) or not isinstance(out, int):
        return None
    model = resp.get("model")
    return inp, out, model if isinstance(model, str) else ""


def _try_gemini_shape(resp: dict) -> tuple[int, int, str] | None:
    usage = resp.get("usageMetadata")
    if not isinstance(usage, dict):
        return None
    prompt = usage.get("promptTokenCount")
    completion = usage.get("candidatesTokenCount")
    if not isinstance(prompt, int) or not isinstance(completion, int):
        return None
    # Gemini uses "modelVersion" (resolved version) in the response.
    # Fall back to "model" if some custom server emits the alias.
    model = resp.get("modelVersion") or resp.get("model")
    return prompt, completion, model if isinstance(model, str) else ""


def _try_cohere_shape(resp: dict) -> tuple[int, int, str] | None:
    meta = resp.get("meta")
    if not isinstance(meta, dict):
        return None
    billed = meta.get("billed_units")
    if not isinstance(billed, dict):
        return None
    # Cohere v2 uses {input_tokens, output_tokens}; older Cohere v1
    # uses {input_units, output_units}. Accept both.
    inp = billed.get("input_tokens", billed.get("input_units"))
    out = billed.get("output_tokens", billed.get("output_units"))
    if not isinstance(inp, int) or not isinstance(out, int):
        return None
    model = resp.get("model")
    return inp, out, model if isinstance(model, str) else ""
