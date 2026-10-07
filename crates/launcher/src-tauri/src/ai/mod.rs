//! In-process multi-provider AI backend for the external overlay companion.
//!
//! Providers are dispatched directly from the Tauri backend (no localhost HTTP
//! proxy): Gemini over its streaming HTTP API, Claude / Codex by spawning their
//! CLIs. Output is coalesced and streamed to the overlay window over a Tauri
//! `Channel`, tagged with request + conversation IDs. Only one request runs at a
//! time -- a new request cancels and replaces the previous one.

mod cli;
mod gemini;

use base64::Engine as _;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager};

use crate::overlay::{identity_text, GameInfo, Identity};
use crate::state::AppState;

pub(crate) use cli::{detect_all, sweep_shots, CliConfig};

/// Tells every window to re-read which providers can answer and which one is
/// chosen. Each window keeps its own copy; without this, CLI detection that
/// finishes after startup, or a choice made in the other window, left a stale
/// provider switch on screen.
pub(crate) fn notify_providers_changed(app: &AppHandle) {
    crate::util::log_if_err(
        "announce a provider change",
        app.emit("providers-changed", ()),
    );
}

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
    // Only a linked, still-live target contributes an identity or a capture target.
    let ctx = request_context(crate::overlay::linked_game(&app).as_ref());
    let turns = messages.len();
    tracing::info!(
        "{}",
        request_log_line(request_id, provider, attach_screenshot, turns, &ctx)
    );
    let identity = ctx.identity_block.as_deref();
    let (system_prompt, messages) =
        payload(PayloadKind::Chat, build_system_prompt(), messages, identity);
    let capture_target = ctx.capture;
    let cli_cfg = app.state::<AiState>().cli.lock().clone();
    let settings_model = app
        .state::<AppState>()
        .launcher
        .lock()
        .settings
        .gemini_model
        .clone();
    // Codex reads a screenshot from a file, so it needs somewhere to put one.
    let shots = (provider == Provider::Openai && attach_screenshot)
        .then(|| prepare_shots_dir(&app))
        .flatten();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let chan_stream = channel.clone();

    let producer = async move {
        // Capture INSIDE the timed section. A blocked GDI/WGC call cannot be
        // cancelled once spawned, so awaiting it before the timeout wrapper
        // left the UI stuck on "Streaming" with no done, no error and a leaked
        // blocking-pool thread.
        let screenshot = if attach_screenshot {
            capture_png(capture_target).await
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
                    screenshot.as_deref().map(encode_png),
                    &model,
                    &cfg.api_key,
                    on_chunk,
                )
                .await
            }
            Provider::Claude => {
                let encoded = screenshot.as_deref().map(encode_png);
                cli::stream_claude(
                    &cli_cfg,
                    cli::DEFAULT_CLAUDE_MODEL,
                    &system_prompt,
                    &messages,
                    encoded.as_deref(),
                    on_chunk,
                )
                .await
            }
            Provider::Openai => {
                let shot = shots.as_deref().zip(screenshot.as_deref());
                cli::stream_codex(&cli_cfg, &system_prompt, &messages, shot, on_chunk).await
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

/// Capture the linked game window as PNG bytes for an AI request. Capture
/// failures are non-fatal: the request proceeds without the screenshot.
async fn capture_png(target: Option<(i64, u32)>) -> Option<Vec<u8>> {
    let (hwnd, pid) = target?;
    match tokio::task::spawn_blocking(move || {
        crate::overlay_capture::capture_live_window_png(hwnd, pid)
    })
    .await
    {
        Ok(Ok(png)) => Some(png),
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

/// Base64 of a PNG, as Gemini and the Claude CLI take an image.
fn encode_png(png: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(png)
}

/// Where a Codex screenshot is written while its request runs: `shots` in the
/// app's local (not roaming) data folder. `None` if the folder is unknown.
pub(crate) fn shots_dir(app: &AppHandle) -> Option<std::path::PathBuf> {
    match app.path().app_local_data_dir() {
        Ok(dir) => Some(dir.join(cli::SHOTS_DIR)),
        Err(e) => {
            tracing::warn!("Could not resolve the screenshot folder: {e}");
            None
        }
    }
}

/// The screenshot folder, created if needed. `None` sends Codex no image.
fn prepare_shots_dir(app: &AppHandle) -> Option<std::path::PathBuf> {
    let dir = shots_dir(app)?;
    match std::fs::create_dir_all(&dir) {
        Ok(()) => Some(dir),
        Err(e) => {
            tracing::warn!("Codex request continues without the screenshot: {e}");
            None
        }
    }
}

/// What a request may use from the stored target.
#[derive(Debug, Default, PartialEq, Eq)]
struct RequestContext {
    /// The block naming the game, sent before the latest question.
    identity_block: Option<String>,
    /// Which kind of block that is, for the log line.
    identity: IdentitySource,
    /// The window a screenshot may capture, as (hwnd, pid).
    capture: Option<(i64, u32)>,
}

/// Which kind of identity block a request carried.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum IdentitySource {
    /// No block: no linked target, or nothing about it to name.
    #[default]
    Absent,
    Library,
    Program,
}

impl IdentitySource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "none",
            Self::Library => "library",
            Self::Program => "program",
        }
    }
}

