mod api;
mod capture;
mod config;
mod game_detect;
mod logging;
mod state;
mod ui;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use hudhook::hooks::dx11::ImguiDx11Hooks;
use hudhook::hooks::dx12::ImguiDx12Hooks;
use hudhook::windows::Win32::Foundation::HINSTANCE;
use hudhook::windows::Win32::System::LibraryLoader::GetModuleHandleA;
use hudhook::windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use hudhook::windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_F9};
use hudhook::*;
use once_cell::sync::Lazy;
use tokio::runtime::Runtime;
use tracing::info;

use crate::config::{GraphicsApi, DLL_HINSTANCE, CONFIG};
use crate::state::STATE;

/// Set to true once render() is called, confirming hooks are active.
static RENDER_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Global tokio runtime for async API work. 2 worker threads.
static RUNTIME: Lazy<Runtime> = Lazy::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime")
});

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
    logged_first_render: bool,
}

impl CompanionRenderLoop {
    fn new() -> Self {
        Self {
            f9_was_pressed: false,
            logged_first_render: false,
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

    fn render(&mut self, ui: &mut imgui::Ui) {
        if !self.logged_first_render {
            RENDER_ACTIVE.store(true, Ordering::SeqCst);
            info!("render() called — hooks are active!");
            self.logged_first_render = true;
        }

        // --- Hotkey toggle (F9) with rising-edge debounce ---
        let f9_pressed = unsafe { GetAsyncKeyState(VK_F9.0 as i32) } & (1 << 15) != 0;
        if f9_pressed && !self.f9_was_pressed {
            let mut state = STATE.lock();
            state.visible = !state.visible;
        }
        self.f9_was_pressed = f9_pressed;

        // --- Draw UI if visible ---
        let visible = STATE.lock().visible;
        if visible {
            ui::draw_panel(ui);
        }
    }

    fn message_filter(&self, _io: &imgui::Io) -> MessageFilter {
        let visible = STATE.lock().visible;
        if visible {
            MessageFilter::InputAll
        } else {
            MessageFilter::empty()
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
        // Save HINSTANCE before spawning — needed by config.rs to find config.toml
        let _ = DLL_HINSTANCE.set(hmodule);

        std::thread::spawn(move || {
            // Set up tracing FIRST so we capture hudhook's internal logs.
            init_tracing();
            info!("DllMain: thread started");

            // Wait for DXGI — required by both DX12 and DX11.
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
                        info!("ERROR: No supported graphics API detected — ejecting");
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
                    info!("DX9 detected but not yet supported — ejecting");
                    eject();
                    return;
                }
                GraphicsApi::Opengl => {
                    info!("OpenGL detected but not yet supported — ejecting");
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
                if RENDER_ACTIVE.load(Ordering::SeqCst) {
                    info!("Hooks confirmed active after {i}s");
                    break;
                }
                info!("Waiting for first render call... {i}s");
            }
            if !RENDER_ACTIVE.load(Ordering::SeqCst) {
                info!("WARNING: render() not called after 10s — hooks may not be intercepting Present()");
            }

            // Park thread indefinitely so nothing gets dropped
            // (destructors can crash the host game).
            loop {
                std::thread::park();
            }
        });
    }
}
