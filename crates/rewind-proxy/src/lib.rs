use anyhow::Result;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Bytes, Frame};
use rewind_store::{Session, Step, StepStatus, StepType, Store, Timeline};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use futures_util::StreamExt;

type BoxBody = http_body_util::combinators::BoxBody<Bytes, hyper::Error>;

pub struct ProxyServer {
    store: Arc<Mutex<Store>>,
    session_id: String,
    timeline_id: String,
    step_counter: Arc<Mutex<u32>>,
    upstream_base: String,
    instant_replay: bool,
    /// Fork-and-execute: parent steps to replay (steps 1..=fork_at_step served from cache)
    replay_steps: Option<Vec<Step>>,
    /// Fork-and-execute: step number cutoff — steps <= this are served from cache
    fork_at_step: Option<u32>,
    /// Skip TLS certificate verification for upstream connections
    insecure: bool,
}

#[derive(Clone)]
struct ProxyState {
    store: Arc<Mutex<Store>>,
    session_id: String,
    timeline_id: String,
    step_counter: Arc<Mutex<u32>>,
    upstream_base: String,
    instant_replay: bool,
    replay_steps: Option<Arc<Vec<Step>>>,
    fork_at_step: Option<u32>,
    client: reqwest::Client,
}

impl ProxyServer {
    pub fn new(store: Store, session_name: &str, upstream_base: &str, instant_replay: bool, insecure: bool) -> Result<Self> {
        let session = Session::new(session_name);
        let timeline = Timeline::new_root(&session.id);

        store.create_session(&session)?;
        store.create_timeline(&timeline)?;

        tracing::info!(
            session_id = %session.id,
            timeline_id = %timeline.id,
            "Created new recording session: {}",
            session_name,
        );

        Ok(ProxyServer {
            store: Arc::new(Mutex::new(store)),
            session_id: session.id,
            timeline_id: timeline.id,
            step_counter: Arc::new(Mutex::new(0)),
            upstream_base: upstream_base.to_string(),
            instant_replay,
            replay_steps: None,
            fork_at_step: None,
            insecure,
        })
    }

    /// Create a proxy in fork-and-execute mode: steps 1..=fork_at serve cached responses,
    /// steps after that forward to upstream and record into the forked timeline.
    pub fn new_fork_execute(
        store: Store,
        session_id: &str,
        fork_timeline_id: &str,
        replay_steps: Vec<Step>,
        fork_at_step: u32,
        upstream_base: &str,
    ) -> Result<Self> {
        tracing::info!(
            session_id = %session_id,
            fork_timeline_id = %fork_timeline_id,
            fork_at_step = fork_at_step,
            cached_steps = replay_steps.len(),
            "Starting fork-and-execute proxy",
        );

        Ok(ProxyServer {
            store: Arc::new(Mutex::new(store)),
            session_id: session_id.to_string(),
            timeline_id: fork_timeline_id.to_string(),
            step_counter: Arc::new(Mutex::new(0)),
            upstream_base: upstream_base.to_string(),
            instant_replay: false,
            replay_steps: Some(replay_steps),
            fork_at_step: Some(fork_at_step),
            insecure: false,
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn timeline_id(&self) -> &str {
        &self.timeline_id
    }

    pub async fn run(self, addr: SocketAddr) -> Result<()> {
        let listener = TcpListener::bind(addr).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::AddrInUse {
                anyhow::anyhow!(
                    "Port {} already in use. Another rewind instance running? \
                     Stop it with: kill $(lsof -ti :{}) or use --port to pick a different port.",
                    addr.port(),
                    addr.port(),
                )
            } else {
                anyhow::anyhow!("Failed to bind {}: {}", addr, e)
            }
        })?;
        tracing::info!("Rewind proxy listening on {}", addr);

        let state = ProxyState {
            store: self.store,
            session_id: self.session_id,
            timeline_id: self.timeline_id,
            step_counter: self.step_counter,
            upstream_base: self.upstream_base,
            instant_replay: self.instant_replay,
            replay_steps: self.replay_steps.map(Arc::new),
            fork_at_step: self.fork_at_step,
            client: make_client(self.insecure),
        };

        loop {
            let (stream, _) = listener.accept().await?;
            let io = TokioIo::new(stream);
            let state = state.clone();

            tokio::task::spawn(async move {
                if let Err(err) = http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |req| {
                            let state = state.clone();
                            async move { handle_request(req, state).await }
                        }),
                    )
                    .await
                {
                    tracing::error!("Connection error: {:?}", err);
                }
            });
        }
    }
}

