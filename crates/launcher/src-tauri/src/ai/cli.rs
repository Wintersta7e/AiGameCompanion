//! In-process Claude / Codex CLI streaming. Spawns the provider CLI directly
//! (no localhost HTTP proxy), feeds it the chat history over stdin, and forwards
//! each decoded text chunk to a caller-supplied callback. The child is spawned
//! with `kill_on_drop` so aborting the owning task terminates the CLI process.

use std::fmt::Write as _;
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;

use futures_util::StreamExt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio_stream::wrappers::LinesStream;

use super::ChatMessage;

/// Default Claude model when the user has not configured one. Codex ignores the
/// model (that CLI rejects an explicit `-m`), so no default is needed there.
pub const DEFAULT_CLAUDE_MODEL: &str = "claude-haiku-4-5";

/// Name of the Codex working directory (used as both the WSL `/tmp/<name>` path
/// and the Windows `temp_dir().join(<name>)` path).
const CODEX_WORKDIR: &str = "aigc-codex-workdir";

/// Windows `CREATE_NO_WINDOW` flag -- prevents console popups from `wsl.exe` and
/// other console-subsystem processes.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// How to invoke a CLI tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliMode {
    /// Not available on this system.
    Unavailable,
    /// Available directly on the Windows PATH.
    Native,
    /// Available inside WSL (invoke via `wsl.exe`).
    Wsl,
}

impl CliMode {
    pub fn is_available(self) -> bool {
        !matches!(self, Self::Unavailable)
    }

    /// Human label for where the CLI was detected.
    pub fn location(self) -> &'static str {
        match self {
            Self::Native => "PATH",
            Self::Wsl => "WSL",
            Self::Unavailable => "",
        }
    }
}

/// Cached CLI availability, detected once at startup on a background thread.
#[derive(Debug, Clone)]
pub struct CliConfig {
    pub claude: CliMode,
    pub codex: CliMode,
    pub codex_workdir: String,
}

impl Default for CliConfig {
    fn default() -> Self {
        Self {
            claude: CliMode::Unavailable,
            codex: CliMode::Unavailable,
            codex_workdir: String::new(),
        }
    }
}

/// Which content the parser decoded from one CLI stdout line.
#[derive(Debug, PartialEq, Eq)]
enum Parsed {
    Text(String),
    Error(String),
}

/// Configure a `std::process::Command` to run silently (no console popup on
/// Windows, stdout/stderr discarded everywhere). Used for fire-and-forget
/// probes where we only care about the exit status.
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
fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
}

/// Escape a string for use inside a `bash -c` / `bash -ic` command.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Check if a CLI tool is available, first natively on the Windows PATH, then
/// inside WSL (using `bash -ic` so nvm / profile PATH is sourced).
pub fn detect_cli(name: &str) -> CliMode {
    let native = silent(std::process::Command::new(name).arg("--version"))
        .status()
        .is_ok_and(|status| status.success());
    if native {
        return CliMode::Native;
    }

    let version_cmd = format!("{name} --version");
    let wsl =
        silent(std::process::Command::new("wsl.exe").args(["--", "bash", "-ic", &version_cmd]))
            .status()
            .is_ok_and(|status| status.success());
    if wsl {
        return CliMode::Wsl;
    }

    CliMode::Unavailable
}

/// Codex requires a git directory -- ensure a temp workdir with `git init` exists.
pub fn ensure_codex_workdir(mode: CliMode) -> String {
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
        let _ = silent(
            std::process::Command::new("git")
                .args(["init"])
                .current_dir(&dir),
        )
        .status();
    }
    dir.to_string_lossy().into_owned()
}

/// Validate a model name: ASCII alphanumeric + hyphens, dots, underscores.
fn validate_model_name(model: &str) -> Result<(), String> {
    if model.is_empty() || model.len() > 128 {
        return Err("Invalid model name.".to_owned());
    }
    if !model
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
    {
        return Err("Invalid model name.".to_owned());
    }
    Ok(())
}

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

    let mut out = serde_json::to_string(&input_msg).unwrap_or_else(|e| {
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

/// Parse a single NDJSON line from Claude CLI stdout.
fn parse_claude_line(line: &str) -> Option<Parsed> {
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
                Some(Parsed::Text(text.to_owned()))
            } else {
                None
            }
        }
        "result" => {
            let is_error = v
                .get("is_error")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if is_error {
                let error_msg = v
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Unknown Claude CLI error");
                Some(Parsed::Error(error_msg.to_owned()))
            } else {
                // Successful result -- stream is complete.
                None
            }
        }
        // system, assistant, etc -- ignore.
        _ => None,
    }
}

