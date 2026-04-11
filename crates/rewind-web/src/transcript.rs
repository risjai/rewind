//! Transcript parsing for Claude Code JSONL transcript files.
//!
//! Claude Code writes a JSONL transcript to `~/.claude/projects/{project}/{session-id}.jsonl`.
//! Each line is a JSON object; we care about entries with `"type": "assistant"` that contain
//! token usage data in `message.usage`.
//!
//! This module provides:
//! - Parsing of transcript files to extract aggregated token counts
//! - A background sync function that updates hook sessions with real token data

use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::AppState;

// ── Transcript entry types (minimal) ────────────────────────

/// We only need to parse enough of each JSONL line to extract token usage.
/// Serde will skip unknown fields.
#[derive(serde::Deserialize)]
struct TranscriptEntry {
    #[serde(rename = "type")]
    entry_type: Option<String>,
    message: Option<AssistantMessage>,
}

#[derive(serde::Deserialize)]
struct AssistantMessage {
    model: Option<String>,
    usage: Option<TokenUsage>,
}

#[derive(serde::Deserialize)]
struct TokenUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

// ── Aggregated result ───────────────────────────────────────

struct TranscriptSummary {
    total_input_tokens: u64,
    total_output_tokens: u64,
    total_cache_read_tokens: u64,
    total_cache_creation_tokens: u64,
    model: Option<String>,
}

impl TranscriptSummary {
    /// New tokens only (input + output) — what you pay full price for.
    fn new_tokens(&self) -> u64 {
        self.total_input_tokens + self.total_output_tokens
    }

    /// Cache tokens (read + creation) — served from prompt cache at 90% discount.
    fn cache_tokens(&self) -> u64 {
        self.total_cache_read_tokens + self.total_cache_creation_tokens
    }
}

// ── Parsing ─────────────────────────────────────────────────

/// Parse a Claude Code JSONL transcript file and aggregate token usage.
///
/// Reads line-by-line to handle files that are being actively written to.
/// Malformed or incomplete lines are silently skipped.
fn parse_transcript(path: &Path) -> anyhow::Result<TranscriptSummary> {
    let file = std::fs::File::open(path)
        .map_err(|e| anyhow::anyhow!("Failed to open transcript {}: {e}", path.display()))?;

    let reader = BufReader::new(file);

    let mut summary = TranscriptSummary {
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        total_cache_creation_tokens: 0,
        model: None,
    };

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue, // I/O error on a line, skip
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Parse the line; skip if malformed (common for partial writes)
        let entry: TranscriptEntry = match serde_json::from_str(trimmed) {
            Ok(e) => e,
            Err(_) => continue,
        };

        // We only care about "assistant" entries
        if entry.entry_type.as_deref() != Some("assistant") {
            continue;
        }

        let message = match entry.message {
            Some(m) => m,
            None => continue,
        };

        // Capture model from the first assistant entry
        if summary.model.is_none() {
            summary.model = message.model;
        }

        if let Some(usage) = message.usage {
            summary.total_input_tokens += usage.input_tokens;
            summary.total_output_tokens += usage.output_tokens;
            summary.total_cache_read_tokens += usage.cache_read_input_tokens;
            summary.total_cache_creation_tokens += usage.cache_creation_input_tokens;
        }
    }

    Ok(summary)
}

// ── Sync function ───────────────────────────────────────────

