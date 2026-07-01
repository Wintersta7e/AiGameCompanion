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

use crate::overlay::{GameInfo, OverlayState};

pub use cli::{detect_cli, ensure_codex_workdir, CliConfig};

/// Backstop timeout for a single request, covering a hung CLI that never closes
/// stdout. Gemini has its own (shorter) HTTP timeout, so this is the CLI ceiling.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(3);

/// The provider a request targets. Serialized lowercase to match the overlay UI
/// (`"gemini"` / `"claude"` / `"openai"`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Gemini,
    Claude,
    Openai,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gemini => "gemini",
            Self::Claude => "claude",
            Self::Openai => "openai",
        }
    }
}

/// One chat turn sent from the overlay UI.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// A streamed event delivered to the overlay window over the request's Channel.
/// `kind` is `"chunk"` | `"done"` | `"error"`; every event carries the request +
/// conversation IDs so the UI can ignore output from superseded requests.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SageEvent {
    kind: &'static str,
    request_id: u64,
    conversation_id: u64,
    #[serde(skip_serializing_if = "String::is_empty")]
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

impl SageEvent {
    fn chunk(request_id: u64, conversation_id: u64, text: String) -> Self {
        Self {
            kind: "chunk",
            request_id,
            conversation_id,
            text,
            message: None,
        }
    }

    fn done(request_id: u64, conversation_id: u64) -> Self {
        Self {
            kind: "done",
            request_id,
            conversation_id,
            text: String::new(),
            message: None,
        }
    }

    fn error(request_id: u64, conversation_id: u64, message: String) -> Self {
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
pub struct ProviderAvailability {
    pub gemini: bool,
    pub claude: bool,
    pub openai: bool,
    /// Where each CLI was detected ("PATH" / "WSL" / "").
    pub claude_where: String,
    pub openai_where: String,
}

/// Parameters of a chat request, deserialized from the `ask_sage` command.
pub struct RequestParams {
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
pub struct AiState {
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
    pub fn set_cli(&self, cfg: CliConfig) {
        *self.cli.lock() = cfg;
    }

    /// Report which providers can currently serve a request. Gemini depends on a
    /// readable config with a key + model; Claude / Codex on a detected CLI.
    pub fn availability(&self) -> ProviderAvailability {
        let cli = self.cli.lock();
        ProviderAvailability {
            gemini: gemini::load_config().is_ok(),
            claude: cli.claude.is_available(),
            openai: cli.codex.is_available(),
            claude_where: cli.claude.location().to_owned(),
            openai_where: cli.codex.location().to_owned(),
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
    pub fn cancel(&self, request_id: u64) {
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
pub fn spawn_request(app: &AppHandle, params: RequestParams, channel: Channel<SageEvent>) {
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
    let (system_prompt, game_hwnd) = {
        let overlay = app.state::<OverlayState>();
        let game = overlay.game.lock();
        (
            build_system_prompt(game.as_ref()),
            game.as_ref().map(|g| g.hwnd),
        )
    };
    let cli_cfg = app.state::<AiState>().cli.lock().clone();

    // Screenshots are skipped for OpenAI (Codex `--image` is broken upstream).
    let screenshot = if attach_screenshot && provider != Provider::Openai {
        capture_base64(game_hwnd).await
    } else {
        None
    };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let chan_stream = channel.clone();

    let producer = async move {
        let on_chunk = move |text: String| {
            tx.send(text)
                .map_err(|_| "overlay window closed".to_owned())
        };
        match provider {
            Provider::Gemini => {
                let cfg = gemini::load_config()?;
                gemini::stream(
                    &messages,
                    &system_prompt,
                    screenshot,
                    &cfg.model,
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
            let _ = chan_stream.send(SageEvent::chunk(request_id, conversation_id, batch));
        }
    };

    // Backstop timeout: a hung CLI (no output, never closing stdout) would
    // otherwise leave the join pending forever, stranding the UI on "Streaming".
    // On elapse the futures drop -- killing any CLI child via kill_on_drop.
    let streamed = async { tokio::join!(producer, consumer).0 };
    let result = match tokio::time::timeout(REQUEST_TIMEOUT, streamed).await {
        Ok(result) => result,
        Err(_) => Err("Request timed out. Try again.".to_owned()),
    };

    let event = match result {
        Ok(()) => SageEvent::done(request_id, conversation_id),
        Err(message) => SageEvent::error(request_id, conversation_id, message),
    };
    let _ = channel.send(event);

    app.state::<AiState>().clear_if(request_id);
}

/// Capture the stored game window and base64-encode it as PNG for an AI request.
/// Capture failures are non-fatal: the request proceeds without the screenshot.
async fn capture_base64(game_hwnd: Option<i64>) -> Option<String> {
    let hwnd = game_hwnd?;
    match tokio::task::spawn_blocking(move || crate::overlay_capture::capture_window_png(hwnd))
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

/// The Sage persona prompt, optionally grounded with the detected game name.
fn build_system_prompt(game: Option<&GameInfo>) -> String {
    let mut prompt = default_system_prompt();
    if let Some(game) = game {
        let name = if game.title.trim().is_empty() {
            std::path::Path::new(&game.exe)
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            game.title.trim().to_owned()
        };
        if !name.is_empty() {
            let _ = write!(prompt, " The player is currently playing {name}.");
        }
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

/// Capture the game window and translate any foreign text in it to English via
/// Gemini. A one-shot call, independent of the chat request slot.
pub async fn translate_capture(game_hwnd: i64) -> Result<String, String> {
    let png =
        tokio::task::spawn_blocking(move || crate::overlay_capture::capture_window_png(game_hwnd))
            .await
            .map_err(|error| format!("capture task failed: {error}"))??;
    let screenshot = base64::engine::general_purpose::STANDARD.encode(png);
    let cfg = gemini::load_config()?;
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
        &cfg.model,
        &cfg.api_key,
        |chunk| {
            out.push_str(&chunk);
            Ok(())
        },
    )
    .await?;
    Ok(out.trim().to_owned())
}
