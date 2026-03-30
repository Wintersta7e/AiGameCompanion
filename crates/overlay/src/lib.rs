mod api;
mod capture;
mod config;
mod game_detect;
mod logging;
mod provider;
mod proxy_client;
mod state;
mod translation;
mod ui;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use hudhook::hooks::dx11::ImguiDx11Hooks;
use hudhook::hooks::dx12::ImguiDx12Hooks;
use hudhook::windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, WPARAM};
use hudhook::windows::Win32::System::LibraryLoader::GetModuleHandleA;
use hudhook::windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use hudhook::windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_F9};
use hudhook::windows::Win32::UI::WindowsAndMessaging::{
    ClipCursor, GetClipCursor, SetCursor, LoadCursorW, IDC_ARROW,
    WM_SETCURSOR, HTCLIENT,
};
use hudhook::*;
use once_cell::sync::Lazy;
use tokio::runtime::Runtime;
use tracing::{error, info};

use crate::config::{GraphicsApi, DLL_HINSTANCE, CONFIG, parse_vk_code};
use crate::state::{ChatMessage, MessageRole, STATE};

/// Set to true once render() is called, confirming hooks are active.
static RENDER_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Mirrors `STATE.visible` without requiring a lock. Updated under lock,
/// read lock-free on every frame to avoid unnecessary mutex contention.
static OVERLAY_VISIBLE: AtomicBool = AtomicBool::new(false);

/// Global tokio runtime for async API work. 2 worker threads.
/// Uses `OnceLock` + explicit error handling instead of `expect()` to avoid
/// panicking inside the host game process.
static RUNTIME: Lazy<Option<Runtime>> = Lazy::new(|| {
    match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => Some(rt),
        Err(e) => {
            error!("Failed to create tokio runtime: {e}. API features disabled.");
            None
        }
    }
});

/// Spawn an async API request on the tokio runtime.
/// Used by both ui.rs (no-screenshot path) and lib.rs (post-capture path).
pub(crate) fn spawn_api_request(
    gen: u64,
    messages: Vec<ChatMessage>,
    screenshot: Option<String>,
) {
    let Some(rt) = RUNTIME.as_ref() else {
        let mut state = STATE.lock();
        state.error = Some("API unavailable: tokio runtime failed to start.".into());
        state.is_loading = false;
        return;
    };

    // Capture last user message now (O(1) — it's always at the end) to avoid
    // a linear scan inside the lock after the async task completes.
    let last_user_msg = messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::User)
        .map(|m| m.content.clone())
        .unwrap_or_default();

    rt.spawn(async move {
        let result = api::send_message(messages, screenshot, gen).await;
        let mut state = STATE.lock();
        if state.request_generation == gen {
            match result {
                Ok(response) => {
                    state.push_message(ChatMessage::new(
                        MessageRole::Assistant,
                        response.clone(),
                    ));
                    state.streaming_response.clear();
                    state.is_loading = false;
                    // Drop the lock BEFORE file I/O
                    drop(state);
                    logging::log_exchange(&last_user_msg, &response);
                }
                Err(err) => {
                    if !state.streaming_response.is_empty() {
                        let partial = state.streaming_response.clone();
                        state.streaming_response.clear();
                        state.push_message(ChatMessage::new(
                            MessageRole::Assistant,
                            partial,
                        ));
                    }
                    state.error = Some(err);
                    state.is_loading = false;
                }
            }
        }
    });
}

/// Set up tracing-subscriber to write to companion.log next to the DLL.
/// Captures both our logs and hudhook's internal tracing (Present hook,
/// pipeline init, command queue matching, etc.).
fn init_tracing() {
    let Some(dir) = config::dll_directory() else { return };
    let file_appender = tracing_appender::rolling::never(dir, "companion.log");
    let subscriber = tracing_subscriber::fmt()
        .with_writer(file_appender)
        .with_ansi(false)
        .with_target(true)
        .with_max_level(tracing::Level::INFO)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);
}

/// Read the proxy.port file from the DLL directory.
/// Returns `(port, token)` if the file exists and has both lines.
fn read_proxy_port_file() -> Option<(u16, String)> {
    let dir = config::dll_directory()?;
    let path = dir.join("proxy.port");
    let contents = std::fs::read_to_string(&path).ok()?;
    let mut lines = contents.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let token = lines.next()?.trim().to_string();
    if token.is_empty() {
        return None;
    }
    Some((port, token))
}

