//! External game-process watcher for playtime tracking. Replaces the old
//! injector-lifetime tracking now that we no longer inject: after a game is
//! launched, this finds its process by executable name, waits for it to exit,
//! and adds the elapsed minutes to the game's playtime.
//!
//! Windows-only; a no-op stub elsewhere so the launcher still compiles for the
//! Linux test runner.
//!
//! Note: playtime for an in-flight session is only committed when the game
//! exits, so quitting the launcher mid-session discards that session (same as
//! the old injector-lifetime behaviour). The watcher thread is detached and
//! ends on its own when the game exits or the find window elapses.

use tauri::AppHandle;

/// Watch a launched game on a background thread: emit `game-linked` once its
/// process appears, then `game-finished` (recording playtime) when it exits.
/// If the process never appears within the find window, emit `game-finished`
/// so the UI resets to idle rather than showing a spurious error.
#[cfg(windows)]
pub fn spawn_game_watch(app: AppHandle, game_id: String, exe_name: String) {
    std::thread::spawn(move || imp::watch(&app, &game_id, &exe_name));
}

#[cfg(not(windows))]
pub fn spawn_game_watch(_app: AppHandle, _game_id: String, _exe_name: String) {}

#[cfg(windows)]
mod imp {
    use std::time::{Duration, Instant};

    use tauri::{AppHandle, Emitter, Manager};
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    use crate::state::AppState;

    /// How long to wait for the launched game process to appear before giving up.
    /// Generous because Steam may update/download the game or show a pre-launch
    /// dialog before the real process starts.
    const FIND_TIMEOUT: Duration = Duration::from_mins(10);
    const FIND_POLL: Duration = Duration::from_millis(750);
    /// `WaitForSingleObject` timeout meaning "wait forever" (0xFFFFFFFF).
    const INFINITE: u32 = u32::MAX;

    pub fn watch(app: &AppHandle, game_id: &str, exe_name: &str) {
        let Some(pid) = wait_for_process(exe_name) else {
            // The process never appeared (slow update, wrong exe, ...). Reset the
            // card to idle rather than flagging an error: the game did launch, we
            // just could not attach a playtime watcher to it.
            let _ = app.emit("game-finished", game_id);
            app.state::<AppState>()
                .active_sessions
                .lock()
                .remove(game_id);
            return;
        };

        let started = Instant::now();
        let _ = app.emit("game-linked", game_id);

        wait_for_exit(pid, exe_name);

        let elapsed_mins = started.elapsed().as_secs() / 60;
        let _ = app.emit("game-finished", game_id);

        let state = app.state::<AppState>();
        state.active_sessions.lock().remove(game_id);
        if elapsed_mins > 0 {
            let mut launcher = state.launcher.lock();
            if let Some(game) = launcher.games.iter_mut().find(|g| g.id == game_id) {
                game.play_time_minutes += elapsed_mins;
                tracing::info!(
                    "Session ended for {}: +{}min (total: {}min)",
                    game.name,
                    elapsed_mins,
                    game.play_time_minutes
                );
            }
            drop(launcher);
            if let Err(e) = state.save() {
                tracing::error!("Failed to save play time: {e}");
            }
        }
    }

    /// Poll for a process whose image name matches `exe_name` until it appears or
    /// `FIND_TIMEOUT` elapses.
    fn wait_for_process(exe_name: &str) -> Option<u32> {
        let deadline = Instant::now() + FIND_TIMEOUT;
        loop {
            if let Some(pid) = find_pid(exe_name) {
                return Some(pid);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(FIND_POLL);
        }
    }

    /// Find the PID of the first process whose image name equals `exe_name`
    /// (case-insensitive basename).
    fn find_pid(exe_name: &str) -> Option<u32> {
        for_each_process(|pid, name| name.eq_ignore_ascii_case(exe_name).then_some(pid))
    }

    /// Block until the process `pid` exits. Uses a wait handle when available,
    /// otherwise falls back to polling the process list.
    fn wait_for_exit(pid: u32, exe_name: &str) {
        unsafe {
            if let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                if !handle.is_invalid() {
                    WaitForSingleObject(handle, INFINITE);
                    let _ = CloseHandle(handle);
                    return;
                }
            }
        }
        // Fallback (process could not be opened): poll until a process with this
        // PID *and* matching image name is gone, so a reused PID for a different
        // image counts as "exited".
        while for_each_process(|current, name| {
            (current == pid && name.eq_ignore_ascii_case(exe_name)).then_some(())
        })
        .is_some()
        {
            std::thread::sleep(Duration::from_secs(2));
        }
    }

    /// Walk the process snapshot, returning the first `Some` produced by `f`.
    fn for_each_process<T>(mut f: impl FnMut(u32, &str) -> Option<T>) -> Option<T> {
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
            let mut entry = PROCESSENTRY32W {
                dwSize: u32::try_from(std::mem::size_of::<PROCESSENTRY32W>()).unwrap_or(0),
                ..Default::default()
            };
            let mut result = None;
            if Process32FirstW(snapshot, &raw mut entry).is_ok() {
                loop {
                    let name = wide_to_string(&entry.szExeFile);
                    if let Some(value) = f(entry.th32ProcessID, &name) {
                        result = Some(value);
                        break;
                    }
                    if Process32NextW(snapshot, &raw mut entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snapshot);
            result
        }
    }

    fn wide_to_string(wide: &[u16]) -> String {
        let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
        String::from_utf16_lossy(&wide[..end])
    }
}
