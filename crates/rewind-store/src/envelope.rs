//! Response envelope storage shape (Step 0.3).
//!
//! Pre-v0.13 the `Step.response_blob` was a content-addressed pointer to a
//! naked response body — no status code, no headers. Agents that read
//! `Retry-After`, `x-rate-limit-*`, or auth-rotation headers saw a divergent
//! response on cache hit. v0.13 introduces a structured envelope so cached
//! responses can be replayed byte-identical to the original.
//!
//! # Format discrimination
//!
//! The `Step.response_blob_format` column (added in the v0.13 migration —
//! see [`crate::db`]) discriminates the payload format. `0` = legacy naked
//! body (read as `{status: 200, headers: [], body}` for back-compat). `1` =
//! [`ResponseEnvelope`] serialized via the helpers below. Unknown formats
//! fall back to `0` parsing for forward-compat — a v0.13 SDK reading a
//! future v0.14 envelope-v2 won't crash, it'll just return synthetic 200.
//!
//! # Storage
//!
//! Envelope blobs live in the same content-addressed blob store as
//! request/response bodies. The serialized form is JSON for now (debug-
//! friendly, modest size cost vs CBOR/MessagePack). If size becomes a
//! constraint we can switch to a binary format under a new `format = 2`
//! discriminator without disturbing existing data.
//!
//! # Headers
//!
//! Headers are scrubbed at record time — hop-by-hop headers (per RFC 7230
//! §6.1), `Set-Cookie`, and `Authorization` are stripped. Use
//! [`scrub_response_headers`] before constructing the envelope.

use serde::{Deserialize, Serialize};

use crate::redact;

/// Format discriminator values for `Step.response_blob_format`.
pub const FORMAT_NAKED_LEGACY: u8 = 0;
pub const FORMAT_ENVELOPE_V1: u8 = 1;

/// Structured response captured at record time. New writes always use this
/// shape; reads switch on the step's `response_blob_format` column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    /// HTTP status code. Defaults to 200 when reading legacy naked blobs.
    pub status: u16,

    /// Response headers in original order. Scrubbed of hop-by-hop, Set-Cookie,
    /// Authorization. Use `scrub_response_headers` before constructing.
    pub headers: Vec<(String, String)>,

    /// Raw response body bytes (UTF-8 JSON for OpenAI/Anthropic responses,
    /// arbitrary bytes for other providers).
    #[serde(with = "bytes_as_base64")]
    pub body: Vec<u8>,
}

impl ResponseEnvelope {
    /// Construct an envelope with the given status and body, scrubbing the
    /// supplied headers via `scrub_response_headers`. Convenience for the
    /// record path so callers don't have to remember to scrub.
    pub fn new<I, K, V>(status: u16, headers: I, body: Vec<u8>) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let scrubbed = scrub_response_headers(headers);
        ResponseEnvelope { status, headers: scrubbed, body }
    }

    /// Serialize to JSON bytes for storage in the blob store. Pair with
    /// `Step.response_blob_format = FORMAT_ENVELOPE_V1`.
    pub fn to_blob_bytes(&self) -> Vec<u8> {
        // Unwrap is safe — Vec<u8>/u16/Vec<(String,String)> never fail to
        // serialize. If serde_json ever does fail here, the bug is in
        // `bytes_as_base64`, not the data.
        serde_json::to_vec(self).expect("ResponseEnvelope serialization is infallible")
    }

    /// Parse a blob according to its format discriminator.
    /// `FORMAT_NAKED_LEGACY` (0) treats the bytes as the response body
    /// directly with synthetic `status=200, headers=[]`. `FORMAT_ENVELOPE_V1`
    /// (1) deserializes JSON. Unknown formats fall back to legacy parsing
    /// for forward-compat.
    pub fn from_blob_bytes(format: u8, bytes: &[u8]) -> Self {
        match format {
            FORMAT_ENVELOPE_V1 => match serde_json::from_slice::<ResponseEnvelope>(bytes) {
                Ok(env) => env,
                Err(e) => {
                    // Storage corruption or schema mismatch. Log and degrade
                    // gracefully to legacy treatment so the dashboard keeps
                    // working — a divergent header is recoverable, a panic
                    // here would brick the session view.
                    tracing::warn!(
                        error = %e,
                        len = bytes.len(),
                        "ResponseEnvelope deserialization failed; falling back to legacy naked-body treatment"
                    );
                    ResponseEnvelope::legacy_naked(bytes.to_vec())
                }
            },
            FORMAT_NAKED_LEGACY => ResponseEnvelope::legacy_naked(bytes.to_vec()),
            // Forward-compat: unknown future format (e.g. v0.14 binary).
            // Treat as legacy so a v0.13 server can still serve dashboards
            // for sessions written by a future server. Mismatch is logged.
            other => {
                tracing::debug!(
                    format = other,
                    "Unknown response_blob_format; treating as legacy naked"
                );
                ResponseEnvelope::legacy_naked(bytes.to_vec())
            }
        }
    }

    /// Legacy naked-body envelope. Synthetic 200, no headers.
    fn legacy_naked(body: Vec<u8>) -> Self {
        ResponseEnvelope { status: 200, headers: Vec::new(), body }
    }
}

