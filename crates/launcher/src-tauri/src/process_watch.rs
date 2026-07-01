//! External game-session watcher for link status + playtime. Replaces the old
//! injector-lifetime tracking now that we no longer inject.
//!
//! Steam games are watched via Steam's own registry running-flag (keyed by
//! appid) -- the signal Steam uses for "in-game" status -- rather than guessing
//! the game executable. Non-Steam games are watched by their process name.
//! Either way: emit `game-linked` when the session starts and `game-finished`
//! (recording elapsed minutes) when it ends. If the session never starts within
//! the find window, emit `game-finished` so the UI resets to idle.
//!
//! Windows-only; a no-op stub elsewhere so the launcher still compiles for the
//! Linux test runner.
//!
//! Note: playtime for an in-flight session is only committed when the game
//! exits, so quitting the launcher mid-session discards that session. Watcher
//! threads are detached and end on their own when the game exits or the find
//! window elapses.

use tauri::AppHandle;

/// Watch a Steam game by appid via Steam's registry running-flag.
#[cfg(windows)]
pub fn spawn_steam_watch(app: AppHandle, game_id: String, app_id: String) {
    std::thread::spawn(move || imp::watch_steam(&app, &game_id, &app_id));
}

#[cfg(not(windows))]
pub fn spawn_steam_watch(_app: AppHandle, _game_id: String, _app_id: String) {}

/// Watch a non-Steam game by finding its process by executable name.
#[cfg(windows)]
pub fn spawn_game_watch(app: AppHandle, game_id: String, exe_name: String) {
    std::thread::spawn(move || imp::watch_exe(&app, &game_id, &exe_name));
}

#[cfg(not(windows))]
pub fn spawn_game_watch(_app: AppHandle, _game_id: String, _exe_name: String) {}

#[cfg(windows)]
mod imp {
    use std::time::{Duration, Instant};

    use tauri::{AppHandle, Emitter, Manager};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    use crate::state::AppState;

    /// How long to wait for the game session to start before giving up. Generous
    /// because Steam may update/download the game or show a pre-launch dialog.
    const FIND_TIMEOUT: Duration = Duration::from_mins(10);
    const FIND_POLL: Duration = Duration::from_millis(750);
    /// How often to re-check whether a linked session is still running.
    const EXIT_POLL: Duration = Duration::from_secs(2);
    /// `WaitForSingleObject` timeout meaning "wait forever" (0xFFFFFFFF).
    const INFINITE: u32 = u32::MAX;

    /// Watch a Steam game via `HKCU\Software\Valve\Steam\Apps\<appid>\Running`.
    pub fn watch_steam(app: &AppHandle, game_id: &str, app_id: &str) {
        if !wait_until(FIND_TIMEOUT, FIND_POLL, || steam_running(app_id)) {
            // Never started (long update, or cancelled at the pre-launch dialog).
            finish_session(app, game_id, 0);
            return;
        }
        let started = Instant::now();
        let _ = app.emit("game-linked", game_id);
        while steam_running(app_id) {
            std::thread::sleep(EXIT_POLL);
        }
        finish_session(app, game_id, elapsed_mins(started));
    }

    /// Watch a non-Steam game by its executable image name.
    pub fn watch_exe(app: &AppHandle, game_id: &str, exe_name: &str) {
        let Some(pid) = wait_until_some(FIND_TIMEOUT, FIND_POLL, || find_pid(exe_name)) else {
            // The process never appeared (slow update, wrong exe, ...).
            finish_session(app, game_id, 0);
            return;
        };
        let started = Instant::now();
        let _ = app.emit("game-linked", game_id);
        wait_for_exit(pid, exe_name);
        finish_session(app, game_id, elapsed_mins(started));
    }

    fn elapsed_mins(started: Instant) -> u64 {
        started.elapsed().as_secs() / 60
    }

    /// Emit `game-finished`, release the session reservation, and -- if any time
    /// elapsed -- add the minutes to the game's playtime and persist.
    fn finish_session(app: &AppHandle, game_id: &str, elapsed_mins: u64) {
        let _ = app.emit("game-finished", game_id);
        let state = app.state::<AppState>();
        state.active_sessions.lock().remove(game_id);
        if elapsed_mins == 0 {
            return;
        }
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

    /// Whether Steam marks `app_id` as currently running.
    fn steam_running(app_id: &str) -> bool {
        let subkey = wide(&format!("Software\\Valve\\Steam\\Apps\\{app_id}"));
        hkcu_dword(&subkey, &wide("Running")) == Some(1)
    }

    /// Read a DWORD value under HKCU by subkey + value name; None if absent.
    fn hkcu_dword(subkey: &[u16], value: &[u16]) -> Option<u32> {
        let mut data: u32 = 0;
        let mut size = u32::try_from(std::mem::size_of::<u32>()).ok()?;
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                PCWSTR(value.as_ptr()),
                RRF_RT_REG_DWORD,
                None,
                Some((&raw mut data).cast()),
                Some(&raw mut size),
            )
        };
        (status == ERROR_SUCCESS).then_some(data)
    }

    /// UTF-16, null-terminated, for the wide Win32 registry APIs.
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Poll `cond` until true or `timeout` elapses; returns whether it went true.
    fn wait_until(timeout: Duration, poll: Duration, mut cond: impl FnMut() -> bool) -> bool {
        wait_until_some(timeout, poll, || cond().then_some(())).is_some()
    }

    /// Poll `f` until it yields `Some` or `timeout` elapses.
    fn wait_until_some<T>(
        timeout: Duration,
        poll: Duration,
        mut f: impl FnMut() -> Option<T>,
    ) -> Option<T> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(value) = f() {
                return Some(value);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(poll);
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
            std::thread::sleep(EXIT_POLL);
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
