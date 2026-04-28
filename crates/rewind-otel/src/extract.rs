use anyhow::{Context, Result};
use rewind_store::models::{Session, Step, Timeline};
use rewind_store::Store;
use serde_json::Value;
use std::collections::HashMap;

/// All data needed for OTel export, pre-loaded synchronously from the Store.
/// This struct is Send + Sync (no rusqlite::Connection) so it can cross async boundaries.
#[derive(Debug)]
pub struct SessionExportData {
    pub session: Session,
    pub timelines: Vec<Timeline>,
    pub steps_by_timeline: HashMap<String, Vec<Step>>,
    /// Parsed blobs (request + response) keyed by SHA-256 hash.
    pub blobs: HashMap<String, Value>,
}

/// Options controlling which timelines to extract.
#[derive(Debug, Clone, Default)]
pub struct ExtractOptions {
    /// Export a specific timeline by ID. None = main timeline.
    pub timeline_id: Option<String>,
    /// Export all timelines (overrides timeline_id).
    pub all_timelines: bool,
}

/// Extract all data needed for OTel export from the Store (synchronous).
///
/// This must be called before entering an async context because Store
/// wraps rusqlite::Connection which is not Send/Sync.
pub fn extract_session_data(
    store: &Store,
    session_id: &str,
    opts: &ExtractOptions,
) -> Result<SessionExportData> {
    // 1. Get session
    let session = store
        .get_session(session_id)?
        .with_context(|| format!("Session not found: {}", session_id))?;

    // 2. Get timelines and filter
    let all_timelines = store.get_timelines(session_id)?;
    let timelines = select_timelines(&all_timelines, opts)?;

    // 3. Get steps per timeline + collect blob references.
    //
    // Step 0.3 (Phase 0 follow-up): response blobs need envelope-aware
    // unwrap, but the unwrap depends on the per-step
    // `response_blob_format`. Track request blobs (always naked JSON)
    // separately from response blob references (hash + format pair) so
    // the resolution pass can apply the correct decoder.
    //
    // Same hash always implies same bytes (content-addressed) which
    // implies same format, so we collect (hash, format) pairs and
    // dedup on hash alone — the format is consistent across uses.
    let mut steps_by_timeline = HashMap::new();
    let mut request_hashes: Vec<String> = Vec::new();
    let mut response_refs: Vec<(String, u8)> = Vec::new();

    for tl in &timelines {
        let steps = store.get_steps(&tl.id)?;
        for step in &steps {
            if !step.request_blob.is_empty() {
                request_hashes.push(step.request_blob.clone());
            }
            if !step.response_blob.is_empty() {
                response_refs.push((step.response_blob.clone(), step.response_blob_format));
            }
        }
        steps_by_timeline.insert(tl.id.clone(), steps);
    }

    request_hashes.sort();
    request_hashes.dedup();
    response_refs.sort_by(|a, b| a.0.cmp(&b.0));
    response_refs.dedup_by(|a, b| a.0 == b.0);

    let mut blobs = HashMap::new();

    // 4a. Request blobs are always raw JSON (no envelope), so a direct
    // `get_json` is correct.
    for hash in &request_hashes {
        match store.blobs.get_json::<Value>(hash) {
            Ok(value) => {
                blobs.insert(hash.clone(), value);
            }
            Err(e) => {
                tracing::warn!(hash = %hash, error = %e, "Failed to read request blob, skipping");
            }
        }
    }

    // 4b. Response blobs go through the envelope-aware decoder. The
    // resulting cached `Value` is the inner model response, so
    // downstream consumers (`attributes::llm_call_attributes`,
    // `gen_ai.output.messages` extraction) operate on the same shape
    // they did pre-v0.13. Pre-migration format=0 blobs round-trip via
    // `from_blob_bytes`'s legacy fallback.
    for (hash, format) in &response_refs {
        let raw = match store.blobs.get(hash) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(hash = %hash, error = %e, "Failed to read response blob, skipping");
                continue;
            }
        };
        let envelope = rewind_store::ResponseEnvelope::from_blob_bytes(*format, &raw);
        match serde_json::from_slice::<Value>(&envelope.body) {
            Ok(value) => {
                blobs.insert(hash.clone(), value);
            }
            Err(e) => {
                tracing::warn!(hash = %hash, error = %e, "Response blob is not valid JSON; skipping");
            }
        }
    }

    Ok(SessionExportData {
        session,
        timelines,
        steps_by_timeline,
        blobs,
    })
}

