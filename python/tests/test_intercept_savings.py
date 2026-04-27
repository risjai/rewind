"""Tests for ``rewind_agent.intercept._savings``.

The savings counter is process-wide and lives across tests, so each
test calls ``reset()`` to start from a clean slate. These tests cover
correctness of the increment math, longest-prefix matching, custom
cost tables, and the snapshot's frozen-ness.
"""

from __future__ import annotations

import threading
import unittest

from rewind_agent.intercept._savings import (
    MODEL_PRICING_USD_PER_MILLION,
    SavingsSnapshot,
    record_cache_hit,
    reset,
    savings,
)


class TestSnapshotImmutability(unittest.TestCase):
    def test_snapshot_is_frozen_dataclass(self) -> None:
        reset()
        record_cache_hit(model="gpt-4o", tokens_in=1000, tokens_out=500)
        snap = savings()
        self.assertIsInstance(snap, SavingsSnapshot)
        with self.assertRaises(Exception):  # FrozenInstanceError, AttributeError on python <3.11
            snap.cache_hits = 999  # type: ignore[misc]


class TestRecordCacheHit(unittest.TestCase):
    def setUp(self) -> None:
        reset()

    def test_basic_increment(self) -> None:
        record_cache_hit(model="gpt-4o", tokens_in=1_000_000, tokens_out=500_000)
        snap = savings()
        self.assertEqual(snap.cache_hits, 1)
        self.assertEqual(snap.tokens_saved_in, 1_000_000)
        self.assertEqual(snap.tokens_saved_out, 500_000)
        # gpt-4o = 2.50 in / 10.00 out per 1M tokens
        # 1M * 2.50 + 0.5M * 10.00 = 2.50 + 5.00 = 7.50 USD
        self.assertAlmostEqual(snap.cost_saved_usd_estimate, 7.50, places=4)

    def test_multiple_hits_accumulate(self) -> None:
        record_cache_hit(model="gpt-4o-mini", tokens_in=1000, tokens_out=500)
        record_cache_hit(model="gpt-4o-mini", tokens_in=2000, tokens_out=1000)
        snap = savings()
        self.assertEqual(snap.cache_hits, 2)
        self.assertEqual(snap.tokens_saved_in, 3000)
        self.assertEqual(snap.tokens_saved_out, 1500)
        # 2 calls × (1k * 0.15 + 0.5k * 0.60) and (2k * 0.15 + 1k * 0.60), per million
        # = (1000*0.15 + 500*0.60 + 2000*0.15 + 1000*0.60) / 1_000_000
        # = (150 + 300 + 300 + 600) / 1_000_000 = 1350/1M = 0.00135
        self.assertAlmostEqual(snap.cost_saved_usd_estimate, 0.00135, places=6)

    def test_unknown_model_zero_cost_but_tokens_count(self) -> None:
        record_cache_hit(model="bespoke-private-model-v1", tokens_in=10_000, tokens_out=5_000)
        snap = savings()
        # Tokens still counted toward totals — useful even when we
        # can't price them.
        self.assertEqual(snap.cache_hits, 1)
        self.assertEqual(snap.tokens_saved_in, 10_000)
        self.assertEqual(snap.tokens_saved_out, 5_000)
        self.assertEqual(snap.cost_saved_usd_estimate, 0.0)

    def test_negative_tokens_clamped_to_zero(self) -> None:
        # Defensive against malformed step records (a corrupt
        # response that yielded negative usage). The counter
        # shouldn't propagate the corruption.
        record_cache_hit(model="gpt-4o", tokens_in=-100, tokens_out=-50)
        snap = savings()
        self.assertEqual(snap.tokens_saved_in, 0)
        self.assertEqual(snap.tokens_saved_out, 0)
        self.assertEqual(snap.cost_saved_usd_estimate, 0.0)
        # The hit still counts — we did serve a cached response.
        self.assertEqual(snap.cache_hits, 1)

    def test_non_int_tokens_clamped_to_zero(self) -> None:
        # Defensive: malformed step records or extractor edge cases.
        # ``isinstance(..., int)`` filter in record_cache_hit should
        # silently zero non-ints rather than raise.
        record_cache_hit(model="gpt-4o", tokens_in="oops", tokens_out=None)  # type: ignore[arg-type]
        snap = savings()
        self.assertEqual(snap.tokens_saved_in, 0)
        self.assertEqual(snap.tokens_saved_out, 0)
        self.assertEqual(snap.cache_hits, 1)


