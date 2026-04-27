//! Canonical hashing for replay-cache content validation.
//!
//! Single source of truth for the post-redaction request hash recorded on
//! each step (`Step::request_hash`) and computed at lookup time
//! (`do_replay_lookup`). Both record paths (proxy and explicit-API) and the
//! lookup path call `normalize_and_hash` on the request body so that hash
//! comparisons are deterministic across paths.
//!
//! The function does **not** parse or canonicalize JSON shape (key ordering,
//! whitespace). Two semantically-equivalent bodies with differing serialization
//! will hash differently — callers that need shape-canonical hashing should
//! re-serialize their JSON via `serde_json` before passing in. v1 favors
//! determinism over canonicalization to keep the hash trivially auditable.
//!
//! # Determinism
//!
//! Given the same `body`, this function MUST return the same hash regardless
//! of which crate calls it. The redaction passes (`redact::redact_request_body`)
//! are pure functions over byte slices. SHA-256 is deterministic. The only
//! way for hashes to diverge across record/lookup is if the redaction patterns
//! drift — which is why `redact` lives in `rewind-store` (a single crate) and
//! is not re-implemented by callers.

use sha2::{Digest, Sha256};

use crate::redact;

/// Compute the canonical post-redaction SHA-256 hash of a request body.
///
/// Returns lower-case hex (e.g. `"a3f1c9..."`) — same format as the
/// content-addressed blob store's hash, so existing tooling that displays
/// hashes can render this column without special handling.
///
/// Pre-migration steps (`request_hash IS NULL`) bypass content validation
/// entirely; this function is only called for new writes (record path) and
/// for lookups against post-migration steps.
pub fn normalize_and_hash(body: &[u8]) -> String {
    let redacted = redact::redact_request_body(body);
    let mut hasher = Sha256::new();
    hasher.update(&redacted);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_across_invocations() {
        let body = br#"{"model":"gpt-4o","messages":[{"role":"user","content":"hello"}]}"#;
        let h1 = normalize_and_hash(body);
        let h2 = normalize_and_hash(body);
        assert_eq!(h1, h2, "same input must produce same hash");
        assert_eq!(h1.len(), 64, "SHA-256 hex is 64 chars");
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()), "hex output");
    }

    #[test]
    fn redaction_normalizes_secrets() {
        // Same semantic request, different bearer tokens. After redaction the
        // tokens become "[REDACTED]" so the hashes match. This is the key
        // property — rotating credentials don't break replay cache.
        let body_a = br#"{"model":"gpt-4o","authorization":"Bearer abc123longabc123long"}"#;
        let body_b = br#"{"model":"gpt-4o","authorization":"Bearer xyz789longxyz789long"}"#;
        assert_eq!(
            normalize_and_hash(body_a),
            normalize_and_hash(body_b),
            "redacted secrets must collide so replay survives credential rotation"
        );
    }

    #[test]
    fn different_payloads_diverge() {
        let body_a = br#"{"messages":[{"role":"user","content":"hello"}]}"#;
        let body_b = br#"{"messages":[{"role":"user","content":"goodbye"}]}"#;
        assert_ne!(
            normalize_and_hash(body_a),
            normalize_and_hash(body_b),
            "semantically-distinct bodies must hash differently"
        );
    }

    #[test]
    fn empty_body_hashes_consistently() {
        // Edge case — some hook events have no body. Should not panic.
        let h = normalize_and_hash(b"");
        assert_eq!(h.len(), 64);
        // SHA-256 of redact_request_body(b"") which is still b""
        // = empty-string SHA-256 = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        assert_eq!(
            h,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn non_utf8_body_does_not_panic() {
        // redact_request_body returns input unchanged for non-UTF-8; we
        // just need to make sure normalize_and_hash itself doesn't blow up.
        let body = &[0xff, 0xfe, 0xfd, 0xfc, 0xfb];
        let h = normalize_and_hash(body);
        assert_eq!(h.len(), 64);
    }
}