/// Select timelines based on extraction options.
fn select_timelines(all: &[Timeline], opts: &ExtractOptions) -> Result<Vec<Timeline>> {
    if opts.all_timelines {
        return Ok(all.to_vec());
    }

    if let Some(ref id) = opts.timeline_id {
        let tl = all
            .iter()
            .find(|t| t.id == *id)
            .with_context(|| format!("Timeline not found: {}", id))?;
        return Ok(vec![tl.clone()]);
    }

    // Default: main timeline (first one, which is the root — no parent)
    let main = all
        .iter()
        .find(|t| t.parent_timeline_id.is_none())
        .or_else(|| all.first())
        .with_context(|| "Session has no timelines")?;

    Ok(vec![main.clone()])
}

impl SessionExportData {
    /// Total number of steps across all selected timelines.
    pub fn total_steps(&self) -> usize {
        self.steps_by_timeline.values().map(|s| s.len()).sum()
    }

    /// Look up a blob by its SHA-256 hash.
    pub fn get_blob(&self, hash: &str) -> Option<&Value> {
        self.blobs.get(hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rewind_store::{ResponseEnvelope, FORMAT_ENVELOPE_V1, FORMAT_NAKED_LEGACY};

    fn build_store_with_step(
        response_bytes: Vec<u8>,
        format: u8,
    ) -> (Store, tempfile::TempDir, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();

        let session = rewind_store::Session {
            id: "sess-otel".into(),
            name: "otel-test".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            status: rewind_store::SessionStatus::Recording,
            source: rewind_store::SessionSource::Hooks,
            total_steps: 0,
            total_tokens: 0,
            metadata: serde_json::json!({}),
            thread_id: None,
            thread_ordinal: None,
            client_session_key: None,
        };
        store.create_session(&session).unwrap();
        let timeline = rewind_store::Timeline::new_root(&session.id);
        store.create_timeline(&timeline).unwrap();

        let resp_hash = store.blobs.put(&response_bytes).unwrap();
        let mut step = rewind_store::Step::new_llm_call(&timeline.id, &session.id, 1, "gpt-4o");
        step.response_blob = resp_hash.clone();
        step.response_blob_format = format;
        step.tokens_in = 100;
        step.tokens_out = 50;
        store.create_step(&step).unwrap();

        (store, dir, session.id, resp_hash)
    }

    /// Phase 0 follow-up: OTel export pipeline must hand `attributes.rs`
    /// the INNER body, not the envelope wrapper. Otherwise span attributes
    /// like `gen_ai.output.messages` would contain `{status: 200, headers,
    /// body}` instead of the model response, polluting Datadog/Jaeger
    /// dashboards downstream.
    #[test]
    fn extract_unwraps_envelope_format_response_blob() {
        let inner_body = br#"{"choices":[{"message":{"role":"assistant","content":"hello"}}]}"#;
        let envelope = ResponseEnvelope {
            status: 200,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: inner_body.to_vec(),
        };
        let envelope_bytes = serde_json::to_vec(&envelope).unwrap();
        let (store, _dir, session_id, resp_hash) =
            build_store_with_step(envelope_bytes, FORMAT_ENVELOPE_V1);

        let data = extract_session_data(&store, &session_id, &ExtractOptions::default()).unwrap();
        let cached = data.get_blob(&resp_hash).expect("response blob cached");

        // Critical assertion: the cached value is the INNER body (model
        // response shape), not the envelope wrapper. If this fails,
        // every OTel export downstream of this regresses.
        assert_eq!(
            cached.pointer("/choices/0/message/content").and_then(|v| v.as_str()),
            Some("hello"),
            "OTel cache must hold the inner body, not the envelope wrapper",
        );
        assert!(
            cached.get("status").is_none(),
            "envelope 'status' field leaked into the OTel attribute pipeline"
        );
        assert!(
            cached.get("headers").is_none(),
            "envelope 'headers' field leaked into the OTel attribute pipeline"
        );
        assert!(
            cached.get("body").is_none(),
            "envelope 'body' field leaked into the OTel attribute pipeline"
        );
    }

    /// Phase 0 follow-up: pre-migration data (format=0) round-trips
    /// unchanged. Existing v0.12.x DBs upgraded to v0.13 read identically
    /// to before — no migration script required for blob content,
    /// the column DEFAULT 0 + legacy decoder do the work.
    #[test]
    fn extract_round_trips_legacy_format_response_blob() {
        let raw = br#"{"choices":[{"message":{"role":"assistant","content":"legacy"}}]}"#;
        let (store, _dir, session_id, resp_hash) =
            build_store_with_step(raw.to_vec(), FORMAT_NAKED_LEGACY);

        let data = extract_session_data(&store, &session_id, &ExtractOptions::default()).unwrap();
        let cached = data.get_blob(&resp_hash).expect("response blob cached");

        assert_eq!(
            cached.pointer("/choices/0/message/content").and_then(|v| v.as_str()),
            Some("legacy"),
            "legacy format reads identically to pre-v0.13",
        );
    }
}