fn make_client(insecure: bool) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .connect_timeout(std::time::Duration::from_secs(30));
    if insecure {
        builder = builder.danger_accept_invalid_certs(true);
    }
    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

fn is_stream_request(body: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
        .unwrap_or(false)
}

async fn handle_request(
    req: Request<Incoming>,
    state: ProxyState,
) -> Result<Response<BoxBody>, hyper::Error> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();

    // ── Health check endpoint — no DB access, <1ms ──
    if path == "/_rewind/health" && method == hyper::Method::GET {
        let step_count = *state.step_counter.lock().unwrap();
        let body = serde_json::json!({
            "status": "ok",
            "version": env!("CARGO_PKG_VERSION"),
            "session": state.session_id,
            "steps": step_count,
        });
        let resp = Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(box_full(Bytes::from(body.to_string())))
            .unwrap();
        return Ok(resp);
    }

    let step_number = {
        let mut counter = state.step_counter.lock().unwrap();
        *counter += 1;
        *counter
    };

    tracing::info!(step = step_number, method = %method, path = %path, "Intercepting request");

    let (parts, body) = req.into_parts();
    let body_bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(e) => {
            tracing::error!("Failed to collect request body: {:?}", e);
            let resp = Response::builder()
                .status(502)
                .body(box_full(Bytes::from("Proxy error")))
                .unwrap();
            return Ok(resp);
        }
    };

    let model = extract_model(&body_bytes)
        .or_else(|| extract_model_from_path(&path))
        .unwrap_or_else(|| "unknown".to_string());

    let request_hash = {
        let store = state.store.lock().unwrap();
        store.blobs.put(&body_bytes).unwrap_or_default()
    };

    let streaming = is_stream_request(&body_bytes);

    // ── Fork-and-Execute: serve parent steps from cache ──
    if let (Some(replay_steps), Some(fork_at)) = (&state.replay_steps, state.fork_at_step)
        && step_number <= fork_at
        && let Some(parent_step) = replay_steps.iter().find(|s| s.step_number == step_number)
    {
        let resp_data = {
            let store = state.store.lock().unwrap();
            store.blobs.get(&parent_step.response_blob).unwrap_or_default()
        };

        // Record a replayed step in the forked timeline
        let mut step = Step::new_llm_call(&state.timeline_id, &state.session_id, step_number, &parent_step.model);
        step.status = parent_step.status.clone();
        step.duration_ms = 0;
        step.tokens_in = parent_step.tokens_in;
        step.tokens_out = parent_step.tokens_out;
        step.request_blob = request_hash.clone();
        step.response_blob = parent_step.response_blob.clone();
        step.step_type = parent_step.step_type.clone();

        {
            let store = state.store.lock().unwrap();
            let _ = store.create_step(&step);
            let _ = store.update_session_stats(&state.session_id, step_number, 0);
        }

        tracing::info!(
            step = step_number, model = %parent_step.model,
            "⏪ Fork replay — served cached step {}/{} (0ms, 0 tokens)",
            step_number, fork_at,
        );

        let response = Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .header("x-rewind-replay", "fork")
            .header("x-rewind-cached-step", step_number.to_string())
            .body(box_full(Bytes::from(resp_data)))
            .unwrap();
        return Ok(response);
    }

    // ── Instant Replay: check cache before hitting upstream ──
    if state.instant_replay && !streaming
        && let Some(cached) = {
            let store = state.store.lock().unwrap();
            store.cache_get(&request_hash).ok().flatten()
        } {
            // Cache hit! Return recorded response instantly
            let resp_data = {
                let store = state.store.lock().unwrap();
                store.blobs.get(&cached.response_blob).unwrap_or_default()
            };

            // Record as a replayed step
            let mut step = Step::new_llm_call(&state.timeline_id, &state.session_id, step_number, &model);
            step.status = StepStatus::Success;
            step.duration_ms = 0;
            step.tokens_in = cached.tokens_in;
            step.tokens_out = cached.tokens_out;
            step.request_blob = request_hash.clone();
            step.response_blob = cached.response_blob.clone();

            {
                let store = state.store.lock().unwrap();
                let _ = store.create_step(&step);
                let _ = store.update_session_stats(&state.session_id, step_number, 0);
                let _ = store.cache_hit(&request_hash);
            }

            tracing::info!(
                step = step_number, model = %model,
                "⚡ Instant Replay cache hit — saved {}+{} tokens",
                cached.tokens_in, cached.tokens_out,
            );

            let response = Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .header("x-rewind-cache", "hit")
                .header("x-rewind-saved-tokens", format!("{}", cached.tokens_in + cached.tokens_out))
                .body(box_full(Bytes::from(resp_data)))
                .unwrap();
            return Ok(response);
        }

    let upstream_url = format!("{}{}", state.upstream_base, path);
    let mut upstream_req = state.client.request(
        reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap(),
        &upstream_url,
    );

    for (key, value) in parts.headers.iter() {
        if key == "host" || key == "connection" {
            continue;
        }
        if let Ok(v) = value.to_str() {
            upstream_req = upstream_req.header(key.as_str(), v);
        }
    }

    let start = std::time::Instant::now();
    let upstream_resp = upstream_req.body(body_bytes.to_vec()).send().await;

    match upstream_resp {
        Ok(resp) if streaming => {
            handle_streaming_response(resp, state, step_number, model, request_hash, start).await
        }
        Ok(resp) => {
            handle_buffered_response(resp, state, step_number, model, request_hash, start).await
        }
        Err(e) => {
            let duration_ms = start.elapsed().as_millis() as u64;
            tracing::error!(step = step_number, "Upstream request failed: {:?}", e);

            let mut step = Step::new_llm_call(
                &state.timeline_id,
                &state.session_id,
                step_number,
                &model,
            );
            step.status = StepStatus::Error;
            step.duration_ms = duration_ms;
            step.request_blob = request_hash;
            step.error = Some(format!("Upstream error: {}", e));

            {
                let store = state.store.lock().unwrap();
                let _ = store.create_step(&step);
            }

            Ok(Response::builder()
                .status(502)
                .body(box_full(Bytes::from(format!("Upstream error: {}", e))))
                .unwrap())
        }
    }
}