/// Iterate over all active hook sessions, read their transcript files,
/// and update the session's total_tokens in the store.
///
/// Returns the number of sessions successfully updated.
pub fn sync_transcript_tokens(state: &AppState) -> anyhow::Result<usize> {
    let mut updated = 0usize;

    // Collect session info without holding store lock during I/O.
    // We iterate the DashMap (in-memory hook sessions) and for each,
    // look up the transcript_path from the store's session metadata.
    let sessions_to_check: Vec<(String, String)> = {
        // (rewind_session_id, claude_session_id)
        state
            .hooks
            .sessions
            .iter()
            .map(|entry| {
                let claude_id = entry.key().clone();
                let rewind_id = entry.value().session_id.clone();
                (rewind_id, claude_id)
            })
            .collect()
    };

    for (rewind_session_id, _claude_session_id) in &sessions_to_check {
        // Read transcript_path from session metadata (brief lock)
        let transcript_path = {
            let store = match state.store.lock() {
                Ok(s) => s,
                Err(_) => continue,
            };
            let session = match store.get_session(rewind_session_id) {
                Ok(Some(s)) => s,
                _ => continue,
            };
            session
                .metadata
                .get("transcript_path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        };

        let transcript_path = match transcript_path {
            Some(p) => p,
            None => continue, // No transcript_path in metadata
        };

        let path = Path::new(&transcript_path);
        if !path.exists() {
            continue;
        }

        // Parse transcript (no lock held during file I/O)
        let summary = match parse_transcript(path) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("Transcript parse error for {}: {e}", transcript_path);
                continue;
            }
        };

        let new_tokens = summary.new_tokens();
        let cache_tokens = summary.cache_tokens();
        if new_tokens == 0 && cache_tokens == 0 {
            continue; // No tokens found yet
        }

        // Update the store (brief lock)
        {
            let store = match state.store.lock() {
                Ok(s) => s,
                Err(_) => continue,
            };

            let session = match store.get_session(rewind_session_id) {
                Ok(Some(s)) => s,
                _ => continue,
            };

            // Check if anything changed
            let stored_cache = session.metadata.get("cache_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            if session.total_tokens != new_tokens || stored_cache != cache_tokens {
                // total_tokens = new tokens only (input + output)
                if let Err(e) = store.update_session_tokens(rewind_session_id, new_tokens) {
                    tracing::debug!("Failed to update session tokens: {e}");
                    continue;
                }

                // Store cache_tokens and model in metadata
                let mut meta = session.metadata.clone();
                meta["cache_tokens"] = serde_json::json!(cache_tokens);
                if let Some(ref model) = summary.model {
                    meta["model"] = serde_json::json!(model);
                }
                let _ = store.update_session_metadata(rewind_session_id, &meta);

                // Emit a session update event for WebSocket live updates
                let _ = state.event_tx.send(crate::StoreEvent::SessionUpdated {
                    session_id: rewind_session_id.clone(),
                    status: session.status.as_str().to_string(),
                    total_steps: session.total_steps,
                    total_tokens: new_tokens,
                });

                updated += 1;
            }
        }
    }

    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_parse_transcript_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();

        // Write a few transcript entries
        writeln!(file, r#"{{"type":"human","message":{{"content":"hello"}},"timestamp":"2026-04-11T00:00:00Z"}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"model":"claude-opus-4-6","usage":{{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":200,"cache_creation_input_tokens":1000}},"stop_reason":"tool_use","content":[]}},"timestamp":"2026-04-11T00:00:01Z"}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"model":"claude-opus-4-6","usage":{{"input_tokens":150,"output_tokens":75,"cache_read_input_tokens":300,"cache_creation_input_tokens":0}},"stop_reason":"end_turn","content":[]}},"timestamp":"2026-04-11T00:00:02Z"}}"#).unwrap();
        // Malformed line (simulating partial write)
        writeln!(file, r#"{{"type":"assistant","message":{{"model":"claude-opus-4-6","usag"#).unwrap();

        let summary = parse_transcript(&path).unwrap();
        assert_eq!(summary.total_input_tokens, 250);
        assert_eq!(summary.total_output_tokens, 125);
        assert_eq!(summary.total_cache_read_tokens, 500);
        assert_eq!(summary.total_cache_creation_tokens, 1000);
        assert_eq!(summary.model.as_deref(), Some("claude-opus-4-6"));
        // new_tokens = 250 + 125 = 375
        assert_eq!(summary.new_tokens(), 375);
        // cache_tokens = 500 + 1000 = 1500
        assert_eq!(summary.cache_tokens(), 1500);
    }

    #[test]
    fn test_parse_transcript_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        std::fs::File::create(&path).unwrap();

        let summary = parse_transcript(&path).unwrap();
        assert_eq!(summary.new_tokens(), 0);
        assert!(summary.model.is_none());
    }

    #[test]
    fn test_parse_transcript_no_assistant_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no_assistant.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, r#"{{"type":"human","message":{{"content":"hello"}},"timestamp":"2026-04-11T00:00:00Z"}}"#).unwrap();
        writeln!(file, r#"{{"type":"system","message":{{"content":"init"}},"timestamp":"2026-04-11T00:00:00Z"}}"#).unwrap();

        let summary = parse_transcript(&path).unwrap();
        assert_eq!(summary.new_tokens(), 0);
        assert!(summary.model.is_none());
    }

    #[test]
    fn test_parse_transcript_missing_file() {
        let result = parse_transcript(Path::new("/nonexistent/path/foo.jsonl"));
        assert!(result.is_err());
    }
}
