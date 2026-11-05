//! In-process multi-provider AI backend for the external overlay companion.
//!
//! Providers are dispatched directly from the Tauri backend (no localhost HTTP
//! proxy): Gemini over its streaming HTTP API, Claude / Codex by spawning their
//! CLIs. Output is coalesced and streamed to the overlay window over a Tauri
//! `Channel`, tagged with request + conversation IDs. Chat requests run in the
//! chat lane, where a new question cancels and replaces the previous one and a
//! recap is refused while one runs. Translation has a separate lane, so
//! neither cancels the other. A chat request's history comes from the game's
//! saved chat, and its answer is saved before its final event.

mod cli;
mod gemini;
mod prompt;
pub(crate) mod search;

use base64::Engine as _;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager};

use crate::chat_store::{ChatStore, SaveState, SavedExchange};
use crate::overlay::{identity_text, GameInfo, Identity};
use crate::shots::{ShotSlot, ShotStore};
use crate::state::AppState;
use search::SearchReport;

pub(crate) use cli::{detect_all, sweep_shots, CliConfig};

/// Tells every window to re-read which providers can answer and which one is
/// chosen. Each window keeps its own copy; without this, CLI detection that
/// finishes after startup, or a choice made in the other window, left a stale
/// provider switch on screen.
pub(crate) fn notify_providers_changed<R: tauri::Runtime>(app: &AppHandle<R>) {
    crate::util::log_if_err(
        "announce a provider change",
        app.emit("providers-changed", ()),
    );
}

/// Backstop timeout for a single request, covering a hung CLI that never closes
/// stdout. Gemini has its own (shorter) HTTP timeout, so this is the CLI ceiling.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(3);
/// How long quitting waits for a running request to end before it deletes the
/// screenshot files and exits anyway.
const EXIT_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

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

    /// Whether a request to this provider may carry a screenshot. Codex takes
    /// one through a file, the others inline.
    pub(crate) const fn takes_screenshot(self) -> bool {
        match self {
            Self::Gemini | Self::Claude | Self::Openai => true,
        }
    }

    /// The name the overlay shows for this provider.
    const fn label(self) -> &'static str {
        match self {
            Self::Gemini => "Gemini",
            Self::Claude => "Claude",
            Self::Openai => "OpenAI",
        }
    }
}

/// One chat turn sent to a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// What every event of a request carries: its ids, and the number of the chat
/// on screen it was admitted for.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EventHead {
    pub request_id: u64,
    pub conversation_id: u64,
    pub display_generation: u64,
}

/// A streamed event delivered to the overlay window over the request's Channel.
/// `kind` is `"chunk"` | `"done"` | `"error"`; every event carries its
/// request's `EventHead`, so the UI can ignore output from superseded requests
/// and from another chat. A chat `done` says whether the answer was saved.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SageEvent {
    kind: &'static str,
    request_id: u64,
    conversation_id: u64,
    display_generation: u64,
    #[serde(skip_serializing_if = "String::is_empty")]
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    save: Option<SaveState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    search: Option<SearchReport>,
}

impl SageEvent {
    const fn of(kind: &'static str, head: EventHead) -> Self {
        Self {
            kind,
            request_id: head.request_id,
            conversation_id: head.conversation_id,
            display_generation: head.display_generation,
            text: String::new(),
            message: None,
            save: None,
            search: None,
        }
    }

    fn chunk(head: EventHead, text: String) -> Self {
        Self {
            text,
            ..Self::of("chunk", head)
        }
    }

    fn done(head: EventHead, save: Option<SaveState>, search: Option<SearchReport>) -> Self {
        Self {
            save,
            search,
            ..Self::of("done", head)
        }
    }

    fn error(head: EventHead, message: String) -> Self {
        Self {
            message: Some(message),
            ..Self::of("error", head)
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

/// A chat request as the overlay sends it.
pub(crate) struct ChatArgs {
    pub request_id: u64,
    pub conversation_id: u64,
    pub provider: Provider,
    pub question: String,
    /// The held chat screenshot the page showed, to send with the question.
    pub shot_id: Option<u64>,
    /// Whether the answer may search the web.
    pub search: bool,
    /// Hints first, as the page showed it when the question was sent.
    pub hints: bool,
    /// Asks where the player left off, from the chat so far.
    pub recap: bool,
}

/// The flags an admitted request runs with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Effective {
    pub search: bool,
    pub hints: bool,
    pub shot_id: Option<u64>,
}

/// `search` as the search gate allowed it, hints never for a recap, and the
/// screenshot id exactly as sent.
pub(crate) const fn effective_flags(
    search: bool,
    hints: bool,
    recap: bool,
    shot_id: Option<u64>,
) -> Effective {
    Effective {
        search,
        hints: hints && !recap,
        shot_id,
    }
}

/// An admitted request and everything it runs on.
pub(crate) enum RequestKind {
    Chat {
        target: GameInfo,
        key: String,
        /// The chat's clear generation at admission: the answer saves only under it.
        clear_generation: u64,
        /// The saved exchanges sent again before the question.
        tail: Vec<ChatMessage>,
        question: String,
        png: Option<Vec<u8>>,
        recap: bool,
        effective: Effective,
    },
}

/// Why a chat request was refused before it started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefusalCode {
    NoLinkedGame,
    WindowClosed,
    GameChanged,
    NothingToRecap,
    SearchUnavailable,
    ScreenshotUnavailable,
    ChatBusy,
}

impl RefusalCode {
    /// How the log line names the refusal.
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::NoLinkedGame => "no linked game",
            Self::WindowClosed => "linked window closed",
            Self::GameChanged => "game changed",
            Self::NothingToRecap => "nothing to recap",
            Self::SearchUnavailable => "search unavailable",
            Self::ScreenshotUnavailable => "screenshot unavailable",
            Self::ChatBusy => "chat busy",
        }
    }
}

/// A refused request: why, and the text the overlay shows.
#[derive(Debug)]
pub(crate) struct Refusal {
    pub code: RefusalCode,
    pub message: String,
}

const GAME_CHANGED: &str = "The game changed -- ask again.";
const NOTHING_TO_RECAP: &str = "There is nothing in this chat to recap yet.";
const CHAT_BUSY: &str = "Wait for the current answer to finish.";
const RECAP_NO_SHOT: &str =
    "A recap can't include a screenshot. Remove the screenshot to ask where you were.";

/// An admitted request: the head of its events, what it runs on, and what its
/// log line records.
pub(crate) struct Admitted {
    head: EventHead,
    kind: RequestKind,
    line: RequestLine,
}

/// What admitting and installing a chat request did, with its one log line;
/// a refusal also carries the one event the overlay receives.
enum Admission {
    Refused { log: String, event: SageEvent },
    Started { log: String },
}

/// What the per-request log line records: never a name, a title, a key or
/// the question.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is one yes/no field of the log line"
)]
struct RequestLine {
    request_id: u64,
    provider: Provider,
    screenshot: bool,
    /// Chat turns sent, the new question included.
    turns: usize,
    linked: bool,
    identity: IdentitySource,
    search: bool,
    hints: bool,
    recap: bool,
}

/// What a finished answer is saved with, besides its text and time.
struct ExchangeDraft {
    question: String,
    provider: Provider,
    model: String,
    screenshot: bool,
}

/// A lane's in-flight request. Aborting `handle` cancels the request and --
/// because CLI children are spawned with `kill_on_drop` -- kills any child.
struct Active {
    request_id: u64,
    handle: tauri::async_runtime::JoinHandle<()>,
}

/// A lane refused a request because it already runs one.
#[derive(Debug)]
struct LaneBusy;

/// Install a request in `slot`, under one hold of its lock. With
/// `refuse_if_busy`, a busy lane refuses without calling `spawn`; otherwise
/// the new request replaces and cancels the running one. `spawn` only
/// schedules a future, so it may run under the lock.
fn install_in(
    slot: &Mutex<Option<Active>>,
    request_id: u64,
    refuse_if_busy: bool,
    spawn: impl FnOnce() -> tauri::async_runtime::JoinHandle<()>,
) -> Result<(), LaneBusy> {
    let previous = {
        let mut guard = slot.lock();
        if refuse_if_busy && guard.is_some() {
            return Err(LaneBusy);
        }
        guard.replace(Active {
            request_id,
            handle: spawn(),
        })
    };
    if let Some(previous) = previous {
        previous.handle.abort();
    }
    Ok(())
}

/// Empty `slot` once its request finishes, unless a newer request already
/// replaced it.
fn release_in(slot: &Mutex<Option<Active>>, request_id: u64) {
    slot.lock()
        .take_if(|active| active.request_id == request_id);
}

