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

#[derive(Default)]
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
    /// Accumulates text chunks during streaming. Rendered by UI while is_loading is true.
    /// When streaming completes, this gets moved into a ChatMessage and cleared.
    pub streaming_response: String,
    /// Detected game name, resolved once at init.
    pub game_name: Option<String>,
    /// When true, the render loop skips drawing the overlay and performs capture.
    pub capture_pending: bool,
    /// Frames to wait with overlay hidden before capturing.
    pub capture_wait_frames: u8,
    /// Captured screenshot data, ready for the async task to pick up.
    pub captured_screenshot: Option<String>,
    /// If true, a send was initiated with screenshot; spawn API call after capture completes.
    pub send_pending_capture: bool,
}

pub static STATE: Lazy<Mutex<AppState>> = Lazy::new(|| Mutex::new(AppState::default()));