/// Strip headers that should not survive a record→replay round-trip.
///
/// Removes:
/// - Hop-by-hop headers per RFC 7230 §6.1 (transfer-encoding, te, trailer,
///   upgrade, proxy-authorization, proxy-connection, keep-alive, expect)
/// - `Set-Cookie` (session-binding; replaying a stale Set-Cookie poisons
///   the new client)
/// - `Authorization` (rare on responses but seen in some token-rotation
///   patterns; we don't want to surface a recorded credential at replay)
///
/// Header name comparison is case-insensitive. Original casing of remaining
/// headers is preserved.
pub fn scrub_response_headers<I, K, V>(headers: I) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    headers
        .into_iter()
        .filter(|(k, _v)| {
            let lower = k.as_ref().to_ascii_lowercase();
            if redact::is_hop_by_hop(&lower) {
                return false;
            }
            !matches!(lower.as_str(), "set-cookie" | "authorization")
        })
        .map(|(k, v)| (k.as_ref().to_string(), v.as_ref().to_string()))
        .collect()
}

/// Serde adapter — encodes Vec<u8> as base64 in JSON. SQLite can store
/// arbitrary bytes but JSON cannot, and the blob store treats blobs as
/// opaque content-addressed bytes. Base64 is ~33% bigger than raw but
/// keeps the envelope debug-friendly via `jq` / `cat`.
mod bytes_as_base64 {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        // No external base64 dep — the standard library's char encoding is
        // sufficient for our needs. Engine: RFC 4648 §4 (no padding skipping).
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
        for chunk in bytes.chunks(3) {
            let b0 = chunk[0];
            let b1 = chunk.get(1).copied().unwrap_or(0);
            let b2 = chunk.get(2).copied().unwrap_or(0);
            out.push(ALPHABET[(b0 >> 2) as usize] as char);
            out.push(ALPHABET[(((b0 & 0b11) << 4) | (b1 >> 4)) as usize] as char);
            if chunk.len() >= 2 {
                out.push(ALPHABET[(((b1 & 0b1111) << 2) | (b2 >> 6)) as usize] as char);
            } else {
                out.push('=');
            }
            if chunk.len() == 3 {
                out.push(ALPHABET[(b2 & 0b111111) as usize] as char);
            } else {
                out.push('=');
            }
        }
        out.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        let mut decoded = Vec::with_capacity(s.len() * 3 / 4);
        let bytes = s.as_bytes();
        let mut buf: u32 = 0;
        let mut bits: u32 = 0;
        for &c in bytes {
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' => break,
                b'\n' | b'\r' | b' ' | b'\t' => continue,
                _ => return Err(serde::de::Error::custom(format!("invalid base64 byte: {c:#x}"))),
            };
            buf = (buf << 6) | v as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                decoded.push((buf >> bits) as u8);
                buf &= (1 << bits) - 1;
            }
        }
        Ok(decoded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_envelope_v1() {
        let env = ResponseEnvelope::new(
            200,
            vec![
                ("Content-Type", "application/json"),
                ("X-Request-Id", "abc-123"),
            ],
            br#"{"choices":[{"message":{"content":"hello"}}]}"#.to_vec(),
        );
        let blob = env.to_blob_bytes();
        let parsed = ResponseEnvelope::from_blob_bytes(FORMAT_ENVELOPE_V1, &blob);
        assert_eq!(parsed, env, "round-trip preserves status, headers, body");
    }

    #[test]
    fn legacy_blob_reads_as_status_200() {
        let raw = br#"{"choices":[{"message":{"content":"hi"}}]}"#;
        let parsed = ResponseEnvelope::from_blob_bytes(FORMAT_NAKED_LEGACY, raw);
        assert_eq!(parsed.status, 200, "legacy synthesizes 200");
        assert!(parsed.headers.is_empty(), "legacy has no recorded headers");
        assert_eq!(parsed.body, raw, "legacy body is the whole blob");
    }

    #[test]
    fn unknown_format_degrades_to_legacy() {
        let raw = b"some future binary blob";
        let parsed = ResponseEnvelope::from_blob_bytes(99, raw);
        assert_eq!(parsed.status, 200);
        assert_eq!(parsed.body, raw);
    }

    #[test]
    fn corrupted_envelope_degrades_to_legacy() {
        // An envelope-v1 blob whose JSON is invalid. Don't panic; treat as
        // legacy. Behavior is logged via tracing::warn.
        let bogus = b"this is not json";
        let parsed = ResponseEnvelope::from_blob_bytes(FORMAT_ENVELOPE_V1, bogus);
        assert_eq!(parsed.status, 200);
        assert_eq!(parsed.body, bogus);
    }

    #[test]
    fn scrub_strips_hop_by_hop() {
        let scrubbed = scrub_response_headers(vec![
            ("Content-Type", "application/json"),
            ("Transfer-Encoding", "chunked"),
            ("Connection", "keep-alive"), // not hop-by-hop in our list — kept
            ("Keep-Alive", "timeout=5"),
        ]);
        let names: Vec<&str> = scrubbed.iter().map(|(k, _)| k.as_str()).collect();
        assert!(names.contains(&"Content-Type"));
        assert!(!names.iter().any(|n| n.eq_ignore_ascii_case("transfer-encoding")));
        assert!(!names.iter().any(|n| n.eq_ignore_ascii_case("keep-alive")));
    }

    #[test]
    fn scrub_strips_set_cookie_and_authorization() {
        let scrubbed = scrub_response_headers(vec![
            ("Content-Type", "application/json"),
            ("Set-Cookie", "session=abc; Path=/"),
            ("Authorization", "Bearer xyz"),
            ("X-Request-Id", "req-1"),
        ]);
        let names: Vec<String> = scrubbed
            .iter()
            .map(|(k, _)| k.to_ascii_lowercase())
            .collect();
        assert!(names.contains(&"content-type".to_string()));
        assert!(names.contains(&"x-request-id".to_string()));
        assert!(!names.contains(&"set-cookie".to_string()));
        assert!(!names.contains(&"authorization".to_string()));
    }

    #[test]
    fn scrub_is_case_insensitive() {
        let scrubbed = scrub_response_headers(vec![
            ("AUTHORIZATION", "Bearer x"),
            ("set-cookie", "v=1"),
            ("TRANSFER-ENCODING", "chunked"),
        ]);
        assert!(scrubbed.is_empty(), "all 3 should be stripped regardless of case");
    }

    #[test]
    fn empty_headers_round_trip() {
        let env = ResponseEnvelope::new(204, Vec::<(&str, &str)>::new(), Vec::new());
        let blob = env.to_blob_bytes();
        let parsed = ResponseEnvelope::from_blob_bytes(FORMAT_ENVELOPE_V1, &blob);
        assert_eq!(parsed.status, 204);
        assert!(parsed.headers.is_empty());
        assert!(parsed.body.is_empty());
    }

    #[test]
    fn binary_body_survives_base64() {
        // Random non-UTF8 bytes — exercises base64 codec in both directions.
        let body: Vec<u8> = (0..=255u8).collect();
        let env = ResponseEnvelope::new(200, Vec::<(&str, &str)>::new(), body.clone());
        let blob = env.to_blob_bytes();
        let parsed = ResponseEnvelope::from_blob_bytes(FORMAT_ENVELOPE_V1, &blob);
        assert_eq!(parsed.body, body, "binary bytes must survive JSON round-trip via base64");
    }

    #[test]
    fn known_base64_alphabet() {
        // Sanity — encode "Man" → "TWFu", well-known canonical example.
        let env = ResponseEnvelope::new(200, Vec::<(&str, &str)>::new(), b"Man".to_vec());
        let blob = env.to_blob_bytes();
        let blob_str = std::str::from_utf8(&blob).unwrap();
        assert!(
            blob_str.contains(r#""body":"TWFu""#),
            "expected canonical RFC 4648 base64 for \"Man\", got {}",
            blob_str
        );
    }
}
