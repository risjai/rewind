//! Secret redaction for recorded request and response blobs.
//!
//! See `docs/security-audit.md` §HIGH-01, §HIGH-06, §MEDIUM-06.
//!
//! Redaction is applied **before** writing to the blob store so that secrets
//! never reach disk. The redacted form is `[REDACTED]`.
//!
//! Lives in `rewind-store` (rather than `rewind-proxy`) so both proxy-record
//! and explicit-API-record paths can apply identical redaction passes — a
//! prerequisite for `normalize_and_hash` (cache-validation hashing must be
//! deterministic across record paths).

use std::sync::LazyLock;
use regex::Regex;

const REPLACEMENT: &str = "[REDACTED]";

/// Headers that must be stripped from request blobs before storage.
/// Checked case-insensitively.
const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "cookie",
    "proxy-authorization",
    "x-auth-token",
];

/// Hop-by-hop headers that proxies must not forward to upstream (RFC 7230 §6.1).
pub const HOP_BY_HOP_HEADERS: &[&str] = &[
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
    "proxy-authorization",
    "proxy-connection",
    "keep-alive",
    "expect",
];

/// Returns true if `header_name` is a hop-by-hop header (case-insensitive).
pub fn is_hop_by_hop(header_name: &str) -> bool {
    HOP_BY_HOP_HEADERS.contains(&header_name.to_ascii_lowercase().as_str())
}

/// Redact known secret patterns from a byte blob (JSON or raw text).
/// Returns a new `Vec<u8>` with secrets replaced by `[REDACTED]`.
///
/// Patterns matched:
/// - OpenAI API keys: `sk-[a-zA-Z0-9]{20,}`
/// - AWS access key IDs: `AKIA[0-9A-Z]{16}`
/// - Bearer tokens: `Bearer [a-zA-Z0-9_\-.]{10,}`
///
/// This is best-effort — novel secret formats will not be caught.
/// Defense-in-depth: the proxy already requires auth (PR #133) and the blob
/// store uses restrictive file permissions (PR #5).
pub fn redact_secrets(data: &[u8]) -> Vec<u8> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(concat!(
            r"sk-[a-zA-Z0-9]{20,}",            // OpenAI
            r"|AKIA[0-9A-Z]{16}",               // AWS access key ID
            r"|Bearer [a-zA-Z0-9_\-.]{10,}",    // Bearer token
            // Dropped the generic `[0-9a-f]{40,}` pattern — it over-redacted
            // git SHAs, blob store hashes, content-hashes, and request IDs.
            // The three named patterns above cover the actual threat.
        ))
        .unwrap()
    });

    let text = match std::str::from_utf8(data) {
        Ok(s) => s,
        Err(_) => {
            // Binary/compressed body — regex can't operate on non-UTF-8.
            // This can happen when the upstream sends Content-Encoding: gzip
            // and reqwest doesn't decompress (gzip feature not enabled).
            // Log so it doesn't fail silently.
            tracing::debug!(
                len = data.len(),
                "Skipping secret redaction on non-UTF-8 blob (compressed or binary)"
            );
            return data.to_vec();
        }
    };

    RE.replace_all(text, REPLACEMENT).into_owned().into_bytes()
}

/// Redact a JSON request body by removing values of sensitive header-like keys.
/// Operates on the serialized JSON bytes so we don't need to parse/reserialize
/// the full body — we just pattern-match `"key": "value"` pairs.
///
/// This handles the case where request bodies echo headers (some LLM SDKs
/// include auth in the body rather than headers).
pub fn redact_request_body(data: &[u8]) -> Vec<u8> {
    static KEY_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"(?i)"(api[_-]?key|api[_-]?secret|authorization|x-api-key|secret|password|token|access[_-]?token|refresh[_-]?token|private[_-]?key|client[_-]?secret|aws[_-]?secret[_-]?access[_-]?key|bearer|credentials)"\s*:\s*"[^"]*""#,
        )
        .unwrap()
    });

    let text = match std::str::from_utf8(data) {
        Ok(s) => s,
        Err(_) => {
            tracing::debug!(
                len = data.len(),
                "Skipping request body redaction on non-UTF-8 blob"
            );
            return data.to_vec();
        }
    };

    KEY_RE
        .replace_all(text, |caps: &regex::Captures| {
            // Preserve the key name, redact only the value.
            let full = caps.get(0).unwrap().as_str();
            if let Some(colon_pos) = full.find(':') {
                let key_part = &full[..=colon_pos];
                format!("{} \"{}\"", key_part, REPLACEMENT)
            } else {
                full.to_string()
            }
        })
        .into_owned()
        .into_bytes()
}

/// Returns true if a header name (lowercase) should be stripped from stored blobs.
pub fn is_sensitive_header(name: &str) -> bool {
    SENSITIVE_HEADERS.contains(&name.to_ascii_lowercase().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_openai_key() {
        let input = br#"{"api_key": "sk-abc123def456ghi789jkl012mno345pqr678stu901v"}"#;
        let out = String::from_utf8(redact_secrets(input)).unwrap();
        assert!(!out.contains("sk-abc"), "OpenAI key should be redacted: {out}");
        assert!(out.contains(REPLACEMENT));
    }

    #[test]
    fn redacts_aws_key() {
        let input = b"AKIAIOSFODNN7EXAMPLE";
        let out = String::from_utf8(redact_secrets(input)).unwrap();
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(out.contains(REPLACEMENT));
    }

    #[test]
    fn redacts_bearer_token() {
        let input = br#"Authorization: Bearer eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.long_token"#;
        let out = String::from_utf8(redact_secrets(input)).unwrap();
        assert!(!out.contains("eyJhbGci"), "Bearer token should be redacted: {out}");
        assert!(out.contains(REPLACEMENT));
    }

    #[test]
    fn preserves_short_strings() {
        let input = b"Hello world, model=gpt-4o, tokens=500";
        let out = redact_secrets(input);
        assert_eq!(input.to_vec(), out, "non-secret data should be unchanged");
    }

    #[test]
    fn redacts_request_body_keys() {
        let input = br#"{"model": "gpt-4o", "api_key": "sk-secret123456789012345", "messages": []}"#;
        let out = String::from_utf8(redact_request_body(input)).unwrap();
        assert!(!out.contains("sk-secret"), "api_key value should be redacted: {out}");
        assert!(out.contains("\"api_key\":"), "key name should be preserved");
        assert!(out.contains("\"model\""), "non-sensitive keys should be untouched");
    }

    #[test]
    fn redacts_authorization_in_body() {
        let input = br#"{"authorization": "Bearer abc123xyz789long", "data": "safe"}"#;
        let out = String::from_utf8(redact_request_body(input)).unwrap();
        assert!(!out.contains("abc123xyz"));
        assert!(out.contains("\"data\": \"safe\""));
    }

    #[test]
    fn hop_by_hop_detection() {
        assert!(is_hop_by_hop("transfer-encoding"));
        assert!(is_hop_by_hop("te"));
        assert!(is_hop_by_hop("upgrade"));
        assert!(!is_hop_by_hop("content-type"));
        assert!(!is_hop_by_hop("authorization"));
    }

    #[test]
    fn sensitive_header_detection() {
        assert!(is_sensitive_header("Authorization"));
        assert!(is_sensitive_header("authorization"));
        assert!(is_sensitive_header("X-Api-Key"));
        assert!(is_sensitive_header("cookie"));
        assert!(!is_sensitive_header("content-type"));
        assert!(!is_sensitive_header("accept"));
    }
}