/// Check if a module (DLL) is loaded in the current process.
fn is_module_loaded(name: &str) -> bool {
    let Ok(cname) = std::ffi::CString::new(name) else { return false };
    unsafe {
        GetModuleHandleA(hudhook::windows::core::PCSTR(cname.as_ptr() as *const u8))
            .is_ok()
    }
}

/// Auto-detect the graphics API by probing loaded DLLs.
/// Priority: DX12 > DX11 > DX9 > OpenGL (some games load multiple).
fn detect_graphics_api() -> Option<GraphicsApi> {
    if is_module_loaded("d3d12.dll") {
        info!("Detected d3d12.dll");
        return Some(GraphicsApi::Dx12);
    }
    if is_module_loaded("d3d11.dll") {
        info!("Detected d3d11.dll");
        return Some(GraphicsApi::Dx11);
    }
    if is_module_loaded("d3d9.dll") {
        info!("Detected d3d9.dll");
        return Some(GraphicsApi::Dx9);
    }
    if is_module_loaded("opengl32.dll") {
        info!("Detected opengl32.dll");
        return Some(GraphicsApi::Opengl);
    }
    None
}

struct CompanionRenderLoop {
    f9_was_pressed: bool,
    translate_was_pressed: bool,
    logged_first_render: bool,
    /// Saved cursor clip rect from the game, restored when the overlay hides.
    saved_clip_rect: Option<hudhook::windows::Win32::Foundation::RECT>,
    /// Whether we previously had the overlay visible (for edge-triggered clip/cursor logic).
    was_visible: bool,
}

impl CompanionRenderLoop {
    fn new() -> Self {
        Self {
            f9_was_pressed: false,
            translate_was_pressed: false,
            logged_first_render: false,
            saved_clip_rect: None,
            was_visible: false,
        }
    }
}

