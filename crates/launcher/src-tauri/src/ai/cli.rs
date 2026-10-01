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
pub(crate) const DEFAULT_CLAUDE_MODEL: &str = "claude-haiku-4-5";

/// Name of the Codex working directory (used as both the WSL `/tmp/<name>` path
/// and the Windows `temp_dir().join(<name>)` path).
const CODEX_WORKDIR: &str = "aigc-codex-workdir";
/// Cap on total stdout bytes from a CLI child, matching the Gemini stream cap.
const MAX_STREAM_BYTES: usize = 2 * 1024 * 1024;
/// How many stderr lines to keep for the failure message.
const STDERR_TAIL_LINES: usize = 5;
/// Folder, under the app's local data directory, that holds a Codex
/// screenshot while its request runs.
pub(crate) const SHOTS_DIR: &str = "shots";
/// Marker printed by the WSL shell immediately before the CLI runs.
///
/// `bash -lic` sources the user's interactive `.bashrc`, which is where many
/// installs put the CLI on PATH -- but it is also where nvm/fastfetch/"you have
/// mail" banners print, and Codex output is plain text, so those lines were
/// parsed as the start of the model's answer. Everything up to and including
/// this marker is discarded, which keeps the PATH without the noise.
const WSL_SENTINEL: &str = "__AIGC_STREAM_BEGIN__";

/// Windows `CREATE_NO_WINDOW` flag -- prevents console popups from `wsl.exe` and
/// other console-subsystem processes.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// How to invoke a CLI tool.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CliMode {
    /// Not available on this system.
    #[default]
    Unavailable,
    /// Available directly on the Windows PATH.
    Native,
    /// Available inside WSL (invoke via `wsl.exe`).
    Wsl,
}

impl CliMode {
    pub(crate) const fn is_available(self) -> bool {
        !matches!(self, Self::Unavailable)
    }

    /// Human label for where the CLI was detected.
    pub(crate) const fn location(self) -> &'static str {
        match self {
            Self::Native => "PATH",
            Self::Wsl => "WSL",
            Self::Unavailable => "",
        }
    }
}

/// Cached CLI availability, detected once at startup on a background thread.
#[derive(Debug, Clone, Default)]
pub(crate) struct CliConfig {
    pub claude: CliMode,
    pub codex: CliMode,
    pub codex_workdir: String,
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

/// The WSL user's home directory, resolved once.
///
/// Scripts interpolate this path instead of writing `$HOME`. Under the old
/// `wsl.exe --` form the command line went through the user's default shell
/// first, which expanded every `$VAR`, `$(...)` and backtick before `bash -lic`
/// ran -- a `$HOME` in the script was that shell's value, not bash's, and the
/// Codex workdir resolved to `""`. `wsl_exec` removes that shell; the passwd
/// database still needs no environment to answer.
fn wsl_home() -> Option<&'static str> {
    static HOME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let out = windowless(&mut wsl_exec(&[
            "sh",
            "-c",
            r#"getent passwd "$(id -u)" | cut -d: -f6"#,
        ]))
        .output()
        .ok()?;
        let home = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if !out.status.success() || !home.starts_with('/') {
            tracing::warn!(
                "Could not resolve the WSL home (exit {}): stdout [{home}], stderr [{}]",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
            return None;
        }
        tracing::info!("WSL home resolved: {home}");
        Some(home)
    })
    .as_deref()
}

/// Hide the console window without touching the pipes, for probes whose output
/// we actually read. `silent` discards stdout, so a command whose result is read
/// back through `output()` must use this instead -- that mix-up is what made the
/// Codex workdir probe return an empty path on every run.
#[cfg_attr(
    not(windows),
    expect(
        clippy::missing_const_for_fn,
        reason = "the Windows build calls creation_flags, which is not const"
    )
)]
fn windowless(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Apply the Windows no-window flag to a tokio `Command`. No-op on non-Windows
/// so the launcher crate compiles for the Linux test runner.
#[cfg_attr(
    not(windows),
    expect(
        unused_variables,
        clippy::needless_pass_by_ref_mut,
        clippy::missing_const_for_fn,
        reason = "off Windows the command is left untouched, and the Windows build calls creation_flags, which is not const"
    )
)]
fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
}

/// Escape a string for use inside a `bash -c` / `bash -lic` command.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Build every `wsl.exe` child here, as `wsl.exe --exec <program> <args...>`.
///
/// `--exec` hands the arguments to the Linux program as argv. The `--` form
/// passes the command line to the user's default shell instead, which re-parsed
/// it before `bash` ran: `$(...)`, backticks and `$VAR` expanded, `\\` collapsed
/// and a line after a newline ran, so `shell_escape` was not the only quoting
/// layer and text such as a window title could run commands. Measured with ten
/// hostile inputs: `--` altered six, `--exec` none.
fn wsl_exec(args: &[&str]) -> std::process::Command {
    let mut cmd = std::process::Command::new("wsl.exe");
    cmd.arg("--exec").args(args);
    cmd
}

