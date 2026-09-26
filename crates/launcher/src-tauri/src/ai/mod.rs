//! In-process multi-provider AI backend for the external overlay companion.
//!
//! Providers are dispatched directly from the Tauri backend (no localhost HTTP
//! proxy): Gemini over its streaming HTTP API, Claude / Codex by spawning their
//! CLIs. Output is coalesced and streamed to the overlay window over a Tauri
//! `Channel`, tagged with request + conversation IDs. Only one request runs at a
//! time -- a new request cancels and replaces the previous one.

mod cli;
mod gemini;

use std::fmt::Write as _;

use base64::Engine as _;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager};

use crate::overlay::GameInfo;
use crate::state::AppState;

pub(crate) use cli::{detect_all, CliConfig};

/// Backstop timeout for a single request, covering a hung CLI that never closes
/// stdout. Gemini has its own (shorter) HTTP timeout, so this is the CLI ceiling.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(3);

/// The provider a request targets. Serialized lowercase to match the overlay UI
/// (`"gemini"` / `"claude"` / `"openai"`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Provider {
    #[default]
    Gemini,
    Claude,
    Openai,
}

impl Provider {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Gemini => "gemini",
            Self::Claude => "claude",
            Self::Openai => "openai",
        }
    }
}

/// One chat turn sent from the overlay UI.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// A streamed event delivered to the overlay window over the request's Channel.
/// `kind` is `"chunk"` | `"done"` | `"error"`; every event carries the request +
/// conversation IDs so the UI can ignore output from superseded requests.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SageEvent {
    kind: &'static str,
    request_id: u64,
    conversation_id: u64,
    #[serde(skip_serializing_if = "String::is_empty")]
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

impl SageEvent {
    const fn chunk(request_id: u64, conversation_id: u64, text: String) -> Self {
        Self {
            kind: "chunk",
            request_id,
            conversation_id,
            text,
            message: None,
        }
    }

    const fn done(request_id: u64, conversation_id: u64) -> Self {
        Self {
            kind: "done",
            request_id,
            conversation_id,
            text: String::new(),
            message: None,
        }
    }

    const fn error(request_id: u64, conversation_id: u64, message: String) -> Self {
        Self {
            kind: "error",
            request_id,
            conversation_id,
            text: String::new(),
            message: Some(message),
        }
    }
}

/// Which providers can currently serve a request.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProviderAvailability {
    pub gemini: bool,
    pub claude: bool,
    pub openai: bool,
    /// Where each CLI was detected ("PATH" / "WSL" / "").
    pub claude_where: String,
    pub openai_where: String,
    /// The Gemini model a request names now.
    pub gemini_model: String,
    /// The Gemini model used when Settings names none.
    pub gemini_fallback_model: String,
    /// The Claude model the CLI is asked for.
    pub claude_model: String,
    /// The Codex model; empty because the CLI picks its own default.
    pub openai_model: String,
}

/// Parameters of a chat request, deserialized from the `ask_sage` command.
pub(crate) struct RequestParams {
    pub request_id: u64,
    pub conversation_id: u64,
    pub provider: Provider,
    pub messages: Vec<ChatMessage>,
    pub attach_screenshot: bool,
}

/// The single in-flight request (if any). Aborting `handle` cancels the request
/// and -- because CLI children are spawned with `kill_on_drop` -- kills any child.
struct Active {
    request_id: u64,
    handle: tauri::async_runtime::JoinHandle<()>,
}

/// Backend AI state: cached CLI availability plus the active-request slot.
pub(crate) struct AiState {
    cli: Mutex<CliConfig>,
    active: Mutex<Option<Active>>,
}

impl Default for AiState {
    fn default() -> Self {
        Self {
            cli: Mutex::new(CliConfig::default()),
            active: Mutex::new(None),
        }
    }
}

impl AiState {
    /// Store the CLI availability detected on the background startup thread.
    pub(crate) fn set_cli(&self, cfg: CliConfig) {
        *self.cli.lock() = cfg;
    }