impl ImguiRenderLoop for CompanionRenderLoop {
    fn initialize<'a>(
        &'a mut self,
        ctx: &mut imgui::Context,
        _render_context: &'a mut dyn hudhook::RenderContext,
    ) {
        // Scale the default font for high-res displays.
        // The display size isn't available yet in initialize(), so we read
        // the desktop resolution via GetSystemMetrics.
        let screen_w = unsafe {
            hudhook::windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                hudhook::windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN,
            )
        } as f32;
        let scale = (screen_w / 1920.0).max(1.0);
        let font_size = 18.0 * scale;
        info!("Screen width {screen_w}, UI scale {scale:.2}x, font size {font_size:.0}px");
        ctx.fonts().add_font(&[imgui::FontSource::DefaultFontData {
            config: Some(imgui::FontConfig {
                size_pixels: font_size,
                ..Default::default()
            }),
        }]);
        ctx.style_mut().scale_all_sizes(scale);
    }

    fn before_render<'a>(
        &'a mut self,
        ctx: &mut imgui::Context,
        _render_context: &'a mut dyn hudhook::RenderContext,
    ) {
        let visible = OVERLAY_VISIBLE.load(Ordering::Acquire);
        let capturing = STATE.lock().capture_pending;

        // ImGui software cursor -- games hide the hardware cursor via
        // SetCursor(NULL) on every WM_SETCURSOR, so we draw our own.
        // Disable during capture so the cursor doesn't burn into screenshots.
        ctx.io_mut().mouse_draw_cursor = visible && !capturing;

        // Edge-triggered: save/restore the game's ClipCursor rect.
        if visible && !self.was_visible {
            // Overlay just opened -- save and release cursor clip.
            let mut rect = hudhook::windows::Win32::Foundation::RECT::default();
            if unsafe { GetClipCursor(&mut rect) }.is_ok() {
                self.saved_clip_rect = Some(rect);
            }
            unsafe { let _ = ClipCursor(None); }
        } else if !visible && self.was_visible {
            // Overlay just closed -- restore game's cursor clip.
            if let Some(ref rect) = self.saved_clip_rect.take() {
                unsafe { let _ = ClipCursor(Some(rect)); }
            }
        }
        self.was_visible = visible;
    }

    fn render(&mut self, ui: &mut imgui::Ui) {
        if !self.logged_first_render {
            RENDER_ACTIVE.store(true, Ordering::Relaxed);
            info!("render() called -- hooks are active!");
            self.logged_first_render = true;
        }

        // --- Hotkey toggle with rising-edge debounce ---
        // Use configured hotkey (default F9), fall back to F9 if unparseable.
        let toggle_vk = parse_vk_code(&CONFIG.overlay.hotkey).unwrap_or(VK_F9.0 as i32);
        let toggle_pressed = unsafe { GetAsyncKeyState(toggle_vk) } & (1 << 15) != 0;
        if toggle_pressed && !self.f9_was_pressed {
            let mut state = STATE.lock();
            state.visible = !state.visible;
            OVERLAY_VISIBLE.store(state.visible, Ordering::Release);
        }
        self.f9_was_pressed = toggle_pressed;

        // --- Translate hotkey (F10 default) with rising-edge debounce ---
        if CONFIG.translation.enabled {
            if let Some(vk) = parse_vk_code(&CONFIG.overlay.translate_hotkey) {
                let translate_pressed = unsafe { GetAsyncKeyState(vk) } & (1 << 15) != 0;
                if translate_pressed && !self.translate_was_pressed {
                    let mut state = STATE.lock();
                    // Only trigger if not already loading and no capture in progress
                    if !state.is_loading && !state.capture_pending {
                        state.push_message(ChatMessage::translation(
                            MessageRole::User,
                            "[Translate screen]".into(),
                        ));
                        state.is_loading = true;
                        state.error = None;
                        state.request_generation += 1;
                        state.streaming_response.clear();
                        state.capture_pending = true;
                        state.capture_wait_frames = 2;
                        state.captured_screenshot = None;
                        state.send_pending_capture = true;
                        state.translate_pending = true;
                        state.visible = true;
                        OVERLAY_VISIBLE.store(true, Ordering::Release);
                    }
                }
                self.translate_was_pressed = translate_pressed;
            }
        }

        // --- Hide-capture-show ---
        {
            let mut state = STATE.lock();
            if state.capture_pending {
                if state.capture_wait_frames > 0 {
                    state.capture_wait_frames -= 1;
                    return; // skip drawing, let clean frame render
                }
                // Waited enough frames. Capture now (off render thread via spawn_blocking).
                state.capture_pending = false;
                drop(state);

                if let Some(rt) = RUNTIME.as_ref() {
                    rt.spawn_blocking(move || {
                        let result = std::panic::catch_unwind(|| {
                            capture::capture_screenshot()
                        });
                        let mut state = STATE.lock();
                        state.captured_screenshot = match result {
                            Ok(screenshot) => screenshot,
                            Err(_) => {
                                error!("Screenshot capture panicked");
                                None
                            }
                        };
                        state.capture_complete = true;
                    });
                } else {
                    // No runtime -- reset state so we don't get stuck
                    let mut state = STATE.lock();
                    state.send_pending_capture = false;
                    state.translate_pending = false;
                    state.is_loading = false;
                    state.error = Some("Screenshot unavailable: tokio runtime not started.".into());
                }
                return; // don't draw this frame
            }

            // Check if we need to spawn an API call after capture completed
            if state.send_pending_capture && state.capture_complete {
                state.send_pending_capture = false;
                state.capture_complete = false;
                let is_translate = state.translate_pending;
                state.translate_pending = false;
                // Extract data under lock, then drop before cloning messages
                let gen = state.request_generation;
                let screenshot = state.captured_screenshot.take();
                let messages = state.messages.clone();

                if screenshot.is_none() {
                    state.error = Some("Screenshot capture failed -- sending text only.".into());
                }

                drop(state);

                if is_translate {
                    translation::spawn_translate_request(gen, messages, screenshot);
                } else {
                    spawn_api_request(gen, messages, screenshot);
                }
            }
        }

        // --- Draw UI if visible (lock-free check) ---
        if OVERLAY_VISIBLE.load(Ordering::Acquire) {
            ui::draw_panel(ui);
        }
    }

    fn message_filter(&self, _io: &imgui::Io) -> MessageFilter {
        if OVERLAY_VISIBLE.load(Ordering::Acquire) {
            MessageFilter::InputAll
        } else {
            MessageFilter::empty()
        }
    }

    fn on_wnd_proc(&self, _hwnd: HWND, umsg: u32, _wparam: WPARAM, lparam: LPARAM) {
        // Games constantly call SetCursor(NULL) via WM_SETCURSOR to hide the
        // hardware cursor. When the overlay is visible, force the arrow cursor
        // so the user can see where they're clicking. The ImGui software cursor
        // (mouse_draw_cursor) is the primary cursor, but setting the hardware
        // cursor too avoids a brief flicker when the OS processes WM_SETCURSOR
        // before our frame renders.
        if umsg == WM_SETCURSOR
            && OVERLAY_VISIBLE.load(Ordering::Acquire)
            && (lparam.0 as u32 & 0xFFFF) == HTCLIENT
        {
            if let Ok(arrow) = unsafe { LoadCursorW(None, IDC_ARROW) } {
                unsafe { SetCursor(arrow); }
            }
        }
    }
}