/// `wsl.exe --exec bash -lic <script>`: the script is parsed by that bash only.
fn wsl_bash(script: &str) -> std::process::Command {
    wsl_exec(&["bash", "-lic", script])
}

/// The version probe run inside WSL, with the CLI name quoted like any argument.
fn version_script(name: &str) -> String {
    format!("{} --version", shell_escape(name))
}

/// Check if a CLI tool is available, first natively on the Windows PATH, then
/// inside WSL, using `bash -lic`.
///
/// Both flags are required, and each was measured from the running launcher:
/// `-i` sources `.bashrc`, where nvm puts Codex (plain `-lc` made Codex
/// undetectable), and `-l` sources the profile, where `~/.local/bin` is added --
/// without it Claude reported "Not found" on a machine that has it installed.
/// The script reaches that bash through `wsl_bash`, so no other shell parses it
/// first. See `WSL_SENTINEL` for how the interactive shell's banner output is
/// kept out of the model stream.
pub(crate) fn detect_cli(name: &str) -> CliMode {
    let native = silent(std::process::Command::new(name).arg("--version"))
        .status()
        .is_ok_and(|status| status.success());
    if native {
        return CliMode::Native;
    }

    let wsl = silent(&mut wsl_bash(&version_script(name)))
        .status()
        .is_ok_and(|status| status.success());
    if wsl {
        return CliMode::Wsl;
    }

    CliMode::Unavailable
}

/// Detect both CLIs and resolve the Codex working directory in one pass.
///
/// Codex without a usable workdir is reported `Unavailable`: the binary is
/// there but cannot answer, and offering it anyway surfaces as a bare "No such
/// file or directory" from the CLI long after the user picked the provider.
pub(crate) fn detect_all() -> CliConfig {
    let claude = detect_cli("claude");
    let detected_codex = detect_cli("codex");
    let (codex, codex_workdir) = ensure_codex_workdir(detected_codex).map_or_else(
        || (CliMode::Unavailable, String::new()),
        |dir| (detected_codex, dir),
    );
    CliConfig {
        claude,
        codex,
        codex_workdir,
    }
}

/// Codex requires a git directory -- ensure a workdir with `git init` exists.
///
/// `None` means no usable directory: the caller must then treat Codex as
/// unavailable rather than spawn it with a path that does not exist, which the
/// CLI reports only as a bare "No such file or directory".
pub(crate) fn ensure_codex_workdir(mode: CliMode) -> Option<String> {
    if mode == CliMode::Wsl {
        // Under the user's home, not shared /tmp. Codex reads instruction files
        // (AGENTS.md) from its working directory, and a fixed
        // `/tmp/aigc-codex-workdir` can be pre-created by any other local user
        // -- the `[ -d dir/.git ]` guard then no-ops and every Codex answer is
        // steered by their file. `-s read-only` blocks writes, not reads.
        //
        // The path is interpolated from `wsl_home`, never written as `$HOME`.
        // Under the old `wsl.exe --` form the user's default shell expanded
        // `$HOME` before bash ran -- which is why an `export HOME=...` in the
        // same script did not help -- so this resolved to "" and Codex died on
        // a missing directory. `wsl_bash` uses `--exec`, which removes that shell.
        let home = wsl_home()?;
        let dir = format!("{home}/.cache/{CODEX_WORKDIR}");
        let quoted = shell_escape(&dir);
        let script = format!(
            "mkdir -p {quoted} && chmod 700 {quoted} && \
             {{ [ -d {quoted}/.git ] || git -C {quoted} init >/dev/null; }}"
        );
        // `windowless`, not `silent`: `silent` discards stderr, and a failure
        // here must say why -- otherwise the only symptom is Codex exiting with
        // "No such file or directory" long afterwards.
        let output = windowless(&mut wsl_bash(&script)).output();
        return match output {
            Ok(out) if out.status.success() => Some(dir),
            Ok(out) => {
                tracing::warn!(
                    "Preparing the Codex workdir {dir} failed ({}): {}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                );
                None
            }
            Err(err) => {
                tracing::warn!("Could not run the Codex workdir setup: {err}");
                None
            }
        };
    }

    let dir = std::env::temp_dir().join(CODEX_WORKDIR);
    if !dir.exists() {
        crate::util::log_if_err("create the Codex workdir", std::fs::create_dir_all(&dir));
        crate::util::log_if_err(
            "git init the Codex workdir",
            silent(
                std::process::Command::new("git")
                    .args(["init"])
                    .current_dir(&dir),
            )
            .status(),
        );
    }
    if dir.is_dir() {
        Some(dir.to_string_lossy().into_owned())
    } else {
        tracing::warn!("Codex workdir {} could not be created", dir.display());
        None
    }
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
        // A successful result is the stream's end.
        "result" => claude_result_error(&v).map(Parsed::Error),
        // system, assistant, etc -- ignore.
        _ => None,
    }
}