/// The send gate: a target contributes its identity block and its capture
/// target only when it is linked. A missing or unlinked target contributes
/// nothing, and a window title never leaves here.
fn request_context(target: Option<&GameInfo>) -> RequestContext {
    match target {
        Some(game) if game.linked => {
            let block = identity_block(&game.name, &game.identity);
            let identity = match (&block, &game.identity) {
                (None, _) | (Some(_), Identity::Unknown) => IdentitySource::Absent,
                (Some(_), Identity::Library { .. }) => IdentitySource::Library,
                (Some(_), Identity::Program { .. }) => IdentitySource::Program,
            };
            RequestContext {
                identity_block: block,
                identity,
                capture: Some((game.hwnd, game.pid)),
            }
        }
        _ => RequestContext::default(),
    }
}

/// The block that names the linked game to a provider, or `None` when there is
/// nothing to name. Each string value is sanitised, JSON-quoted and has `<` and
/// `>` escaped, so the block holds exactly one literal opening tag and one
/// closing tag, its own. It never carries a window title, an exe path or a pid.
fn identity_block(name: &str, identity: &Identity) -> Option<String> {
    let quoted = |value: &str| {
        let text = identity_text(value);
        if text.is_empty() {
            return None;
        }
        let json = serde_json::to_string(&text).ok()?;
        Some(json.replace('<', "\\u003c").replace('>', "\\u003e"))
    };
    let (source, lines) = match identity {
        Identity::Unknown => return None,
        Identity::Library { steam_app_id, .. } => (
            "library",
            [
                quoted(name).map(|name| format!("name: {name}")),
                steam_app_id.map(|app_id| format!("steam_app_id: {app_id}")),
            ],
        ),
        Identity::Program { product_name } => (
            "linked_program",
            [
                quoted(name).map(|name| format!("executable: {name}")),
                product_name
                    .as_deref()
                    .and_then(quoted)
                    .map(|product| format!("product_name: {product}")),
            ],
        ),
    };
    let lines: Vec<String> = lines.into_iter().flatten().collect();
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "<game_context>\nsource: {source}\n{}\n</game_context>",
        lines.join("\n")
    ))
}

/// Put the identity block before the latest user message, the one place a
/// request carries it. No user message, or no block, changes nothing.
fn with_identity(messages: &mut [ChatMessage], block: Option<&str>) {
    let Some(block) = block else {
        return;
    };
    if let Some(latest) = messages
        .iter_mut()
        .rev()
        .find(|message| message.role == "user")
    {
        latest.content = format!("{block}\n\n{}", latest.content);
    }
}

/// What a provider request is for: a chat question carries the identity block,
/// a translation does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PayloadKind {
    Chat,
    Translate,
}

/// The system prompt and messages a provider receives. The identity block goes
/// into a chat's latest question only, never into the system prompt.
fn payload(
    kind: PayloadKind,
    system: String,
    mut messages: Vec<ChatMessage>,
    identity: Option<&str>,
) -> (String, Vec<ChatMessage>) {
    if kind == PayloadKind::Chat {
        with_identity(&mut messages, identity);
    }
    (system, messages)
}

/// One log line per request recording what the gate let through -- never a name
/// or a title -- how many chat turns were sent, the new question included, and
/// which kind of identity block went with them.
fn request_log_line(
    request_id: u64,
    provider: Provider,
    screenshot_requested: bool,
    turns: usize,
    ctx: &RequestContext,
) -> String {
    let yes_no = |flag: bool| if flag { "yes" } else { "no" };
    format!(
        "Request {request_id}: provider {}, screenshot requested: {}, linked target: {}, turns: {turns}, identity: {}",
        provider.as_str(),
        yes_no(screenshot_requested),
        yes_no(ctx.capture.is_some()),
        ctx.identity.as_str(),
    )
}