// ── Non-streaming (original behavior) ──────────────────────────

async fn handle_buffered_response(
    resp: reqwest::Response,
    state: ProxyState,
    step_number: u32,
    model: String,
    request_hash: String,
    start: std::time::Instant,
) -> Result<Response<BoxBody>, hyper::Error> {
    let status = resp.status();
    let resp_bytes = resp.bytes().await.unwrap_or_default();
    let duration_ms = start.elapsed().as_millis() as u64;

    let (tokens_in, tokens_out) = extract_usage(&resp_bytes);

    let response_hash = {
        let store = state.store.lock().unwrap();
        store.blobs.put(&resp_bytes).unwrap_or_default()
    };

    let step_status = if status.is_success() { StepStatus::Success } else { StepStatus::Error };
    let error = if !status.is_success() {
        Some(format!("HTTP {}: {}", status.as_u16(),
            String::from_utf8_lossy(&resp_bytes).chars().take(200).collect::<String>()))
    } else {
        None
    };

    let mut step = Step::new_llm_call(&state.timeline_id, &state.session_id, step_number, &model);
    step.status = step_status;
    step.duration_ms = duration_ms;
    step.tokens_in = tokens_in;
    step.tokens_out = tokens_out;
    step.request_blob = request_hash.clone();
    step.response_blob = response_hash.clone();
    step.error = error;

    if is_tool_call_response(&resp_bytes) {
        step.step_type = StepType::ToolCall;
    }

    {
        let store = state.store.lock().unwrap();
        if let Err(e) = store.create_step(&step) {
            tracing::error!("Failed to save step: {:?}", e);
        }
        let _ = store.update_session_stats(&state.session_id, step_number, tokens_in + tokens_out);
    }

    // Populate Instant Replay cache on success
    if state.instant_replay && step.status == StepStatus::Success && tokens_in > 0 {
        let store = state.store.lock().unwrap();
        let _ = store.cache_put(&request_hash, &response_hash, &model, tokens_in, tokens_out);
    }

    tracing::info!(
        step = step_number, model = %model,
        tokens_in, tokens_out, duration_ms,
        status = %step.status.as_str(),
        "Step recorded"
    );

    let mut response = Response::builder().status(status);
    response = response.header("content-type", "application/json");
    Ok(response.body(box_full(resp_bytes)).unwrap())
}

