use anyhow::{Context, Result};
use rewind_store::models::{Session, Span, Step, Timeline};
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
    pub spans: Vec<Span>,
    /// Parsed request blobs keyed by SHA-256 hash.
    pub request_blobs: HashMap<String, Value>,
    /// Parsed response blobs keyed by SHA-256 hash.
    pub response_blobs: HashMap<String, Value>,
}

/// Options controlling which timelines to extract.
#[derive(Debug, Clone)]
pub struct ExtractOptions {
    /// Export a specific timeline by ID. None = main timeline.
    pub timeline_id: Option<String>,
    /// Export all timelines (overrides timeline_id).
    pub all_timelines: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            timeline_id: None,
            all_timelines: false,
        }
    }
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

    // 3. Get steps per timeline + collect blob hashes
    let mut steps_by_timeline = HashMap::new();
    let mut blob_hashes: Vec<String> = Vec::new();

    for tl in &timelines {
        let steps = store.get_steps(&tl.id)?;
        for step in &steps {
            if !step.request_blob.is_empty() {
                blob_hashes.push(step.request_blob.clone());
            }
            if !step.response_blob.is_empty() {
                blob_hashes.push(step.response_blob.clone());
            }
        }
        steps_by_timeline.insert(tl.id.clone(), steps);
    }

    // 4. Get spans
    let spans = store.get_spans_by_session(session_id)?;

    // 5. Resolve blobs (deduplicated)
    blob_hashes.sort();
    blob_hashes.dedup();

    let mut request_blobs = HashMap::new();
    let mut response_blobs = HashMap::new();

    for hash in &blob_hashes {
        if let Ok(value) = store.blobs.get_json::<Value>(hash) {
            // Store in both maps — we don't know which is request vs response
            // until we look it up by the step's field. Both maps share the same pool.
            request_blobs.insert(hash.clone(), value.clone());
            response_blobs.insert(hash.clone(), value);
        } else {
            tracing::warn!(hash = %hash, "Failed to read blob, skipping");
        }
    }

    Ok(SessionExportData {
        session,
        timelines,
        steps_by_timeline,
        spans,
        request_blobs,
        response_blobs,
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

    /// Look up a request blob by its hash.
    pub fn get_request_blob(&self, hash: &str) -> Option<&Value> {
        self.request_blobs.get(hash)
    }

    /// Look up a response blob by its hash.
    pub fn get_response_blob(&self, hash: &str) -> Option<&Value> {
        self.response_blobs.get(hash)
    }
}