/// What Gemini needs from the key store and the legacy config file: whether a
/// key is stored, and the file's model (`""` when it names none).
#[derive(Debug, Clone, Default)]
pub(crate) struct GeminiPresence {
    pub key: bool,
    pub file_model: String,
}

/// Read the key store and the legacy config file for Gemini. Slow (the OS
/// credential store, a file), so callers cache the result in `AiState`.
pub(crate) fn gemini_presence() -> GeminiPresence {
    GeminiPresence {
        key: gemini::load_config().is_ok(),
        file_model: gemini::file_model(),
    }
}

/// Backend AI state: cached CLI and Gemini availability plus the two request
/// lanes, chat and translation.
pub(crate) struct AiState {
    cli: Mutex<CliConfig>,
    gemini: Mutex<GeminiPresence>,
    chat: Mutex<Option<Active>>,
    translate: Mutex<Option<Active>>,
}

impl Default for AiState {
    fn default() -> Self {
        Self {
            cli: Mutex::new(CliConfig::default()),
            gemini: Mutex::new(GeminiPresence::default()),
            chat: Mutex::new(None),
            translate: Mutex::new(None),
        }
    }
}

impl AiState {
    /// Store the CLI availability detected on the background startup thread.
    pub(crate) fn set_cli(&self, cfg: CliConfig) {
        *self.cli.lock() = cfg;
    }

    /// Store what `gemini_presence` read.
    pub(crate) fn set_gemini(&self, presence: GeminiPresence) {
        *self.gemini.lock() = presence;
    }

    /// Report which providers can currently serve a request, and the Gemini model
    /// a request would name given `settings_model` (the Settings choice). Gemini
    /// depends on a stored key; Claude / Codex on a detected CLI. Reads only the
    /// cached state, never the key store or a file.
    pub(crate) fn availability(&self, settings_model: &str) -> ProviderAvailability {
        let presence = self.gemini.lock().clone();
        let cli = self.cli.lock();
        ProviderAvailability {
            gemini: presence.key,
            claude: cli.claude.is_available(),
            openai: cli.codex.is_available(),
            claude_where: cli.claude.location().to_owned(),
            openai_where: cli.codex.location().to_owned(),
            gemini_model: gemini::resolve_model(settings_model, &presence.file_model),
            gemini_fallback_model: gemini::resolve_model("", &presence.file_model),
            claude_model: cli::DEFAULT_CLAUDE_MODEL.to_owned(),
            openai_model: String::new(),
        }
    }

    /// Cancel `request_id` in whichever lane runs it (Stop button).
    pub(crate) fn cancel(&self, request_id: u64) {
        for slot in [&self.chat, &self.translate] {
            let mut guard = slot.lock();
            if let Some(active) = guard.take_if(|active| active.request_id == request_id) {
                active.handle.abort();
                return;
            }
        }
    }

    /// End the running requests of both lanes, then delete every Codex
    /// screenshot file, before the launcher exits. `std::process::exit` runs no
    /// destructors: without this, quitting while an answer streamed left the
    /// CLI answering on the user's account and its screenshot on disk until the
    /// next start.
    pub(crate) fn stop_for_exit(&self, shots_dir: Option<&std::path::Path>) {
        let running: Vec<Active> = [&self.chat, &self.translate]
            .into_iter()
            .filter_map(|slot| slot.lock().take())
            .collect();
        if !running.is_empty() {
            let count = running.len();
            for active in &running {
                active.handle.abort();
            }
            // Each request's future is dropped on a runtime thread; waiting for
            // that is what kills the CLI and deletes its screenshot.
            let ended = tauri::async_runtime::block_on(async move {
                let deadline = tokio::time::Instant::now() + EXIT_WAIT;
                let mut ended = true;
                for active in running {
                    ended &= tokio::time::timeout_at(deadline, active.handle)
                        .await
                        .is_ok();
                }
                ended
            });
            if ended {
                tracing::info!("Stopped {count} running request(s) before exit");
            } else {
                tracing::warn!(
                    "A running request did not stop within {} s of exit",
                    EXIT_WAIT.as_secs()
                );
            }
        }
        if let Some(dir) = shots_dir {
            let removed = sweep_shots(dir);
            if removed > 0 {
                tracing::info!("Removed {removed} screenshot file(s) before exit");
            }
        }
    }
}

/// Admit or refuse a chat request, in a fixed order, before any lane changes
/// and without I/O: a linked target, its chat on screen, something to recap,
/// the search gate, then the screenshot -- claimed last, so every refusal
/// leaves it held. `target` is the live linked target, `stored` the target
/// as read before it was checked to be alive.
pub(crate) fn admit_chat(
    args: ChatArgs,
    target: Option<&GameInfo>,
    stored: Option<&GameInfo>,
    store: &ChatStore,
    shots: &ShotStore,
) -> Result<Admitted, Refusal> {
    let refuse = |code, message: &str| Refusal {
        code,
        message: message.to_owned(),
    };
    let Some(target) = target else {
        let message = crate::shots::missing_target(stored);
        let code = if message == crate::shots::WINDOW_CLOSED {
            RefusalCode::WindowClosed
        } else {
            RefusalCode::NoLinkedGame
        };
        return Err(refuse(code, message));
    };
    let Some(key) = crate::overlay::game_key(target) else {
        return Err(refuse(
            RefusalCode::NoLinkedGame,
            crate::overlay::NO_LINKED_GAME,
        ));
    };
    let snapshot = store.snapshot(&key, args.provider);
    if !snapshot.displayed {
        return Err(refuse(RefusalCode::GameChanged, GAME_CHANGED));
    }
    let accepts_image = args.provider.takes_screenshot() && !args.recap;
    if args.recap && snapshot.tail.is_empty() {
        return Err(refuse(RefusalCode::NothingToRecap, NOTHING_TO_RECAP));
    }
    let search =
        search::search_gate(args.provider, args.search, args.recap).map_err(|message| Refusal {
            code: RefusalCode::SearchUnavailable,
            message,
        })?;
    if args.recap && args.shot_id.is_some() {
        return Err(refuse(RefusalCode::ScreenshotUnavailable, RECAP_NO_SHOT));
    }
    let png = shots
        .claim_for_request(
            ShotSlot::Chat,
            args.shot_id,
            crate::shots::capture_target(Some(target)),
            None,
            accepts_image,
        )
        .map_err(|refusal| Refusal {
            code: RefusalCode::ScreenshotUnavailable,
            message: refusal.message(args.provider.label()),
        })?;
    let effective = effective_flags(search, args.hints, args.recap, args.shot_id);
    let ctx = request_context(Some(target));
    let line = RequestLine {
        request_id: args.request_id,
        provider: args.provider,
        screenshot: effective.shot_id.is_some(),
        turns: snapshot.tail.len() + 1,
        linked: ctx.capture.is_some(),
        identity: ctx.identity,
        search: effective.search,
        hints: effective.hints,
        recap: args.recap,
    };
    Ok(Admitted {
        head: EventHead {
            request_id: args.request_id,
            conversation_id: args.conversation_id,
            display_generation: snapshot.display_generation,
        },
        kind: RequestKind::Chat {
            target: target.clone(),
            key,
            clear_generation: snapshot.clear_generation,
            tail: snapshot.tail,
            question: args.question,
            png,
            recap: args.recap,
            effective,
        },
        line,
    })
}

/// Admit a chat request and install it in the chat lane through `spawn`: a
/// question replaces and cancels the running one, a recap is refused while
/// one runs. A refusal changes no lane and spawns nothing; its event carries
/// the store's current generation.
fn admit_and_install(
    args: ChatArgs,
    target: Option<&GameInfo>,
    stored: Option<&GameInfo>,
    ai: &AiState,
    store: &ChatStore,
    shots: &ShotStore,
    spawn: impl FnOnce(EventHead, RequestKind) -> tauri::async_runtime::JoinHandle<()>,
) -> Admission {
    let (request_id, conversation_id) = (args.request_id, args.conversation_id);
    let refused = |code: RefusalCode, message: String| {
        let head = EventHead {
            request_id,
            conversation_id,
            display_generation: store.display_generation(),
        };
        Admission::Refused {
            log: refusal_log_line(request_id, code),
            event: SageEvent::error(head, message),
        }
    };
    let Admitted { head, kind, line } = match admit_chat(args, target, stored, store, shots) {
        Ok(admitted) => admitted,
        Err(refusal) => return refused(refusal.code, refusal.message),
    };
    let recap = matches!(kind, RequestKind::Chat { recap: true, .. });
    match install_in(&ai.chat, request_id, recap, || spawn(head, kind)) {
        Ok(()) => Admission::Started {
            log: request_log_line(&line),
        },
        Err(LaneBusy) => refused(RefusalCode::ChatBusy, CHAT_BUSY.to_owned()),
    }
}

