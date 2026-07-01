//! External (no-injection) overlay companion: foreground-game detection and the
//! show/focus/hide state machine driven by the global toggle hotkey.
//!
//! The Win32 specifics compile only on Windows; on other hosts (the launcher's
//! pure-logic tests run on Linux) the helpers degrade to no-ops so the crate
//! still builds.

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

/// Snapshot of the foreground game window at the moment the overlay was opened.
#[derive(Clone, Debug, Default, Serialize)]
pub struct GameInfo {
    /// Native window handle, stored as i64 so it crosses the serde/IPC boundary.
    pub hwnd: i64,
    pub pid: u32,
    pub exe: String,
    pub title: String,
}

/// Remembers the game window that had focus before the overlay was shown, so
/// focus can be handed back when the overlay hides.
#[derive(Default)]
pub struct OverlayState {
    pub game: parking_lot::Mutex<Option<GameInfo>>,
}

/// Start a Gemini request and stream its result back to the overlay window.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands deserialize owned values.
pub fn ask_sage(app: AppHandle, prompt: String) {
    tauri::async_runtime::spawn(async move {
        let result = async {
            let config = crate::ai::load_gemini_config()?;
            crate::ai::stream_gemini(prompt, config.api_key, config.model, |chunk| {
                app.emit_to("overlay", "sage-token", chunk)
                    .map_err(|error| format!("failed to emit response token: {error}"))
            })
            .await
        }
        .await;

        match result {
            Ok(()) => {
                if let Err(error) = app.emit_to("overlay", "sage-done", ()) {
                    tracing::warn!("failed to emit sage-done: {error}");
                }
            }
            Err(message) => {
                if let Err(error) = app.emit_to("overlay", "sage-error", message) {
                    tracing::warn!("failed to emit sage-error: {error}");
                }
            }
        }
    });
}

/// Capture the last foreground game window to a temporary PNG file.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri command state is injected as an owned handle.
pub fn capture_game(app: AppHandle) -> Result<String, String> {
    let hwnd = app
        .state::<OverlayState>()
        .game
        .lock()
        .as_ref()
        .map(|game| game.hwnd)
        .ok_or_else(|| "no game detected -- open the overlay over a game first".to_owned())?;

    let png = crate::overlay_capture::capture_window_png(hwnd)?;
    let byte_count = png.len();
    let path = std::env::temp_dir().join("sage-capture.png");
    std::fs::write(&path, png)
        .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
    Ok(format!("captured {byte_count} bytes -> {}", path.display()))
}

/// Toggle the overlay window hidden <-> interactive.
///
/// On show: capture the current foreground window (the game) BEFORE the overlay
/// steals focus, store it, then show + focus the overlay and report what we
/// detected to the overlay UI. On hide: hand focus back to the stored game.
pub fn toggle(app: &AppHandle) {
    let Some(overlay) = app.get_webview_window("overlay") else {
        return;
    };

    if overlay.is_visible().unwrap_or(false) {
        let _ = overlay.hide();
        if let Some(state) = app.try_state::<OverlayState>() {
            if let Some(game) = state.game.lock().clone() {
                focus_window(game.hwnd);
            }
        }
    } else {
        let game = foreground_game(std::process::id());
        if let Some(state) = app.try_state::<OverlayState>() {
            (*state.game.lock()).clone_from(&game);
        }
        let _ = overlay.show();
        let _ = overlay.set_focus();
        // A null payload tells the overlay UI "no game detected".
        let _ = app.emit_to("overlay", "overlay-status", game);
    }
}

#[cfg(windows)]
fn foreground_game(self_pid: u32) -> Option<GameInfo> {
    imp::foreground_game(self_pid)
}

#[cfg(not(windows))]
fn foreground_game(_self_pid: u32) -> Option<GameInfo> {
    None
}

#[cfg(windows)]
fn focus_window(hwnd: i64) {
    imp::focus_window(hwnd);
}

#[cfg(not(windows))]
fn focus_window(_hwnd: i64) {}

#[cfg(windows)]
mod imp {
    use super::GameInfo;
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HWND};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId, SetForegroundWindow,
    };

    pub fn foreground_game(self_pid: u32) -> Option<GameInfo> {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.0 == 0 {
                return None;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&raw mut pid));
            if pid == 0 || pid == self_pid {
                return None;
            }
            let exe = exe_path(pid).unwrap_or_default();
            let mut buf = [0u16; 512];
            let n = GetWindowTextW(hwnd, &mut buf);
            let title = String::from_utf16_lossy(&buf[..usize::try_from(n).unwrap_or(0)]);
            Some(GameInfo {
                hwnd: hwnd.0 as i64,
                pid,
                exe,
                title,
            })
        }
    }

    unsafe fn exe_path(pid: u32) -> Option<String> {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = u32::try_from(buf.len()).unwrap_or(0);
        let res = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &raw mut len,
        );
        let _ = CloseHandle(handle);
        res.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }

    pub fn focus_window(hwnd: i64) {
        unsafe {
            let _ = SetForegroundWindow(HWND(isize::try_from(hwnd).unwrap_or(0)));
        }
    }
}