/// Parse a single line from Codex CLI stdout. `codex exec` prints plain text, so
/// non-JSON lines are emitted verbatim; JSON lines (refusals, structured output)
/// are decoded.
fn parse_codex_line(line: &str) -> Option<Parsed> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
        if let Some(refusal_type) = v.get("type").and_then(serde_json::Value::as_str) {
            if refusal_type == "refusal" {
                let msg = v
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Model refused the request");
                return Some(Parsed::Error(msg.to_owned()));
            }
        }

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
                return Some(Parsed::Text(collected));
            }
        }

        if let Some(text) = v.get("text").and_then(serde_json::Value::as_str) {
            if !text.is_empty() {
                return Some(Parsed::Text(text.to_owned()));
            }
        }

        tracing::debug!("Ignoring unrecognized codex JSON: {line}");
        return None;
    }

    Some(Parsed::Text(line.to_owned()))
}

/// Stream a Claude response by spawning the Claude CLI in stream-json mode.
pub async fn stream_claude<F>(
    cfg: &CliConfig,
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    screenshot: Option<&str>,
    on_chunk: F,
) -> Result<(), String>
where
    F: FnMut(String) -> Result<(), String>,
{
    if !cfg.claude.is_available() {
        return Err("Claude CLI is not available on this system.".to_owned());
    }
    validate_model_name(model)?;

    let mut cmd = if let CliMode::Wsl = cfg.claude {
        let claude_args = format!(
            "claude -p --input-format stream-json --output-format stream-json \
             --verbose --include-partial-messages --tools '' \
             --no-session-persistence --model {} --system-prompt {}",
            shell_escape(model),
            shell_escape(system_prompt),
        );
        let mut c = Command::new("wsl.exe");
        c.args(["--", "bash", "-ic", &claude_args]);
        c
    } else {
        let mut c = Command::new("claude");
        c.args([
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
            model,
            "--system-prompt",
            system_prompt,
        ]);
        c
    };

    let input = build_claude_input(messages, screenshot);
    run_cli(&mut cmd, input, on_chunk, parse_claude_line, "Claude").await
}

/// Stream a Codex response by spawning the Codex CLI in `exec` mode.
pub async fn stream_codex<F>(
    cfg: &CliConfig,
    system_prompt: &str,
    messages: &[ChatMessage],
    on_chunk: F,
) -> Result<(), String>
where
    F: FnMut(String) -> Result<(), String>,
{
    if !cfg.codex.is_available() {
        return Err("Codex CLI is not available on this system.".to_owned());
    }

    let work_dir = cfg.codex_workdir.as_str();
    let mut cmd = if let CliMode::Wsl = cfg.codex {
        let codex_cmd = format!(
            "codex -a never -s read-only -C {} exec --skip-git-repo-check",
            shell_escape(work_dir),
        );
        let mut c = Command::new("wsl.exe");
        c.args(["--", "bash", "-ic", &codex_cmd]);
        c
    } else {
        let mut c = Command::new("codex");
        c.args([
            "-a",
            "never",
            "-s",
            "read-only",
            "-C",
            work_dir,
            "exec",
            "--skip-git-repo-check",
        ]);
        c
    };

    let input = build_codex_input(system_prompt, messages);
    run_cli(&mut cmd, input, on_chunk, parse_codex_line, "Codex").await
}

