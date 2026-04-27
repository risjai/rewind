use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use http_body_util::Full;
use hyper::body::Bytes;
use rewind_proxy::ProxyServer;
use rewind_store::Store;
use serde_json::json;
use std::net::SocketAddr;
use tempfile::TempDir;
use tokio::net::TcpListener;

/// Start a mock upstream HTTP server that returns a canned OpenAI response.
/// Returns the address it's listening on.
async fn start_mock_upstream(response_body: serde_json::Value) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let body_bytes = serde_json::to_vec(&response_body).unwrap();

    tokio::spawn(async move {
        // Accept multiple connections
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = TokioIo::new(stream);
            let body = body_bytes.clone();

            tokio::spawn(async move {
                let _ = http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |_req: Request<Incoming>| {
                            let body = body.clone();
                            async move {
                                Ok::<_, hyper::Error>(
                                    Response::builder()
                                        .status(200)
                                        .header("content-type", "application/json")
                                        .body(Full::new(Bytes::from(body)))
                                        .unwrap(),
                                )
                            }
                        }),
                    )
                    .await;
            });
        }
    });

    addr
}

/// Start a mock upstream that returns a 500 error.
async fn start_error_upstream() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = TokioIo::new(stream);

            tokio::spawn(async move {
                let _ = http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(|_req: Request<Incoming>| async {
                            Ok::<_, hyper::Error>(
                                Response::builder()
                                    .status(500)
                                    .header("content-type", "application/json")
                                    .body(Full::new(Bytes::from(
                                        r#"{"error":{"message":"Internal server error"}}"#,
                                    )))
                                    .unwrap(),
                            )
                        }),
                    )
                    .await;
            });
        }
    });

    addr
}

/// Start a mock upstream that returns SSE streaming response.
async fn start_streaming_upstream() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = TokioIo::new(stream);

            tokio::spawn(async move {
                let _ = http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(|_req: Request<Incoming>| async {
                            let sse_body = concat!(
                                "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}],\"model\":\"gpt-4o\"}\n\n",
                                "data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n",
                                "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\n",
                                "data: [DONE]\n\n",
                            );
                            Ok::<_, hyper::Error>(
                                Response::builder()
                                    .status(200)
                                    .header("content-type", "text/event-stream")
                                    .body(Full::new(Bytes::from(sse_body)))
                                    .unwrap(),
                            )
                        }),
                    )
                    .await;
            });
        }
    });

    addr
}

fn setup_store() -> (TempDir, Store) {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    (tmp, store)
}

/// Send a request through the proxy and return the response.
async fn send_request(proxy_addr: SocketAddr, body: &serde_json::Value) -> reqwest::Response {
    let client = reqwest::Client::new();
    client
        .post(format!("http://{}/v1/chat/completions", proxy_addr))
        .header("content-type", "application/json")
        .json(body)
        .send()
        .await
        .unwrap()
}

// ── Test: Buffered Request Records Step ──────────────────────────

#[tokio::test]
async fn buffered_request_records_step() {
    let upstream_response = json!({
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "Hello!"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 50, "completion_tokens": 10}
    });

    let upstream_addr = start_mock_upstream(upstream_response.clone()).await;
    let (_tmp, store) = setup_store();

    let proxy = ProxyServer::new(
        store,
        "test-session",
        &format!("http://{}", upstream_addr),
        false,
        false,
    )
    .unwrap();
    let session_id = proxy.session_id().to_string();
    let timeline_id = proxy.timeline_id().to_string();

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener); // Free the port for the proxy

    tokio::spawn(async move {
        proxy.run(proxy_addr).await.unwrap();
    });

    // Wait for proxy to start
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let req_body = json!({
        "model": "gpt-4o",
        "messages": [{"role": "user", "content": "Hi"}]
    });

    let resp = send_request(proxy_addr, &req_body).await;
    assert_eq!(resp.status(), 200);

    let resp_json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(resp_json["choices"][0]["message"]["content"], "Hello!");

    // Wait for store write
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Verify step was recorded by opening the same store
    let store = Store::open(_tmp.path()).unwrap();
    let steps = store.get_steps(&timeline_id).unwrap();
    assert_eq!(steps.len(), 1, "Expected 1 step recorded");

    let step = &steps[0];
    assert_eq!(step.step_number, 1);
    assert_eq!(step.model, "gpt-4o");
    assert_eq!(step.tokens_in, 50);
    assert_eq!(step.tokens_out, 10);
    assert_eq!(step.status.as_str(), "success");
    // duration_ms can be 0 for fast localhost round-trips (sub-millisecond)

    // Verify blobs exist
    let resp_data = store.blobs.get(&step.response_blob).unwrap();
    assert!(!resp_data.is_empty());
    let req_data = store.blobs.get(&step.request_blob).unwrap();
    assert!(!req_data.is_empty());

    // Verify session was updated
    let session = store.get_session(&session_id).unwrap().unwrap();
    assert_eq!(session.total_steps, 1);
}

// ── Test: Upstream Error Records Error Step ──────────────────────

#[tokio::test]
async fn upstream_error_records_error_step() {
    let upstream_addr = start_error_upstream().await;
    let (_tmp, store) = setup_store();

    let proxy = ProxyServer::new(
        store,
        "test-error",
        &format!("http://{}", upstream_addr),
        false,
        false,
    )
    .unwrap();
    let timeline_id = proxy.timeline_id().to_string();

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    tokio::spawn(async move {
        proxy.run(proxy_addr).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let req_body = json!({
        "model": "gpt-4o",
        "messages": [{"role": "user", "content": "Hi"}]
    });

    let resp = send_request(proxy_addr, &req_body).await;
    assert_eq!(resp.status(), 500);

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let store = Store::open(_tmp.path()).unwrap();
    let steps = store.get_steps(&timeline_id).unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].status.as_str(), "error");
    assert!(steps[0].error.is_some());
}