/// The message of a Claude CLI `result` frame that reports an error, or `None`
/// when the frame reports success.
///
/// A frame is an error when `is_error` is true or its `subtype` starts with
/// `error`. The CLI writes its reason to `result` or `errors`; the HTTP status
/// or the subtype stands in only when neither carries text.
fn claude_result_error(frame: &serde_json::Value) -> Option<String> {
    use serde_json::Value;
    let subtype = frame.get("subtype").and_then(Value::as_str);
    let is_error = frame.get("is_error").and_then(Value::as_bool) == Some(true)
        || subtype.is_some_and(|subtype| subtype.starts_with("error"));
    if !is_error {
        return None;
    }
    let result = frame
        .get("result")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    let errors = || {
        let list = frame.get("errors").and_then(Value::as_array)?;
        let texts: Vec<&str> = list
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .collect();
        (!texts.is_empty()).then(|| texts.join("; "))
    };
    let status = || {
        let status = frame.get("api_error_status").and_then(Value::as_u64)?;
        Some(format!("Claude CLI error (HTTP {status})"))
    };
    let named = || {
        let subtype = subtype.filter(|subtype| !subtype.is_empty() && *subtype != "success")?;
        Some(format!("Claude CLI error ({subtype})"))
    };
    let message = result
        .or_else(errors)
        .or_else(status)
        .or_else(named)
        .unwrap_or_else(|| "Claude CLI returned an error without a message".to_owned());
    Some(
        message
            .chars()
            .take(super::gemini::MAX_ERROR_MESSAGE_CHARS)
            .collect(),
    )
}

/// Parse a single line from Codex CLI stdout. `codex exec` prints plain text, so
/// non-JSON lines (blank ones included) are emitted verbatim plus their newline;
/// JSON lines (refusals, structured output) are decoded.
fn parse_codex_line(line: &str) -> Option<Parsed> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
        if v.get("type").and_then(serde_json::Value::as_str) == Some("refusal") {
            let msg = v
                .get("content")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Model refused the request");
            return Some(Parsed::Error(msg.to_owned()));
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

        // An object carrying a protocol "type" we don't recognize is a control
        // frame -- drop it. Anything else (a bare JSON value, or an object with
        // no "type") is the model's answer that happens to be JSON: fall through
        // and emit it verbatim rather than silently dropping it.
        if v.get("type").and_then(serde_json::Value::as_str).is_some() {
            tracing::debug!("Ignoring unrecognized codex control frame: {line}");
            return None;
        }
    }

    // Put back the newline `lines()` removed, so the answer keeps its line
    // breaks and blank lines (paragraphs, lists).
    Some(Parsed::Text(format!("{line}\n")))
}

/// A new screenshot file name in `dir`. It has to be unique, not secret: the
/// directory belongs to this user and this app.
fn shot_path(dir: &std::path::Path) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    dir.join(format!("aigc-shot-{}-{nanos}.png", std::process::id()))
}

/// A screenshot written to disk for one Codex request. Dropping it deletes the
/// file, so it lives exactly as long as the request's future.
#[derive(Debug)]
struct TempShot {
    path: std::path::PathBuf,
}

impl TempShot {
    /// Write `png` to a new file at exactly `path`. Fails if anything already
    /// exists there, so an existing file is never written through. The file is
    /// closed before this returns.
    fn create(path: std::path::PathBuf, png: &[u8]) -> Result<Self, String> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("could not create the screenshot file: {e}"))?;
        let written = std::io::Write::write_all(&mut file, png);
        drop(file);
        // From here the guard owns the file, so a failed write removes it.
        let shot = Self { path };
        written.map_err(|e| format!("could not write the screenshot file: {e}"))?;
        Ok(shot)
    }
}

impl Drop for TempShot {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.path) {
            tracing::warn!("Could not delete a Codex screenshot file: {e}");
        }
    }
}

/// Delete every regular file in the shots directory: leftovers of a crash, a
/// failed delete or a killed process. Returns how many were removed; a missing
/// directory removes none.
pub(crate) fn sweep_shots(dir: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => removed += 1,
            Err(e) => tracing::warn!("Could not delete a leftover screenshot: {e}"),
        }
    }
    removed
}