/// Admit or refuse a chat request from the overlay and start it. A refusal is
/// logged and sent on `channel` as its one `error` event. Returns whether the
/// request started, and so whether its screenshot, if any, went with it.
pub(crate) fn ask(app: &AppHandle, args: ChatArgs, channel: &Channel<SageEvent>) -> bool {
    // Read before the liveness check, which clears a closed window.
    let stored = crate::overlay::stored_game(app);
    let target = crate::overlay::linked_game(app);
    // Until setup manages the store no chat is on screen: an empty store refuses.
    let unmanaged = ChatStore::new(None);
    let managed = app.try_state::<ChatStore>();
    let store = managed.as_ref().map_or(&unmanaged, |store| store.inner());
    let provider = args.provider;
    let (worker, events) = (app.clone(), channel.clone());
    let admission = admit_and_install(
        args,
        target.as_ref(),
        stored.as_ref(),
        app.state::<AiState>().inner(),
        store,
        app.state::<ShotStore>().inner(),
        |head, kind| tauri::async_runtime::spawn(run(worker, provider, head, kind, events)),
    );
    match admission {
        Admission::Started { log } => {
            tracing::info!("{log}");
            true
        }
        Admission::Refused { log, event } => {
            tracing::info!("{log}");
            crate::util::log_if_err("send a refused request's event", channel.send(event));
            false
        }
    }
}

/// The model a request to `provider` names, as availability reports it.
fn request_model(provider: Provider, available: &ProviderAvailability) -> String {
    match provider {
        Provider::Gemini => available.gemini_model.clone(),
        Provider::Claude => available.claude_model.clone(),
        Provider::Openai => available.openai_model.clone(),
    }
}

/// Drive one admitted request end to end: build the system prompt and the
/// messages, stream the provider with the claimed screenshot (if any) through
/// a coalescing buffer, leave the lane, save the answer, and only then send
/// the final event -- so an answer shown as finished is already saved.
async fn run(
    app: AppHandle,
    provider: Provider,
    head: EventHead,
    kind: RequestKind,
    channel: Channel<SageEvent>,
) {
    let RequestKind::Chat {
        target,
        key,
        clear_generation,
        tail,
        question,
        png,
        recap,
        effective,
    } = kind;
    let (system_prompt, messages) =
        chat_payload(&target, tail, question.clone(), recap, effective.hints);
    // Read shared state up front so no state guard is held across an await.
    let (cli_cfg, model) = {
        let ai = app.state::<AiState>();
        let settings_model = app
            .state::<AppState>()
            .launcher
            .lock()
            .settings
            .gemini_model
            .clone();
        let model = request_model(provider, &ai.availability(&settings_model));
        let cli = ai.cli.lock().clone();
        (cli, model)
    };
    let draft = ExchangeDraft {
        question,
        provider,
        model: model.clone(),
        screenshot: png.is_some(),
    };
    // Codex reads a screenshot from a file, so it needs somewhere to put one.
    let shots = (provider == Provider::Openai && png.is_some())
        .then(|| prepare_shots_dir(&app))
        .flatten();

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();

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
                    png.as_deref().map(encode_png),
                    &model,
                    &cfg.api_key,
                    on_chunk,
                )
                .await
            }
            Provider::Claude => {
                let encoded = png.as_deref().map(encode_png);
                cli::stream_claude(
                    &cli_cfg,
                    &model,
                    &system_prompt,
                    &messages,
                    encoded.as_deref(),
                    on_chunk,
                )
                .await
            }
            Provider::Openai => {
                let (dir, png) = (shots.as_deref(), png.as_deref());
                cli::stream_codex(&cli_cfg, &system_prompt, &messages, dir, png, on_chunk).await
            }
        }
    };

    let consumer = forward_chunks(rx, channel.clone(), head);

    // Backstop timeout: a hung CLI (no output, never closing stdout) would
    // otherwise leave the join pending forever, stranding the UI on "Streaming".
    // On elapse the futures drop -- killing any CLI child via kill_on_drop.
    let streamed = async {
        match tokio::join!(producer, consumer) {
            (Ok(()), Ok(answer)) => (Ok(()), answer),
            (Err(message), _) | (Ok(()), Err(message)) => (Err(message), String::new()),
        }
    };
    let (result, answer) = tokio::time::timeout(REQUEST_TIMEOUT, streamed)
        .await
        .unwrap_or_else(|_elapsed| {
            (
                Err("Request timed out. Try again.".to_owned()),
                String::new(),
            )
        });

    release_in(&app.state::<AiState>().chat, head.request_id);
    let saved = SaveTo {
        key,
        clear_generation,
        draft,
    };
    let event = finish_on_pool(&app, head, result.map(|()| None), answer, saved).await;
    crate::util::log_if_err("send the final sage event", channel.send(event));
}

/// The system prompt and messages of a chat request: the saved tail, then the
/// question, which alone carries the identity block.
fn chat_payload(
    target: &GameInfo,
    tail: Vec<ChatMessage>,
    question: String,
    recap: bool,
    hints: bool,
) -> (String, Vec<ChatMessage>) {
    let identity = request_context(Some(target)).identity_block;
    let mut messages = tail;
    messages.push(ChatMessage {
        role: "user".to_owned(),
        content: question,
    });
    let system_prompt = prompt::assemble(prompt_kind(recap, hints));
    payload(
        PayloadKind::Chat,
        system_prompt,
        messages,
        identity.as_deref(),
    )
}

/// Where a finished answer is saved: its chat, the chat's clear generation at
/// admission, and what the answer is saved with.
struct SaveTo {
    key: String,
    clear_generation: u64,
    draft: ExchangeDraft,
}

/// The final event of a request, built on the blocking pool so saving the
/// answer may block on file I/O. A failed join saves nothing.
async fn finish_on_pool(
    app: &AppHandle,
    head: EventHead,
    outcome: Result<Option<SearchReport>, String>,
    answer: String,
    to: SaveTo,
) -> SageEvent {
    let fallback = match &outcome {
        Ok(_) => SageEvent::done(head, Some(SaveState::Failed), None),
        Err(message) => SageEvent::error(head, message.clone()),
    };
    let mode = crate::commands::chat::mode_of(app);
    let worker = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        finish_request(head, outcome, &answer, to.draft, Utc::now(), |entry| {
            worker
                .try_state::<ChatStore>()
                .map_or(SaveState::Failed, |store| {
                    store.append_exchange(&to.key, to.clear_generation, entry, mode)
                })
        })
    })
    .await
    .unwrap_or(fallback)
}

/// The final event of a finished request. An answer that arrived in full is
/// saved first through `store` (`None` when it holds no text) and its `done`
/// carries the save status. A failed answer -- an error, a timeout, or one the
/// provider cut short -- saves nothing, so no part of it is sent again.
fn finish_request(
    head: EventHead,
    outcome: Result<Option<SearchReport>, String>,
    answer: &str,
    draft: ExchangeDraft,
    now: DateTime<Utc>,
    store: impl FnOnce(Option<SavedExchange>) -> SaveState,
) -> SageEvent {
    match outcome {
        Ok(search) => {
            let entry = (!answer.trim().is_empty()).then(|| SavedExchange {
                saved_at: now,
                question: draft.question,
                answer: answer.to_owned(),
                provider: draft.provider,
                model: draft.model,
                screenshot: draft.screenshot,
                search: search.clone(),
            });
            let save = store(entry);
            SageEvent::done(head, Some(save), search)
        }
        Err(message) => SageEvent::error(head, message),
    }
}

