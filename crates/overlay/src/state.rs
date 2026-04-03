use std::collections::HashSet;

use once_cell::sync::Lazy;
use parking_lot::Mutex;

use crate::provider::Provider;

/// Maximum messages kept in memory. Oldest are evicted when exceeded.
/// This bounds per-frame clone cost and prevents unbounded memory growth.
const MAX_STORED_MESSAGES: usize = 100;

/// Replace common Unicode characters with ASCII equivalents so ImGui's
/// default font (Latin-1 only) can render them. Without this, em dashes,
/// smart quotes, trademark symbols etc. show as "?" and corrupt nearby glyphs.
pub fn sanitize_for_imgui(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            // Dashes
            '\u{2014}' => out.push_str("--"), // em dash
            '\u{2013}' => out.push('-'),      // en dash
            '\u{2015}' => out.push_str("--"), // horizontal bar
            // Quotes
            '\u{201C}' | '\u{201D}' => out.push('"'), // left/right double quote
            '\u{2018}' | '\u{2019}' => out.push('\''), // left/right single quote
            '\u{201E}' | '\u{201F}' => out.push('"'), // double low-9 / high-reversed-9
            '\u{00AB}' | '\u{00BB}' => out.push('"'), // guillemets
            // Symbols
            '\u{2122}' => out.push_str("(TM)"), // trademark
            '\u{00A9}' => out.push_str("(C)"),  // copyright
            '\u{00AE}' => out.push_str("(R)"),  // registered
            '\u{2026}' => out.push_str("..."),  // ellipsis
            '\u{2022}' => out.push_str("* "),   // bullet
            '\u{00B7}' => out.push_str("* "),   // middle dot
            // Spaces
            '\u{00A0}' => out.push(' '), // non-breaking space
            '\u{2009}' => out.push(' '), // thin space
            '\u{200B}' => {}             // zero-width space (drop)
            // Arrows
            '\u{2192}' => out.push_str("->"),  // right arrow
            '\u{2190}' => out.push_str("<-"),  // left arrow
            '\u{2194}' => out.push_str("<->"), // left-right arrow
            // Math
            '\u{2264}' => out.push_str("<="), // less-than or equal
            '\u{2265}' => out.push_str(">="), // greater-than or equal
            '\u{2260}' => out.push_str("!="), // not equal
            '\u{00D7}' => out.push('x'),      // multiplication sign
            '\u{00F7}' => out.push('/'),      // division sign
            // Drop C0/C1 control characters (NUL truncates ImGui C strings,
            // others produce garbled glyphs). Preserve \n and \t which ImGui handles.
            _ if ch.is_ascii_control() => {
                if ch == '\n' || ch == '\t' {
                    out.push(ch);
                }
            }
            // Pass through ASCII printable and Latin-1 printable range as-is
            _ if ch.is_ascii() || ('\u{00A1}'..='\u{00FF}').contains(&ch) => out.push(ch),
            // Everything else outside Latin-1: replace with '?'
            _ => out.push('?'),
        }
    }
    out
}

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
        Self {
            role,
            content,
            is_translation: false,
        }
    }

    pub fn translation(role: MessageRole, content: String) -> Self {
        Self {
            role,
            content,
            is_translation: true,
        }
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
    pub proxy_providers: HashSet<Provider>,
    /// True if a proxy was discovered but the health check hasn't run yet.
    /// The check is deferred until the overlay is first opened so the tokio
    /// runtime doesn't start during the DX12 stabilization window.
    pub health_check_needed: bool,
    /// True once the deferred proxy health check has been attempted.
    /// Co-located with `health_check_needed` so both flags share the same
    /// lock and survive DX12 hook retries (which recreate CompanionRenderLoop).
    pub health_check_done: bool,
}

impl AppState {
    /// Returns true if the given provider is usable right now.
    /// Gemini requires a direct API key; Claude/OpenAI need an active proxy.
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
            while self
                .messages
                .first()
                .is_some_and(|m| m.role == MessageRole::Assistant)
            {
                self.messages.remove(0);
            }
        }
    }
}

pub static STATE: Lazy<Mutex<AppState>> = Lazy::new(|| Mutex::new(AppState::default()));