/// Spawn a CLI child, write `input` to stdin, and stream parsed stdout lines to
/// `on_chunk`. stdin/stdout/stderr are driven concurrently in this one future so
/// that aborting the owning task drops the child; `kill_on_drop` then terminates
/// it. Note: in WSL mode the direct child is `wsl.exe`, so this ends the relay
/// but may orphan the in-distro CLI process (a known limitation, same as before).
async fn run_cli<F, P>(
    cmd: &mut Command,
    input: String,
    mut on_chunk: F,
    parse_line: P,
    label: &str,
) -> Result<(), String>
where
    F: FnMut(String) -> Result<(), String>,
    P: Fn(&str) -> Option<Parsed>,
{
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);
    no_window(cmd);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn {label} CLI: {e}"))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| format!("Failed to open {label} stdin."))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("Failed to open {label} stdout."))?;
    let stderr = child.stderr.take();

    let write_fut = async move {
        let _ = stdin.write_all(input.as_bytes()).await;
        let _ = stdin.flush().await;
        // Dropping stdin closes the pipe so the CLI knows the input is complete.
    };

    let stderr_fut = async move {
        if let Some(stderr) = stderr {
            let reader = BufReader::new(stderr);
            let mut lines = LinesStream::new(reader.lines());
            while let Some(Ok(line)) = lines.next().await {
                if !line.trim().is_empty() {
                    tracing::warn!("{label} stderr: {line}");
                }
            }
        }
    };

    let read_fut = async {
        let reader = BufReader::new(stdout);
        let mut lines = LinesStream::new(reader.lines());
        while let Some(item) = lines.next().await {
            let line = item.map_err(|e| format!("Failed to read from {label} CLI: {e}"))?;
            if line.trim().is_empty() {
                continue;
            }
            match parse_line(&line) {
                Some(Parsed::Text(text)) => on_chunk(text)?,
                Some(Parsed::Error(message)) => return Err(message),
                None => {}
            }
        }
        Ok(())
    };

    let ((), (), read_result) = tokio::join!(write_fut, stderr_fut, read_fut);
    // Drop the child last so `kill_on_drop` reaps it if it is still running.
    drop(child);
    read_result
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
        for bad in [
            "../foo", "foo/bar", "foo\\bar", "foo bar", "foo:bar", "foo$",
        ] {
            assert!(validate_model_name(bad).is_err(), "should reject: {bad}");
        }
    }

    #[test]
    fn validate_model_name_rejects_non_ascii() {
        assert!(validate_model_name("mod\u{e8}le").is_err());
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

    // ---------------- parse_claude_line ----------------

    #[test]
    fn parse_claude_line_extracts_text_delta() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"PONG"}}}"#;
        assert_eq!(parse_claude_line(line), Some(Parsed::Text("PONG".to_owned())));
    }

    #[test]
    fn parse_claude_line_ignores_non_text_deltas() {
        let start = r#"{"type":"stream_event","event":{"type":"message_start","message":{"role":"assistant"}}}"#;
        let block = r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#;
        assert_eq!(parse_claude_line(start), None);
        assert_eq!(parse_claude_line(block), None);
    }

    #[test]
    fn parse_claude_line_ignores_system_and_assistant_frames() {
        let system = r#"{"type":"system","subtype":"init","session_id":"x"}"#;
        let assistant = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"PONG"}]}}"#;
        assert_eq!(parse_claude_line(system), None);
        assert_eq!(parse_claude_line(assistant), None);
    }

    #[test]
    fn parse_claude_line_treats_successful_result_as_stream_end() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"PONG"}"#;
        assert_eq!(parse_claude_line(line), None);
    }

    #[test]
    fn parse_claude_line_surfaces_error_result_message() {
        let line = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"error":"quota exceeded"}"#;
        assert_eq!(
            parse_claude_line(line),
            Some(Parsed::Error("quota exceeded".to_owned()))
        );
    }

    #[test]
    fn parse_claude_line_uses_fallback_when_error_result_has_no_message() {
        let line = r#"{"type":"result","is_error":true}"#;
        assert_eq!(
            parse_claude_line(line),
            Some(Parsed::Error("Unknown Claude CLI error".to_owned()))
        );
    }

    #[test]
    fn parse_claude_line_skips_malformed_or_empty_lines() {
        assert_eq!(parse_claude_line("not json"), None);
        assert_eq!(parse_claude_line(""), None);
    }

    // ---------------- parse_codex_line ----------------

    #[test]
    fn parse_codex_line_emits_plain_text_verbatim() {
        assert_eq!(parse_codex_line("PONG"), Some(Parsed::Text("PONG".to_owned())));
    }

    #[test]
    fn parse_codex_line_surfaces_refusal() {
        let line = r#"{"type":"refusal","content":"I can't help with that"}"#;
        assert_eq!(
            parse_codex_line(line),
            Some(Parsed::Error("I can't help with that".to_owned()))
        );
    }

    #[test]
    fn parse_codex_line_collects_output_text_from_content_array() {
        let line = r#"{"content":[{"type":"output_text","text":"hello"},{"type":"output_text","text":" world"}]}"#;
        assert_eq!(
            parse_codex_line(line),
            Some(Parsed::Text("hello world".to_owned()))
        );
    }

    #[test]
    fn parse_codex_line_extracts_top_level_text_field() {
        let line = r#"{"text":"hi there"}"#;
        assert_eq!(parse_codex_line(line), Some(Parsed::Text("hi there".to_owned())));
    }

    #[test]
    fn parse_codex_line_skips_unrecognized_json_object() {
        let line = r#"{"type":"token_count","tokens":42}"#;
        assert_eq!(parse_codex_line(line), None);
    }
}
