use std::collections::HashMap;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::os::windows::process::CommandExt as _;
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

/// How to invoke a CLI tool.
#[derive(Debug, Clone, Copy)]
enum CliMode {
    /// Not available on this system.
    Unavailable,
    /// Available directly on Windows PATH.
    Native,
    /// Available inside WSL (invoke via `wsl.exe`).
    Wsl,
}

impl CliMode {
    fn is_available(self) -> bool {
        !matches!(self, Self::Unavailable)
    }
}

struct ProxyState {
    token: String,
    active_child: Mutex<Option<(u64, Child)>>,
    claude_mode: CliMode,
    codex_mode: CliMode,
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

/// Windows `CREATE_NO_WINDOW` flag -- prevents console popups from
/// `wsl.exe` and other console subsystem processes.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Escape a string for use in a bash -c / -ic command.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn generate_token() -> String {
    let bytes: [u8; 32] = rand::random();
    hex::encode(bytes)
}

/// Check if a CLI tool is available, first natively on Windows PATH,
/// then inside WSL (using `bash -ic` to pick up nvm/profile PATH).
fn detect_cli(name: &str) -> CliMode {
    // Try native Windows first.
    let native = std::process::Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if native {
        return CliMode::Native;
    }

    // Try via WSL with interactive shell so .bashrc (nvm, etc.) is sourced.
    let version_cmd = format!("{name} --version");
    let wsl = std::process::Command::new("wsl.exe")
        .args(["--", "bash", "-ic", &version_cmd])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if wsl {
        return CliMode::Wsl;
    }

    CliMode::Unavailable
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
    providers.insert("claude".to_owned(), state.claude_mode.is_available());
    providers.insert("openai".to_owned(), state.codex_mode.is_available());

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
            if !state.claude_mode.is_available() {
                return Ok(error_stream_response("Claude CLI is not available on this system"));
            }
            handle_claude(state, req).await
        }
        "openai" | "codex" => {
            if !state.codex_mode.is_available() {
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
    let claude_args = format!(
        "claude -p --input-format stream-json --output-format stream-json \
         --verbose --include-partial-messages --tools '' \
         --no-session-persistence --model {} --system-prompt {}",
        shell_escape(&req.model),
        shell_escape(&req.system_prompt),
    );
    let mut cmd = if let CliMode::Wsl = state.claude_mode {
        let mut c = Command::new("wsl.exe");
        c.args(["--", "bash", "-ic", &claude_args]);
        c
    } else {
        let mut c = Command::new("claude");
        c.args([
            "-p", "--input-format", "stream-json",
            "--output-format", "stream-json", "--verbose",
            "--include-partial-messages", "--tools", "",
            "--no-session-persistence", "--model", &req.model,
            "--system-prompt", &req.system_prompt,
        ]);
        c
    };
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.creation_flags(CREATE_NO_WINDOW);

    let mut child = cmd.spawn().map_err(|e| {
        tracing::error!("Failed to spawn claude CLI: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Build NDJSON input with image embedded in the content array.
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

    // Log stderr in the background so we can diagnose Claude CLI failures.
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = LinesStream::new(reader.lines());
            while let Some(Ok(line)) = lines.next().await {
                if !line.trim().is_empty() {
                    tracing::warn!("claude stderr: {line}");
                }
            }
        });
    }

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
    // Codex requires a git directory -- use a temp dir with git init.
    let is_wsl = matches!(state.codex_mode, CliMode::Wsl);
    let work_dir_str = if is_wsl {
        // WSL-side path. Ensure it exists.
        let dir = "/tmp/aigc-codex-workdir";
        let _ = std::process::Command::new("wsl.exe")
            .args(["--", "bash", "-c", &format!("mkdir -p {dir} && git -C {dir} init")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        dir.to_string()
    } else {
        let dir = std::env::temp_dir().join("aigc-codex-workdir");
        if !dir.exists() {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::process::Command::new("git")
                .args(["init"])
                .current_dir(&dir)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .status();
        }
        dir.to_string_lossy().to_string()
    };

    // Let Codex use its own default model (gpt-5.3-codex) unless the user
    // explicitly configured a codex-compatible model. The overlay's
    // openai.model (gpt-4o) is for direct API use, not Codex CLI.
    let mut cmd = if is_wsl {
        let codex_cmd = format!(
            "codex -a never -s read-only -C {} exec --skip-git-repo-check",
            shell_escape(&work_dir_str),
        );
        let mut c = Command::new("wsl.exe");
        c.args(["--", "bash", "-ic", &codex_cmd]);
        c
    } else {
        let mut c = Command::new("codex");
        c.args([
            "-a", "never",
            "-s", "read-only", "-C", &work_dir_str,
            "exec", "--skip-git-repo-check",
        ]);
        c
    };
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.creation_flags(CREATE_NO_WINDOW);

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

    // Log stderr in the background so we can diagnose Codex CLI failures.
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = LinesStream::new(reader.lines());
            while let Some(Ok(line)) = lines.next().await {
                if !line.trim().is_empty() {
                    tracing::warn!("codex stderr: {line}");
                }
            }
        });
    }

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
    // Collect all messages into a single user turn. Claude stream-json expects
    // one user message; conversation history is concatenated as text context.
    let mut combined_text = String::new();
    for msg in messages {
        if !combined_text.is_empty() {
            combined_text.push('\n');
        }
        let _ = write!(combined_text, "[{}]: {}", msg.role, msg.content);
    }

    let mut content_parts = vec![serde_json::json!({
        "type": "text",
        "text": combined_text,
    })];

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

/// Parse a single line from Codex CLI stdout.
///
/// Codex `exec` outputs plain text (no `--json` flag), so non-JSON lines
/// are emitted as text chunks. If the line happens to be JSON (e.g. a
/// refusal object), we parse it specially.
fn parse_codex_line(line: &str) -> Option<String> {
    // Try JSON first (handles refusals and structured output).
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
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

        // Top-level text field
        if let Some(text) = v.get("text").and_then(serde_json::Value::as_str) {
            if !text.is_empty() {
                return Some(sse_text(text));
            }
        }

        // Unrecognized JSON structure -- skip
        tracing::debug!("Ignoring unrecognized codex JSON: {line}");
        return None;
    }

    // Plain text line -- emit as-is.
    Some(sse_text(line))
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
    let claude_mode = detect_cli("claude");
    let codex_mode = detect_cli("codex");

    tracing::info!(
        "CLI availability -- claude: {claude_mode:?}, codex: {codex_mode:?}"
    );

    let state = Arc::new(ProxyState {
        token: token.clone(),
        active_child: Mutex::new(None),
        claude_mode,
        codex_mode,
    });

    let router = Router::new()
        .route("/health", get(health))
        .route("/chat", post(chat))
        .route("/cancel", post(cancel))
        .layer(axum::extract::DefaultBodyLimit::max(10 * 1024 * 1024)) // 10 MB for screenshots
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

/// Start the proxy server on a background thread with its own tokio runtime.
pub fn spawn_proxy_thread() {
    std::thread::spawn(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create proxy runtime");
        rt.block_on(async {
            match start_proxy().await {
                Ok(addr) => {
                    tracing::info!("Proxy server started on {addr}");
                    std::future::pending::<()>().await;
                }
                Err(e) => tracing::error!("Failed to start proxy: {e}"),
            }
        });
    });
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