class TestPrefixMatching(unittest.TestCase):
    def setUp(self) -> None:
        reset()

    def test_long_prefix_wins_over_short(self) -> None:
        # gpt-4o-mini and gpt-4o both prefix "gpt-4o-mini-2024-07-18".
        # Longest-prefix wins ⇒ gpt-4o-mini's pricing.
        record_cache_hit(
            model="gpt-4o-mini-2024-07-18", tokens_in=1_000_000, tokens_out=1_000_000
        )
        snap = savings()
        # gpt-4o-mini: 0.15 in, 0.60 out → 0.75 USD
        self.assertAlmostEqual(snap.cost_saved_usd_estimate, 0.75, places=4)

    def test_case_insensitive_match(self) -> None:
        # User-supplied model name might be uppercased; pricing match
        # should still fire.
        record_cache_hit(model="GPT-4O", tokens_in=1_000_000, tokens_out=0)
        snap = savings()
        self.assertAlmostEqual(snap.cost_saved_usd_estimate, 2.50, places=4)

    def test_empty_model_zero_cost(self) -> None:
        record_cache_hit(model="", tokens_in=1_000_000, tokens_out=1_000_000)
        snap = savings()
        self.assertEqual(snap.cost_saved_usd_estimate, 0.0)


class TestCustomCostTable(unittest.TestCase):
    def setUp(self) -> None:
        reset()

    def test_override_pricing(self) -> None:
        # Self-hosted model with operator-known pricing.
        custom = {"my-private-llama": (0.10, 0.20)}
        record_cache_hit(
            model="my-private-llama-7b",
            tokens_in=1_000_000,
            tokens_out=500_000,
            cost_table=custom,
        )
        snap = savings()
        # 1M * 0.10 + 0.5M * 0.20 = 0.10 + 0.10 = 0.20
        self.assertAlmostEqual(snap.cost_saved_usd_estimate, 0.20, places=4)


class TestThreadSafety(unittest.TestCase):
    def test_concurrent_increments_match_serial_total(self) -> None:
        reset()
        n_threads = 16
        per_thread_calls = 100

        def worker() -> None:
            for _ in range(per_thread_calls):
                record_cache_hit(model="gpt-4o", tokens_in=10, tokens_out=5)

        threads = [threading.Thread(target=worker) for _ in range(n_threads)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        snap = savings()
        # Serial-equivalent total — proves the lock prevents lost
        # increments under contention.
        expected_hits = n_threads * per_thread_calls
        expected_in = expected_hits * 10
        expected_out = expected_hits * 5
        self.assertEqual(snap.cache_hits, expected_hits)
        self.assertEqual(snap.tokens_saved_in, expected_in)
        self.assertEqual(snap.tokens_saved_out, expected_out)


class TestResetClearsAllFields(unittest.TestCase):
    def test_reset_zeros_everything(self) -> None:
        record_cache_hit(model="gpt-4o", tokens_in=1000, tokens_out=500)
        record_cache_hit(model="claude-3-5-sonnet", tokens_in=2000, tokens_out=1000)
        before = savings()
        self.assertGreater(before.cache_hits, 0)

        reset()
        after = savings()
        self.assertEqual(after.cache_hits, 0)
        self.assertEqual(after.tokens_saved_in, 0)
        self.assertEqual(after.tokens_saved_out, 0)
        self.assertEqual(after.cost_saved_usd_estimate, 0.0)


class TestPricingTableInvariants(unittest.TestCase):
    """Sanity checks on the pricing table itself, so a bad merge
    doesn't ship completely-wrong numbers."""

    def test_all_keys_lowercase(self) -> None:
        # Prefix matching lowercases the input; table keys must too.
        for key in MODEL_PRICING_USD_PER_MILLION:
            self.assertEqual(key, key.lower(), f"{key!r} should be lowercase")

    def test_all_values_are_pairs_of_positive_floats(self) -> None:
        for key, value in MODEL_PRICING_USD_PER_MILLION.items():
            self.assertEqual(len(value), 2, f"{key!r} should have exactly 2 prices")
            inp, out = value
            self.assertIsInstance(inp, float, f"{key!r} input price should be float")
            self.assertIsInstance(out, float, f"{key!r} output price should be float")
            self.assertGreater(inp, 0, f"{key!r} input price should be > 0")
            self.assertGreater(out, 0, f"{key!r} output price should be > 0")


if __name__ == "__main__":
    unittest.main()