// ── Streaming (SSE pass-through + accumulate) ──────────────────

async fn handle_streaming_response(
    resp: reqwest::Response,
    state: ProxyState,
    step_number: u32,
    model: String,
    request_hash: String,
    start: std::time::Instant,
) -> Result<Response<BoxBody>, hyper::Error> {
    let status = resp.status();

    // Get content-type from upstream (should be text/event-stream)
    let content_type = resp.headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("text/event-stream")
        .to_string();

    // We'll stream chunks through a channel: upstream → channel → client
    // While also accumulating all chunks for recording.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(64);

    let store = state.store.clone();
    let session_id = state.session_id.clone();
    let timeline_id = state.timeline_id.clone();
    let model_clone = model.clone();

    // Spawn a task that reads from upstream and:
    //   1. Forwards each chunk to the channel (→ client)
    //   2. Accumulates raw bytes for recording
    //   3. Parses SSE events to reconstruct the final response
    tokio::spawn(async move {
        let mut byte_stream = resp.bytes_stream();
        let mut accumulated_raw = Vec::new();
        let mut assembled_text = String::new();
        let mut total_input_tokens: u64 = 0;
        let mut total_output_tokens: u64 = 0;
        let mut response_model = model_clone.clone();
        let mut has_tool_calls = false;
        let mut tool_calls_json: Vec<serde_json::Value> = Vec::new();

        while let Some(chunk_result) = byte_stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    accumulated_raw.extend_from_slice(&chunk);

                    // Parse SSE events from this chunk
                    let chunk_str = String::from_utf8_lossy(&chunk);
                    for line in chunk_str.lines() {
                        if let Some(data) = line.strip_prefix("data: ") {
                            if data == "[DONE]" {
                                continue;
                            }
                            if let Ok(event) = serde_json::from_str::<serde_json::Value>(data) {
                                parse_sse_event(&event, &mut assembled_text, &mut total_input_tokens,
                                    &mut total_output_tokens, &mut response_model,
                                    &mut has_tool_calls, &mut tool_calls_json);
                            }
                        }
                    }

                    // Forward chunk to client
                    if tx.send(Ok(Frame::data(chunk))).await.is_err() {
                        break; // client disconnected
                    }
                }
                Err(e) => {
                    tracing::error!(step = step_number, "Streaming chunk error: {:?}", e);
                    break;
                }
            }
        }

        // Stream is done — record the assembled response
        let duration_ms = start.elapsed().as_millis() as u64;

        // Build a synthetic complete response for storage
        let synthetic_response = build_synthetic_response(
            &response_model, &assembled_text, total_input_tokens, total_output_tokens,
            has_tool_calls, &tool_calls_json,
        );

        let resp_bytes = serde_json::to_vec(&synthetic_response).unwrap_or_default();

        let response_hash = {
            let s = store.lock().unwrap();
            s.blobs.put(&resp_bytes).unwrap_or_default()
        };

        // Also store raw SSE for forensics
        {
            let s = store.lock().unwrap();
            let _ = s.blobs.put(&accumulated_raw); // available via hash if needed
        }

        let mut step = Step::new_llm_call(&timeline_id, &session_id, step_number, &model_clone);
        step.status = StepStatus::Success;
        step.duration_ms = duration_ms;
        step.tokens_in = total_input_tokens;
        step.tokens_out = total_output_tokens;
        step.request_blob = request_hash;
        step.response_blob = response_hash;

        if has_tool_calls {
            step.step_type = StepType::ToolCall;
        }

        {
            let s = store.lock().unwrap();
            if let Err(e) = s.create_step(&step) {
                tracing::error!("Failed to save streaming step: {:?}", e);
            }
            let _ = s.update_session_stats(&session_id, step_number, total_input_tokens + total_output_tokens);
        }

        tracing::info!(
            step = step_number, model = %model_clone,
            tokens_in = total_input_tokens, tokens_out = total_output_tokens,
            duration_ms, "Streaming step recorded"
        );
    });

    // Build a streaming response body from the channel
    let body_stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let stream_body = StreamBody::new(body_stream);
    let boxed: BoxBody = http_body_util::BodyExt::boxed(stream_body);

    let mut response = Response::builder().status(status);
    response = response.header("content-type", content_type);
    response = response.header("cache-control", "no-cache");

    Ok(response.body(boxed).unwrap())
}

