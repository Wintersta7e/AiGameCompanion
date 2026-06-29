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
    /// Generation at which capture was initiated. Used by spawn_blocking to
    /// reject stale capture results after cancel+resend races.
    pub capture_generation: u64,
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
    // health_check_needed/done moved to static AtomicBool (HEALTH_CHECK_NEEDED in lib.rs)
    // to avoid locking STATE on every visible frame.
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

    /// Invalidates any in-flight request: bumps `request_generation` so async
    /// results are dropped, and resets every "in-flight" flag (loading state,
    /// streaming buffer, capture state machine, CAPTURE_ACTIVE atomic).
    /// Returns `(provider, old_generation)` so the caller can fire
    /// `proxy_client::send_cancel` after dropping the lock. Does NOT touch
    /// `messages`, `input_buffer`, or `error` -- callers customize those.
    pub fn cancel_in_flight(&mut self) -> (Provider, u64) {
        let snapshot = (self.active_provider, self.request_generation);
        self.request_generation += 1;
        self.is_loading = false;
        self.streaming_response.clear();
        self.capture_pending = false;
        self.capture_complete = false;
        self.send_pending_capture = false;
        self.translate_pending = false;
        self.captured_screenshot = None;
        crate::CAPTURE_ACTIVE.store(false, std::sync::atomic::Ordering::Release);
        snapshot
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

#[cfg(test)]
mod tests {
    use super::*;

    fn user(content: &str) -> ChatMessage {
        ChatMessage::new(MessageRole::User, content.to_owned())
    }
    fn assistant(content: &str) -> ChatMessage {
        ChatMessage::new(MessageRole::Assistant, content.to_owned())
    }

    // ---------------- sanitize_for_imgui ----------------

    #[test]
    fn sanitize_replaces_unicode_with_ascii() {
        assert_eq!(sanitize_for_imgui("a\u{2014}b"), "a--b"); // em dash
        assert_eq!(sanitize_for_imgui("\u{201C}hi\u{201D}"), "\"hi\""); // smart quotes
        assert_eq!(sanitize_for_imgui("x\u{2026}"), "x..."); // ellipsis
        assert_eq!(sanitize_for_imgui("a\u{2192}b"), "a->b"); // right arrow
        assert_eq!(sanitize_for_imgui("n\u{2265}1"), "n>=1"); // >=
    }

    #[test]
    fn sanitize_preserves_ascii_newlines_and_tabs() {
        assert_eq!(sanitize_for_imgui("plain text\n\t!"), "plain text\n\t!");
    }

    #[test]
    fn sanitize_drops_nul_and_replaces_unknown_glyphs() {
        assert_eq!(sanitize_for_imgui("a\u{0}b"), "ab"); // NUL dropped
        assert_eq!(sanitize_for_imgui("\u{1F600}"), "?"); // emoji outside Latin-1
    }

    // ---------------- push_message ----------------

    #[test]
    fn push_message_keeps_everything_under_cap() {
        let mut s = AppState::default();
        for i in 0..MAX_STORED_MESSAGES {
            s.push_message(user(&format!("m{i}")));
        }
        assert_eq!(s.messages.len(), MAX_STORED_MESSAGES);
    }

    #[test]
    fn push_message_evicts_oldest_over_cap() {
        let mut s = AppState::default();
        for i in 0..=MAX_STORED_MESSAGES {
            s.push_message(user(&format!("m{i}")));
        }
        assert_eq!(s.messages.len(), MAX_STORED_MESSAGES);
        assert_eq!(s.messages.first().unwrap().content, "m1"); // m0 evicted
        assert_eq!(
            s.messages.last().unwrap().content,
            format!("m{MAX_STORED_MESSAGES}")
        );
    }

    #[test]
    fn push_message_ensures_first_is_user_after_eviction() {
        let mut s = AppState::default();
        // Arrange so evicting the oldest leaves an Assistant at the front,
        // which the eviction logic must then also drop.
        s.push_message(user("first"));
        s.push_message(assistant("orphan"));
        for i in 0..(MAX_STORED_MESSAGES - 1) {
            s.push_message(user(&format!("u{i}")));
        }
        assert!(
            s.messages.first().unwrap().role == MessageRole::User,
            "history must start with a User message"
        );
        assert!(
            !s.messages.iter().any(|m| m.content == "orphan"),
            "leading Assistant message should be removed after eviction"
        );
    }

    // ---------------- cancel_in_flight ----------------

    #[test]
    fn cancel_in_flight_bumps_generation_and_resets_all_flags() {
        let mut s = AppState {
            request_generation: 7,
            is_loading: true,
            streaming_response: "partial".to_owned(),
            capture_pending: true,
            capture_complete: true,
            send_pending_capture: true,
            translate_pending: true,
            captured_screenshot: Some("img".to_owned()),
            ..Default::default()
        };

        let (_provider, old_gen) = s.cancel_in_flight();

        assert_eq!(old_gen, 7);
        assert_eq!(s.request_generation, 8);
        assert!(!s.is_loading);
        assert!(s.streaming_response.is_empty());
        assert!(!s.capture_pending);
        assert!(!s.capture_complete);
        assert!(!s.send_pending_capture);
        assert!(!s.translate_pending);
        assert!(s.captured_screenshot.is_none());
    }
}