// ── Test: Streaming Request Records Synthetic Response ───────────

#[tokio::test]
async fn streaming_request_records_synthetic_response() {
    let upstream_addr = start_streaming_upstream().await;
    let (_tmp, store) = setup_store();

    let proxy = ProxyServer::new(
        store,
        "test-stream",
        &format!("http://{}", upstream_addr),
        false,
        false,
    )
    .unwrap();
    let timeline_id = proxy.timeline_id().to_string();

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    tokio::spawn(async move {
        proxy.run(proxy_addr).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let req_body = json!({
        "model": "gpt-4o",
        "messages": [{"role": "user", "content": "Hi"}],
        "stream": true
    });

    let resp = send_request(proxy_addr, &req_body).await;
    assert_eq!(resp.status(), 200);

    // Consume the streaming body
    let body_text = resp.text().await.unwrap();
    assert!(body_text.contains("Hello"), "Stream should contain 'Hello'");
    assert!(body_text.contains(" world"), "Stream should contain ' world'");

    // Wait for the background task to finalize and record the step
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let store = Store::open(_tmp.path()).unwrap();
    let steps = store.get_steps(&timeline_id).unwrap();
    assert_eq!(steps.len(), 1, "Expected 1 streaming step recorded");

    let step = &steps[0];
    assert_eq!(step.model, "gpt-4o");
    assert_eq!(step.tokens_in, 10);
    assert_eq!(step.tokens_out, 5);
    assert_eq!(step.status.as_str(), "success");

    // Verify the synthetic response was stored (not raw SSE).
    // v0.13: response_blob is now a ResponseEnvelope (format=1) — unwrap to
    // get the assembled synthetic body before parsing as JSON.
    let resp_data = store.blobs.get(&step.response_blob).unwrap();
    let envelope = rewind_store::ResponseEnvelope::from_blob_bytes(
        step.response_blob_format,
        &resp_data,
    );
    let synthetic: serde_json::Value = serde_json::from_slice(&envelope.body).unwrap();
    assert_eq!(synthetic["choices"][0]["message"]["content"], "Hello world");
}

// ── Test: Instant Replay Cache Hit ───────────────────────────────

#[tokio::test]
async fn instant_replay_cache_hit() {
    let upstream_response = json!({
        "model": "gpt-4o",
        "choices": [{"message": {"role": "assistant", "content": "Cached!"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 30, "completion_tokens": 5}
    });

    let upstream_addr = start_mock_upstream(upstream_response).await;
    let (_tmp, store) = setup_store();

    let proxy = ProxyServer::new(
        store,
        "test-cache",
        &format!("http://{}", upstream_addr),
        true, // instant_replay enabled
        false,
    )
    .unwrap();
    let timeline_id = proxy.timeline_id().to_string();

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    tokio::spawn(async move {
        proxy.run(proxy_addr).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let req_body = json!({
        "model": "gpt-4o",
        "messages": [{"role": "user", "content": "test cache"}]
    });

    // First request — cache miss, hits upstream
    let resp1 = send_request(proxy_addr, &req_body).await;
    assert_eq!(resp1.status(), 200);
    let cache_header1 = resp1.headers().get("x-rewind-cache");
    assert!(cache_header1.is_none(), "First request should not have cache hit header");

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Second request — identical body, should be a cache hit
    let resp2 = send_request(proxy_addr, &req_body).await;
    assert_eq!(resp2.status(), 200);
    let cache_header2 = resp2.headers().get("x-rewind-cache").map(|v| v.to_str().unwrap().to_string());
    assert_eq!(cache_header2.as_deref(), Some("hit"), "Second request should be a cache hit");

    let saved_tokens_header = resp2.headers().get("x-rewind-saved-tokens").map(|v| v.to_str().unwrap().to_string());
    assert_eq!(saved_tokens_header.as_deref(), Some("35")); // 30 + 5

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let store = Store::open(_tmp.path()).unwrap();
    let steps = store.get_steps(&timeline_id).unwrap();
    assert_eq!(steps.len(), 2, "Both requests should be recorded as steps");

    // Second step should have duration_ms = 0 (served from cache)
    assert_eq!(steps[1].duration_ms, 0);
}

// ── Test: Step Counter Increments Across Requests ────────────────

#[tokio::test]
async fn step_counter_increments() {
    let upstream_response = json!({
        "model": "gpt-4o",
        "choices": [{"message": {"content": "ok"}}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 2}
    });

    let upstream_addr = start_mock_upstream(upstream_response).await;
    let (_tmp, store) = setup_store();

    let proxy = ProxyServer::new(
        store,
        "test-counter",
        &format!("http://{}", upstream_addr),
        false,
        false,
    )
    .unwrap();
    let timeline_id = proxy.timeline_id().to_string();

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    tokio::spawn(async move {
        proxy.run(proxy_addr).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Send 3 requests with different bodies to avoid any dedup
    for i in 1..=3 {
        let req_body = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": format!("msg {}", i)}]
        });
        let resp = send_request(proxy_addr, &req_body).await;
        assert_eq!(resp.status(), 200);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let store = Store::open(_tmp.path()).unwrap();
    let steps = store.get_steps(&timeline_id).unwrap();
    assert_eq!(steps.len(), 3);
    assert_eq!(steps[0].step_number, 1);
    assert_eq!(steps[1].step_number, 2);
    assert_eq!(steps[2].step_number, 3);
}