/// Parse a single SSE event and accumulate state
fn parse_sse_event(
    event: &serde_json::Value,
    text: &mut String,
    input_tokens: &mut u64,
    output_tokens: &mut u64,
    model: &mut String,
    has_tool_calls: &mut bool,
    tool_calls: &mut Vec<serde_json::Value>,
) {
    // OpenAI streaming format: choices[0].delta.content
    if let Some(choices) = event.get("choices").and_then(|c| c.as_array()) {
        if let Some(delta) = choices.first().and_then(|c| c.get("delta")) {
            if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                text.push_str(content);
            }
            if let Some(tc) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                *has_tool_calls = true;
                for call in tc {
                    tool_calls.push(call.clone());
                }
            }
        }
        // OpenAI sends usage in the final chunk
        if let Some(usage) = event.get("usage") {
            *input_tokens = usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(*input_tokens);
            *output_tokens = usage.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(*output_tokens);
        }
    }

    // Anthropic streaming format
    if let Some(event_type) = event.get("type").and_then(|t| t.as_str()) {
        match event_type {
            "message_start" => {
                if let Some(msg) = event.get("message") {
                    if let Some(m) = msg.get("model").and_then(|m| m.as_str()) {
                        *model = m.to_string();
                    }
                    if let Some(usage) = msg.get("usage") {
                        *input_tokens = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    }
                }
            }
            "content_block_start" => {
                if let Some(block) = event.get("content_block")
                    && block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        *has_tool_calls = true;
                        tool_calls.push(block.clone());
                    }
            }
            "content_block_delta" => {
                if let Some(delta) = event.get("delta") {
                    if let Some(t) = delta.get("text").and_then(|t| t.as_str()) {
                        text.push_str(t);
                    }
                    // Tool input JSON delta — accumulate into the last tool call's input,
                    // NOT into the text buffer (which would corrupt content text).
                    if let Some(partial) = delta.get("partial_json").and_then(|p| p.as_str())
                        && let Some(last_tc) = tool_calls.last_mut()
                    {
                        let existing = last_tc.get("_partial_input")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        last_tc["_partial_input"] = serde_json::Value::String(
                            format!("{}{}", existing, partial),
                        );
                    }
                }
            }
            "message_delta" => {
                if let Some(usage) = event.get("usage") {
                    *output_tokens = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(*output_tokens);
                }
            }
            _ => {}
        }
    }

    // Model field at top level (OpenAI)
    if let Some(m) = event.get("model").and_then(|m| m.as_str())
        && !m.is_empty() {
            *model = m.to_string();
        }
}

