//! Integration tests for the auth middleware & fail-closed bind check.
//!
//! See docs/security-audit.md §CRITICAL-02 and plans/squishy-strolling-bachman.md.

use std::net::SocketAddr;

use rewind_store::Store;
use rewind_web::WebServer;
use reqwest::StatusCode;
use tempfile::TempDir;

// ── Fail-closed startup ──────────────────────────────────────

#[tokio::test]
async fn run_refuses_nonloopback_bind_without_token() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let server = WebServer::new_standalone(store); // no token

    // 0.0.0.0 is non-loopback per IpAddr::is_loopback
    let addr: SocketAddr = "0.0.0.0:0".parse().unwrap();

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        server.run(addr),
    )
    .await
    .expect("server.run should return an error immediately, not block");

    let err = result.expect_err("expected fail-closed error");
    let msg = err.to_string();
    assert!(
        msg.contains("non-loopback") || msg.contains("auth token"),
        "unexpected error message: {msg}"
    );
}

#[tokio::test]
async fn run_refuses_ipv6_unspecified_bind_without_token() {
    // :: (IPv6 unspecified) is not loopback and must also fail-closed.
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let server = WebServer::new_standalone(store);

    let addr: SocketAddr = "[::]:0".parse().unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        server.run(addr),
    )
    .await
    .expect("server.run should return immediately");
    let err = result.expect_err("expected fail-closed on ::");
    assert!(
        err.to_string().contains("non-loopback") || err.to_string().contains("auth token"),
        "unexpected error message: {err}"
    );
}

#[tokio::test]
async fn run_allows_loopback_bind_without_token() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let server = WebServer::new_standalone(store);

    // Loopback + no token: should bind, run, and serve /_rewind/health.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    tokio::spawn(async move { let _ = server.run(addr).await; });
    assert_server_alive(addr).await;
    // Hold the tempdir for the test duration.
    drop(tmp);
}

#[tokio::test]
async fn run_allows_nonloopback_with_auth_disabled() {
    // `--no-auth` escape hatch: non-loopback bind with no token AND
    // explicit opt-out should start.
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let server = WebServer::new_standalone(store).with_auth_disabled(true);

    let addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
    let fut = server.run(addr);
    // No listener trick here — use a short timeout; a successful bind means
    // run() enters the axum::serve loop and the future stays pending.
    match tokio::time::timeout(std::time::Duration::from_millis(300), fut).await {
        Err(_elapsed) => {}
        Ok(Ok(_)) => panic!("server returned Ok unexpectedly"),
        Ok(Err(e)) => panic!("--no-auth should bypass fail-closed check: {e}"),
    }
    drop(tmp);
}

#[tokio::test]
async fn run_allows_nonloopback_with_token() {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let server = WebServer::new_standalone(store).with_auth_token(Some("t".into()));

    let addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
    let fut = server.run(addr);
    match tokio::time::timeout(std::time::Duration::from_millis(300), fut).await {
        Err(_elapsed) => {}
        Ok(Ok(_)) => panic!("server returned Ok unexpectedly"),
        Ok(Err(e)) => panic!("server failed on 0.0.0.0 with token: {e}"),
    }
    drop(tmp);
}

/// Probe /_rewind/health until it returns 200 or we give up.
async fn assert_server_alive(addr: SocketAddr) {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(200))
        .build()
        .unwrap();
    for _ in 0..60 {
        if let Ok(resp) = client
            .get(format!("http://{addr}/_rewind/health"))
            .send()
            .await
            && resp.status().is_success()
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("server never became healthy on {addr}");
}

// ── Live-server middleware behavior ─────────────────────────

