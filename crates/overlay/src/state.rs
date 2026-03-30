use std::collections::HashSet;

use once_cell::sync::Lazy;
use parking_lot::Mutex;

use crate::provider::Provider;

/// Maximum messages kept in memory. Oldest are evicted when exceeded.
/// This bounds per-frame clone cost and prevents unbounded memory growth.
const MAX_STORED_MESSAGES: usize = 100;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Clone)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: String,
    /// If true, this message is an internal translation request/response
    /// and should be excluded from the conversation history sent to the API.
    pub is_translation: bool,
}

impl ChatMessage {
    pub fn new(role: MessageRole, content: String) -> Self {
        Self { role, content, is_translation: false }
    }

    pub fn translation(role: MessageRole, content: String) -> Self {
        Self { role, content, is_translation: true }
    }
}

#[derive(Default)]
pub struct AppState {
    pub visible: bool,
    pub messages: Vec<ChatMessage>,
    pub input_buffer: String,
    pub attach_screenshot: bool,
    pub is_loading: bool,
    pub error: Option<String>,
    /// Incremented on each send; async tasks compare against this before writing results.
    pub request_generation: u64,
    /// Accumulates text chunks during streaming. Rendered by UI while is_loading is true.
    pub streaming_response: String,
    /// Detected game name, resolved once at init.
    pub game_name: Option<String>,
    /// When true, the render loop skips drawing the overlay and performs capture.
    pub capture_pending: bool,
    /// Frames to wait with overlay hidden before capturing.
    pub capture_wait_frames: u8,
    /// Captured screenshot data, ready for the async task to pick up.
    pub captured_screenshot: Option<String>,
    /// Set to true by the capture task when it finishes (success or failure).
    pub capture_complete: bool,
    /// If true, a send was initiated with screenshot; spawn API call after capture completes.
    pub send_pending_capture: bool,
    /// If true, the pending capture is for translation (not a normal screenshot send).
    pub translate_pending: bool,
    /// Active AI provider (set from config, overridden by UI dropdown).
    pub active_provider: Provider,
    /// Localhost proxy port (read from proxy.port file at init).
    pub proxy_port: Option<u16>,
    /// Bearer token for proxy auth (read from proxy.port file at init).
    pub proxy_token: Option<String>,
    /// Which CLI providers are available (populated from proxy /health endpoint).
    #[allow(dead_code)] // Used by provider dispatch (Task 4) and UI dropdown (Task 5).
    pub proxy_providers: HashSet<Provider>,
}

impl AppState {
    /// Returns true if the given provider is usable right now.
    /// Gemini requires a direct API key; Claude/OpenAI need an active proxy.
    #[allow(dead_code)] // Used by UI dropdown (Task 5) and provider dispatch (Task 4).
    pub fn is_provider_available(&self, provider: Provider) -> bool {
        match provider {
            Provider::Gemini => !crate::config::CONFIG.api.gemini.key.is_empty(),
            Provider::Claude | Provider::Openai => self.proxy_providers.contains(&provider),
        }
    }

    /// Push a message, evicting the oldest if the cap is exceeded.
    /// Always ensures the first message is a User message after eviction.
    pub fn push_message(&mut self, msg: ChatMessage) {
        self.messages.push(msg);
        if self.messages.len() > MAX_STORED_MESSAGES {
            let excess = self.messages.len() - MAX_STORED_MESSAGES;
            self.messages.drain(..excess);
            // Ensure we start with a User message
            while self.messages.first().is_some_and(|m| m.role == MessageRole::Assistant) {
                self.messages.remove(0);
            }
        }
    }
}

pub static STATE: Lazy<Mutex<AppState>> = Lazy::new(|| Mutex::new(AppState::default()));
