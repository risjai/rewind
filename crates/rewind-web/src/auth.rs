//! Bearer-token auth middleware for the Rewind web API.
//!
//! See `docs/security-audit.md` §CRITICAL-02. Fail-closed on non-loopback binds.
//!
//! Behavior:
//! - `auth_token` resolved at startup from (1) CLI flag, (2) `REWIND_AUTH_TOKEN`,
//!   (3) a file at `~/.rewind/auth_token` auto-generated on first run.
//! - When `AppState::auth_token` is `None`, the middleware is a no-op (loopback
//!   default preserves the current dev UX).
//! - When set, requests to protected routes must send `Authorization: Bearer <token>`.
//!   Comparison is constant-time (`subtle::ConstantTimeEq`).
//! - `/_rewind/health` and static SPA assets are mounted outside this middleware,
//!   so they remain accessible without a token (see `lib.rs::build_router`).

use std::path::{Path, PathBuf};

use axum::{
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use rand::RngCore;
use subtle::ConstantTimeEq;

use crate::AppState;

/// How a token was resolved — used only for startup logging.
#[derive(Debug)]
pub enum TokenSource {
    CliFlag,
    EnvVar,
    ExistingFile(PathBuf),
    Generated(PathBuf),
}

/// Resolve the auth token in priority order:
/// 1. Explicit `cli_override` (set by `--auth-token` flag)
/// 2. `REWIND_AUTH_TOKEN` env var (via `std::env::var`)
/// 3. File at `{data_dir}/auth_token` — auto-generated (64 hex chars) if missing
///
/// Returns `Ok((token, source))`. The file, if created, is chmod 0600 on unix.
pub fn resolve_or_generate_token(
    cli_override: Option<String>,
    data_dir: &Path,
) -> anyhow::Result<(String, TokenSource)> {
    resolve_with_env(cli_override, std::env::var("REWIND_AUTH_TOKEN").ok(), data_dir)
}

// TODO(security): add `rewind auth rotate` command that deletes
// ~/.rewind/auth_token and forces regeneration on next non-loopback start.
// Currently tokens are long-lived static credentials with no expiry or audit.
// Track in follow-up to audit MEDIUM-10 (to be filed).

/// Test-injectable variant: takes the env value as an explicit argument rather
/// than reading `std::env`. Production code should prefer `resolve_or_generate_token`.
pub(crate) fn resolve_with_env(
    cli_override: Option<String>,
    env_value: Option<String>,
    data_dir: &Path,
) -> anyhow::Result<(String, TokenSource)> {
    if let Some(tok) = cli_override.filter(|t| !t.is_empty()) {
        return Ok((tok, TokenSource::CliFlag));
    }
    if let Some(tok) = env_value.filter(|t| !t.is_empty()) {
        return Ok((tok, TokenSource::EnvVar));
    }

    let path = data_dir.join("auth_token");
    if path.exists() {
        let tok = std::fs::read_to_string(&path)?.trim().to_string();
        if !tok.is_empty() {
            return Ok((tok, TokenSource::ExistingFile(path)));
        }
        // Empty file — fall through to regenerate.
    }

    std::fs::create_dir_all(data_dir)?;
    let token = generate_token();

    // Atomic create + chmod: avoid the umask window where `fs::write` creates
    // the file at 0644 before a later `set_permissions` tightens it.
    // If another process won the race, re-read instead of overwriting.
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut f) => {
                f.write_all(token.as_bytes())?;
                Ok((token, TokenSource::Generated(path)))
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Concurrent writer won — read their token.
                let tok = std::fs::read_to_string(&path)?.trim().to_string();
                Ok((tok, TokenSource::ExistingFile(path)))
            }
            Err(e) => Err(e.into()),
        }
    }
    #[cfg(not(unix))]
    {
        // Non-unix: best-effort atomic create. OS ACLs provide access control.
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                use std::io::Write;
                f.write_all(token.as_bytes())?;
                tracing::warn!(
                    "Auth token file created at {} — rely on OS ACLs for access control (non-unix)",
                    path.display()
                );
                Ok((token, TokenSource::Generated(path)))
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let tok = std::fs::read_to_string(&path)?.trim().to_string();
                Ok((tok, TokenSource::ExistingFile(path)))
            }
            Err(e) => Err(e.into()),
        }
    }
}

fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Middleware that enforces Bearer-token auth when `AppState::auth_token` is `Some`.
///
/// No-op when the token is `None` (preserving the loopback-unauthenticated default).
///
/// WebSocket upgrade requests also accept `?token=<token>` as a query-param
/// fallback (the browser `WebSocket` constructor cannot set headers). Only
/// honored for `GET /api/ws` — all other routes require the `Authorization` header.
pub async fn auth_middleware(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let Some(ref expected) = state.auth_token else {
        return next.run(req).await;
    };

    let presented = extract_token(&req);

    let ok = match presented {
        Some(tok) => {
            // Constant-time equality — only compares when lengths match.
            let a = tok.as_bytes();
            let b = expected.as_bytes();
            a.len() == b.len() && bool::from(a.ct_eq(b))
        }
        None => false,
    };

    if ok {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            r#"{"error":"invalid or missing auth token"}"#,
        )
            .into_response()
    }
}

/// Extract the presented bearer token from either the `Authorization` header
/// or, for the WebSocket upgrade path only, the `token` query parameter.
fn extract_token(req: &Request<axum::body::Body>) -> Option<String> {
    // 1. Authorization: Bearer <token> (preferred, works for HTTP and WS-with-headers)
    if let Some(tok) = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        return Some(tok.to_string());
    }

    // 2. ?token=<token> — ONLY on /api/ws because browsers can't send headers
    //    on a WebSocket upgrade. Accepting this on other routes would expose
    //    the token in server logs, Referer headers, and browser history.
    if req.uri().path() == "/api/ws"
        && let Some(q) = req.uri().query()
    {
        for pair in q.split('&') {
            if let Some(v) = pair.strip_prefix("token=") {
                let decoded = percent_decode(v);
                if !decoded.is_empty() {
                    return Some(decoded);
                }
            }
        }
    }

    None
}

/// Minimal percent-decoder for query-param token values. Handles `%XX` and `+`.
fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        out.push(((h << 4) | l) as u8 as char);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i] as char);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b as char);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_token_is_64_hex_chars() {
        let t = generate_token();
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        // Two calls should differ with overwhelming probability.
        assert_ne!(t, generate_token());
    }

    #[test]
    fn resolve_prefers_cli_override() {
        let dir = tempfile::tempdir().unwrap();
        let (tok, src) = resolve_with_env(
            Some("from-cli".into()),
            Some("from-env".into()),
            dir.path(),
        )
        .unwrap();
        assert_eq!(tok, "from-cli");
        assert!(matches!(src, TokenSource::CliFlag));
    }

    #[test]
    fn resolve_prefers_env_over_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("auth_token"), "from-file").unwrap();
        let (tok, src) =
            resolve_with_env(None, Some("from-env".into()), dir.path()).unwrap();
        assert_eq!(tok, "from-env");
        assert!(matches!(src, TokenSource::EnvVar));
    }

    #[test]
    fn resolve_reads_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("auth_token"), "stored-token\n").unwrap();
        let (tok, src) = resolve_with_env(None, None, dir.path()).unwrap();
        assert_eq!(tok, "stored-token");
        assert!(matches!(src, TokenSource::ExistingFile(_)));
    }

    #[test]
    fn resolve_generates_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let (tok, src) = resolve_with_env(None, None, dir.path()).unwrap();
        assert_eq!(tok.len(), 64);
        assert!(matches!(src, TokenSource::Generated(_)));
        let on_disk = std::fs::read_to_string(dir.path().join("auth_token")).unwrap();
        assert_eq!(on_disk.trim(), tok);
    }

    #[test]
    fn resolve_treats_empty_cli_and_env_as_unset() {
        let dir = tempfile::tempdir().unwrap();
        let (_tok, src) =
            resolve_with_env(Some("".into()), Some("".into()), dir.path()).unwrap();
        assert!(matches!(src, TokenSource::Generated(_)));
    }

    #[cfg(unix)]
    #[test]
    fn generated_token_file_is_mode_600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (_tok, _src) = resolve_with_env(None, None, dir.path()).unwrap();
        let meta = std::fs::metadata(dir.path().join("auth_token")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn concurrent_token_creation_reads_winner() {
        // Simulate a race: the second caller sees the file already created by
        // a concurrent caller and reads its token instead of overwriting.
        let dir = tempfile::tempdir().unwrap();
        let (first_tok, first_src) = resolve_with_env(None, None, dir.path()).unwrap();
        assert!(matches!(first_src, TokenSource::Generated(_)));

        // Second call: file now exists → should read, not regenerate.
        let (second_tok, second_src) = resolve_with_env(None, None, dir.path()).unwrap();
        assert_eq!(first_tok, second_tok, "second caller must get the same token");
        assert!(
            matches!(second_src, TokenSource::ExistingFile(_)),
            "expected ExistingFile on second call, got {second_src:?}"
        );
    }

    // Note: we can't easily exercise the internal AlreadyExists race branch
    // (where `path.exists()` returns false but `create_new` then returns
    // AlreadyExists because another writer won in between) from a single-
    // threaded unit test. The branch is defended by the if-exists-read-first
    // early return tested above, plus code review on the OpenOptions block.
}