/// # Safety
/// Called by the OS loader. `hmodule` must be a valid HINSTANCE for this DLL.
#[no_mangle]
#[allow(non_snake_case)]
pub unsafe extern "system" fn DllMain(
    hmodule: HINSTANCE,
    reason: u32,
    _: *mut std::ffi::c_void,
) {
    if reason == DLL_PROCESS_ATTACH {
        // Save HINSTANCE before spawning -- needed by config.rs to find config.toml.
        // If already set, this is a duplicate DLL_PROCESS_ATTACH -- bail.
        if DLL_HINSTANCE.set(hmodule).is_err() {
            return;
        }

        std::thread::spawn(move || {
            // Set up tracing FIRST so we capture hudhook's internal logs.
            init_tracing();
            info!("DllMain: thread started");

            // Wait for DXGI -- required by both DX12 and DX11.
            info!("Waiting for graphics DLLs...");
            while !is_module_loaded("dxgi.dll") {
                std::thread::sleep(Duration::from_millis(100));
            }
            info!("dxgi.dll loaded");

            // Determine which graphics API to hook.
            let api = if let Some(forced) = CONFIG.overlay.graphics_api {
                info!("Config override: graphics_api = {forced}");
                forced
            } else {
                // Wait for a graphics DLL to appear (up to 15s).
                info!("Auto-detecting graphics API...");
                let mut detected = None;
                for _ in 0..150 {
                    detected = detect_graphics_api();
                    if detected.is_some() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                match detected {
                    Some(api) => {
                        info!("Auto-detected: {api}");
                        api
                    }
                    None => {
                        info!("ERROR: No supported graphics API detected -- ejecting");
                        eject();
                        return;
                    }
                }
            };

            // Give the game time to create its device and swapchain.
            info!("Waiting for swapchain creation...");
            std::thread::sleep(Duration::from_secs(2));

            // Detect game name (window title should be set by now).
            let game_name = game_detect::detect_game_name();
            if let Some(ref name) = game_name {
                info!("Game: {name}");
            }
            STATE.lock().game_name = game_name.clone();

            // Read proxy.port file (written by the launcher's proxy server).
            if let Some((port, token)) = read_proxy_port_file() {
                let mut state = STATE.lock();
                state.proxy_port = Some(port);
                state.proxy_token = Some(token);
                info!("Proxy discovered at localhost:{port}");
            } else {
                info!("No proxy.port file found -- CLI providers unavailable");
            }

            // Set the active provider from config.
            {
                let mut state = STATE.lock();
                state.active_provider = CONFIG.api.provider;
            }

            // Initialize session log
            logging::init_session_log(game_name.as_deref());

            // Build and apply hooks for the detected API.
            info!("Building {api} hooks...");
            let result = match api {
                GraphicsApi::Dx12 => {
                    let hh = Hudhook::builder()
                        .with::<ImguiDx12Hooks>(CompanionRenderLoop::new())
                        .with_hmodule(hmodule)
                        .build();
                    hh.apply()
                }
                GraphicsApi::Dx11 => {
                    let hh = Hudhook::builder()
                        .with::<ImguiDx11Hooks>(CompanionRenderLoop::new())
                        .with_hmodule(hmodule)
                        .build();
                    hh.apply()
                }
                GraphicsApi::Dx9 => {
                    info!("DX9 detected but not yet supported -- ejecting");
                    eject();
                    return;
                }
                GraphicsApi::Opengl => {
                    info!("OpenGL detected but not yet supported -- ejecting");
                    eject();
                    return;
                }
            };

            match result {
                Ok(()) => info!("apply() succeeded for {api}"),
                Err(e) => {
                    info!("apply() failed for {api}: {e:?}");
                    eject();
                    return;
                }
            }

            // Monitor: check if render() is being called within 10 seconds.
            info!("Monitoring hook activity...");
            for i in 1..=10 {
                std::thread::sleep(Duration::from_secs(1));
                if RENDER_ACTIVE.load(Ordering::Relaxed) {
                    info!("Hooks confirmed active after {i}s");
                    break;
                }
                info!("Waiting for first render call... {i}s");
            }
            if !RENDER_ACTIVE.load(Ordering::Relaxed) {
                info!("WARNING: render() not called after 10s -- hooks may not be intercepting Present()");
            }

            // Park thread indefinitely so nothing gets dropped
            // (destructors can crash the host game).
            loop {
                std::thread::park();
            }
        });
    }
}
