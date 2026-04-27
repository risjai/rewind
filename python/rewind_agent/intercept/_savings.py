"""Process-lifetime cache-hit savings counter.

Phase 1 ships this so a freshly-installed agent can report "tokens
saved" / "USD saved" without round-tripping to the dashboard. The
dashboard's totals come from stored steps and are authoritative across
processes; this module's totals are scoped to a single
``intercept.install()`` lifetime and reset on ``uninstall()``.

The counter is thread-safe — incremented by adapter callbacks running
on whatever thread/event-loop the underlying HTTP client uses. A single
:class:`threading.Lock` guards mutation; reads use the GIL's atomic
attribute read.

## Usage

::

   from rewind_agent import intercept
   intercept.install()

   # … agent runs, hits some cached responses, hits some live …

   snap = intercept.savings()
   print(f"saved {snap.cache_hits} hits "
         f"= {snap.tokens_saved_in + snap.tokens_saved_out} tokens "
         f"≈ ${snap.cost_saved_usd_estimate:.4f}")

The cost estimate uses a small in-process pricing table (see
:data:`MODEL_PRICING_USD_PER_MILLION`). Unknown models contribute zero
USD but still count toward token totals. Override per-call by passing
``cost_table=`` to :func:`record_cache_hit`.
"""

from __future__ import annotations

import threading
from dataclasses import dataclass


@dataclass(frozen=True)
class SavingsSnapshot:
    """Read-only snapshot of the savings counter at a moment in time.

    Frozen so callers can't accidentally mutate the live counter through
    a returned reference. Get fresh data with another :func:`savings()`
    call.
    """

    #: Number of LLM cache hits served since install (or last reset).
    cache_hits: int
    #: Total prompt-side tokens saved (would have been billed on miss).
    tokens_saved_in: int
    #: Total completion-side tokens saved.
    tokens_saved_out: int
    #: Best-effort USD estimate using :data:`MODEL_PRICING_USD_PER_MILLION`.
    #: Zero for cache hits whose model isn't in the pricing table.
    cost_saved_usd_estimate: float


# Per-million-token USD pricing for the most common providers as of
# this PR. Numbers are approximate spot prices; provider price changes
# don't break the counter, they just drift the estimate. Operators
# wanting precision pass ``cost_table=`` to record_cache_hit() or
# subscribe to dashboard totals (which use the Rust-side pricing
# table that's kept current).
#
# Keys are lowercased, prefix-matched. So "gpt-4o" matches "gpt-4o-2024-11-20",
# "gpt-4o-mini", etc. Conservative — when multiple prefixes match,
# the longest wins.
MODEL_PRICING_USD_PER_MILLION: dict[str, tuple[float, float]] = {
    # OpenAI
    "gpt-4o-mini": (0.15, 0.60),
    "gpt-4o": (2.50, 10.00),
    "gpt-4-turbo": (10.00, 30.00),
    "gpt-4": (30.00, 60.00),
    "gpt-3.5-turbo": (0.50, 1.50),
    "o1-mini": (3.00, 12.00),
    "o1": (15.00, 60.00),
    # Anthropic
    "claude-3-5-sonnet": (3.00, 15.00),
    "claude-3-5-haiku": (1.00, 5.00),
    "claude-3-opus": (15.00, 75.00),
    "claude-3-sonnet": (3.00, 15.00),
    "claude-3-haiku": (0.25, 1.25),
    "claude-opus-4": (15.00, 75.00),
    "claude-sonnet-4": (3.00, 15.00),
    # Google Gemini
    "gemini-1.5-pro": (1.25, 5.00),
    "gemini-1.5-flash": (0.075, 0.30),
    "gemini-2.0-flash": (0.10, 0.40),
    # Meta / Together / Groq
    "llama-3.3-70b": (0.59, 0.79),
    "llama-3.1-70b": (0.59, 0.79),
    "llama-3.1-405b": (3.50, 3.50),
    # Mistral
    "mistral-large": (2.00, 6.00),
    "mistral-small": (0.20, 0.60),
}


_lock = threading.Lock()
_state = {
    "cache_hits": 0,
    "tokens_saved_in": 0,
    "tokens_saved_out": 0,
    "cost_saved_usd_estimate": 0.0,
}


def record_cache_hit(
    *,
    model: str,
    tokens_in: int,
    tokens_out: int,
    cost_table: dict[str, tuple[float, float]] | None = None,
) -> None:
    """Record a single cache hit's savings.

    Called by :mod:`._flow` on every cache-hit code path. Thread-safe
    via :data:`_lock`. If ``model`` doesn't match any prefix in the
    cost table, the USD estimate increment is zero but the call still
    counts toward ``cache_hits`` and the token totals.

    Parameters
    ----------
    model:
        Model name as recorded in the step. Case-insensitive prefix
        match against the cost table; unknown models contribute zero
        USD.
    tokens_in:
        Prompt-side token count from the recorded step. Negative or
        non-int values are treated as zero — defensive against
        malformed step records.
    tokens_out:
        Completion-side token count.
    cost_table:
        Override the default :data:`MODEL_PRICING_USD_PER_MILLION`
        pricing. Useful for self-hosted models or non-listed providers
        the operator wants to attribute correctly.
    """
    safe_in = tokens_in if isinstance(tokens_in, int) and tokens_in > 0 else 0
    safe_out = tokens_out if isinstance(tokens_out, int) and tokens_out > 0 else 0
    table = cost_table if cost_table is not None else MODEL_PRICING_USD_PER_MILLION
    cost = _estimate_cost_usd(model, safe_in, safe_out, table)

    with _lock:
        _state["cache_hits"] += 1
        _state["tokens_saved_in"] += safe_in
        _state["tokens_saved_out"] += safe_out
        _state["cost_saved_usd_estimate"] += cost


def savings() -> SavingsSnapshot:
    """Read the current savings counter.

    Returns a frozen snapshot; the live counter continues to update
    in the background.
    """
    with _lock:
        return SavingsSnapshot(
            cache_hits=_state["cache_hits"],
            tokens_saved_in=_state["tokens_saved_in"],
            tokens_saved_out=_state["tokens_saved_out"],
            cost_saved_usd_estimate=_state["cost_saved_usd_estimate"],
        )


def reset() -> None:
    """Zero the counter. Called by ``uninstall()`` and exposed for tests."""
    with _lock:
        _state["cache_hits"] = 0
        _state["tokens_saved_in"] = 0
        _state["tokens_saved_out"] = 0
        _state["cost_saved_usd_estimate"] = 0.0


def _estimate_cost_usd(
    model: str,
    tokens_in: int,
    tokens_out: int,
    table: dict[str, tuple[float, float]],
) -> float:
    """Longest-prefix match against the pricing table.

    Lowercases ``model`` and finds the longest key that's a prefix.
    Returns 0.0 if no prefix matches.
    """
    if not model:
        return 0.0
    needle = model.lower()
    best_match: str | None = None
    for key in table:
        if needle.startswith(key) and (best_match is None or len(key) > len(best_match)):
            best_match = key
    if best_match is None:
        return 0.0
    in_per_m, out_per_m = table[best_match]
    return (tokens_in * in_per_m + tokens_out * out_per_m) / 1_000_000