    /// Report which providers can currently serve a request, and the Gemini model
    /// a request would name given `settings_model` (the Settings choice). Gemini
    /// depends on a stored key; Claude / Codex on a detected CLI.
    pub(crate) fn availability(&self, settings_model: &str) -> ProviderAvailability {
        let file_model = gemini::file_model();
        let cli = self.cli.lock();
        ProviderAvailability {
            gemini: gemini::load_config().is_ok(),
            claude: cli.claude.is_available(),
            openai: cli.codex.is_available(),
            claude_where: cli.claude.location().to_owned(),
            openai_where: cli.codex.location().to_owned(),
            gemini_model: gemini::resolve_model(settings_model, &file_model),
            gemini_fallback_model: gemini::resolve_model("", &file_model),
            claude_model: cli::DEFAULT_CLAUDE_MODEL.to_owned(),
            openai_model: String::new(),
        }
    }

    /// Cancel the previous request (if any) and install the new one.
    fn replace_active(&self, request_id: u64, handle: tauri::async_runtime::JoinHandle<()>) {
        let mut guard = self.active.lock();
        if let Some(previous) = guard.take() {
            previous.handle.abort();
        }
        *guard = Some(Active { request_id, handle });
    }

    /// Cancel `request_id` if it is the active request (Stop button).
    pub(crate) fn cancel(&self, request_id: u64) {
        let mut guard = self.active.lock();
        if let Some(active) = guard.take_if(|active| active.request_id == request_id) {
            active.handle.abort();
        }
    }

    /// Clear the active slot once a request finishes, unless it was already
    /// replaced by a newer request.
    fn clear_if(&self, request_id: u64) {
        let mut guard = self.active.lock();
        guard.take_if(|active| active.request_id == request_id);
    }
}

/// Spawn a chat request, cancelling and replacing any request already running.
pub(crate) fn spawn_request(app: &AppHandle, params: RequestParams, channel: Channel<SageEvent>) {
    let request_id = params.request_id;
    let handle = tauri::async_runtime::spawn(run(app.clone(), params, channel));
    app.state::<AiState>().replace_active(request_id, handle);
}

/// Drive one request end to end: build the system prompt + optional screenshot,
/// stream the provider through a coalescing buffer, and emit terminal events.
async fn run(app: AppHandle, params: RequestParams, channel: Channel<SageEvent>) {
    let RequestParams {
        request_id,
        conversation_id,
        provider,
        messages,
        attach_screenshot,
    } = params;

    // Read shared state up front so no state guard is held across an await.
    // Only a linked, still-live target contributes a name or a capture target.
    let ctx = request_context(crate::overlay::linked_game(&app).as_ref());
    tracing::info!(
        "{}",
        request_log_line(request_id, provider, attach_screenshot, &ctx)
    );
    let system_prompt = build_system_prompt(ctx.game_name.as_deref());
    let capture_target = ctx.capture;
    let cli_cfg = app.state::<AiState>().cli.lock().clone();
    let settings_model = app
        .state::<AppState>()
        .launcher
        .lock()
        .settings
        .gemini_model
        .clone();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let chan_stream = channel.clone();

    let producer = async move {
        // Capture INSIDE the timed section. A blocked GDI/WGC call cannot be
        // cancelled once spawned, so awaiting it before the timeout wrapper
        // left the UI stuck on "Streaming" with no done, no error and a leaked
        // blocking-pool thread.
        // Screenshots are skipped for OpenAI (Codex `--image` is broken upstream).
        let screenshot = if attach_screenshot && provider != Provider::Openai {
            capture_base64(capture_target).await
        } else {
            None
        };
        let on_chunk = move |text: String| {
            tx.send(text)
                .map_err(|_| "overlay window closed".to_owned())
        };
        match provider {
            Provider::Gemini => {
                let cfg = gemini::load_config()?;
                let model = gemini::resolve_model(&settings_model, &gemini::file_model());
                gemini::stream(
                    &messages,
                    &system_prompt,
                    screenshot,
                    &model,
                    &cfg.api_key,
                    on_chunk,
                )
                .await
            }
            Provider::Claude => {
                cli::stream_claude(
                    &cli_cfg,
                    cli::DEFAULT_CLAUDE_MODEL,
                    &system_prompt,
                    &messages,
                    screenshot.as_deref(),
                    on_chunk,
                )
                .await
            }
            Provider::Openai => {
                cli::stream_codex(&cli_cfg, &system_prompt, &messages, on_chunk).await
            }
        }
    };

    // Coalesce bursts: drain everything queued into a single Channel message so a
    // fast per-token provider (Claude deltas) does not flood the IPC boundary.
    let consumer = async move {
        while let Some(first) = rx.recv().await {
            let mut batch = first;
            while let Ok(more) = rx.try_recv() {
                batch.push_str(&more);
            }
            // A send failure means the overlay webview is gone. Swallowing it
            // kept the loop running and the CLI subprocess streaming (and
            // billing) to nobody. Closing `rx` makes the producer's next
            // `tx.send` fail, which ends the request and drops the child.
            if chan_stream
                .send(SageEvent::chunk(request_id, conversation_id, batch))
                .is_err()
            {
                tracing::info!("Overlay channel closed; ending request {request_id}");
                rx.close();
                return;
            }
        }
    };

    // Backstop timeout: a hung CLI (no output, never closing stdout) would
    // otherwise leave the join pending forever, stranding the UI on "Streaming".
    // On elapse the futures drop -- killing any CLI child via kill_on_drop.
    let streamed = async { tokio::join!(producer, consumer).0 };
    let result = tokio::time::timeout(REQUEST_TIMEOUT, streamed)
        .await
        .unwrap_or_else(|_elapsed| Err("Request timed out. Try again.".to_owned()));

    let event = match result {
        Ok(()) => SageEvent::done(request_id, conversation_id),
        Err(message) => SageEvent::error(request_id, conversation_id, message),
    };
    crate::util::log_if_err("send the final sage event", channel.send(event));

    app.state::<AiState>().clear_if(request_id);
}