/// Forward a request's text to the overlay, coalescing bursts: everything
/// queued is drained into a single Channel message so a fast per-token
/// provider (Claude deltas) does not flood the IPC boundary. Returns the text
/// the overlay received, or an error once the overlay is gone.
async fn forward_chunks(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    channel: Channel<SageEvent>,
    head: EventHead,
) -> Result<String, String> {
    let mut answer = String::new();
    while let Some(first) = rx.recv().await {
        let mut batch = first;
        while let Ok(more) = rx.try_recv() {
            batch.push_str(&more);
        }
        answer.push_str(&batch);
        // A send failure means the overlay webview is gone. Swallowing it
        // kept the loop running and the CLI subprocess streaming (and
        // billing) to nobody. Closing `rx` makes the producer's next
        // `tx.send` fail, which ends the request and drops the child.
        if channel.send(SageEvent::chunk(head, batch)).is_err() {
            tracing::info!("Overlay channel closed; ending request {}", head.request_id);
            rx.close();
            return Err("overlay window closed".to_owned());
        }
    }
    Ok(answer)
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

/// The screenshot folder, created if needed. `None` ends a Codex request that
/// carries a screenshot.
fn prepare_shots_dir(app: &AppHandle) -> Option<std::path::PathBuf> {
    let dir = shots_dir(app)?;
    match std::fs::create_dir_all(&dir) {
        Ok(()) => Some(dir),
        Err(e) => {
            tracing::warn!("Could not create the screenshot folder: {e}");
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

/// The system prompt a chat request sends, from the flags it runs with.
const fn prompt_kind(recap: bool, hints: bool) -> prompt::PromptKind {
    if recap {
        prompt::PromptKind::Recap
    } else {
        prompt::PromptKind::Chat { hints }
    }
}

/// One log line per started request recording what the gate let through --
/// never a name, a title or the question.
fn request_log_line(line: &RequestLine) -> String {
    let yes_no = |flag: bool| if flag { "yes" } else { "no" };
    format!(
        "Request {}: provider {}, screenshot requested: {}, linked target: {}, turns: {}, identity: {}, search: {}, hints: {}, recap: {}",
        line.request_id,
        line.provider.as_str(),
        yes_no(line.screenshot),
        yes_no(line.linked),
        line.turns,
        line.identity.as_str(),
        yes_no(line.search),
        yes_no(line.hints),
        yes_no(line.recap),
    )
}

/// The one log line of a refused request.
fn refusal_log_line(request_id: u64, code: RefusalCode) -> String {
    format!("Request {request_id}: refused: {}", code.code())
}

/// Check a Gemini model id chosen in Settings before it is saved.
pub(crate) fn validate_gemini_model(model: &str) -> Result<(), String> {
    gemini::validate_model(model)
}

/// Why a translation is refused before its screenshot is claimed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TranslateBlock {
    /// No linked game to translate.
    NotLinked,
    /// The translating provider cannot be used.
    Unavailable,
    /// The translating provider takes no screenshots.
    NoScreenshots,
}

impl TranslateBlock {
    /// The name its log line gives the refusal.
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::NotLinked => "no linked game",
            Self::Unavailable => "unavailable",
            Self::NoScreenshots => "no screenshots",
        }
    }
}

/// Whether a translation may go ahead: the first failing check, in order.
pub(crate) const fn translate_gate(
    linked: bool,
    available: bool,
    takes_screenshot: bool,
) -> Result<(), TranslateBlock> {
    if !linked {
        Err(TranslateBlock::NotLinked)
    } else if !available {
        Err(TranslateBlock::Unavailable)
    } else if !takes_screenshot {
        Err(TranslateBlock::NoScreenshots)
    } else {
        Ok(())
    }
}

/// Translate any foreign text in the held translate screenshot `shot_id` to
/// English via Gemini. A one-shot call, independent of the chat request slot.
/// The screenshot is claimed first: a refusal sends nothing and keeps it held.
pub(crate) async fn translate_held(app: &AppHandle, shot_id: u64) -> Result<Translation, String> {
    let provider = Provider::Gemini;
    let stored = crate::overlay::stored_game(app);
    let game = crate::overlay::linked_game(app);
    let target = crate::shots::capture_target(game.as_ref());
    let key = gemini::load_config();
    if let Err(block) = translate_gate(target.is_some(), key.is_ok(), provider.takes_screenshot()) {
        tracing::info!("Translate: refused: {}", block.code());
        return Err(match block {
            TranslateBlock::NotLinked => crate::shots::missing_target(stored.as_ref()).to_owned(),
            TranslateBlock::Unavailable => key.err().unwrap_or_default(),
            TranslateBlock::NoScreenshots => format!(
                "{} can't receive screenshots, so it can't translate.",
                provider.label()
            ),
        });
    }
    let cfg = key?;
    let region = crate::region::linked_region(app, game.as_ref());
    let png = app
        .state::<ShotStore>()
        .claim_for_request(
            ShotSlot::Translate,
            Some(shot_id),
            target,
            region.as_ref(),
            provider.takes_screenshot(),
        )
        // A named screenshot is claimed or refused, never skipped: treat no
        // bytes as gone.
        .and_then(|png| png.ok_or(crate::shots::ShotRefusal::Gone))
        .map_err(|refusal| {
            tracing::info!("Translate: refused: screenshot unavailable");
            refusal.message(provider.label())
        })?;
    let screenshot = encode_png(&png);
    let settings_model = app
        .state::<AppState>()
        .launcher
        .lock()
        .settings
        .gemini_model
        .clone();
    let model = gemini::resolve_model(&settings_model, &gemini::file_model());
    let (system, messages) = payload(
        PayloadKind::Translate,
        prompt::TRANSLATE_SYSTEM.to_owned(),
        vec![ChatMessage {
            role: "user".to_owned(),
            content: prompt::TRANSLATE_REQUEST.to_owned(),
        }],
        None,
    );
    let mut out = String::new();
    let streamed = gemini::stream(
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
    .await;
    translation_outcome(&out, streamed)
}

/// A translation as the overlay shows it: the text, and why it ended early
/// when it did.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Translation {
    pub text: String,
    pub cut_short: Option<String>,
}

/// What a translation stream leaves the user: `text` is what arrived. A
/// stream that fails after some text keeps it with the reason; one that
/// failed before any text is only the error.
fn translation_outcome(text: &str, streamed: Result<(), String>) -> Result<Translation, String> {
    let text = text.trim().to_owned();
    match streamed {
        Ok(()) => Ok(Translation {
            text,
            cut_short: None,
        }),
        Err(error) if !text.is_empty() => Ok(Translation {
            text,
            cut_short: Some(error),
        }),
        Err(error) => Err(error),
    }
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
            game_key: "library:g1".into(),
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
        assert_eq!(empty, []);
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
        for hints in [true, false] {
            let (prompt, messages) = payload(
                PayloadKind::Chat,
                prompt::assemble(prompt_kind(false, hints)),
                question(),
                linked.identity_block.as_deref(),
            );
            let (plain, _) = payload(
                PayloadKind::Chat,
                prompt::assemble(prompt_kind(false, hints)),
                question(),
                untargeted.identity_block.as_deref(),
            );
            println!(
                "hints {hints}: system prompt {} bytes linked, {} bytes without a target",
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
            let line = request_log_line(&RequestLine {
                request_id: 3,
                provider: Provider::Claude,
                screenshot: true,
                turns: 1,
                linked: linked.capture.is_some(),
                identity: linked.identity,
                search: false,
                hints,
                recap: false,
            });
            for (what, text) in [
                ("prompt", prompt.as_str()),
                ("argv", argv.join(" ").as_str()),
                ("block", block.as_str()),
                ("log line", line.as_str()),
            ] {
                let key = target(true).game_key;
                for withheld in ["SECRET-TITLE", r"C:\Games\Foo", r"c:\games\foo", &key] {
                    assert!(!text.contains(withheld), "{what} contains {withheld}");
                }
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
    fn prompt_kind_selects_by_flags() {
        for (recap, hints, expected) in [
            (false, true, prompt::PromptKind::Chat { hints: true }),
            (false, false, prompt::PromptKind::Chat { hints: false }),
            (true, true, prompt::PromptKind::Recap),
            (true, false, prompt::PromptKind::Recap),
        ] {
            let kind = prompt_kind(recap, hints);
            println!("recap {recap}, hints {hints} -> {kind:?}");
            assert_eq!(kind, expected);
        }
    }

    #[test]
    fn request_log_line_names_no_game() {
        let line = |request_id, provider, screenshot, turns, linked, identity, hints, recap| {
            request_log_line(&RequestLine {
                request_id,
                provider,
                screenshot,
                turns,
                linked,
                identity,
                search: false,
                hints,
                recap,
            })
        };
        let mut lines = Vec::new();
        for (text, expected) in [
            (
                line(3, Provider::Claude, true, 3, true, IdentitySource::Library, true, false),
                "Request 3: provider claude, screenshot requested: yes, linked target: yes, turns: 3, identity: library, search: no, hints: yes, recap: no",
            ),
            (
                line(9, Provider::Gemini, false, 5, true, IdentitySource::Program, false, true),
                "Request 9: provider gemini, screenshot requested: no, linked target: yes, turns: 5, identity: program, search: no, hints: no, recap: yes",
            ),
            (
                line(3, Provider::Claude, true, 1, false, IdentitySource::Absent, false, false),
                "Request 3: provider claude, screenshot requested: yes, linked target: no, turns: 1, identity: none, search: no, hints: no, recap: no",
            ),
        ] {
            println!("{text}");
            assert_eq!(text, expected);
            lines.push(text);
        }
        for (code, expected) in [
            (
                RefusalCode::NoLinkedGame,
                "Request 4: refused: no linked game",
            ),
            (
                RefusalCode::WindowClosed,
                "Request 4: refused: linked window closed",
            ),
            (RefusalCode::GameChanged, "Request 4: refused: game changed"),
            (
                RefusalCode::NothingToRecap,
                "Request 4: refused: nothing to recap",
            ),
            (
                RefusalCode::SearchUnavailable,
                "Request 4: refused: search unavailable",
            ),
            (
                RefusalCode::ScreenshotUnavailable,
                "Request 4: refused: screenshot unavailable",
            ),
            (RefusalCode::ChatBusy, "Request 4: refused: chat busy"),
        ] {
            let text = refusal_log_line(4, code);
            println!("{text}");
            assert_eq!(text, expected);
            lines.push(text);
        }
        // The line an admitted request logs, from the real admission.
        let fixture = Fixture::new("log_line", 1);
        let game = target(true);
        let admitted = admit_chat(
            ChatArgs {
                question: "where is the key".to_owned(),
                ..args(5)
            },
            Some(&game),
            Some(&game),
            &fixture.store,
            &ShotStore::default(),
        )
        .map_err(|refusal| refusal.message)
        .expect("the request was refused");
        let text = request_log_line(&admitted.line);
        println!("{text}");
        assert_eq!(
            text,
            "Request 5: provider gemini, screenshot requested: no, linked target: yes, turns: 3, identity: library, search: no, hints: yes, recap: no"
        );
        lines.push(text);
        for text in &lines {
            for withheld in [
                "SECRET-TITLE",
                "$(id)",
                "Real Name",
                r"C:\Games\Foo\foo.exe",
                game.game_key.as_str(),
                "where is the key",
            ] {
                assert!(!text.contains(withheld), "{text} contains {withheld}");
            }
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
    fn availability_reads_only_the_cache() {
        let ai = AiState::default();
        ai.set_gemini(GeminiPresence {
            key: true,
            file_model: "gemini-from-file".into(),
        });
        let availability = ai.availability("");
        println!("{availability:?}");
        assert!(availability.gemini, "the cached key presence was not used");
        assert_eq!(availability.gemini_model, "gemini-from-file");
        assert_eq!(availability.gemini_fallback_model, "gemini-from-file");
        let availability = ai.availability("gemini-x");
        println!("{availability:?}");
        assert_eq!(availability.gemini_model, "gemini-x");
        assert_eq!(availability.gemini_fallback_model, "gemini-from-file");
    }

    #[test]
    fn every_provider_change_is_announced() {
        // Its five callers: CLI detection at startup, set_active_provider,
        // set_gemini_key, recheck_clis, update_settings. The definition is
        // generic, so its name is followed by `<`, not `(`.
        let needle = concat!("notify_providers", "_changed(");
        let (files, count) = crate::util::count_in_sources(needle, None);
        println!("scanned {files} files, found {count} occurrence(s) of {needle}");
        assert!(files > 0, "the source scan found no files");
        assert_eq!(count, 5, "a provider change that no window hears about");
    }

    /// Every provider. A new variant fails to compile in the match; add it to
    /// the list too.
    fn all_providers() -> [Provider; 3] {
        let all = [Provider::Gemini, Provider::Claude, Provider::Openai];
        for provider in all {
            match provider {
                Provider::Gemini | Provider::Claude | Provider::Openai => {}
            }
        }
        all
    }

    #[test]
    fn every_provider_takes_screenshots() {
        for provider in all_providers() {
            println!("{}: {}", provider.as_str(), provider.takes_screenshot());
            assert!(provider.takes_screenshot(), "{}", provider.as_str());
        }
    }

    #[test]
    fn provider_labels_match_the_overlay() {
        let source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/lib/stores/companion.svelte.ts"
        ));
        for provider in all_providers() {
            let entry = format!("{}: {{ label: '{}'", provider.as_str(), provider.label());
            println!("{entry}");
            assert!(source.contains(&entry), "PROVIDERS lacks {entry}");
        }
    }

    #[test]
    fn translate_gate_order() {
        for bits in 0..8_u8 {
            let (linked, available, takes) = (bits & 4 != 0, bits & 2 != 0, bits & 1 != 0);
            let expected = if !linked {
                Err(TranslateBlock::NotLinked)
            } else if !available {
                Err(TranslateBlock::Unavailable)
            } else if !takes {
                Err(TranslateBlock::NoScreenshots)
            } else {
                Ok(())
            };
            let got = translate_gate(linked, available, takes);
            println!(
                "linked {linked}, available {available}, takes {takes}: {:?}",
                got.map_err(TranslateBlock::code)
            );
            assert_eq!(got, expected);
        }
        let codes = [
            TranslateBlock::NotLinked,
            TranslateBlock::Unavailable,
            TranslateBlock::NoScreenshots,
        ]
        .map(TranslateBlock::code);
        assert_eq!(codes, ["no linked game", "unavailable", "no screenshots"]);
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

    // ---------------- request lanes ----------------

    /// Sets its flag when dropped: the request holding it was cancelled.
    struct SetOnDrop(std::sync::Arc<std::sync::atomic::AtomicBool>);

    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// A request that never ends on its own, and the flag its drop sets.
    struct Pending {
        dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl Pending {
        fn new() -> Self {
            Self {
                dropped: std::sync::Arc::default(),
            }
        }

        fn spawn(&self) -> tauri::async_runtime::JoinHandle<()> {
            let guard = SetOnDrop(std::sync::Arc::clone(&self.dropped));
            tauri::async_runtime::spawn(async move {
                let _guard = guard;
                std::future::pending::<()>().await;
            })
        }

        fn is_dropped(&self) -> bool {
            self.dropped.load(std::sync::atomic::Ordering::SeqCst)
        }

        /// Whether the request is dropped within a second.
        async fn dropped_soon(&self) -> bool {
            for _ in 0..100 {
                if self.is_dropped() {
                    return true;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            self.is_dropped()
        }

        /// Whether the request still runs after a short settle.
        async fn still_runs(&self) -> bool {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            !self.is_dropped()
        }
    }

    #[tokio::test]
    async fn lanes_are_independent() {
        let ai = AiState::default();
        let (chat1, chat2, chat3, chat4) = (
            Pending::new(),
            Pending::new(),
            Pending::new(),
            Pending::new(),
        );
        let (t1, t2) = (Pending::new(), Pending::new());

        install_in(&ai.chat, 1, false, || chat1.spawn()).unwrap();
        install_in(&ai.chat, 2, false, || chat2.spawn()).unwrap();
        println!("chat 2 installed over chat 1");
        assert!(chat1.dropped_soon().await, "chat 2 did not replace chat 1");
        assert!(chat2.still_runs().await, "chat 2 was dropped");

        install_in(&ai.translate, 11, false, || t1.spawn()).unwrap();
        println!("translation 11 installed");
        assert!(chat2.still_runs().await, "a translation dropped chat 2");

        install_in(&ai.chat, 3, false, || chat3.spawn()).unwrap();
        println!("chat 3 installed");
        assert!(chat2.dropped_soon().await, "chat 3 did not replace chat 2");
        assert!(t1.still_runs().await, "chat 3 dropped translation 11");

        install_in(&ai.translate, 12, false, || t2.spawn()).unwrap();
        println!("translation 12 installed");
        assert!(t1.dropped_soon().await, "translation 12 did not replace 11");
        assert!(chat3.still_runs().await, "a translation dropped chat 3");

        ai.cancel(3);
        println!("cancelled chat 3");
        assert!(chat3.dropped_soon().await, "cancel did not drop chat 3");
        assert!(
            t2.still_runs().await,
            "cancelling a chat dropped translation 12"
        );

        install_in(&ai.chat, 4, false, || chat4.spawn()).unwrap();
        ai.cancel(12);
        println!("chat 4 installed, cancelled translation 12");
        assert!(
            t2.dropped_soon().await,
            "cancel did not drop translation 12"
        );
        assert!(
            chat4.still_runs().await,
            "cancelling a translation dropped chat 4"
        );

        let (chat5, chat6) = (Pending::new(), Pending::new());
        install_in(&ai.chat, 5, false, || chat5.spawn()).unwrap();
        assert!(chat4.dropped_soon().await, "chat 5 did not replace chat 4");
        release_in(&ai.chat, 5);
        install_in(&ai.chat, 6, false, || chat6.spawn()).unwrap();
        println!("chat 5 released, chat 6 installed");
        assert!(chat5.still_runs().await, "a released request was dropped");

        let mut spawned = 0;
        let refused = install_in(&ai.chat, 7, true, || {
            spawned += 1;
            Pending::new().spawn()
        });
        println!("a refusing install in a busy lane: {refused:?}, spawned {spawned}");
        assert!(
            matches!(refused, Err(LaneBusy)),
            "a busy lane was not refused"
        );
        assert_eq!(spawned, 0, "a refused install spawned its request");
        assert!(chat6.still_runs().await, "a refused install dropped chat 6");

        let t3 = Pending::new();
        let accepted = install_in(&ai.translate, 13, true, || {
            spawned += 1;
            t3.spawn()
        });
        println!("a refusing install in an idle lane: {accepted:?}, spawned {spawned}");
        assert!(accepted.is_ok(), "an idle lane refused a request");
        assert_eq!(spawned, 1, "an accepted install did not spawn its request");

        ai.cancel(6);
        ai.cancel(13);
        assert!(chat6.dropped_soon().await, "cancel did not drop chat 6");
        assert!(
            t3.dropped_soon().await,
            "cancel did not drop translation 13"
        );
    }

    #[test]
    fn quitting_stops_both_lanes() {
        let ai = AiState::default();
        let (chat, translation) = (Pending::new(), Pending::new());
        install_in(&ai.chat, 1, false, || chat.spawn()).unwrap();
        install_in(&ai.translate, 2, false, || translation.spawn()).unwrap();

        ai.stop_for_exit(None);

        println!(
            "after quit: chat dropped {}, translation dropped {}",
            chat.is_dropped(),
            translation.is_dropped()
        );
        assert!(chat.is_dropped(), "quitting left the chat request running");
        assert!(
            translation.is_dropped(),
            "quitting left the translation running"
        );
    }

    const WRITABLE: crate::chat_store::StoreMode =
        crate::chat_store::StoreMode { read_only: false };

    /// The fixture target's chat on screen in a fresh store holding
    /// `exchanges` saved exchanges; its folder is removed on drop.
    struct Fixture {
        store: ChatStore,
        dir: std::path::PathBuf,
        key: String,
    }

    impl Fixture {
        fn new(label: &str, exchanges: usize) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("aigc_launcher_ai_{}_{label}", std::process::id()));
            drop(std::fs::remove_dir_all(&dir));
            std::fs::create_dir_all(&dir).unwrap();
            let store = ChatStore::new(Some(dir.clone()));
            let key = target(true).game_key;
            store.load(&key, WRITABLE);
            store.show(&key, WRITABLE);
            for n in 0..exchanges {
                let snapshot = store.snapshot(&key, Provider::Gemini);
                let save = store.append_exchange(
                    &key,
                    snapshot.clear_generation,
                    Some(saved(&format!("Question {n}"))),
                    WRITABLE,
                );
                assert_eq!(save, SaveState::On, "the fixture's exchange was not saved");
            }
            Self { store, dir, key }
        }

        /// Every file in the folder: name, size and modification time.
        fn listing(&self) -> Vec<(String, u64, std::time::SystemTime)> {
            let mut files: Vec<_> = std::fs::read_dir(&self.dir)
                .unwrap()
                .map(|item| {
                    let item = item.unwrap();
                    let meta = item.metadata().unwrap();
                    (
                        item.file_name().to_string_lossy().into_owned(),
                        meta.len(),
                        meta.modified().unwrap(),
                    )
                })
                .collect();
            files.sort();
            files
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            drop(std::fs::remove_dir_all(&self.dir));
        }
    }

    fn saved(question: &str) -> SavedExchange {
        SavedExchange {
            saved_at: Utc::now(),
            question: question.to_owned(),
            answer: "Go north.".to_owned(),
            provider: Provider::Gemini,
            model: "test-model".to_owned(),
            screenshot: false,
            search: None,
        }
    }

    /// A text-only Gemini question with hints first on.
    fn args(request_id: u64) -> ChatArgs {
        ChatArgs {
            request_id,
            conversation_id: 1,
            provider: Provider::Gemini,
            question: "Where next?".to_owned(),
            shot_id: None,
            search: false,
            hints: true,
            recap: false,
        }
    }

    fn recap_args(request_id: u64) -> ChatArgs {
        ChatArgs {
            recap: true,
            ..args(request_id)
        }
    }

    /// Another linked game than `target(true)`.
    fn other_game() -> GameInfo {
        GameInfo {
            hwnd: 43,
            game_key: "library:g2".into(),
            ..target(true)
        }
    }

    /// Hold a chat screenshot of `target(true)` in `shots`; returns its id.
    fn hold_shot(shots: &ShotStore) -> u64 {
        let prepared = crate::shots::Prepared {
            png: vec![7; 4],
            width: 2,
            height: 1,
        };
        shots
            .complete(shots.begin(ShotSlot::Chat), (42, 7), None, prepared)
            .unwrap()
            .unwrap();
        assert!(shots.png(1).is_some(), "the screenshot is not held");
        1
    }

    fn refused(outcome: Result<Admitted, Refusal>, case: &str) -> Refusal {
        match outcome {
            Ok(_) => panic!("{case}: admitted"),
            Err(refusal) => refusal,
        }
    }

    fn admitted(outcome: Result<Admitted, Refusal>, case: &str) -> Admitted {
        outcome.unwrap_or_else(|refusal| panic!("{case}: refused {refusal:?}"))
    }

    /// `snake_case` to `camelCase`, as serde renames a command's arguments.
    fn camel(name: &str) -> String {
        let mut out = String::new();
        let mut upper = false;
        for c in name.chars() {
            if c == '_' {
                upper = true;
            } else if upper {
                out.extend(c.to_uppercase());
                upper = false;
            } else {
                out.push(c);
            }
        }
        out
    }

    /// The top-level items of the object literal `text` starts inside.
    fn object_items(text: &str) -> Vec<String> {
        let (mut items, mut item, mut depth) = (Vec::new(), String::new(), 0_i32);
        for c in text.chars() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' if depth == 0 => break,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => {
                    items.push(std::mem::take(&mut item));
                    continue;
                }
                _ => {}
            }
            item.push(c);
        }
        items.push(item);
        items
            .into_iter()
            .map(|item| item.trim().to_owned())
            .filter(|item| !item.is_empty())
            .collect()
    }

    const ASK_KEYS: [&str; 9] = [
        "requestId",
        "conversationId",
        "provider",
        "channel",
        "question",
        "shotId",
        "search",
        "hints",
        "recap",
    ];

    #[test]
    fn ask_sage_signature_matches_the_page() {
        let command = include_str!("../commands/ai.rs");
        let page = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/lib/components/Overlay.svelte"
        ));
        let (_, rest) = command
            .split_once("fn ask_sage(")
            .expect("commands/ai.rs has no ask_sage");
        let (params, _) = rest.split_once(')').unwrap();
        let names: Vec<String> = params
            .split(',')
            .filter_map(|param| param.split_once(':'))
            .filter(|(_, ty)| {
                let ty = ty.trim();
                ty != "AppHandle" && !ty.starts_with("State<") && !ty.ends_with("WebviewWindow")
            })
            .map(|(name, _)| camel(name.trim()))
            .collect();
        println!("ask_sage parameters: {names:?}");
        assert_eq!(names, ASK_KEYS, "ask_sage's parameters");

        let call = "invoke('ask_sage'";
        println!("{call}: {} in Overlay.svelte", page.matches(call).count());
        assert_eq!(page.matches(call).count(), 1, "the page's ask_sage calls");
        let (_, rest) = page
            .split_once("invoke('ask_sage', {")
            .expect("the page's ask_sage call passes no object");
        let items = object_items(rest);
        let keys: Vec<&str> = items
            .iter()
            .map(|item| {
                item.split_once(':')
                    .map_or(item.as_str(), |(key, _)| key)
                    .trim()
            })
            .collect();
        println!("the page sends: {keys:?}");
        assert_eq!(keys, ASK_KEYS, "the page's ask_sage keys");
        assert!(
            items.iter().any(|item| item.starts_with("recap:")),
            "recap is not sent as `recap:`"
        );
        for needle in ["messages:", concat!("attach", "Screenshot:")] {
            println!("{needle}: {}", page.matches(needle).count());
            assert_eq!(
                page.matches(needle).count(),
                0,
                "{needle} in Overlay.svelte"
            );
        }
    }

    #[test]
    fn sage_event_mirrors_rust() {
        let page = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/lib/components/Overlay.svelte"
        ));
        let chat = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/lib/utils/chat.ts"
        ));
        let source = format!("{page}\n{chat}");
        let head = EventHead {
            request_id: 1,
            conversation_id: 2,
            display_generation: 3,
        };
        let report = || SearchReport {
            searched: true,
            queries: vec!["foo".to_owned()],
            sources: vec![search::SearchSource {
                title: "Foo".to_owned(),
                url: "https://example.com/".to_owned(),
                host: "example.com".to_owned(),
            }],
            suggestion_html: "<div>foo</div>".to_owned(),
            note: search::SearchNote::None,
            hints: false,
        };
        let typescript = crate::util::ts_fields(page, "interface SageEvent {");
        println!("TypeScript: {typescript:?}");
        assert!(!typescript.is_empty(), "no SageEvent in Overlay.svelte");
        for (kind, full, empty) in [
            (
                "chunk",
                SageEvent::chunk(head, "Go".to_owned()),
                SageEvent::chunk(head, "Go".to_owned()),
            ),
            (
                "error",
                SageEvent::error(head, "x".to_owned()),
                SageEvent::error(head, "x".to_owned()),
            ),
            (
                "done",
                SageEvent::done(head, Some(SaveState::On), Some(report())),
                SageEvent::done(head, None, None),
            ),
        ] {
            let full = crate::util::json_fields(&serde_json::to_value(&full).unwrap());
            let empty = crate::util::json_fields(&serde_json::to_value(&empty).unwrap());
            println!("{kind}: full {full:?}, unset {empty:?}");
            assert!(
                full.iter().any(|(key, _)| key == "displayGeneration"),
                "{kind} carries no displayGeneration"
            );
            let mismatches = crate::util::mirror_mismatches(&typescript, &full, &empty, &source);
            assert!(mismatches.is_empty(), "{kind}: {mismatches:#?}");
        }
        let bare = serde_json::to_value(SageEvent::done(head, None, None)).unwrap();
        assert!(
            bare.get("save").is_none(),
            "a done without a save sent one: {bare}"
        );
        let kinds = crate::util::alias_mismatches(page, "SageKind", &["chunk", "done", "error"]);
        assert!(kinds.is_empty(), "{kinds:#?}");
    }

    #[test]
    fn recap_effective_values() {
        let fixture = Fixture::new("effective", 1);
        let game = target(true);
        for search in [false, true] {
            for hints in [false, true] {
                for recap in [false, true] {
                    for with_shot in [true, false] {
                        let shots = ShotStore::default();
                        let shot = hold_shot(&shots);
                        let request = ChatArgs {
                            search,
                            hints,
                            recap,
                            shot_id: with_shot.then_some(shot),
                            ..args(1)
                        };
                        let outcome =
                            admit_chat(request, Some(&game), Some(&game), &fixture.store, &shots);
                        let case = format!(
                            "search {search}, hints {hints}, recap {recap}, shot {with_shot}"
                        );
                        if recap && with_shot {
                            let refusal = refused(outcome, &case);
                            println!("{case}: refused {:?}", refusal.code);
                            assert_eq!(refusal.code, RefusalCode::ScreenshotUnavailable, "{case}");
                            assert_eq!(refusal.message, RECAP_NO_SHOT, "{case}");
                        } else if !recap && search {
                            let refusal = refused(outcome, &case);
                            println!("{case}: refused {:?}", refusal.code);
                            assert_eq!(refusal.code, RefusalCode::SearchUnavailable, "{case}");
                        } else {
                            let admitted = admitted(outcome, &case);
                            let line = request_log_line(&admitted.line);
                            let RequestKind::Chat { effective, png, .. } = admitted.kind;
                            println!("{case}: {effective:?}, {line}");
                            if recap {
                                assert_eq!(
                                    effective,
                                    Effective {
                                        search: false,
                                        hints: false,
                                        shot_id: None
                                    },
                                    "{case}"
                                );
                                assert!(
                                    line.ends_with("search: no, hints: no, recap: yes"),
                                    "{case}: {line}"
                                );
                            } else {
                                assert_eq!(effective.hints, hints, "{case}");
                                assert!(!effective.search, "{case}");
                                assert_eq!(png.is_some(), with_shot, "{case}");
                                let requested = if with_shot { "yes" } else { "no" };
                                assert!(
                                    line.contains(&format!("screenshot requested: {requested}")),
                                    "{case}: {line}"
                                );
                            }
                        }
                        let held = shots.png(shot).is_some();
                        println!("{case}: still held {held}");
                        assert_eq!(held, !(with_shot && !recap && !search), "{case}");
                    }
                }
            }
        }
    }

    /// Drive `request` through admission and the chat lane, counting spawns.
    /// `game` is the live target, stored as it is.
    fn drive(
        request: ChatArgs,
        game: Option<&GameInfo>,
        ai: &AiState,
        fixture: &Fixture,
        shots: &ShotStore,
    ) -> (Admission, usize) {
        drive_stored(request, game, game, ai, fixture, shots)
    }

    /// `drive`, with the target as stored before the liveness check apart.
    fn drive_stored(
        request: ChatArgs,
        game: Option<&GameInfo>,
        stored: Option<&GameInfo>,
        ai: &AiState,
        fixture: &Fixture,
        shots: &ShotStore,
    ) -> (Admission, usize) {
        let mut spawned = 0;
        let admission =
            admit_and_install(request, game, stored, ai, &fixture.store, shots, |_, _| {
                spawned += 1;
                Pending::new().spawn()
            });
        (admission, spawned)
    }

    /// A send after the linked window closed names the closed window, logs a
    /// code of its own and keeps the screenshot held; a target never linked
    /// still reads as no linked game.
    #[tokio::test]
    async fn send_names_a_closed_linked_window() {
        let fixture = Fixture::new("window_closed", 1);
        let ai = AiState::default();
        let (linked, never) = (target(true), target(false));
        let rows = [
            (
                "linked window closed",
                Some(&linked),
                RefusalCode::WindowClosed,
                crate::shots::WINDOW_CLOSED,
            ),
            (
                "stored, never linked",
                Some(&never),
                RefusalCode::NoLinkedGame,
                crate::overlay::NO_LINKED_GAME,
            ),
            (
                "nothing stored",
                None,
                RefusalCode::NoLinkedGame,
                crate::overlay::NO_LINKED_GAME,
            ),
        ];
        for (case, stored, code, message) in rows {
            let shots = ShotStore::default();
            let shot = hold_shot(&shots);
            let request = ChatArgs {
                shot_id: Some(shot),
                ..args(2)
            };
            let (admission, spawned) = drive_stored(request, None, stored, &ai, &fixture, &shots);
            println!("{case}:");
            refusal_event(admission, &fixture, code, message);
            assert_eq!(spawned, 0, "{case}: a refused request was spawned");
            assert!(ai.chat.lock().is_none(), "{case}: the lane changed");
            assert!(
                shots.png(shot).is_some(),
                "{case}: the screenshot is no longer held"
            );
        }
        assert_eq!(
            refusal_log_line(2, RefusalCode::WindowClosed),
            "Request 2: refused: linked window closed"
        );
    }

    /// The refused event's JSON, checked: one `error` with `message` and the
    /// store's current generation.
    fn refusal_event(admission: Admission, fixture: &Fixture, code: RefusalCode, message: &str) {
        let Admission::Refused { log, event } = admission else {
            panic!("{code:?}: started");
        };
        let event = serde_json::to_value(&event).unwrap();
        println!("{log} -> {event}");
        assert_eq!(log, refusal_log_line(2, code));
        assert_eq!(event["kind"], "error", "{code:?}");
        assert_eq!(event["message"], message, "{code:?}");
        assert_eq!(
            event["displayGeneration"],
            fixture.store.display_generation(),
            "{code:?}: the event's generation"
        );
    }

    #[tokio::test]
    async fn refusals_come_first_and_cancel_nothing() {
        type Row<'a> = (
            &'a str,
            &'a Fixture,
            Option<&'a GameInfo>,
            ChatArgs,
            RefusalCode,
            &'a str,
        );
        let empty = Fixture::new("refusals_empty", 0);
        let filled = Fixture::new("refusals_filled", 1);
        let (a, b) = (target(true), other_game());
        let ai = AiState::default();
        let running = Pending::new();
        install_in(&ai.chat, 1, false, || running.spawn()).unwrap();
        let shots = ShotStore::default();
        let search_text = search::search_gate(Provider::Gemini, true, false).unwrap_err();
        let gone = crate::shots::ShotRefusal::Gone.message("Gemini");
        let rows: [Row<'_>; 6] = [
            (
                "no target",
                &empty,
                None,
                args(2),
                RefusalCode::NoLinkedGame,
                crate::overlay::NO_LINKED_GAME,
            ),
            (
                "another game",
                &filled,
                Some(&b),
                args(2),
                RefusalCode::GameChanged,
                GAME_CHANGED,
            ),
            (
                "recap of nothing",
                &empty,
                Some(&a),
                recap_args(2),
                RefusalCode::NothingToRecap,
                NOTHING_TO_RECAP,
            ),
            (
                "search",
                &filled,
                Some(&a),
                ChatArgs {
                    search: true,
                    ..args(2)
                },
                RefusalCode::SearchUnavailable,
                &search_text,
            ),
            (
                "screenshot gone",
                &filled,
                Some(&a),
                ChatArgs {
                    shot_id: Some(99),
                    ..args(2)
                },
                RefusalCode::ScreenshotUnavailable,
                &gone,
            ),
            (
                "recap while busy",
                &filled,
                Some(&a),
                recap_args(2),
                RefusalCode::ChatBusy,
                CHAT_BUSY,
            ),
        ];
        for (case, fixture, game, request, code, message) in rows {
            let files = fixture.listing();
            let memory = fixture.store.snapshot(&fixture.key, Provider::Gemini);
            let (admission, spawned) = drive(request, game, &ai, fixture, &shots);
            println!("{case}:");
            refusal_event(admission, fixture, code, message);
            assert_eq!(spawned, 0, "{case}: a refused request was spawned");
            assert!(
                running.still_runs().await,
                "{case}: the running request was dropped"
            );
            assert_eq!(
                ai.chat.lock().as_ref().map(|active| active.request_id),
                Some(1),
                "{case}: the lane changed"
            );
            assert_eq!(fixture.listing(), files, "{case}: the chats folder changed");
            assert_eq!(
                fixture.store.snapshot(&fixture.key, Provider::Gemini),
                memory,
                "{case}: the store changed"
            );
        }
        ai.cancel(1);
        assert!(running.dropped_soon().await);
    }

    #[tokio::test]
    async fn displayed_key_check() {
        let fixture = Fixture::new("displayed", 1);
        let ai = AiState::default();
        let shots = ShotStore::default();
        let (admission, spawned) = drive(args(2), Some(&other_game()), &ai, &fixture, &shots);
        refusal_event(admission, &fixture, RefusalCode::GameChanged, GAME_CHANGED);
        assert_eq!(spawned, 0, "a refused request was spawned");
        assert!(ai.chat.lock().is_none(), "the lane changed");

        let game = target(true);
        fixture.store.delete_all(WRITABLE).unwrap();
        let refusal = refused(
            admit_chat(args(3), Some(&game), Some(&game), &fixture.store, &shots),
            "after delete all",
        );
        println!("after delete all: {:?}", refusal.code);
        assert_eq!(refusal.code, RefusalCode::GameChanged);
        fixture.store.load(&fixture.key, WRITABLE);
        fixture.store.show(&fixture.key, WRITABLE);
        let shown = admitted(
            admit_chat(args(4), Some(&game), Some(&game), &fixture.store, &shots),
            "shown again",
        );
        println!("shown again: {}", request_log_line(&shown.line));
    }

    #[tokio::test]
    async fn recap_admission() {
        let empty = Fixture::new("recap_empty", 0);
        let filled = Fixture::new("recap_filled", 1);
        let game = target(true);
        let ai = AiState::default();
        let shots = ShotStore::default();
        let (admission, _) = drive(recap_args(2), Some(&game), &ai, &empty, &shots);
        refusal_event(
            admission,
            &empty,
            RefusalCode::NothingToRecap,
            NOTHING_TO_RECAP,
        );

        let running = Pending::new();
        install_in(&ai.chat, 1, false, || running.spawn()).unwrap();
        let (admission, spawned) = drive(recap_args(2), Some(&game), &ai, &filled, &shots);
        refusal_event(admission, &filled, RefusalCode::ChatBusy, CHAT_BUSY);
        assert_eq!(spawned, 0);
        assert!(
            running.still_runs().await,
            "a refused recap dropped the answer"
        );
        ai.cancel(1);
        assert!(running.dropped_soon().await);

        let (admission, spawned) = drive(recap_args(2), Some(&game), &ai, &filled, &shots);
        let Admission::Started { log } = admission else {
            panic!("an idle recap was refused");
        };
        println!("idle: {log}");
        assert_eq!(spawned, 1);
        assert!(log.contains("turns: 3"), "{log}");
        assert!(log.ends_with("recap: yes"), "{log}");
        ai.cancel(2);

        for hints in [true, false] {
            let recap = prompt::assemble(prompt_kind(true, hints))
                .matches(prompt::RECAP_TASK)
                .count();
            let chat = prompt::assemble(prompt_kind(false, hints))
                .matches(prompt::RECAP_TASK)
                .count();
            println!("hints {hints}: recap prompt {recap}, chat prompt {chat}");
            assert_eq!((recap, chat), (1, 0), "hints {hints}");
        }
    }

    #[test]
    fn finish_request_saves_before_done() {
        let head = EventHead {
            request_id: 1,
            conversation_id: 2,
            display_generation: 3,
        };
        let now = Utc::now();
        let draft = || ExchangeDraft {
            question: "Where next?".to_owned(),
            provider: Provider::Claude,
            model: "test-model".to_owned(),
            screenshot: true,
        };
        let mut calls = Vec::new();
        let event = finish_request(head, Ok(None), "text", draft(), now, |entry| {
            calls.push(entry);
            SaveState::Failed
        });
        let event = serde_json::to_value(&event).unwrap();
        println!("an answer: {calls:?} -> {event}");
        let expected = SavedExchange {
            saved_at: now,
            question: "Where next?".to_owned(),
            answer: "text".to_owned(),
            provider: Provider::Claude,
            model: "test-model".to_owned(),
            screenshot: true,
            search: None,
        };
        assert_eq!(calls, [Some(expected)], "the saved entry");
        assert_eq!(event["kind"], "done");
        assert_eq!(event["save"], serde_json::json!({ "state": "failed" }));

        let mut calls = Vec::new();
        let event = finish_request(head, Ok(None), " \n", draft(), now, |entry| {
            calls.push(entry);
            SaveState::On
        });
        let event = serde_json::to_value(&event).unwrap();
        println!("a blank answer: {calls:?} -> {event}");
        assert_eq!(calls, [None], "a blank answer saved an entry");
        assert_eq!(event["kind"], "done");
        assert_eq!(event["save"], serde_json::json!({ "state": "on" }));

        // A failed answer, and one cut short after some text arrived, are
        // never saved: no entry, and nothing of them is sent again.
        for (case, message, answer) in [
            ("an error", "x", ""),
            ("cut short", "Gemini stopped early (MAX_TOKENS).", "Go no"),
        ] {
            let mut called = 0;
            let event = finish_request(head, Err(message.to_owned()), answer, draft(), now, |_| {
                called += 1;
                SaveState::On
            });
            let event = serde_json::to_value(&event).unwrap();
            println!("{case}: store called {called} times -> {event}");
            assert_eq!(called, 0, "{case}: the store was called");
            assert_eq!(event["kind"], "error", "{case}");
            assert_eq!(event["message"], message, "{case}");
        }
    }

    #[test]
    fn finish_request_has_one_caller() {
        let here = std::path::Path::new("ai").join("mod.rs");
        for (needle, expected) in [
            (concat!("finish_request", "("), 2),
            (concat!("fn finish_request", "("), 1),
        ] {
            let rows = crate::util::count_in_sources_by_file(needle);
            println!("{needle}: {} files scanned", rows.len());
            assert!(!rows.is_empty(), "no source file scanned");
            for (file, count) in &rows {
                if *count > 0 {
                    println!("  {}: {count}", file.display());
                }
                let want = if *file == here { expected } else { 0 };
                assert_eq!(*count, want, "{needle} in {}", file.display());
            }
        }
        let needle = concat!("append_exchange", "(");
        let rows = crate::util::count_in_sources_by_file(needle);
        assert!(!rows.is_empty(), "no source file scanned");
        let outside: Vec<_> = rows
            .iter()
            .filter(|(file, count)| *count > 0 && file != std::path::Path::new("chat_store.rs"))
            .collect();
        println!("{needle} outside chat_store.rs: {outside:?}");
        assert_eq!(outside.len(), 1, "{needle} outside chat_store.rs");
        assert_eq!(outside[0].0, here);
        assert_eq!(outside[0].1, 1);
    }
}
