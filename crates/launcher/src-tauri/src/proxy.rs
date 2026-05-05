use std::collections::HashMap;
use std::fmt::Write as _;
use std::net::SocketAddr;
#[cfg(windows)]
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
    codex_workdir: String,
}

#[derive(Deserialize)]
struct ChatRequest {
    request_id: u64,
    messages: Vec<ChatMessage>,
    screenshot: Option<String>,
    system_prompt: String,
    provider: String,
    model: String,
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
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Name of the Codex working directory (used as both the WSL `/tmp/<name>` path
/// and the Windows `temp_dir().join(<name>)` path).
const CODEX_WORKDIR: &str = "aigc-codex-workdir";

/// Configure a `std::process::Command` to run silently (no console popup on
/// Windows, stdout/stderr discarded everywhere). Used for fire-and-forget
/// subprocesses where we only care about the exit status.
fn silent(cmd: &mut std::process::Command) -> &mut std::process::Command {
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Apply the Windows no-window flag to a tokio `Command`. No-op on non-Windows
/// so the launcher crate compiles for the Linux test runner.
#[allow(unused_variables, clippy::needless_pass_by_ref_mut)]
fn no_window(cmd: &mut tokio::process::Command) {
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
}

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
    let native = silent(std::process::Command::new(name).arg("--version"))
        .status()
        .is_ok_and(|s| s.success());
    if native {
        return CliMode::Native;
    }

    // Try via WSL with interactive shell so .bashrc (nvm, etc.) is sourced.
    let version_cmd = format!("{name} --version");
    let wsl = silent(
        std::process::Command::new("wsl.exe").args(["--", "bash", "-ic", &version_cmd]),
    )
    .status()
    .is_ok_and(|s| s.success());
    if wsl {
        return CliMode::Wsl;
    }

    CliMode::Unavailable
}

/// Validate model name: ASCII alphanumeric + hyphens, dots, underscores.
/// Mirrors the validation in `api.rs` for Gemini models.
fn validate_model_name(model: &str) -> Result<(), StatusCode> {
    if model.is_empty() || model.len() > 128 {
        return Err(StatusCode::BAD_REQUEST);
    }
    if !model
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
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
        "openai" => {
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
    validate_model_name(&req.model)?;
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
    no_window(&mut cmd);

    let mut child = cmd.spawn().map_err(|e| {
        tracing::error!("Failed to spawn claude CLI: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Take pipes before registering -- if any take() fails, kill the child.
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    if stdin.is_none() || stdout.is_none() {
        tracing::error!("Failed to take stdin/stdout from claude process");
        let _ = child.kill().await;
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    let mut stdin = stdin.unwrap();
    let stdout = stdout.unwrap();

    // Register active child for cancellation BEFORE any async work.
    {
        let mut guard = state.active_child.lock().await;
        *guard = Some((req.request_id, child));
    }

    // Build NDJSON input with image embedded in the content array.
    let input = build_claude_input(&req.messages, req.screenshot.as_deref());

    // Write input and close stdin
    tokio::spawn(async move {
        let _ = stdin.write_all(input.as_bytes()).await;
        let _ = stdin.flush().await;
        drop(stdin);
    });

    // Log stderr in the background so we can diagnose Claude CLI failures.
    if let Some(stderr) = stderr {
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

/// Codex requires a git directory -- ensure a temp workdir with `git init` exists.
fn ensure_codex_workdir(mode: CliMode) -> String {
    if let CliMode::Wsl = mode {
        let dir = format!("/tmp/{CODEX_WORKDIR}");
        let _ = silent(std::process::Command::new("wsl.exe").args([
            "--",
            "bash",
            "-c",
            &format!("[ -d {dir}/.git ] || (mkdir -p {dir} && git -C {dir} init)"),
        ]))
        .status();
        return dir;
    }

    let dir = std::env::temp_dir().join(CODEX_WORKDIR);
    if !dir.exists() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = silent(std::process::Command::new("git").args(["init"]).current_dir(&dir))
            .status();
    }
    dir.to_string_lossy().into_owned()
}

async fn handle_codex(
    state: Arc<ProxyState>,
    req: ChatRequest,
) -> Result<Response, StatusCode> {
    validate_model_name(&req.model)?;
    let work_dir_str = state.codex_workdir.as_str();

    // Let Codex use its own default model (gpt-5.3-codex) unless the user
    // explicitly configured a codex-compatible model. The overlay's
    // openai.model (gpt-4o) is for direct API use, not Codex CLI.
    let mut cmd = if let CliMode::Wsl = state.codex_mode {
        let codex_cmd = format!(
            "codex -a never -s read-only -C {} exec --skip-git-repo-check",
            shell_escape(work_dir_str),
        );
        let mut c = Command::new("wsl.exe");
        c.args(["--", "bash", "-ic", &codex_cmd]);
        c
    } else {
        let mut c = Command::new("codex");
        c.args([
            "-a", "never",
            "-s", "read-only", "-C", work_dir_str,
            "exec", "--skip-git-repo-check",
        ]);
        c
    };
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    no_window(&mut cmd);

    let mut child = cmd.spawn().map_err(|e| {
        tracing::error!("Failed to spawn codex CLI: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Take pipes before registering -- if any take() fails, kill the child.
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    if stdin.is_none() || stdout.is_none() {
        tracing::error!("Failed to take stdin/stdout from codex process");
        let _ = child.kill().await;
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    let mut stdin = stdin.unwrap();
    let stdout = stdout.unwrap();

    // Register active child for cancellation BEFORE any async work.
    {
        let mut guard = state.active_child.lock().await;
        *guard = Some((req.request_id, child));
    }

    // Build plain text input for Codex
    let input = build_codex_input(&req.system_prompt, &req.messages);

    tokio::spawn(async move {
        let _ = stdin.write_all(input.as_bytes()).await;
        let _ = stdin.flush().await;
        drop(stdin);
    });

    // Log stderr in the background so we can diagnose Codex CLI failures.
    if let Some(stderr) = stderr {
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

    let mut out = serde_json::to_string(&input_msg)
        .unwrap_or_else(|e| {
            tracing::error!("Failed to serialize Claude input: {e}");
            String::new()
        });
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
        .unwrap_or_else(|_| Response::new(Body::empty()))
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
        .unwrap_or_else(|_| Response::new(Body::empty()))
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

    let codex_workdir = ensure_codex_workdir(codex_mode);

    let state = Arc::new(ProxyState {
        token: token.clone(),
        active_child: Mutex::new(None),
        claude_mode,
        codex_mode,
        codex_workdir,
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
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                tracing::error!("Failed to create proxy runtime: {e}");
                return;
            }
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_owned(),
            content: content.to_owned(),
        }
    }

    // ---------------- shell_escape ----------------

    #[test]
    fn shell_escape_wraps_in_single_quotes() {
        assert_eq!(shell_escape("plain"), "'plain'");
    }

    #[test]
    fn shell_escape_preserves_spaces_and_special_chars() {
        assert_eq!(shell_escape("a b $c & d"), "'a b $c & d'");
    }

    #[test]
    fn shell_escape_escapes_inner_single_quote() {
        // 'it'\''s' -- standard close/escape/reopen pattern
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn shell_escape_handles_empty_string() {
        assert_eq!(shell_escape(""), "''");
    }

    // ---------------- validate_model_name ----------------

    #[test]
    fn validate_model_name_accepts_typical_ids() {
        for ok in [
            "gemini-2.5-flash",
            "claude-haiku-4-5",
            "gpt-4o",
            "model_v2",
            "Some.Model.With.Dots",
            "a",
        ] {
            assert!(validate_model_name(ok).is_ok(), "should accept: {ok}");
        }
    }

    #[test]
    fn validate_model_name_rejects_empty_and_oversize() {
        assert!(validate_model_name("").is_err());
        let oversize = "a".repeat(129);
        assert!(validate_model_name(&oversize).is_err());
    }

    #[test]
    fn validate_model_name_rejects_path_traversal() {
        for bad in ["../foo", "foo/bar", "foo\\bar", "foo bar", "foo:bar", "foo$"] {
            assert!(validate_model_name(bad).is_err(), "should reject: {bad}");
        }
    }

    #[test]
    fn validate_model_name_rejects_non_ascii() {
        assert!(validate_model_name("modèle").is_err());
        assert!(validate_model_name("モデル").is_err());
    }

    // ---------------- build_codex_input ----------------

    #[test]
    fn codex_input_omits_system_prompt_when_empty() {
        let out = build_codex_input("", &[msg("user", "hello")]);
        assert_eq!(out, "[user]: hello\n");
    }

    #[test]
    fn codex_input_includes_system_prompt_with_blank_line() {
        let out = build_codex_input("Be terse.", &[msg("user", "hi")]);
        assert_eq!(out, "Be terse.\n\n[user]: hi\n");
    }

    #[test]
    fn codex_input_concatenates_messages_in_order() {
        let out = build_codex_input(
            "",
            &[msg("user", "q1"), msg("assistant", "a1"), msg("user", "q2")],
        );
        assert_eq!(out, "[user]: q1\n[assistant]: a1\n[user]: q2\n");
    }

    // ---------------- build_claude_input ----------------

    #[test]
    fn claude_input_emits_one_ndjson_line_terminated_by_newline() {
        let out = build_claude_input(&[msg("user", "hello")], None);
        assert!(out.ends_with('\n'));
        // exactly one newline at the very end
        assert_eq!(out.matches('\n').count(), 1);
    }

    #[test]
    fn claude_input_concatenates_history_as_single_user_turn() {
        let out = build_claude_input(
            &[msg("user", "q1"), msg("assistant", "a1"), msg("user", "q2")],
            None,
        );
        let v: serde_json::Value = serde_json::from_str(out.trim_end()).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        let parts = v["message"]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(
            parts[0]["text"].as_str().unwrap(),
            "[user]: q1\n[assistant]: a1\n[user]: q2"
        );
    }

    #[test]
    fn claude_input_appends_image_part_when_screenshot_present() {
        let out = build_claude_input(&[msg("user", "look")], Some("AAAAFAKE=="));
        let v: serde_json::Value = serde_json::from_str(out.trim_end()).unwrap();
        let parts = v["message"]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1]["type"], "image");
        assert_eq!(parts[1]["source"]["type"], "base64");
        assert_eq!(parts[1]["source"]["media_type"], "image/png");
        assert_eq!(parts[1]["source"]["data"], "AAAAFAKE==");
    }

    #[test]
    fn claude_input_omits_image_when_no_screenshot() {
        let out = build_claude_input(&[msg("user", "hi")], None);
        let v: serde_json::Value = serde_json::from_str(out.trim_end()).unwrap();
        let parts = v["message"]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
    }
}
