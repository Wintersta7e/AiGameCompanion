use std::collections::HashMap;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio_stream::wrappers::LinesStream;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

struct ProxyState {
    token: String,
    active_child: Mutex<Option<(u64, Child)>>,
    claude_available: bool,
    codex_available: bool,
}

#[derive(Deserialize)]
struct ChatRequest {
    request_id: u64,
    messages: Vec<ChatMessage>,
    screenshot: Option<String>,
    system_prompt: String,
    provider: String,
    model: String,
    #[allow(dead_code)]
    max_tokens: u32,
}

#[derive(Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct CancelRequest {
    request_id: u64,
}

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    providers: HashMap<String, bool>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn generate_token() -> String {
    let bytes: [u8; 32] = rand::random();
    hex::encode(bytes)
}

fn is_cli_available(name: &str) -> bool {
    // Use std::process for a quick synchronous check during startup
    std::process::Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn validate_token(headers: &HeaderMap, expected: &str) -> Result<(), StatusCode> {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let token = auth.strip_prefix("Bearer ").ok_or(StatusCode::UNAUTHORIZED)?;

    if token == expected {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// Build a single SSE `data:` frame with a trailing double-newline.
fn sse_data(json: &str) -> String {
    format!("data: {json}\n\n")
}

/// Build an SSE text chunk frame.
fn sse_text(text: &str) -> String {
    // Escape for JSON string value
    let escaped = serde_json::to_string(text).unwrap_or_else(|_| format!("\"{text}\""));
    sse_data(&format!("{{\"text\":{escaped}}}"))
}

/// Build an SSE error frame.
fn sse_error(msg: &str) -> String {
    let escaped = serde_json::to_string(msg).unwrap_or_else(|_| format!("\"{msg}\""));
    sse_data(&format!("{{\"error\":{escaped}}}"))
}

const SSE_DONE: &str = "data: [DONE]\n\n";

// ---------------------------------------------------------------------------
// Endpoints
// ---------------------------------------------------------------------------

async fn health(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
) -> Result<axum::Json<HealthResponse>, StatusCode> {
    validate_token(&headers, &state.token)?;

    let mut providers = HashMap::new();
    providers.insert("claude".to_owned(), state.claude_available);
    providers.insert("codex".to_owned(), state.codex_available);

    Ok(axum::Json(HealthResponse {
        status: "ok".to_owned(),
        providers,
    }))
}

async fn chat(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    axum::Json(req): axum::Json<ChatRequest>,
) -> Result<Response, StatusCode> {
    validate_token(&headers, &state.token)?;

    // Kill any in-flight child before spawning a new one
    kill_active_child(&state).await;

    match req.provider.as_str() {
        "claude" => {
            if !state.claude_available {
                return Ok(error_stream_response("Claude CLI is not available on this system"));
            }
            handle_claude(state, req).await
        }
        "codex" => {
            if !state.codex_available {
                return Ok(error_stream_response("Codex CLI is not available on this system"));
            }
            handle_codex(state, req).await
        }
        other => {
            let msg = format!("Unknown provider: {other}");
            Ok(error_stream_response(&msg))
        }
    }
}

async fn cancel(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    axum::Json(req): axum::Json<CancelRequest>,
) -> Result<StatusCode, StatusCode> {
    validate_token(&headers, &state.token)?;

    let mut guard = state.active_child.lock().await;
    if let Some((id, ref mut child)) = *guard {
        if id == req.request_id {
            let _ = child.kill().await;
            *guard = None;
            tracing::info!("Cancelled request {}", req.request_id);
        }
    }
    Ok(StatusCode::OK)
}

// ---------------------------------------------------------------------------
// Provider handlers
// ---------------------------------------------------------------------------

async fn handle_claude(
    state: Arc<ProxyState>,
    req: ChatRequest,
) -> Result<Response, StatusCode> {
    let mut cmd = Command::new("claude");
    cmd.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--tools",
        "",
        "--no-session-persistence",
        "--model",
        &req.model,
        "--append-system-prompt",
        &req.system_prompt,
    ]);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::null());

    let mut child = cmd.spawn().map_err(|e| {
        tracing::error!("Failed to spawn claude CLI: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Build NDJSON input for Claude
    let input = build_claude_input(&req.messages, req.screenshot.as_deref());
    let mut stdin = child.stdin.take().ok_or_else(|| {
        tracing::error!("Failed to take stdin from claude process");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Write input and close stdin
    tokio::spawn(async move {
        let _ = stdin.write_all(input.as_bytes()).await;
        let _ = stdin.flush().await;
        drop(stdin);
    });

    let stdout = child.stdout.take().ok_or_else(|| {
        tracing::error!("Failed to take stdout from claude process");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Register active child for cancellation
    {
        let mut guard = state.active_child.lock().await;
        *guard = Some((req.request_id, child));
    }

    let reader = BufReader::new(stdout);
    let lines = LinesStream::new(reader.lines());

    let request_id = req.request_id;
    let state_clone = Arc::clone(&state);

    let stream = lines
        .filter_map(move |line_result: Result<String, std::io::Error>| {
            let state_ref = Arc::clone(&state_clone);
            async move {
                match line_result {
                    Ok(line) if line.trim().is_empty() => None,
                    Ok(line) => parse_claude_line(&line),
                    Err(e) => {
                        tracing::warn!("Error reading claude stdout: {e}");
                        // Clean up active child on read error
                        let mut guard = state_ref.active_child.lock().await;
                        if matches!(&*guard, Some((id, _)) if *id == request_id) {
                            *guard = None;
                        }
                        Some(sse_error("Failed to read from Claude CLI"))
                    }
                }
            }
        })
        .chain(futures_util::stream::once(async move {
            // Clean up active child when stream ends
            let mut guard = state.active_child.lock().await;
            if matches!(&*guard, Some((id, _)) if *id == request_id) {
                *guard = None;
            }
            SSE_DONE.to_owned()
        }));

    Ok(sse_response(stream))
}

async fn handle_codex(
    state: Arc<ProxyState>,
    req: ChatRequest,
) -> Result<Response, StatusCode> {
    // Codex requires a git directory -- use a temp dir with git init
    let work_dir = std::env::temp_dir().join("aigc-codex-workdir");
    if !work_dir.exists() {
        let _ = std::fs::create_dir_all(&work_dir);
        // Initialize a bare git repo so Codex is happy
        let _ = std::process::Command::new("git")
            .args(["init"])
            .current_dir(&work_dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    let mut cmd = Command::new("codex");
    cmd.args([
        "-a",
        "never",
        "exec",
        "-",
        "--model",
        &req.model,
        "--json",
        "--sandbox",
        "read-only",
        "-C",
        &work_dir.to_string_lossy(),
    ]);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::null());

    let mut child = cmd.spawn().map_err(|e| {
        tracing::error!("Failed to spawn codex CLI: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Build plain text input for Codex
    let input = build_codex_input(&req.system_prompt, &req.messages);
    let mut stdin = child.stdin.take().ok_or_else(|| {
        tracing::error!("Failed to take stdin from codex process");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    tokio::spawn(async move {
        let _ = stdin.write_all(input.as_bytes()).await;
        let _ = stdin.flush().await;
        drop(stdin);
    });

    let stdout = child.stdout.take().ok_or_else(|| {
        tracing::error!("Failed to take stdout from codex process");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    {
        let mut guard = state.active_child.lock().await;
        *guard = Some((req.request_id, child));
    }

    let reader = BufReader::new(stdout);
    let lines = LinesStream::new(reader.lines());

    let request_id = req.request_id;
    let state_clone = Arc::clone(&state);

    let stream = lines
        .filter_map(move |line_result: Result<String, std::io::Error>| {
            let state_ref = Arc::clone(&state_clone);
            async move {
                match line_result {
                    Ok(line) if line.trim().is_empty() => None,
                    Ok(line) => parse_codex_line(&line),
                    Err(e) => {
                        tracing::warn!("Error reading codex stdout: {e}");
                        let mut guard = state_ref.active_child.lock().await;
                        if matches!(&*guard, Some((id, _)) if *id == request_id) {
                            *guard = None;
                        }
                        Some(sse_error("Failed to read from Codex CLI"))
                    }
                }
            }
        })
        .chain(futures_util::stream::once(async move {
            let mut guard = state.active_child.lock().await;
            if matches!(&*guard, Some((id, _)) if *id == request_id) {
                *guard = None;
            }
            SSE_DONE.to_owned()
        }));

    Ok(sse_response(stream))
}

// ---------------------------------------------------------------------------
// Input builders
// ---------------------------------------------------------------------------

fn build_claude_input(messages: &[ChatMessage], screenshot: Option<&str>) -> String {
    // Build the content array for the last user message
    let mut content_parts = Vec::new();

    // Collect all messages into conversation -- Claude stream-json expects a single
    // user turn with all context.  We concatenate prior messages as text context,
    // then add the screenshot if present.
    let mut combined_text = String::new();
    for msg in messages {
        if !combined_text.is_empty() {
            combined_text.push('\n');
        }
        let _ = write!(combined_text, "[{}]: {}", msg.role, msg.content);
    }

    content_parts.push(serde_json::json!({
        "type": "text",
        "text": combined_text,
    }));

    if let Some(data) = screenshot {
        content_parts.push(serde_json::json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/png",
                "data": data,
            }
        }));
    }

    let input_msg = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": content_parts,
        },
        "parent_tool_use_id": null,
        "session_id": null,
    });

    let mut out = serde_json::to_string(&input_msg).unwrap_or_default();
    out.push('\n');
    out
}

fn build_codex_input(system_prompt: &str, messages: &[ChatMessage]) -> String {
    let mut text = String::new();
    if !system_prompt.is_empty() {
        text.push_str(system_prompt);
        text.push_str("\n\n");
    }
    for msg in messages {
        let _ = writeln!(text, "[{}]: {}", msg.role, msg.content);
    }
    text
}

// ---------------------------------------------------------------------------
// Output parsers
// ---------------------------------------------------------------------------

/// Parse a single NDJSON line from Claude CLI stdout.
/// Returns `Some(sse_frame)` to emit, or `None` to skip.
fn parse_claude_line(line: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;

    let msg_type = v.get("type")?.as_str()?;

    match msg_type {
        "stream_event" => {
            let delta_type = v
                .pointer("/event/delta/type")
                .and_then(serde_json::Value::as_str)?;
            if delta_type == "text_delta" {
                let text = v
                    .pointer("/event/delta/text")
                    .and_then(serde_json::Value::as_str)?;
                Some(sse_text(text))
            } else {
                None
            }
        }
        "result" => {
            let is_error = v.get("is_error").and_then(serde_json::Value::as_bool).unwrap_or(false);
            if is_error {
                let error_msg = v
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Unknown Claude CLI error");
                Some(sse_error(error_msg))
            } else {
                // Successful result -- stream is complete
                None
            }
        }
        // system, assistant, etc -- ignore
        _ => None,
    }
}

/// Parse a single JSONL line from Codex CLI stdout.
fn parse_codex_line(line: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;

    // Check for refusal
    if let Some(refusal_type) = v.get("type").and_then(serde_json::Value::as_str) {
        if refusal_type == "refusal" {
            let msg = v
                .get("content")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Model refused the request");
            return Some(sse_error(msg));
        }
    }

    // Look for content array with output_text items
    if let Some(content) = v.get("content").and_then(serde_json::Value::as_array) {
        let mut collected = String::new();
        for item in content {
            if item.get("type").and_then(serde_json::Value::as_str) == Some("output_text") {
                if let Some(text) = item.get("text").and_then(serde_json::Value::as_str) {
                    collected.push_str(text);
                }
            }
        }
        if !collected.is_empty() {
            return Some(sse_text(&collected));
        }
    }

    // Best-effort: check for top-level text field
    if let Some(text) = v.get("text").and_then(serde_json::Value::as_str) {
        if !text.is_empty() {
            return Some(sse_text(text));
        }
    }

    // Unparseable structure -- log and skip
    tracing::debug!("Ignoring unrecognized codex output: {line}");
    None
}

// ---------------------------------------------------------------------------
// SSE response helpers
// ---------------------------------------------------------------------------

fn sse_response(stream: impl futures_util::Stream<Item = String> + Send + 'static) -> Response {
    let body = Body::from_stream(stream.map(Ok::<_, std::convert::Infallible>));

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .body(body)
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::empty())
                .expect("fallback response must build")
        })
}

fn error_stream_response(msg: &str) -> Response {
    let frame = format!("{}{SSE_DONE}", sse_error(msg));
    let stream = futures_util::stream::once(async move { Ok::<_, std::convert::Infallible>(frame) });
    let body = Body::from_stream(stream);

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .body(body)
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::empty())
                .expect("fallback response must build")
        })
}

async fn kill_active_child(state: &ProxyState) {
    let mut guard = state.active_child.lock().await;
    if let Some((id, ref mut child)) = *guard {
        tracing::info!("Killing in-flight request {id}");
        let _ = child.kill().await;
        *guard = None;
    }
}

// ---------------------------------------------------------------------------
// Port file helpers
// ---------------------------------------------------------------------------

fn port_file_path() -> std::path::PathBuf {
    std::env::current_exe()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .parent()
        .map_or_else(|| std::path::PathBuf::from("."), std::path::Path::to_path_buf)
        .join("proxy.port")
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Start the proxy HTTP server on a random available port.
///
/// Writes `proxy.port` (containing PORT and TOKEN) next to the launcher exe.
/// The returned `SocketAddr` is for logging purposes.
///
/// The server is spawned as a background tokio task -- this function returns
/// once the listener is bound and ready.
pub async fn start_proxy() -> Result<SocketAddr, Box<dyn std::error::Error>> {
    let token = generate_token();
    let claude_available = is_cli_available("claude");
    let codex_available = is_cli_available("codex");

    tracing::info!(
        "CLI availability -- claude: {claude_available}, codex: {codex_available}"
    );

    let state = Arc::new(ProxyState {
        token: token.clone(),
        active_child: Mutex::new(None),
        claude_available,
        codex_available,
    });

    let router = Router::new()
        .route("/health", get(health))
        .route("/chat", post(chat))
        .route("/cancel", post(cancel))
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;

    // Write port file
    let port_file = port_file_path();
    let contents = format!("{}\n{token}", addr.port());
    std::fs::write(&port_file, contents).map_err(|e| {
        tracing::error!("Failed to write proxy.port: {e}");
        e
    })?;
    tracing::info!("Wrote proxy.port to {}", port_file.display());

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!("Proxy server error: {e}");
        }
    });

    Ok(addr)
}

/// Remove the proxy.port file. Call on shutdown.
pub fn cleanup_port_file() {
    let path = port_file_path();
    if path.exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::warn!("Failed to remove proxy.port: {e}");
        } else {
            tracing::info!("Cleaned up proxy.port");
        }
    }
}
