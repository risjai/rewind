//! Newtype wrapping a `String` that redacts on output channels.
//!
//! Phase 3 building block: runner auth tokens, app-key-encrypted secrets,
//! and any other operator-supplied credential that flows through the
//! storage layer. The point is to make accidental exposure noisy at
//! review time rather than relying on per-call-site memory of "this
//! field is a secret".
//!
//! ## What's redacted
//!
//! - `Debug` impl: `SensitiveString(***)` — visible in panic messages,
//!   `dbg!()` output, `tracing` field formatters, etc.
//! - `Display` impl: `***` — visible in `format!("{}")`, `println!`, etc.
//! - `Serialize` impl: `"***"` — ANY JSON-serialized output (API responses,
//!   audit logs, OTel attributes, the `serde_json::to_string` call sites).
//!
//! ## What's NOT redacted
//!
//! - `Deserialize` reads the raw bytes verbatim — DB reads, env-var reads,
//!   and registration responses to runners deserialize through this path.
//!   Asymmetric redaction (out: redacted, in: raw) is the correct shape:
//!   the type protects OUTBOUND data, not data at rest.
//! - `expose()` returns the raw `&str`. Explicit method name so reviewers
//!   can grep for it during security review.
//!
//! ## What this is NOT
//!
//! - **Not encryption.** This is a Rust visibility / formatting wrapper.
//!   Encryption-at-rest of secrets is a separate concern (see Phase 3
//!   plan's `crypto` module: AES-256-GCM under `REWIND_RUNNER_SECRET_KEY`).
//! - **Not zeroize-on-drop.** v1 doesn't scrub memory after use. v3.1
//!   may add the `zeroize` crate if a security review demands it; for
//!   now the wrapper is a defensive readability tool, not a hardened
//!   secrets store.
//! - **Not constant-time comparison.** `PartialEq` uses `String::eq`
//!   which is variable-time. For auth-token verification (where timing
//!   attacks could leak token prefixes), use [`SensitiveString::ct_eq`]
//!   which uses the `subtle` crate's constant-time comparison.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use subtle::ConstantTimeEq;

/// A `String` newtype that redacts on `Debug`, `Display`, and `Serialize`.
///
/// See module docs for the full redaction policy. Common usage:
///
/// ```
/// use rewind_store::SensitiveString;
///
/// let token = SensitiveString::new("rwd_runner_aBcDeF123");
///
/// assert_eq!(format!("{token}"), "***");                  // Display redacts
/// assert_eq!(format!("{token:?}"), "SensitiveString(***)"); // Debug redacts
/// assert_eq!(serde_json::to_string(&token).unwrap(), "\"***\""); // serde redacts
/// assert_eq!(token.expose(), "rwd_runner_aBcDeF123");      // explicit unwrap
/// ```
#[derive(Clone)]
pub struct SensitiveString(String);

impl SensitiveString {
    /// Wrap a string. Idiomatic constructor; accepts `&str`, `String`,
    /// or anything that implements `Into<String>`.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Return the raw underlying `&str`. Explicit method name
    /// (`expose`) so a security review can grep for call sites that
    /// intentionally bypass redaction. Use sparingly; the typical
    /// use is computing an HMAC signature or hashing for inbound
    /// auth, where the raw bytes are needed momentarily and then
    /// discarded.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper and return the raw `String`. Same caveats
    /// as [`Self::expose`]; use only when the caller genuinely needs
    /// owned bytes (e.g. moving into a crypto API that takes ownership).
    pub fn into_inner(self) -> String {
        self.0
    }

    /// Constant-time equality comparison for two SensitiveStrings.
    ///
    /// Use this for auth-token verification or any other secret
    /// comparison where a timing attack could leak prefix-match info.
    /// `PartialEq` uses `String::eq` which short-circuits on the first
    /// mismatching byte — fine for non-secret strings, dangerous for
    /// auth tokens. Backed by the `subtle::ConstantTimeEq` trait.
    ///
    /// Returns true iff both strings have the same length AND every
    /// byte matches. Length-different inputs return false in
    /// constant time relative to the shorter input.
    pub fn ct_eq(&self, other: &Self) -> bool {
        bool::from(self.0.as_bytes().ct_eq(other.0.as_bytes()))
    }

    /// Length in bytes (NOT redacted — length itself isn't secret in
    /// our threat model, and exposing it lets callers do reasonable
    /// length-validation without unwrapping).
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the wrapped string is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

// ── Redacted output impls ────────────────────────────────────────

impl fmt::Debug for SensitiveString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SensitiveString(***)")
    }
}

impl fmt::Display for SensitiveString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "***")
    }
}

impl Serialize for SensitiveString {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // ALWAYS redact on outbound JSON — API responses, audit logs,
        // OTel attributes, etc. The asymmetry vs Deserialize is
        // deliberate: the type protects outbound data, not inputs.
        serializer.serialize_str("***")
    }
}

// ── Raw-byte input impls ─────────────────────────────────────────

impl<'de> Deserialize<'de> for SensitiveString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Read raw bytes verbatim — DB reads, env-var reads, registration
        // response parsing all need the actual value to function. The
        // type re-asserts redaction the moment it leaves storage on any
        // outbound channel.
        let s = String::deserialize(deserializer)?;
        Ok(Self(s))
    }
}

// ── Conversions ──────────────────────────────────────────────────

impl From<String> for SensitiveString {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for SensitiveString {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

// PartialEq is variable-time; document this and steer toward ct_eq for
// secret comparisons. Implemented anyway for collections / asserts in
// tests where timing isn't a concern.
impl PartialEq for SensitiveString {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for SensitiveString {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts() {
        let s = SensitiveString::new("supersecret");
        assert_eq!(format!("{s:?}"), "SensitiveString(***)");
        // dbg!() goes through Debug; verify it doesn't leak.
        let dbg_output = format!("{:?}", &s);
        assert!(!dbg_output.contains("supersecret"));
    }