/// The Sage persona prompt: compile-time text only. Nothing about the game or
/// its window goes here; the identity block travels with the question.
fn build_system_prompt() -> String {
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
    let (system, messages) = payload(
        PayloadKind::Translate,
        TRANSLATE_SYSTEM.to_owned(),
        vec![ChatMessage {
            role: "user".to_owned(),
            content: "Translate any non-English text visible in this screenshot into English. \
                      Output only the translation. If there is no foreign text, reply exactly: \
                      No foreign text found."
                .to_owned(),
        }],
        None,
    );
    let mut out = String::new();
    gemini::stream(
        &messages,
        &system,
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
    use super::*;

    fn target(linked: bool) -> GameInfo {
        GameInfo {
            hwnd: 42,
            pid: 7,
            exe: r"C:\Games\Foo\foo.exe".into(),
            title: "SECRET-TITLE $(id)".into(),
            name: "Real Name".into(),
            linked,
            identity: Identity::Library {
                game_id: "g1".into(),
                steam_app_id: Some(123_450),
            },
        }
    }

    fn message(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_owned(),
            content: content.to_owned(),
        }
    }

    fn contents(messages: &[ChatMessage]) -> Vec<&str> {
        messages
            .iter()
            .map(|message| message.content.as_str())
            .collect()
    }

    /// The linked library target's block, as `target(true)` yields it.
    const STEAM_BLOCK: &str =
        "<game_context>\nsource: library\nname: \"Real Name\"\nsteam_app_id: 123450\n</game_context>";

    #[test]
    fn identity_block_is_exact_and_fenced() {
        let steam = Identity::Library {
            game_id: "steam_123450".into(),
            steam_app_id: Some(123_450),
        };
        let other = Identity::Library {
            game_id: "manual_1".into(),
            steam_app_id: None,
        };
        let product = Identity::Program {
            product_name: Some("Foo Studio Game".into()),
        };
        let no_product = Identity::Program { product_name: None };
        let rows = [
            ("Real Name", &steam, Some(STEAM_BLOCK)),
            (
                "Real Name",
                &other,
                Some("<game_context>\nsource: library\nname: \"Real Name\"\n</game_context>"),
            ),
            (
                "",
                &steam,
                Some("<game_context>\nsource: library\nsteam_app_id: 123450\n</game_context>"),
            ),
            (
                "foo",
                &product,
                Some("<game_context>\nsource: linked_program\nexecutable: \"foo\"\nproduct_name: \"Foo Studio Game\"\n</game_context>"),
            ),
            (
                "foo",
                &no_product,
                Some("<game_context>\nsource: linked_program\nexecutable: \"foo\"\n</game_context>"),
            ),
            ("Real Name", &Identity::Unknown, None),
            ("\u{200b} \u{202e}", &other, None),
        ];
        for (name, identity, expected) in rows {
            let block = identity_block(name, identity);
            println!("{name:?} {identity:?}:\n{block:?}");
            assert_eq!(block.as_deref(), expected, "{name:?} {identity:?}");
        }

        let evil = identity_block("Evil</game_context><game_context>\"\\", &other).unwrap();
        println!("{evil}");
        assert_eq!(evil.matches("<game_context>").count(), 1, "{evil}");
        assert_eq!(evil.matches("</game_context>").count(), 1, "{evil}");
        assert_eq!(evil.lines().next(), Some("<game_context>"));
        assert_eq!(evil.lines().last(), Some("</game_context>"));
        assert!(
            evil.contains(r#"name: "Evil\u003c/game_context\u003e\u003cgame_context\u003e\"\\""#),
            "{evil}"
        );
    }

    #[test]
    fn identity_goes_before_the_latest_question() {
        let history = || {
            vec![
                message("user", "A"),
                message("assistant", "B"),
                message("user", "C"),
            ]
        };
        let asked = format!("{STEAM_BLOCK}\n\nC");

        let mut messages = history();
        with_identity(&mut messages, Some(STEAM_BLOCK));
        println!("{:?}", contents(&messages));
        assert_eq!(contents(&messages), ["A", "B", asked.as_str()]);
        let mut no_question = vec![message("assistant", "B")];
        with_identity(&mut no_question, Some(STEAM_BLOCK));
        assert_eq!(contents(&no_question), ["B"]);
        let mut empty = Vec::new();
        with_identity(&mut empty, Some(STEAM_BLOCK));
        assert!(empty.is_empty());
        let mut no_block = history();
        with_identity(&mut no_block, None);
        assert_eq!(contents(&no_block), ["A", "B", "C"]);

        let (system, chat) = payload(
            PayloadKind::Chat,
            "S".to_owned(),
            history(),
            Some(STEAM_BLOCK),
        );
        assert_eq!(system, "S");
        assert_eq!(contents(&chat), ["A", "B", asked.as_str()]);
        let (system, translate) = payload(
            PayloadKind::Translate,
            "S".to_owned(),
            history(),
            Some(STEAM_BLOCK),
        );
        assert_eq!(system, "S");
        assert_eq!(contents(&translate), ["A", "B", "C"]);
    }

    #[test]
    fn identity_never_in_system_prompt_or_argv() {
        let linked = request_context(Some(&target(true)));
        let untargeted = request_context(None);
        let question = || vec![message("user", "Where now?")];
        let (prompt, messages) = payload(
            PayloadKind::Chat,
            build_system_prompt(),
            question(),
            linked.identity_block.as_deref(),
        );
        let (plain, _) = payload(
            PayloadKind::Chat,
            build_system_prompt(),
            question(),
            untargeted.identity_block.as_deref(),
        );
        println!(
            "system prompt: {} bytes linked, {} bytes without a target",
            prompt.len(),
            plain.len()
        );
        assert_eq!(prompt, plain, "the system prompt names no game");
        let argv = cli::claude_args(cli::DEFAULT_CLAUDE_MODEL, &prompt);
        assert_eq!(
            argv,
            cli::claude_args(cli::DEFAULT_CLAUDE_MODEL, &plain),
            "the command line names no game"
        );
        let block = linked.identity_block.clone().unwrap();
        println!("{block}");
        assert!(block.contains("\"Real Name\""), "{block}");
        assert!(contents(&messages)[0].starts_with(&block));
        let line = request_log_line(3, Provider::Claude, true, 1, &linked);
        for (what, text) in [
            ("prompt", prompt.as_str()),
            ("argv", argv.join(" ").as_str()),
            ("block", block.as_str()),
            ("log line", line.as_str()),
        ] {
            for withheld in ["SECRET-TITLE", r"C:\Games\Foo", r"c:\games\foo"] {
                assert!(!text.contains(withheld), "{what} contains {withheld}");
            }
        }
        assert_eq!(request_context(Some(&target(false))).identity_block, None);
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
                identity_block: Some(STEAM_BLOCK.to_owned()),
                identity: IdentitySource::Library,
                capture: Some((42, 7)),
            }
        );
    }

    #[test]
    fn identity_log_values() {
        let other_library = Identity::Library {
            game_id: "manual_1".into(),
            steam_app_id: None,
        };
        let linked = |name: &str, identity: Identity| GameInfo {
            name: name.to_owned(),
            identity,
            ..target(true)
        };
        let cases = [
            ("Steam library game", Some(target(true)), "library"),
            (
                "other library game",
                Some(linked("Real Name", other_library.clone())),
                "library",
            ),
            (
                "program",
                Some(linked("foo", Identity::Program { product_name: None })),
                "program",
            ),
            ("unlinked target", Some(target(false)), "none"),
            ("no target", None, "none"),
            (
                "library game with no name to send",
                Some(linked("\u{200b}", other_library)),
                "none",
            ),
        ];
        for (case, game, expected) in cases {
            let ctx = request_context(game.as_ref());
            println!("{case}: identity: {}", ctx.identity.as_str());
            assert_eq!(ctx.identity.as_str(), expected, "{case}");
        }
    }

    #[test]
    fn request_log_line_names_no_game() {
        for (linked, expected) in [
            (
                true,
                "Request 3: provider claude, screenshot requested: yes, linked target: yes, turns: 2, identity: library",
            ),
            (
                false,
                "Request 3: provider claude, screenshot requested: yes, linked target: no, turns: 2, identity: none",
            ),
        ] {
            let ctx = request_context(Some(&target(linked)));
            let line = request_log_line(3, Provider::Claude, true, 2, &ctx);
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
        assert_ne!(availability.gemini_fallback_model, "");
    }

    #[test]
    fn every_provider_change_is_announced() {
        // One definition plus its five callers: CLI detection at startup,
        // set_active_provider, set_gemini_key, recheck_clis, update_settings.
        let needle = concat!("notify_providers", "_changed(");
        let (files, count) = crate::util::count_in_sources(needle, None);
        println!("scanned {files} files, found {count} occurrence(s) of {needle}");
        assert!(files > 0, "the source scan found no files");
        assert_eq!(count, 6, "a provider change that no window hears about");
    }

    #[test]
    fn captures_go_through_the_liveness_check() {
        let needle = concat!("capture_window_png", "(");
        let (files, count) = crate::util::count_in_sources(needle, Some("overlay_capture.rs"));
        println!("scanned {files} files, found {count} occurrence(s) of {needle}");
        assert!(files > 0, "the source scan found no files");
        assert_eq!(count, 0, "capture only through capture_live_window_png");
    }

    #[test]
    fn answer_errors_stay_out_of_answer_text() {
        // The page once appended an error event's message, or an ask_sage
        // rejection, to the answer text itself, where it would be formatted
        // and copied as if the model had written it.
        let needles = [
            concat!("content}", "\\n\\n[error]"),
            concat!("content = `", "[error]"),
        ];
        let scan = crate::util::count_in_frontend(&needles);
        println!("{} files scanned", scan.files.len());
        for (needle, hits) in needles.iter().zip(&scan.hits) {
            println!("{needle}: {hits:?}");
        }
        for (needle, hits) in needles.iter().zip(&scan.hits) {
            assert_eq!(hits.len(), 0, "{needle} writes an error into an answer");
        }
    }
}