/// Capture the linked game window and base64-encode it as PNG for an AI request.
/// Capture failures are non-fatal: the request proceeds without the screenshot.
async fn capture_base64(target: Option<(i64, u32)>) -> Option<String> {
    let (hwnd, pid) = target?;
    match tokio::task::spawn_blocking(move || {
        crate::overlay_capture::capture_live_window_png(hwnd, pid)
    })
    .await
    {
        Ok(Ok(png)) => Some(base64::engine::general_purpose::STANDARD.encode(png)),
        Ok(Err(error)) => {
            tracing::warn!("screenshot capture failed: {error}");
            None
        }
        Err(error) => {
            tracing::warn!("screenshot capture task failed: {error}");
            None
        }
    }
}

/// What a request may use from the stored target.
#[derive(Debug, Default, PartialEq, Eq)]
struct RequestContext {
    /// The name the system prompt may use.
    game_name: Option<String>,
    /// The window a screenshot may capture, as (hwnd, pid).
    capture: Option<(i64, u32)>,
}

/// The send gate: a target contributes its name and its capture target only
/// when it is linked. A missing or unlinked target contributes nothing, and a
/// window title never leaves here.
fn request_context(target: Option<&GameInfo>) -> RequestContext {
    match target {
        Some(game) if game.linked => RequestContext {
            game_name: Some(game.name.clone()),
            capture: Some((game.hwnd, game.pid)),
        },
        _ => RequestContext::default(),
    }
}

/// One log line per request recording what the gate let through -- never a name
/// or a title.
fn request_log_line(
    request_id: u64,
    provider: Provider,
    screenshot_requested: bool,
    ctx: &RequestContext,
) -> String {
    let yes_no = |flag: bool| if flag { "yes" } else { "no" };
    format!(
        "Request {request_id}: provider {}, screenshot requested: {}, linked target: {}",
        provider.as_str(),
        yes_no(screenshot_requested),
        yes_no(ctx.capture.is_some()),
    )
}

/// The Sage persona prompt, naming the game when the linked target has a name.
fn build_system_prompt(game_name: Option<&str>) -> String {
    let mut prompt = default_system_prompt();
    if let Some(name) = game_name.map(str::trim).filter(|name| !name.is_empty()) {
        let _ = write!(prompt, " The player is currently playing {name}.");
    }
    prompt
}

fn default_system_prompt() -> String {
    "You are Sage, a sharp and knowledgeable game companion embedded in the player's screen. \
     Keep answers short -- 2-3 sentences unless the player asks for detail. \
     Never repeat or rephrase what the player just said. \
     Never state the obvious (e.g. don't say \"I see you're in a menu\"). \
     Jump straight to the useful part: what to do, where to go, or how something works. \
     When you see a screenshot, focus only on what's relevant to the player's question. \
     If no question is asked with a screenshot, give the single most useful observation."
        .to_owned()
}

const TRANSLATE_SYSTEM: &str =
    "You are a screen translator for a gamer. Read the foreign text in the image and translate it \
     into natural English. Be concise; do not add commentary.";

