# SQL Query Explorer -- Ad-Hoc Analytics on Recordings

Rewind is a time-travel debugger for AI agents that records every LLM call for inspection, forking, replay, and diffing. The SQL query explorer lets you run ad-hoc analytics directly against the Rewind database.

## Discover the Schema

```bash
# See all tables and their schemas
rewind query --tables
```

## Example Queries

### Token usage by model

```bash
rewind query "SELECT model, COUNT(*) as calls, SUM(tokens_in + tokens_out) as tokens FROM steps GROUP BY model"
```

### Average step duration by type

```bash
rewind query "SELECT step_type, AVG(duration_ms) as avg_ms FROM steps GROUP BY step_type"
```

### Sessions with errors

```bash
rewind query "SELECT s.name, COUNT(*) as errors FROM steps st JOIN sessions s ON st.session_id = s.id WHERE st.status = 'error' GROUP BY s.name"
```

## Read-Only Safety

Only `SELECT`, `WITH`, `EXPLAIN`, and `PRAGMA` statements are allowed. Write operations are rejected, so you can query safely without risk of corrupting your recordings.