/// Spawn a server on a loopback ephemeral port. Returns the bound address and
/// the owning `TempDir` guard — keep the guard alive until the test ends so
/// the data directory is cleaned up on drop.
async fn spawn_server_with_token(token: Option<String>) -> (SocketAddr, TempDir) {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    // Pick a port by binding, reading local_addr, then dropping.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let server = WebServer::new_standalone(store).with_auth_token(token);
    tokio::spawn(async move {
        let _ = server.run(addr).await;
    });

    // Wait for the server to accept a health probe so we're definitely live.
    // This replaces the flaky fixed-sleep sentinel.
    let probe = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(200))
        .build()
        .unwrap();
    for _ in 0..60 {
        if let Ok(resp) = probe
            .get(format!("http://{addr}/_rewind/health"))
            .send()
            .await
            && resp.status().is_success()
        {
            return (addr, tmp);
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("server did not start on {addr}");
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap()
}

async fn http_get(
    addr: SocketAddr,
    path: &str,
    auth: Option<&str>,
) -> (StatusCode, String) {
    let url = format!("http://{addr}{path}");
    let mut req = client().get(&url);
    if let Some(tok) = auth {
        req = req.bearer_auth(tok);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    (status, body)
}

#[tokio::test]
async fn health_endpoint_bypasses_auth() {
    let (addr, _tmp) = spawn_server_with_token(Some("secret".into())).await;
    let (status, body) = http_get(addr, "/_rewind/health", None).await;
    assert_eq!(status, StatusCode::OK, "health must be accessible without auth");
    assert!(body.contains("\"status\":\"ok\""), "body: {body}");
}

#[tokio::test]
async fn protected_route_requires_token() {
    let (addr, _tmp) = spawn_server_with_token(Some("secret".into())).await;
    let (status, body) = http_get(addr, "/api/sessions", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("invalid or missing auth token"), "body: {body}");
}

#[tokio::test]
async fn protected_route_accepts_correct_token() {
    let (addr, _tmp) = spawn_server_with_token(Some("secret".into())).await;
    let (status, _) = http_get(addr, "/api/sessions", Some("secret")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn protected_route_rejects_wrong_token() {
    let (addr, _tmp) = spawn_server_with_token(Some("secret".into())).await;
    let (status, _) = http_get(addr, "/api/sessions", Some("wrong")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn middleware_is_noop_when_token_is_none() {
    let (addr, _tmp) = spawn_server_with_token(None).await;
    let (status, _) = http_get(addr, "/api/sessions", None).await;
    assert_eq!(status, StatusCode::OK, "no token configured → routes open");
}

#[tokio::test]
async fn eval_route_requires_token() {
    let (addr, _tmp) = spawn_server_with_token(Some("secret".into())).await;
    let (status, _) = http_get(addr, "/api/eval/datasets", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn hook_route_requires_token() {
    let (addr, _tmp) = spawn_server_with_token(Some("secret".into())).await;
    // GET on the POST-only /api/hooks/event: without auth should be 401,
    // with auth should be 405 (method not allowed). We test the unauth case.
    let (status, _) = http_get(addr, "/api/hooks/event", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn otlp_route_requires_token() {
    let (addr, _tmp) = spawn_server_with_token(Some("secret".into())).await;
    let (status, _) = http_get(addr, "/v1/traces", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ── ?token= query-param auth ────────────────────────────────

/// REST endpoints must NOT accept `?token=` — the token would leak via Referer,
/// server logs, and browser history. Only /api/ws accepts it.
#[tokio::test]
async fn rest_rejects_token_query_param() {
    let (addr, _tmp) = spawn_server_with_token(Some("secret".into())).await;
    let (status, _) = http_get(addr, "/api/sessions?token=secret", None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "REST must not accept ?token= (leaks via Referer/logs)"
    );
}

/// WebSocket upgrade accepts `?token=` because the browser WebSocket
/// constructor can't set Authorization.
#[tokio::test]
async fn ws_upgrade_accepts_token_query_param() {
    let (addr, _tmp) = spawn_server_with_token(Some("secret".into())).await;

    // Send a raw WebSocket upgrade request. We don't speak the WS protocol
    // beyond the handshake — success = server returned 101, meaning the
    // middleware accepted the token.
    let status = ws_upgrade_status(addr, "/api/ws?token=secret").await;
    assert_eq!(
        status, 101,
        "WS upgrade with ?token= should succeed (got {status})"
    );
}

#[tokio::test]
async fn ws_upgrade_rejects_missing_token() {
    let (addr, _tmp) = spawn_server_with_token(Some("secret".into())).await;
    let status = ws_upgrade_status(addr, "/api/ws").await;
    assert_eq!(status, 401, "WS upgrade without token should be 401");
}

#[tokio::test]
async fn ws_upgrade_rejects_wrong_token() {
    let (addr, _tmp) = spawn_server_with_token(Some("secret".into())).await;
    let status = ws_upgrade_status(addr, "/api/ws?token=wrong").await;
    assert_eq!(status, 401, "WS upgrade with wrong ?token= should be 401");
}

/// Send a minimal WebSocket upgrade GET and return the numeric status code.
async fn ws_upgrade_status(addr: SocketAddr, path_and_query: &str) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "GET {path_and_query} HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
         Sec-WebSocket-Version: 13\r\n\
         \r\n"
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = [0u8; 256];
    let n = stream.read(&mut buf).await.unwrap_or(0);
    let first_line = std::str::from_utf8(&buf[..n])
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("");
    // "HTTP/1.1 101 Switching Protocols"
    first_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0)
}