/// Codex's arguments when it runs from the Windows PATH. The prompt goes on
/// stdin, never as an argument, and the `=` form keeps the image path bound to
/// its flag. `--ephemeral` keeps the prompt, the history and the screenshot out
/// of Codex's own session files. `--disable shell_tool` takes away the tool
/// that runs commands, and `--ignore-user-config` skips the user's Codex
/// config file (MCP servers, model, effort), so Codex answers with its own
/// default model.
fn codex_args(work_dir: &str, image: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = [
        "-a",
        "never",
        "-s",
        "read-only",
        "--disable",
        "shell_tool",
        "-C",
        work_dir,
        "exec",
        "--skip-git-repo-check",
        "--ephemeral",
        "--ignore-user-config",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    if let Some(image) = image {
        args.push(format!("--image={image}"));
    }
    args
}

/// The script `wsl_bash` runs for Codex. With an image, `wslpath` turns its
/// Windows path into a Linux one inside the same bash; if that fails, Codex
/// does not start and `wslpath`'s own message is the error.
fn codex_wsl_script(work_dir: &str, image: Option<&str>) -> String {
    let codex = format!(
        "codex -a never -s read-only --disable shell_tool -C {} exec \
         --skip-git-repo-check --ephemeral --ignore-user-config",
        shell_escape(work_dir),
    );
    let start = format!("printf '%s\\n' {WSL_SENTINEL}; ");
    image.map_or_else(
        || format!("{start}{codex}"),
        |image| {
            format!(
                "{start}img=$(wslpath -u {}) && {codex} --image=\"$img\"",
                shell_escape(image),
            )
        },
    )
}

/// Claude's arguments, the same in both modes. The prompt goes on stdin.
///
/// Sage only wants text back, so the child runs without the user's setup:
/// `--tools ''` offers no tool, `--safe-mode` skips their CLAUDE.md, plugins,
/// hooks, skills and MCP servers, `--strict-mcp-config` drops every MCP server
/// not named here (none is), `--permission-mode dontAsk` denies instead of
/// asking, and `--no-session-persistence` keeps the conversation off disk.
pub(super) fn claude_args(model: &str, system_prompt: &str) -> Vec<String> {
    [
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
        "--safe-mode",
        "--strict-mcp-config",
        "--permission-mode",
        "dontAsk",
        "--model",
        model,
        "--system-prompt",
        system_prompt,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// The script `wsl_bash` runs for Claude: `claude_args`, each one quoted, so
/// the empty `--tools` value stays a literal `''`.
fn claude_wsl_script(model: &str, system_prompt: &str) -> String {
    let args: Vec<String> = claude_args(model, system_prompt)
        .iter()
        .map(|arg| shell_escape(arg))
        .collect();
    format!("claude {}", args.join(" "))
}

/// Stream a Claude response by spawning the Claude CLI in stream-json mode.
pub(super) async fn stream_claude<F>(
    cfg: &CliConfig,
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    screenshot: Option<&str>,
    on_chunk: F,
) -> Result<(), String>
where
    F: FnMut(String) -> Result<(), String> + Send,
{
    if !cfg.claude.is_available() {
        return Err("Claude CLI is not available on this system.".to_owned());
    }
    validate_model_name(model)?;

    let mut cmd = if cfg.claude == CliMode::Wsl {
        Command::from(wsl_bash(&claude_wsl_script(model, system_prompt)))
    } else {
        let mut c = Command::new("claude");
        c.args(claude_args(model, system_prompt));
        c
    };

    let input = build_claude_input(messages, screenshot);
    // Claude emits stream-json and its parser drops non-JSON, so shell
    // banners cannot reach the user -- no sentinel needed.
    run_cli(&mut cmd, input, on_chunk, parse_claude_line, "Claude", None).await
}

/// Stream a Codex response by spawning the Codex CLI in `exec` mode.
///
/// `shot` is the shots directory and a PNG to show Codex. The file is written
/// here and deleted when this future ends, however it ends: answer, error,
/// timeout, Stop or a newer request.
pub(super) async fn stream_codex<F>(
    cfg: &CliConfig,
    system_prompt: &str,
    messages: &[ChatMessage],
    shot: Option<(&std::path::Path, &[u8])>,
    on_chunk: F,
) -> Result<(), String>
where
    F: FnMut(String) -> Result<(), String> + Send,
{
    if !cfg.codex.is_available() {
        return Err("Codex CLI is not available on this system.".to_owned());
    }

    let temp_shot = shot.and_then(|(dir, png)| match TempShot::create(shot_path(dir), png) {
        Ok(temp) => {
            tracing::info!("Codex request: screenshot attached");
            Some(temp)
        }
        Err(e) => {
            tracing::warn!("Codex request continues without the screenshot: {e}");
            None
        }
    });
    let image = temp_shot
        .as_ref()
        .map(|temp| temp.path.to_string_lossy().into_owned());

    let work_dir = cfg.codex_workdir.as_str();
    let mut cmd = if cfg.codex == CliMode::Wsl {
        Command::from(wsl_bash(&codex_wsl_script(work_dir, image.as_deref())))
    } else {
        let mut c = Command::new("codex");
        c.args(codex_args(work_dir, image.as_deref()));
        c
    };

    let input = build_codex_input(system_prompt, messages);
    let sentinel = matches!(cfg.codex, CliMode::Wsl).then_some(WSL_SENTINEL);
    let result = run_cli(
        &mut cmd,
        input,
        on_chunk,
        parse_codex_line,
        "Codex",
        sentinel,
    )
    .await;
    // Only now may the file go: Codex reads it while `run_cli` runs.
    drop(temp_shot);
    result
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
    skip_until: Option<&str>,
) -> Result<(), String>
where
    F: FnMut(String) -> Result<(), String> + Send,
    P: Fn(&str) -> Option<Parsed> + Send + Sync,
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
        crate::util::log_if_err(
            "write the CLI prompt",
            stdin.write_all(input.as_bytes()).await,
        );
        crate::util::log_if_err("flush the CLI prompt", stdin.flush().await);
        // Dropping stdin closes the pipe so the CLI knows the input is complete.
    };

    // Keep the last few stderr lines: when the child fails without writing any
    // stdout, this is the only thing that can explain why. They are not logged
    // line by line -- Codex echoes the whole prompt, and with it the chat, to
    // stderr -- only counted, and the tail is logged when the CLI fails.
    let stderr_fut = async move {
        let mut tail: Vec<String> = Vec::new();
        let mut count = 0usize;
        if let Some(stderr) = stderr {
            let reader = BufReader::new(stderr);
            let mut lines = LinesStream::new(reader.lines());
            while let Some(Ok(line)) = lines.next().await {
                if !line.trim().is_empty() {
                    count += 1;
                    if tail.len() == STDERR_TAIL_LINES {
                        tail.remove(0);
                    }
                    tail.push(line);
                }
            }
        }
        (tail, count)
    };

    let read_fut = async {
        let reader = BufReader::new(stdout);
        let mut lines = LinesStream::new(reader.lines());
        let mut total_bytes: usize = 0;
        let mut emitted = false;
        // Set only when the command prints WSL_SENTINEL; everything the
        // interactive shell emitted before it is profile noise, not output.
        let mut waiting_for_sentinel = skip_until.is_some();
        while let Some(item) = lines.next().await {
            let line = item.map_err(|e| format!("Failed to read from {label} CLI: {e}"))?;
            if waiting_for_sentinel {
                if skip_until.is_some_and(|marker| line.trim() == marker) {
                    waiting_for_sentinel = false;
                } else if !line.trim().is_empty() {
                    tracing::debug!("{label}: dropping pre-start shell output: {line}");
                }
                continue;
            }
            // Mirror the Gemini stream cap: `lines()` grows one buffer with no
            // ceiling, so a child emitting a huge blob would otherwise grow the
            // launcher's memory byte for byte. The `+ 1` is the newline `lines()`
            // strips, so a flood of blank lines counts too.
            total_bytes = total_bytes.saturating_add(line.len() + 1);
            if total_bytes > MAX_STREAM_BYTES {
                return Err(format!("{label} response exceeded the size limit."));
            }
            // Blank lines reach the parser: they are paragraph breaks in a
            // plain-text answer, and Claude's JSON parser drops them anyway.
            match parse_line(&line) {
                Some(Parsed::Text(text)) => {
                    emitted = true;
                    on_chunk(text)?;
                }
                Some(Parsed::Error(message)) => return Err(message),
                None => {}
            }
        }
        Ok(emitted)
    };

    let ((), (stderr_tail, stderr_lines), read_result) =
        tokio::join!(write_fut, stderr_fut, read_fut);
    tracing::debug!("{label} wrote {stderr_lines} stderr line(s)");
    let emitted = read_result?;

    // Stdout reaching EOF is NOT success. Without this, a CLI that fails before
    // printing anything -- not logged in, binary missing, WSL distro down --
    // reached the user as a completed, empty answer with no error at all.
    let status = child.wait().await;
    drop(child);
    let failed = |stderr_tail: &[String]| {
        let message = cli_failure_message(label, stderr_tail);
        tracing::warn!("{message}");
        Err(message)
    };
    match status {
        Ok(status) if status.success() => {
            if emitted {
                Ok(())
            } else {
                failed(&stderr_tail)
            }
        }
        Ok(status) => {
            tracing::warn!("{label} CLI exited with {status}");
            failed(&stderr_tail)
        }
        Err(e) => Err(format!("Failed to wait for {label} CLI: {e}")),
    }
}

/// Surface the child's own stderr when it has any -- "Invalid API key, please
/// run /login" is actionable in a way that a generic failure string is not.
fn cli_failure_message(label: &str, stderr_tail: &[String]) -> String {
    if stderr_tail.is_empty() {
        format!("{label} CLI produced no output. Check the launcher log.")
    } else {
        format!("{label} CLI failed: {}", stderr_tail.join(" / "))
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

    // ---------------- WSL spawning ----------------

    #[test]
    fn every_wsl_spawn_uses_the_builder() {
        let needle = concat!("Command::new(", "\"wsl.exe\")");
        let (files, count) = crate::util::count_in_sources(needle, None);
        println!("scanned {files} files, found {count} occurrence(s) of {needle}");
        assert!(files > 0, "the source scan found no files");
        assert_eq!(count, 1, "every wsl.exe child must be built by wsl_exec");
    }

    #[test]
    fn wsl_bash_passes_script_without_a_second_shell() {
        let cmd = wsl_bash("S");
        assert_eq!(cmd.get_program(), "wsl.exe");
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            ["--exec", "bash", "-lic", "S"]
        );
    }

    #[test]
    fn version_probe_escapes_the_name() {
        assert_eq!(version_script("x; y"), "'x; y' --version");
    }

    /// Every input must come back byte for byte: anything else means a shell
    /// other than the `bash` we start parsed the command line.
    #[cfg(windows)]
    #[test]
    #[ignore = "needs WSL"]
    fn wsl_round_trips_hostile_text() {
        let inputs = [
            "$(echo INJECTED)",
            "`echo INJECTED`",
            "$HOME and $USER",
            "say \"hi\"",
            "it's",
            r"a\\b",
            r"ends with \",
            "line one\n$(echo INJECTED)",
            "%PATH% !bang!",
            "caf\u{e9} \u{65e5}\u{672c}",
        ];
        let marker = format!("{WSL_SENTINEL}\n");
        for input in inputs {
            let out = wsl_bash(&format!(
                "printf '%s\\n' {WSL_SENTINEL}; printf '%s' {}",
                shell_escape(input)
            ))
            .output()
            .unwrap();
            let stdout = String::from_utf8(out.stdout).unwrap();
            let result = stdout
                .strip_prefix(&marker)
                .or_else(|| {
                    stdout
                        .split_once(&format!("\n{marker}"))
                        .map(|(_, rest)| rest)
                })
                .unwrap();
            println!("input  {input:?}\nresult {result:?}");
            assert_eq!(result, input);
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
        assert_eq!(
            parse_claude_line(line),
            Some(Parsed::Text("PONG".to_owned()))
        );
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
        let line = r#"{"type":"result","subtype":"success","is_error":true,"result":"API Error: 529 overloaded"}"#;
        assert_eq!(
            parse_claude_line(line),
            Some(Parsed::Error("API Error: 529 overloaded".to_owned()))
        );
    }

    #[test]
    fn parse_claude_line_uses_fallback_when_error_result_has_no_message() {
        let line = r#"{"type":"result","is_error":true}"#;
        assert_eq!(
            parse_claude_line(line),
            Some(Parsed::Error(
                "Claude CLI returned an error without a message".to_owned()
            ))
        );
    }

    #[test]
    fn claude_result_error_table() {
        use serde_json::json;
        const FALLBACK: &str = "Claude CLI returned an error without a message";
        let row = |frame: serde_json::Value| {
            let got = claude_result_error(&frame);
            let shown = frame.to_string().chars().take(100).collect::<String>();
            let message = got
                .as_deref()
                .map(|got| got.chars().take(100).collect::<String>());
            println!("{shown} -> {message:?}");
            got
        };
        assert_eq!(
            row(json!({"type":"result","subtype":"success","is_error":true,"result":"X"})),
            Some("X".to_owned())
        );
        assert_eq!(
            row(json!({"type":"result","subtype":"error_during_execution","errors":["A","B"]})),
            Some("A; B".to_owned())
        );
        let http = row(json!({"type":"result","is_error":true,"api_error_status":529})).unwrap();
        assert!(http.contains("HTTP 529"), "{http}");
        let turns = row(json!({"type":"result","subtype":"error_max_turns"})).unwrap();
        assert!(turns.contains("error_max_turns"), "{turns}");
        assert_eq!(
            row(json!({"type":"result","is_error":true})),
            Some(FALLBACK.to_owned())
        );
        let long = row(json!({"type":"result","is_error":true,"result":"\u{e9}".repeat(1000)}));
        assert_eq!(long.map(|message| message.chars().count()), Some(300));
        assert_eq!(
            row(json!({"type":"result","subtype":"success","is_error":false,"result":"PONG"})),
            None
        );
        assert_eq!(
            row(json!({"type":"result","is_error":true,"result":"  ","errors":["  ",7,"B"]})),
            Some("B".to_owned())
        );
        assert_eq!(
            row(json!({"type":"result","subtype":"success","is_error":true})),
            Some(FALLBACK.to_owned())
        );
    }

    #[test]
    fn no_generic_claude_error_text() {
        let needle = concat!("Unknown Claude", " CLI error");
        let (files, count) = crate::util::count_in_sources(needle, None);
        println!("scanned {files} files, found {count} occurrence(s) of {needle}");
        assert!(files > 0, "the source scan found no files");
        assert_eq!(count, 0, "a Claude error must carry the CLI's own text");
    }

    #[test]
    fn parse_claude_line_skips_malformed_or_empty_lines() {
        assert_eq!(parse_claude_line("not json"), None);
        assert_eq!(parse_claude_line(""), None);
    }

    // ---------------- Codex screenshot file ----------------

    /// A fresh, empty directory for one test.
    fn fresh_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("aigc-test-{name}-{}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn temp_shot_is_deleted_on_drop() {
        let dir = fresh_dir("drop");
        let path = shot_path(&dir);
        let shot = TempShot::create(path.clone(), b"PNGDATA").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"PNGDATA");
        drop(shot);
        assert!(!path.exists(), "the screenshot outlived its guard");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn temp_shot_never_writes_through_an_existing_file() {
        let dir = fresh_dir("existing");
        let path = dir.join("taken.png");
        std::fs::write(&path, b"ORIGINAL").unwrap();
        TempShot::create(path.clone(), b"NEW").unwrap_err();
        assert_eq!(std::fs::read(&path).unwrap(), b"ORIGINAL");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn shot_path_names_a_file_in_the_directory() {
        let dir = std::path::Path::new("shots-dir");
        let path = shot_path(dir);
        assert_eq!(path.parent(), Some(dir));
        let name = path.file_name().unwrap().to_str().unwrap();
        println!("{name}");
        let (pid, nanos) = name
            .strip_prefix("aigc-shot-")
            .and_then(|rest| rest.strip_suffix(".png"))
            .and_then(|middle| middle.split_once('-'))
            .unwrap();
        let digits =
            |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
        assert!(digits(pid), "process id {pid:?}");
        assert!(digits(nanos), "timestamp {nanos:?}");
    }

    #[test]
    fn sweep_removes_leftover_shots() {
        let dir = fresh_dir("sweep");
        for name in ["a.png", "b.png", "c.png"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        assert_eq!(sweep_shots(&dir), 3);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(sweep_shots(&dir), 0, "a missing directory is not an error");
    }

    #[test]
    fn codex_args_add_one_image_token() {
        let plain = codex_args("W", None);
        assert_eq!(
            plain,
            [
                "-a",
                "never",
                "-s",
                "read-only",
                "--disable",
                "shell_tool",
                "-C",
                "W",
                "exec",
                "--skip-git-repo-check",
                "--ephemeral",
                "--ignore-user-config"
            ]
        );
        assert!(plain.iter().all(|arg| !arg.contains("--image")));

        let with_image = codex_args("W", Some(r"C:\shots\a b.png"));
        let (last, before) = with_image.split_last().unwrap();
        assert_eq!(before, plain.as_slice());
        assert_eq!(last, r"--image=C:\shots\a b.png");
        assert_eq!(
            with_image
                .iter()
                .filter(|arg| arg.contains("--image"))
                .count(),
            1
        );
    }

    #[test]
    fn codex_wsl_script_hands_the_image_over() {
        assert_eq!(
            codex_wsl_script("/srv/work dir", None),
            "printf '%s\\n' __AIGC_STREAM_BEGIN__; codex -a never -s read-only --disable shell_tool -C '/srv/work dir' exec --skip-git-repo-check --ephemeral --ignore-user-config"
        );

        let image = r"C:\shots\it's $(x) a.png";
        let script = codex_wsl_script("/srv/work dir", Some(image));
        println!("{script}");
        assert!(script.contains(&format!("wslpath -u {}", shell_escape(image))));
        assert_eq!(
            script,
            r#"printf '%s\n' __AIGC_STREAM_BEGIN__; img=$(wslpath -u 'C:\shots\it'\''s $(x) a.png') && codex -a never -s read-only --disable shell_tool -C '/srv/work dir' exec --skip-git-repo-check --ephemeral --ignore-user-config --image="$img""#
        );
        assert_eq!(script.matches("--image=").count(), 1);
    }

    #[test]
    fn codex_runs_ephemeral() {
        for image in [None, Some(r"C:\shots\a b.png")] {
            let args = codex_args("W", image);
            println!("{args:?}");
            assert_eq!(
                args.iter().filter(|arg| *arg == "--ephemeral").count(),
                1,
                "{args:?}"
            );
            let skip = args
                .iter()
                .position(|arg| arg == "--skip-git-repo-check")
                .unwrap();
            assert_eq!(
                args.get(skip + 1).map(String::as_str),
                Some("--ephemeral"),
                "{args:?}"
            );

            let script = codex_wsl_script("/srv/work dir", image);
            println!("{script}");
            assert_eq!(script.matches("--ephemeral").count(), 1, "{script}");
            assert!(
                script.contains("--skip-git-repo-check --ephemeral"),
                "{script}"
            );
        }
    }

    /// Every index at which `flag` appears in `args`.
    fn positions(args: &[String], flag: &str) -> Vec<usize> {
        args.iter()
            .enumerate()
            .filter(|(_, arg)| *arg == flag)
            .map(|(at, _)| at)
            .collect()
    }

    #[test]
    fn claude_children_are_locked_down() {
        let prompt = "You are Sage. It's $(x) `y`";
        let args = claude_args("claude-haiku-4-5", prompt);
        println!("{args:?}");
        let after = |flag: &str| {
            let at = positions(&args, flag);
            assert_eq!(at.len(), 1, "{flag} in {args:?}");
            args.get(at[0] + 1).map(String::as_str)
        };
        for flag in [
            "--safe-mode",
            "--strict-mcp-config",
            "--no-session-persistence",
        ] {
            assert_eq!(positions(&args, flag).len(), 1, "{flag} in {args:?}");
        }
        assert_eq!(after("--permission-mode"), Some("dontAsk"));
        assert_eq!(after("--tools"), Some(""));
        assert_eq!(after("--model"), Some("claude-haiku-4-5"));
        assert_eq!(after("--system-prompt"), Some(prompt));

        let script = claude_wsl_script("claude-haiku-4-5", prompt);
        println!("{script}");
        assert!(script.starts_with("claude "), "{script}");
        for flag in [
            "'--safe-mode'",
            "'--strict-mcp-config'",
            "'--permission-mode' 'dontAsk'",
            "'--tools' ''",
            "'--no-session-persistence'",
        ] {
            assert_eq!(script.matches(flag).count(), 1, "{flag} in {script}");
        }
        assert!(script.contains(&shell_escape(prompt)), "{script}");
    }

    #[test]
    fn codex_children_are_locked_down() {
        for image in [None, Some(r"C:\shots\a b.png")] {
            let args = codex_args("W", image);
            println!("{args:?}");
            let exec = positions(&args, "exec");
            assert_eq!(exec.len(), 1, "{args:?}");
            let disable = positions(&args, "--disable");
            assert_eq!(disable.len(), 1, "{args:?}");
            assert!(disable[0] < exec[0], "--disable must come before exec");
            assert_eq!(
                args.get(disable[0] + 1).map(String::as_str),
                Some("shell_tool")
            );
            let ignore = positions(&args, "--ignore-user-config");
            assert_eq!(ignore.len(), 1, "{args:?}");
            assert!(ignore[0] > exec[0], "--ignore-user-config must follow exec");

            let script = codex_wsl_script("/srv/work dir", image);
            println!("{script}");
            assert_eq!(script.matches(" exec ").count(), 1, "{script}");
            let (before, after) = script.split_once(" exec ").unwrap();
            assert_eq!(
                before.matches("--disable shell_tool").count(),
                1,
                "{script}"
            );
            assert!(!after.contains("--disable"), "{script}");
            assert_eq!(after.matches("--ignore-user-config").count(), 1, "{script}");
            assert!(!before.contains("--ignore-user-config"), "{script}");
        }
    }

    // ---------------- run_cli line handling ----------------

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_output_keeps_line_breaks() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf 'One.\\nTwo.\\n\\n- a\\n- b\\n'"]);
        let mut chunks = Vec::new();
        let result = run_cli(
            &mut cmd,
            String::new(),
            |text| {
                chunks.push(text);
                Ok(())
            },
            parse_codex_line,
            "Codex",
            None,
        )
        .await;
        let joined = chunks.concat();
        println!("{joined:?}");
        assert_eq!(result, Ok(()));
        assert_eq!(joined, "One.\nTwo.\n\n- a\n- b\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cli_stderr_is_not_copied_into_the_log() {
        #[derive(Clone, Default)]
        struct Buf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .map_err(|_| std::io::Error::other("log buffer poisoned"))?
                    .extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        // A process-wide subscriber, installed once: a thread-local one misses
        // events whenever a parallel test registered the call site first
        // (measured: the captured log came back empty in 3 of 5 full runs).
        static LOG: std::sync::OnceLock<Buf> = std::sync::OnceLock::new();
        let buf = LOG
            .get_or_init(|| {
                let buf = Buf::default();
                let writer = buf.clone();
                let subscriber = tracing_subscriber::fmt()
                    .with_max_level(tracing::Level::TRACE)
                    .with_writer(move || writer.clone())
                    .finish();
                tracing::subscriber::set_global_default(subscriber).unwrap();
                buf
            })
            .clone();

        // Codex echoes the prompt -- and so the conversation -- to stderr. A
        // label no other test uses keeps their lines out of the assertions.
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo 'user: SECRET-PROMPT' >&2; echo answer"]);
        let result = run_cli(
            &mut cmd,
            String::new(),
            |_| Ok(()),
            parse_codex_line,
            "Probe",
            None,
        )
        .await;
        let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert_eq!(result, Ok(()));
        assert!(
            !log.contains("SECRET-PROMPT"),
            "stderr text reached the log"
        );
        assert!(
            log.contains("Probe wrote 1 stderr line(s)"),
            "the stderr line count is logged"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn blank_lines_count_toward_the_byte_cap() {
        let script = format!(
            "head -c {} /dev/zero | tr '\\0' '\\n'",
            MAX_STREAM_BYTES + 10
        );
        let mut cmd = Command::new("sh");
        cmd.args(["-c", &script]);
        let result = run_cli(
            &mut cmd,
            String::new(),
            |_| Ok(()),
            parse_codex_line,
            "Codex",
            None,
        )
        .await;
        println!("{result:?}");
        assert_eq!(
            result,
            Err("Codex response exceeded the size limit.".to_owned())
        );
    }

    // ---------------- parse_codex_line ----------------

    #[test]
    fn parse_codex_line_emits_plain_text_verbatim() {
        assert_eq!(
            parse_codex_line("PONG"),
            Some(Parsed::Text("PONG\n".to_owned()))
        );
        // A blank line is a paragraph break, not nothing.
        assert_eq!(parse_codex_line(""), Some(Parsed::Text("\n".to_owned())));
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
        assert_eq!(
            parse_codex_line(line),
            Some(Parsed::Text("hi there".to_owned()))
        );
    }

    #[test]
    fn parse_codex_line_skips_unrecognized_json_object() {
        let line = r#"{"type":"token_count","tokens":42}"#;
        assert_eq!(parse_codex_line(line), None);
    }

    #[test]
    fn parse_codex_line_emits_typeless_json_answer_verbatim() {
        // A JSON answer with no protocol "type" is the model's output, not a
        // control frame -- emit it verbatim instead of dropping it.
        let object = r#"{"ok":true}"#;
        assert_eq!(
            parse_codex_line(object),
            Some(Parsed::Text(format!("{object}\n")))
        );
        assert_eq!(
            parse_codex_line("42"),
            Some(Parsed::Text("42\n".to_owned()))
        );
    }
}
