# SQL Query Explorer — Ad-Hoc Analytics on Recordings

Rewind is a time-travel debugger for AI agents that records every LLM call for inspection, forking, replay, and diffing. The `rewind query` command gives you read-only SQL access to sessions, steps, timelines, baselines, and more.

> **Try it:** Run `rewind demo` to seed sample data, then copy-paste any query below.

## Discover the Schema

```bash
rewind query --tables
```

## Example Queries

### Sessions overview

```bash
rewind query "
  SELECT name, status, total_steps, total_tokens, created_at
  FROM sessions
  ORDER BY created_at DESC
"
```

### Token usage by model

```bash
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
```

### Step-by-step trace with timing

```bash
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
```

### Find failing steps

```bash
rewind query "
  SELECT s.name as session,
         st.step_number,
         st.model,
         st.error
  FROM steps st
  JOIN sessions s ON st.session_id = s.id
  WHERE st.status = 'error'
"
```

### Average duration by step type

```bash
rewind query "
  SELECT step_type,
         COUNT(*) as count,
         ROUND(AVG(duration_ms)) as avg_ms,
         MAX(duration_ms) as max_ms
  FROM steps
  GROUP BY step_type
"
```

### Timeline comparison (main vs fork)

```bash
rewind query "
  SELECT t.label,
         t.fork_at_step,
         COUNT(st.id) as steps,
         SUM(st.tokens_in + st.tokens_out) as tokens
  FROM timelines t
  LEFT JOIN steps st ON st.timeline_id = t.id
  GROUP BY t.id
"
```

### Baseline expected step signatures

```bash
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
```

### Cost estimation (GPT-4o pricing)

```bash
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
```

## Read-Only Safety

Only `SELECT`, `WITH`, `EXPLAIN`, and `PRAGMA` statements are allowed. Write operations are rejected, so you can query safely without risk of corrupting your recordings.