/// Build a synthetic complete response from accumulated streaming data
fn build_synthetic_response(
    model: &str,
    text: &str,
    input_tokens: u64,
    output_tokens: u64,
    has_tool_calls: bool,
    tool_calls: &[serde_json::Value],
) -> serde_json::Value {
    if model.contains("claude") || model.contains("anthropic") {
        // Anthropic format
        let mut content = vec![];
        if !text.is_empty() {
            content.push(serde_json::json!({"type": "text", "text": text}));
        }
        if has_tool_calls {
            for tc in tool_calls {
                let mut tc = tc.clone();
                // Finalize accumulated partial_json into the tool call's input field
                if let Some(partial) = tc.get("_partial_input").and_then(|v| v.as_str()) {
                    if let Ok(input_val) = serde_json::from_str::<serde_json::Value>(partial) {
                        tc["input"] = input_val;
                    } else {
                        tc["input"] = serde_json::Value::String(partial.to_string());
                    }
                    tc.as_object_mut().map(|o| o.remove("_partial_input"));
                }
                content.push(tc);
            }
        }
        serde_json::json!({
            "model": model,
            "type": "message",
            "role": "assistant",
            "content": content,
            "usage": {
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
            }
        })
    } else {
        // OpenAI format
        let mut message = serde_json::json!({"role": "assistant"});
        if !text.is_empty() {
            message["content"] = serde_json::Value::String(text.to_string());
        }
        if has_tool_calls && !tool_calls.is_empty() {
            message["tool_calls"] = serde_json::Value::Array(tool_calls.to_vec());
        }
        serde_json::json!({
            "model": model,
            "choices": [{"index": 0, "message": message, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": input_tokens,
                "completion_tokens": output_tokens,
            }
        })
    }
}

// ── Helpers ────────────────────────────────────────────────────

fn box_full(bytes: Bytes) -> BoxBody {
    http_body_util::BodyExt::boxed(Full::new(bytes).map_err(|_| unreachable!()))
}

fn extract_model(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(String::from))
}

fn extract_model_from_path(path: &str) -> Option<String> {
    if path.contains("/model/") {
        let parts: Vec<&str> = path.split("/model/").collect();
        if parts.len() > 1 {
            let model_and_rest = parts[1];
            let model = model_and_rest.split('/').next().unwrap_or(model_and_rest);
            return Some(model.to_string());
        }
    }
    None
}

fn extract_usage(resp_bytes: &[u8]) -> (u64, u64) {
    let val: serde_json::Value = serde_json::from_slice(resp_bytes).unwrap_or_default();

    if let Some(usage) = val.get("usage") {
        let input = usage.get("prompt_tokens")
            .or(usage.get("input_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let output = usage.get("completion_tokens")
            .or(usage.get("output_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        return (input, output);
    }

    (0, 0)
}

fn is_tool_call_response(resp_bytes: &[u8]) -> bool {
    let val: serde_json::Value = serde_json::from_slice(resp_bytes).unwrap_or_default();

    if let Some(choices) = val.get("choices").and_then(|c| c.as_array()) {
        for choice in choices {
            if let Some(msg) = choice.get("message")
                && msg.get("tool_calls").and_then(|t| t.as_array()).is_some_and(|a| !a.is_empty()) {
                    return true;
                }
        }
    }

    if let Some(content) = val.get("content").and_then(|c| c.as_array()) {
        for block in content {
            if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                return true;
            }
        }
    }

    false
}
