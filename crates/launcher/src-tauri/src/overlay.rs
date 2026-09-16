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
pub(crate) struct GameInfo {
    /// Native window handle, stored as i64 so it crosses the serde/IPC boundary.
    pub(crate) hwnd: i64,
    pub(crate) pid: u32,
    pub(crate) exe: String,
    pub(crate) title: String,
}

/// Remembers the game window that had focus before the overlay was shown, so
/// focus can be handed back when the overlay hides.
#[derive(Default)]
pub(crate) struct OverlayState {
    pub(crate) game: parking_lot::Mutex<Option<GameInfo>>,
}

/// Toggle the overlay window hidden <-> interactive. On hide, hand focus back to
/// the stored game.
pub(crate) fn toggle(app: &AppHandle) {
    let Some(overlay) = app.get_webview_window("overlay") else {
        return;
    };

    if overlay.is_visible().unwrap_or(false) {
        hide(app);
    } else {
        show_overlay(app);
    }
}

/// Hide the overlay and hand focus back to the stored target.
///
/// The overlay's own close button must route through here rather than calling
/// `getCurrentWindow().hide()` from JS: that path skips the handoff, so focus
/// lands wherever Windows picks next instead of returning to the game.
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects the handle by value"
)]
pub(crate) fn hide_overlay(app: AppHandle) {
    hide(&app);
}

fn hide(app: &AppHandle) {
    let Some(overlay) = app.get_webview_window("overlay") else {
        return;
    };
    crate::util::log_if_err("hide overlay", overlay.hide());
    if let Some(game) = live_game(app) {
        focus_window(game.hwnd);
    }
}

/// Show the overlay (if hidden) and fire an action event to the overlay UI, e.g.
/// `translate-request` or `quick-ask` from a global hotkey.
pub(crate) fn trigger(app: &AppHandle, event: &str) {
    let Some(overlay) = app.get_webview_window("overlay") else {
        return;
    };
    if overlay.is_visible().unwrap_or(false) {
        // Already visible does not mean focused: the overlay is always-on-top,
        // so it stays on screen after the user clicks back into the game.
        // Without this the quick-ask input appears while keystrokes still go to
        // the game as movement keys.
        crate::util::log_if_err("focus overlay", overlay.set_focus());
    } else {
        show_overlay(app);
    }
    crate::util::log_if_err("emit overlay action", app.emit_to("overlay", event, ()));
}

/// Capture the current foreground window (the game) BEFORE the overlay steals
/// focus, store it, then show + focus the overlay and report detection to the UI.
#[expect(
    clippy::option_if_let_else,
    reason = "the suggested map_or_else needs a side-effecting closure and reads worse"
)]
fn show_overlay(app: &AppHandle) {
    let Some(overlay) = app.get_webview_window("overlay") else {
        return;
    };
    let detected = foreground_game(std::process::id());
    // A detection miss must not erase what we already had -- alt-tabbing to the
    // launcher itself returns None, and clobbering on that left the user unable
    // to translate a game still running on another monitor. Keep the previous
    // target when it is still alive, drop it when it is not.
    let game = match detected {
        Some(game) => {
            if let Some(state) = app.try_state::<OverlayState>() {
                *state.game.lock() = Some(game.clone());
            }
            Some(game)
        }
        None => live_game(app),
    };
    crate::util::log_if_err("show overlay", overlay.show());
    crate::util::log_if_err("focus overlay", overlay.set_focus());
    // A null payload tells the overlay UI "no game detected".
    crate::util::log_if_err(
        "emit overlay-status",
        app.emit_to("overlay", "overlay-status", game),
    );
}

#[cfg(windows)]
fn foreground_game(self_pid: u32) -> Option<GameInfo> {
    imp::foreground_game(self_pid)
}

#[cfg(not(windows))]
const fn foreground_game(_self_pid: u32) -> Option<GameInfo> {
    None
}

#[cfg(windows)]
fn focus_window(hwnd: i64) {
    imp::focus_window(hwnd);
}

#[cfg(not(windows))]
const fn focus_window(_hwnd: i64) {}

#[cfg(windows)]
fn is_live_window(hwnd: i64, pid: u32) -> bool {
    imp::is_live_window(hwnd, pid)
}

#[cfg(not(windows))]
const fn is_live_window(_hwnd: i64, _pid: u32) -> bool {
    false
}

/// The stored target, but only if its window is still alive and still owned by
/// the process we recorded. Clears the slot otherwise, so a recycled handle can
/// never be screenshotted, uploaded or handed focus.
pub(crate) fn live_game(app: &AppHandle) -> Option<GameInfo> {
    let state = app.try_state::<OverlayState>()?;
    let mut slot = state.game.lock();
    let game = slot.clone()?;
    if is_live_window(game.hwnd, game.pid) {
        return Some(game);
    }
    tracing::info!("Stored target window {} is gone; clearing", game.hwnd);
    *slot = None;
    None
}

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
        GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow,
        SetForegroundWindow, ShowWindow, SW_RESTORE,
    };

    pub(super) fn foreground_game(self_pid: u32) -> Option<GameInfo> {
        // SAFETY: all calls take the handle Windows just returned to us and
        // buffers owned by this frame; GetForegroundWindow may return null,
        // which is checked before the handle is used.
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.0.is_null() {
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

    fn exe_path(pid: u32) -> Option<String> {
        // SAFETY: OpenProcess returns a handle we own and close below; the
        // buffer and length live for the whole call.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
        let mut buf = [0u16; 1024];
        let mut len = u32::try_from(buf.len()).unwrap_or(0);
        // SAFETY: `handle` is live, and `buf`/`len` outlive the call.
        let res = unsafe {
            QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &raw mut len,
            )
        };
        // SAFETY: `handle` came from OpenProcess above and is not used again.
        crate::util::log_if_err("CloseHandle(process)", unsafe { CloseHandle(handle) });
        res.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }

    fn to_hwnd(hwnd: i64) -> HWND {
        HWND(usize::try_from(hwnd).unwrap_or(0) as *mut core::ffi::c_void)
    }

    /// Whether `hwnd` is still a live window owned by `pid`.
    ///
    /// Windows recycles HWND values, so a handle stored when the overlay opened
    /// can later name a completely different window -- which would then be the
    /// one screenshotted and uploaded, or the one handed focus. The pid was
    /// already captured alongside it and went unused; this is what it is for.
    pub(super) fn is_live_window(hwnd: i64, pid: u32) -> bool {
        // SAFETY: `handle` is a plain window handle; IsWindow tolerates a stale
        // or recycled value, which is exactly what this check is for.
        unsafe {
            let handle = to_hwnd(hwnd);
            if !IsWindow(Some(handle)).as_bool() {
                return false;
            }
            let mut current = 0u32;
            GetWindowThreadProcessId(handle, Some(&raw mut current));
            current != 0 && current == pid
        }
    }

    pub(super) fn focus_window(hwnd: i64) {
        // SAFETY: the window-manager calls below take a handle by value and
        // report failure through their return value; a stale handle is not UB.
        unsafe {
            let handle = to_hwnd(hwnd);
            // A minimized target is not restored by SetForegroundWindow alone.
            if IsIconic(handle).as_bool() {
                let _ = ShowWindow(handle, SW_RESTORE);
            }
            if !SetForegroundWindow(handle).as_bool() {
                tracing::warn!("SetForegroundWindow failed for hwnd {hwnd}");
            }
        }
    }
}