/// Check a Gemini model id chosen in Settings before it is saved.
pub(crate) fn validate_gemini_model(model: &str) -> Result<(), String> {
    gemini::validate_model(model)
}

/// Capture the linked game window and translate any foreign text in it to
/// English via Gemini. A one-shot call, independent of the chat request slot.
/// `settings_model` is the Gemini model chosen in Settings (empty for none).
pub(crate) async fn translate_capture(
    hwnd: i64,
    pid: u32,
    settings_model: String,
) -> Result<String, String> {
    let png = tokio::task::spawn_blocking(move || {
        crate::overlay_capture::capture_live_window_png(hwnd, pid)
    })
    .await
    .map_err(|error| format!("capture task failed: {error}"))??;
    let screenshot = base64::engine::general_purpose::STANDARD.encode(png);
    let cfg = gemini::load_config()?;
    let model = gemini::resolve_model(&settings_model, &gemini::file_model());
    let messages = [ChatMessage {
        role: "user".to_owned(),
        content: "Translate any non-English text visible in this screenshot into English. Output \
                  only the translation. If there is no foreign text, reply exactly: No foreign \
                  text found."
            .to_owned(),
    }];
    let mut out = String::new();
    gemini::stream(
        &messages,
        TRANSLATE_SYSTEM,
        Some(screenshot),
        &model,
        &cfg.api_key,
        |chunk| {
            out.push_str(&chunk);
            Ok(())
        },
    )
    .await?;
    Ok(out.trim().to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::print_stdout,
        reason = "the tests print the prompt and what the scans counted"
    )]

    use super::*;

    fn target(linked: bool) -> GameInfo {
        GameInfo {
            hwnd: 42,
            pid: 7,
            exe: r"C:\Games\Foo\foo.exe".into(),
            title: "SECRET-TITLE $(id)".into(),
            name: "Real Name".into(),
            linked,
        }
    }

    #[test]
    fn system_prompt_names_game_never_title() {
        let ctx = request_context(Some(&target(true)));
        let prompt = build_system_prompt(ctx.game_name.as_deref());
        println!("{prompt}");
        assert!(prompt.contains("Real Name"));
        assert!(!prompt.contains("SECRET-TITLE"));

        let untargeted = build_system_prompt(request_context(None).game_name.as_deref());
        assert_eq!(
            untargeted,
            default_system_prompt(),
            "no target names no game"
        );
    }

    #[test]
    fn unlinked_target_contributes_nothing() {
        assert_eq!(request_context(None), RequestContext::default());
        assert_eq!(
            request_context(Some(&target(false))),
            RequestContext::default()
        );
        assert_eq!(
            request_context(Some(&target(true))),
            RequestContext {
                game_name: Some("Real Name".into()),
                capture: Some((42, 7)),
            }
        );
    }

    #[test]
    fn request_log_line_names_no_game() {
        for (linked, expected) in [
            (
                true,
                "Request 3: provider claude, screenshot requested: yes, linked target: yes",
            ),
            (
                false,
                "Request 3: provider claude, screenshot requested: yes, linked target: no",
            ),
        ] {
            let ctx = request_context(Some(&target(linked)));
            let line = request_log_line(3, Provider::Claude, true, &ctx);
            println!("{line}");
            assert_eq!(line, expected);
            assert!(!line.contains("Real Name"));
            assert!(!line.contains("SECRET-TITLE"));
        }
    }

    #[test]
    fn availability_reports_the_models_in_use() {
        let availability = AiState::default().availability("gemini-3.8-flash");
        println!("{availability:?}");
        assert_eq!(availability.claude_model, "claude-haiku-4-5");
        assert_eq!(availability.claude_model, cli::DEFAULT_CLAUDE_MODEL);
        assert_eq!(availability.openai_model, "");
        assert_eq!(availability.gemini_model, "gemini-3.8-flash");
        assert!(!availability.gemini_fallback_model.is_empty());
    }

    #[test]
    fn captures_go_through_the_liveness_check() {
        let needle = concat!("capture_window_png", "(");
        let (files, count) = crate::util::count_in_sources(needle, Some("overlay_capture.rs"));
        println!("scanned {files} files, found {count} occurrence(s) of {needle}");
        assert!(files > 0, "the source scan found no files");
        assert_eq!(count, 0, "capture only through capture_live_window_png");
    }
}
