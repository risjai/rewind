#!/bin/bash
#
# SQL Query Explorer — Run ad-hoc queries against the Rewind database.
#
# Rewind stores everything in SQLite. The `rewind query` command gives you
# read-only SQL access to sessions, steps, timelines, baselines, and more.
#
# Setup:
#     # Seed demo data first (no API keys needed)
#     rewind demo
#
#     # Then run this script
#     bash examples/13_sql_queries.sh

set -e

echo "=== Rewind SQL Query Explorer ==="
echo
echo "First, let's see what tables are available:"
echo '  $ rewind query --tables'
echo

# ── 1. List all sessions with their status ────────────────────────
echo "--- 1. Sessions overview ---"
rewind query "
  SELECT name, status, total_steps, total_tokens,
         created_at
  FROM sessions
  ORDER BY created_at DESC
"
echo

# ── 2. Token usage by model ──────────────────────────────────────
echo "--- 2. Token usage by model ---"
rewind query "
  SELECT model,
         COUNT(*) as calls,
         SUM(tokens_in) as total_in,
         SUM(tokens_out) as total_out,
         SUM(tokens_in + tokens_out) as total_tokens
  FROM steps
  WHERE model != 'tool'
  GROUP BY model
"
echo

# ── 3. Step-by-step trace with timing ────────────────────────────
echo "--- 3. Step trace with timing ---"
rewind query "
  SELECT step_number, step_type, status, model,
         tokens_in || '↓ ' || tokens_out || '↑' as tokens,
         duration_ms || 'ms' as duration,
         CASE WHEN error IS NOT NULL
              THEN substr(error, 1, 60) || '...'
              ELSE 'OK' END as result
  FROM steps
  ORDER BY timeline_id, step_number
"
echo

# ── 4. Find the failing step ─────────────────────────────────────
echo "--- 4. Steps with errors ---"
rewind query "
  SELECT s.name as session,
         st.step_number,
         st.model,
         st.error
  FROM steps st
  JOIN sessions s ON st.session_id = s.id
  WHERE st.status = 'error'
"
echo

# ── 5. Average duration by step type ─────────────────────────────
echo "--- 5. Average duration by step type ---"
rewind query "
  SELECT step_type,
         COUNT(*) as count,
         ROUND(AVG(duration_ms)) as avg_ms,
         MAX(duration_ms) as max_ms
  FROM steps
  GROUP BY step_type
"
echo

# ── 6. Timeline comparison (main vs fork) ────────────────────────
echo "--- 6. Timelines in the demo session ---"
rewind query "
  SELECT t.label,
         t.fork_at_step,
         COUNT(st.id) as steps,
         SUM(st.tokens_in + st.tokens_out) as tokens
  FROM timelines t
  LEFT JOIN steps st ON st.timeline_id = t.id
  GROUP BY t.id
"
echo

# ── 7. Baseline step signatures ──────────────────────────────────
echo "--- 7. Baseline expected step signatures ---"
rewind query "
  SELECT b.name as baseline,
         bs.step_number,
         bs.step_type,
         bs.expected_status,
         bs.expected_model,
         bs.tokens_in || '↓ ' || bs.tokens_out || '↑' as expected_tokens
  FROM baseline_steps bs
  JOIN baselines b ON bs.baseline_id = b.id
  ORDER BY bs.step_number
"
echo

# ── 8. Cost estimation (approximate) ─────────────────────────────
echo "--- 8. Estimated cost per session (GPT-4o pricing) ---"
rewind query "
  SELECT s.name,
         SUM(st.tokens_in) as input_tokens,
         SUM(st.tokens_out) as output_tokens,
         ROUND(SUM(st.tokens_in) * 2.50 / 1000000
             + SUM(st.tokens_out) * 10.00 / 1000000, 4) as est_cost_usd
  FROM steps st
  JOIN sessions s ON st.session_id = s.id
  WHERE st.model != 'tool'
  GROUP BY s.id
"
echo

echo "Done! All queries are read-only — safe to explore."
echo "Try your own: rewind query \"SELECT ...\""