    #[test]
    fn display_redacts() {
        let s = SensitiveString::new("supersecret");
        assert_eq!(format!("{s}"), "***");
        assert!(!format!("{s}").contains("supersecret"));
    }

    #[test]
    fn json_serialize_redacts() {
        let s = SensitiveString::new("supersecret");
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, "\"***\"");
        assert!(!json.contains("supersecret"));
    }

    #[test]
    fn json_serialize_redacts_in_struct() {
        // Real-world shape: a runner row with both a redacted token and
        // public fields. Confirm the redaction holds when nested.
        #[derive(Serialize)]
        struct RunnerLike {
            id: String,
            name: String,
            token: SensitiveString,
        }
        let r = RunnerLike {
            id: "abc".to_string(),
            name: "ray-agent".to_string(),
            token: SensitiveString::new("rwd_runner_zzz"),
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"name\":\"ray-agent\""), "public fields visible");
        assert!(!json.contains("rwd_runner_zzz"), "secret stays out of JSON");
        assert!(json.contains("\"token\":\"***\""), "token redacted in place");
    }

    #[test]
    fn json_deserialize_returns_raw() {
        // Asymmetric: deserialization gives back the actual value
        // (DB reads, env-var bootstrap, etc.). Redaction kicks in
        // again when this value tries to leave through any outbound
        // channel.
        let json = "\"raw-token-value-123\"";
        let s: SensitiveString = serde_json::from_str(json).unwrap();
        assert_eq!(s.expose(), "raw-token-value-123");
    }

    #[test]
    fn round_trip_through_serde_redacts() {
        // Specifically: serializing a SensitiveString and then
        // deserializing the JSON should NOT recover the raw value
        // (since the JSON contains "***"). Locks in the redaction
        // contract.
        let s = SensitiveString::new("original");
        let json = serde_json::to_string(&s).unwrap();
        let recovered: SensitiveString = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.expose(), "***");
        assert_ne!(recovered.expose(), "original",
                   "serde round-trip should NOT recover the raw value");
    }

    #[test]
    fn expose_returns_raw() {
        let s = SensitiveString::new("real-token");
        assert_eq!(s.expose(), "real-token");
    }

    #[test]
    fn into_inner_returns_owned_string() {
        let s = SensitiveString::new("real-token");
        let owned: String = s.into_inner();
        assert_eq!(owned, "real-token");
    }

    #[test]
    fn from_string_and_str() {
        let s1: SensitiveString = "literal".into();
        let s2: SensitiveString = String::from("literal").into();
        let s3 = SensitiveString::new("literal");
        assert_eq!(s1.expose(), "literal");
        assert_eq!(s2.expose(), "literal");
        assert_eq!(s3.expose(), "literal");
    }

    #[test]
    fn clone_preserves_value() {
        let s = SensitiveString::new("original");
        let cloned = s.clone();
        assert_eq!(s.expose(), cloned.expose());
        // Each clone redacts independently in its own Debug output.
        assert_eq!(format!("{cloned:?}"), "SensitiveString(***)");
    }

    #[test]
    fn ct_eq_matches_equal_strings() {
        let a = SensitiveString::new("equal");
        let b = SensitiveString::new("equal");
        assert!(a.ct_eq(&b));
    }

    #[test]
    fn ct_eq_rejects_different_strings() {
        let a = SensitiveString::new("first");
        let b = SensitiveString::new("second");
        assert!(!a.ct_eq(&b));
    }

    #[test]
    fn ct_eq_handles_different_lengths() {
        let a = SensitiveString::new("short");
        let b = SensitiveString::new("longer-string");
        assert!(!a.ct_eq(&b));
    }

    #[test]
    fn ct_eq_empty_strings_match() {
        let a = SensitiveString::new("");
        let b = SensitiveString::new("");
        assert!(a.ct_eq(&b));
    }

    #[test]
    fn partial_eq_works_for_non_secret_use_cases() {
        // PartialEq is variable-time but useful for tests / collections
        // where timing isn't a concern. Document the choice via this
        // explicit test that it's not eq_constant_time.
        let a = SensitiveString::new("equal");
        let b = SensitiveString::new("equal");
        assert_eq!(a, b);
    }

    #[test]
    fn len_and_is_empty_not_redacted() {
        // Length is not secret in our threat model — callers can
        // do basic validation without unwrapping the value.
        let s = SensitiveString::new("12345");
        assert_eq!(s.len(), 5);
        assert!(!s.is_empty());

        let empty = SensitiveString::new("");
        assert_eq!(empty.len(), 0);
        assert!(empty.is_empty());
    }

    /// Deliberate negative test: a panic message containing a
    /// SensitiveString must NOT leak the secret. Phase 3 runner
    /// code should panic-free, but defense-in-depth: even if a
    /// panic happens, the secret stays hidden.
    #[test]
    fn panic_message_does_not_leak() {
        use std::panic;

        let token = SensitiveString::new("super_secret_token_xyz");

        let result = panic::catch_unwind(|| {
            panic!("auth failed for token {token:?}");
        });

        let panic_payload = result.unwrap_err();
        let msg: &str = panic_payload
            .downcast_ref::<String>()
            .map(|s| s.as_str())
            .or_else(|| panic_payload.downcast_ref::<&str>().copied())
            .unwrap_or("");
        assert!(
            !msg.contains("super_secret_token_xyz"),
            "panic message leaked the secret: {msg:?}"
        );
        assert!(msg.contains("***"), "panic message should show redacted form, got: {msg:?}");
    }
}
