use once_cell::sync::Lazy;
use parking_lot::Mutex;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Clone)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: String,
}

pub struct AppState {
    pub visible: bool,
    pub messages: Vec<ChatMessage>,
    pub input_buffer: String,
    pub attach_screenshot: bool,
    pub is_loading: bool,
    pub error: Option<String>,
    /// Incremented on each send; async tasks compare against this before writing results.
    /// If the generation has changed (e.g. user cancelled), the task discards its result.
    pub request_generation: u64,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            visible: false,
            messages: Vec::new(),
            input_buffer: String::new(),
            attach_screenshot: false,
            is_loading: false,
            error: None,
            request_generation: 0,
        }
    }
}

pub static STATE: Lazy<Mutex<AppState>> = Lazy::new(|| Mutex::new(AppState::default()));
