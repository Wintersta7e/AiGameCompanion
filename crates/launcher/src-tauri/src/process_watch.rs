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
//! The watchers are Windows-only; a no-op stub elsewhere so the launcher still
//! compiles for the Linux test runner.
//!
//! Watcher threads are detached and end on their own when the game exits or
//! the find window elapses. Quitting the launcher ends the process before they
//! do, so the exit paths book every running session first.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use tauri::{AppHandle, Manager as _, Runtime};

use crate::models::LauncherState;
use crate::state::AppState;

/// One launch of a game, from its reservation until its play time is booked.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Session {
    /// Which launch this is, so a watcher whose session the exit already
    /// booked ends without booking it again.
    token: u64,
    /// When the game was seen running; `None` while it is still launching.
    started: Option<Instant>,
}

impl Session {
    /// Whole minutes played until `ended`, rounded to nearest rather than
    /// truncated, so a short session books 1 minute instead of vanishing.
    /// Truncating meant a user who plays in sub-minute bursts stayed at zero
    /// playtime forever.
    fn minutes_until(self, ended: Instant) -> u64 {
        self.started.map_or(0, |started| {
            (ended.saturating_duration_since(started).as_secs() + 30) / 60
        })
    }
}

static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

/// Reserve the session slot of `game_id` for a launch, atomically (guard +
/// insert) so two rapid launches cannot both start a running game. The token
/// names this launch. A launch whose game was never seen running (a cancelled
/// pre-launch dialog, a stub exe) is replaced rather than refused for the
/// whole find window; its watcher sees the new token and stops.
pub(crate) fn reserve(state: &AppState, game_id: &str) -> Result<u64, String> {
    let mut sessions = state.active_sessions.lock();
    if sessions.get(game_id).is_some_and(|s| s.started.is_some()) {
        return Err("This game is already running".to_owned());
    }
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    sessions.insert(
        game_id.to_owned(),
        Session {
            token,
            started: None,
        },
    );
    drop(sessions);
    Ok(token)
}

/// End the launch `token` of `game_id` and return its session; `None` when the
/// exit booking already took it.
pub(crate) fn release(state: &AppState, game_id: &str, token: u64) -> Option<Session> {
    let mut sessions = state.active_sessions.lock();
    if sessions.get(game_id).is_some_and(|s| s.token == token) {
        sessions.remove(game_id)
    } else {
        None
    }
}

/// Book the play time of every running session and save once. Quit and the
/// end of the run loop call this: `std::process::exit` ends the watcher
/// threads before they see their games exit, and the whole session's play
/// time was lost.
pub(crate) fn book_running_sessions<R: Runtime>(app: &AppHandle<R>) {
    book_sessions_until(app, Instant::now());
}

fn book_sessions_until<R: Runtime>(app: &AppHandle<R>, ended: Instant) {
    let state = app.state::<AppState>();
    // Taken out, so a watcher finishing later finds no session to book.
    let sessions = std::mem::take(&mut *state.active_sessions.lock());
    let mut launcher = state.launcher.lock();
    let mut booked = false;
    for (game_id, session) in sessions {
        booked |= add_play_time(&mut launcher, &game_id, session.minutes_until(ended)).is_some();
    }
    drop(launcher);
    if booked {
        state.save_or_notify(app, "The play time of the running games");
    }
}

/// Add `minutes` to the play time of `game_id`; the game's name when it was
/// found and `minutes` is not zero.
fn add_play_time(launcher: &mut LauncherState, game_id: &str, minutes: u64) -> Option<String> {
    if minutes == 0 {
        return None;
    }
    let game = launcher.games.iter_mut().find(|g| g.id == game_id)?;
    game.play_time_minutes += minutes;
    tracing::info!(
        "Session ended for {}: +{}min (total: {}min)",
        game.name,
        minutes,
        game.play_time_minutes
    );
    Some(game.name.clone())
}

/// Watch a Steam game by appid via Steam's registry running-flag.
#[cfg(windows)]
pub(crate) fn spawn_steam_watch<R: Runtime>(
    app: AppHandle<R>,
    game_id: String,
    app_id: String,
    token: u64,
) {
    std::thread::spawn(move || imp::watch_steam(&app, &game_id, &app_id, token));
}

#[cfg(not(windows))]
pub(crate) fn spawn_steam_watch<R: Runtime>(
    _app: AppHandle<R>,
    _game_id: String,
    _app_id: String,
    _token: u64,
) {
}

/// Watch a non-Steam game by finding its process by executable name.
#[cfg(windows)]
pub(crate) fn spawn_game_watch<R: Runtime>(
    app: AppHandle<R>,
    game_id: String,
    exe_name: String,
    token: u64,
) {
    std::thread::spawn(move || imp::watch_exe(&app, &game_id, &exe_name, token));
}

#[cfg(not(windows))]
pub(crate) fn spawn_game_watch<R: Runtime>(
    _app: AppHandle<R>,
    _game_id: String,
    _exe_name: String,
    _token: u64,
) {
}

#[cfg(windows)]
mod imp {
    #![expect(
        unsafe_code,
        reason = "this module wraps the Win32 process, wait and registry calls"
    )]

    use std::time::{Duration, Instant};

    use tauri::{AppHandle, Emitter as _, Manager as _, Runtime};
    use windows::Win32::Foundation::WAIT_OBJECT_0;
    use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    use windows::core::PCWSTR;

    use crate::state::AppState;

    /// How long to wait for the game session to start before giving up. Generous
    /// because Steam may update/download the game or show a pre-launch dialog.
    const FIND_TIMEOUT: Duration = Duration::from_mins(10);
    const FIND_POLL: Duration = Duration::from_millis(750);
    /// How often to re-check whether a linked session is still running.
    const EXIT_POLL: Duration = Duration::from_secs(2);
    /// `WaitForSingleObject` timeout meaning "wait forever" (0xFFFFFFFF).
    const INFINITE: u32 = u32::MAX;
    /// Upper bound on a single tracked session, so a Steam running-flag that is
    /// never cleared cannot strand the session reservation indefinitely.
    const MAX_SESSION: Duration = Duration::from_hours(24);

    /// Watch a Steam game via `HKCU\Software\Valve\Steam\Apps\<appid>\Running`.
    pub(super) fn watch_steam<R: Runtime>(
        app: &AppHandle<R>,
        game_id: &str,
        app_id: &str,
        token: u64,
    ) {
        match find_game(app, game_id, token, || steam_running(app_id).then_some(())) {
            Find::Found(()) => {}
            Find::Replaced => return,
            Find::TimedOut => {
                // Never started (long update, or cancelled at the pre-launch dialog).
                finish_session(app, game_id, token, Instant::now());
                return;
            }
        }
        let Some(started) = link(app, game_id, token) else {
            return;
        };
        crate::util::log_if_err("emit game-linked", app.emit("game-linked", game_id));
        let ended = steam_session_end(
            started,
            EXIT_POLL,
            MAX_SESSION,
            || steam_running(app_id),
            || find_pid("steam.exe").is_some(),
        );
        finish_session(app, game_id, token, ended);
    }

    /// Poll a linked Steam session every `poll` until it ends; when it ended.
    /// Steam crashing or being force-killed leaves `Running` set to 1 forever,
    /// so a session whose Steam client is gone ends at the last poll that saw
    /// the client, and `max` caps the rest: without them the session books a
    /// whole day and holds the reservation until the launcher restarts.
    fn steam_session_end(
        started: Instant,
        poll: Duration,
        max: Duration,
        mut running: impl FnMut() -> bool,
        mut client_alive: impl FnMut() -> bool,
    ) -> Instant {
        let mut last_seen = started;
        while running() {
            if !client_alive() {
                tracing::warn!(
                    "The Steam client is gone; ending the session when it was last seen"
                );
                return last_seen;
            }
            last_seen = Instant::now();
            if last_seen.duration_since(started) >= max {
                tracing::warn!(
                    "Steam still reports the game running after {}h; ending session",
                    max.as_secs() / 3600
                );
                return last_seen;
            }
            std::thread::sleep(poll);
        }
        Instant::now()
    }

    /// Watch a non-Steam game by its executable image name.
    pub(super) fn watch_exe<R: Runtime>(
        app: &AppHandle<R>,
        game_id: &str,
        exe_name: &str,
        token: u64,
    ) {
        let pid = match find_game(app, game_id, token, || find_pid(exe_name)) {
            Find::Found(pid) => pid,
            Find::Replaced => return,
            Find::TimedOut => {
                // The process never appeared (slow update, wrong exe, ...).
                finish_session(app, game_id, token, Instant::now());
                return;
            }
        };
        if link(app, game_id, token).is_none() {
            return;
        }
        crate::util::log_if_err("emit game-linked", app.emit("game-linked", game_id));
        wait_for_exit(pid, exe_name);
        finish_session(app, game_id, token, Instant::now());
    }

    /// How the search for a launched game ended.
    enum Find<T> {
        Found(T),
        TimedOut,
        /// A later launch of the game took the slot (or the exit booked it).
        Replaced,
    }

    /// Poll `found` until it yields, the find window ends, or the launch
    /// `token` of `game_id` is no longer the current one.
    fn find_game<R: Runtime, T>(
        app: &AppHandle<R>,
        game_id: &str,
        token: u64,
        mut found: impl FnMut() -> Option<T>,
    ) -> Find<T> {
        let state = app.state::<AppState>();
        let current = || {
            state
                .active_sessions
                .lock()
                .get(game_id)
                .is_some_and(|s| s.token == token)
        };
        let outcome = wait_until_some(FIND_TIMEOUT, FIND_POLL, || {
            if current() {
                found().map(Find::Found)
            } else {
                Some(Find::Replaced)
            }
        });
        outcome.unwrap_or(Find::TimedOut)
    }

    /// Mark the launch `token` of `game_id` running from now; `None` when a
    /// later launch or the exit booking already took it.
    fn link<R: Runtime>(app: &AppHandle<R>, game_id: &str, token: u64) -> Option<Instant> {
        let state = app.state::<AppState>();
        let now = Instant::now();
        let mut sessions = state.active_sessions.lock();
        sessions
            .get_mut(game_id)
            .filter(|s| s.token == token)?
            .started = Some(now);
        drop(sessions);
        Some(now)
    }

    /// Release the session reservation, add the minutes played until `ended`
    /// to the game's playtime and persist, then emit `game-finished`: the page
    /// reloads the library on it, so the minutes must be in the state first.
    /// A session the exit already booked is left alone.
    pub(super) fn finish_session<R: Runtime>(
        app: &AppHandle<R>,
        game_id: &str,
        token: u64,
        ended: Instant,
    ) {
        let state = app.state::<AppState>();
        let Some(session) = super::release(&state, game_id, token) else {
            return;
        };
        let mut launcher = state.launcher.lock();
        let name = super::add_play_time(&mut launcher, game_id, session.minutes_until(ended));
        drop(launcher);
        if let Some(name) = name {
            state.save_or_notify(app, &format!("The play time of {name}"));
        }
        crate::util::log_if_err("emit game-finished", app.emit("game-finished", game_id));
    }

    /// Whether Steam marks `app_id` as currently running.
    fn steam_running(app_id: &str) -> bool {
        let subkey = wide(&format!("Software\\Valve\\Steam\\Apps\\{app_id}"));
        hkcu_dword(&subkey, &wide("Running")) == Some(1)
    }

    /// Read a DWORD value under HKCU by subkey + value name; None if absent.
    fn hkcu_dword(subkey: &[u16], value: &[u16]) -> Option<u32> {
        let mut data: u32 = 0;
        let mut size = u32::try_from(size_of::<u32>()).ok()?;
        // SAFETY: both name buffers are NUL-terminated wide strings owned by the
        // caller, and `data`/`size` describe one live u32 for the whole call.
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
        // SAFETY: OpenProcess takes plain values; a pid that no longer exists
        // fails the open instead of returning a handle.
        let opened = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) };
        if let Ok(handle) = opened {
            // WAIT_FAILED would mean the wait never observed an exit; taking
            // it as "game closed" ends the session instantly and books ~0
            // minutes while the game is still running. Fall through to the
            // polling path instead.
            // SAFETY: `handle` came from OpenProcess above and stays open
            // until the CloseHandle below.
            let wait = unsafe { WaitForSingleObject(handle, INFINITE) };
            // SAFETY: `handle` came from OpenProcess above and is not used again.
            crate::util::log_if_err("CloseHandle(process)", unsafe { CloseHandle(handle) });
            if wait == WAIT_OBJECT_0 {
                return;
            }
            tracing::warn!("WaitForSingleObject on pid {pid} returned {wait:?}; polling");
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
        // SAFETY: CreateToolhelp32Snapshot takes plain values and returns a
        // handle this function owns and closes before returning.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.ok()?;
        let mut entry = PROCESSENTRY32W {
            dwSize: u32::try_from(size_of::<PROCESSENTRY32W>()).unwrap_or(0),
            ..Default::default()
        };
        let mut result = None;
        // SAFETY: `snapshot` is the open handle above, and `entry` is a live,
        // `dwSize`-initialised local that outlives the call.
        let first = unsafe { Process32FirstW(snapshot, &raw mut entry) };
        if first.is_ok() {
            loop {
                let name = wide_to_string(&entry.szExeFile);
                if let Some(value) = f(entry.th32ProcessID, &name) {
                    result = Some(value);
                    break;
                }
                // SAFETY: `snapshot` is still open, and `entry` is the same live,
                // `dwSize`-initialised local.
                let next = unsafe { Process32NextW(snapshot, &raw mut entry) };
                if next.is_err() {
                    break;
                }
            }
        }
        // SAFETY: `snapshot` came from CreateToolhelp32Snapshot above and is not
        // used again.
        crate::util::log_if_err("CloseHandle(snapshot)", unsafe { CloseHandle(snapshot) });
        result
    }

    fn wide_to_string(wide: &[u16]) -> String {
        let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
        String::from_utf16_lossy(wide.get(..end).unwrap_or_default())
    }

    #[cfg(test)]
    mod tests {
        use std::path::PathBuf;
        use std::time::{Duration, Instant};

        use tauri::Manager as _;
        use tauri::test::MockRuntime;

        use super::{finish_session, watch_exe};
        use crate::state::AppState;
        use crate::state::tests::failing_saves;

        /// A mock app holding one game, "g1", saved in a temp dir that is
        /// removed on drop.
        struct OneGame {
            app: tauri::App<MockRuntime>,
            path: PathBuf,
        }

        impl Drop for OneGame {
            fn drop(&mut self) {
                if let Some(dir) = self.path.parent() {
                    drop(std::fs::remove_dir_all(dir));
                }
            }
        }

        fn one_game(label: &str) -> OneGame {
            let dir = std::env::temp_dir().join(format!(
                "aigc_launcher_sessions_{}_{label}",
                std::process::id()
            ));
            drop(std::fs::remove_dir_all(&dir));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("launcher-state.json");
            std::fs::write(
                &path,
                r#"{"games":[{"id":"g1","name":"Foo","source":"manual"}],"settings":{}}"#,
            )
            .unwrap();
            let app = tauri::test::mock_app();
            app.manage(AppState::load(path.clone()));
            OneGame { app, path }
        }

        fn minutes(app: &tauri::App<MockRuntime>) -> u64 {
            minutes_of(app.handle())
        }

        fn minutes_of(app: &tauri::AppHandle<MockRuntime>) -> u64 {
            app.state::<AppState>().launcher.lock().games[0].play_time_minutes
        }

        /// Quitting while a game runs books the session so far, once: the
        /// watcher that later sees the game exit adds nothing.
        #[test]
        fn quit_books_a_running_session_once() {
            let fixture = one_game("quit");
            let app = fixture.app.handle().clone();
            let token = super::super::reserve(&app.state::<AppState>(), "g1").unwrap();
            // A harmless process stands in for the game.
            let mut game = std::process::Command::new("ping.exe")
                .args(["-n", "60", "127.0.0.1"])
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let watcher_app = app.clone();
            let watcher = std::thread::spawn(move || {
                watch_exe(&watcher_app, "g1", "PING.EXE", token);
            });
            let deadline = Instant::now() + Duration::from_secs(20);
            while app.state::<AppState>().active_sessions.lock()["g1"]
                .started
                .is_none()
            {
                assert!(Instant::now() < deadline, "the watcher never saw the game");
                std::thread::sleep(Duration::from_millis(100));
            }

            // The user quits half an hour into the session.
            super::super::book_sessions_until(&app, Instant::now() + Duration::from_mins(30));
            let booked = minutes(&fixture.app);
            let file = std::fs::read_to_string(&fixture.path).unwrap();
            drop(game.kill());
            drop(game.wait());
            watcher.join().unwrap();
            finish_session(&app, "g1", token, Instant::now() + Duration::from_mins(90));

            assert_eq!(booked, 30, "the running session was not booked at quit");
            assert!(file.contains(r#""play_time_minutes": 30"#), "{file}");
            assert_eq!(minutes(&fixture.app), 30, "the session was booked twice");
            assert!(app.state::<AppState>().active_sessions.lock().is_empty());
        }

        /// A launch whose game never appears (a cancelled pre-launch dialog, a
        /// stub exe) can be launched again: the new launch takes the slot and
        /// the first watcher stops without telling the page the game ended.
        #[test]
        fn a_launch_that_never_started_can_be_launched_again() {
            use tauri::Listener as _;

            let fixture = one_game("relaunch");
            let app = fixture.app.handle().clone();
            let finished = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counter = std::sync::Arc::clone(&finished);
            app.listen_any("game-finished", move |_| {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            });
            let first = super::super::reserve(&app.state::<AppState>(), "g1").unwrap();
            let watcher_app = app.clone();
            let watcher = std::thread::spawn(move || {
                watch_exe(&watcher_app, "g1", "never_started_launch_test.exe", first);
            });

            let second = super::super::reserve(&app.state::<AppState>(), "g1");
            assert!(second.is_ok(), "the second launch was refused: {second:?}");
            let deadline = Instant::now() + Duration::from_secs(10);
            while !watcher.is_finished() {
                assert!(
                    Instant::now() < deadline,
                    "the first launch's watcher is still running"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
            let second = second.unwrap();
            assert_eq!(
                app.state::<AppState>().active_sessions.lock()["g1"].token,
                second
            );
            assert_eq!(finished.load(std::sync::atomic::Ordering::SeqCst), 0);

            // A game seen running is still refused a second launch.
            assert!(super::link(&app, "g1", second).is_some());
            assert_eq!(
                super::super::reserve(&app.state::<AppState>(), "g1"),
                Err("This game is already running".to_owned())
            );
        }

        /// Steam killed mid-session leaves its running flag at 1: the session
        /// ends at the last poll that saw the Steam client, not at the cap.
        #[test]
        fn a_steam_client_gone_ends_the_session_when_last_seen() {
            let started = Instant::now();
            let mut polls = Vec::new();
            let ended = super::steam_session_end(
                started,
                Duration::from_millis(10),
                Duration::from_secs(2),
                || true,
                || {
                    polls.push(Instant::now());
                    polls.len() < 4
                },
            );

            println!("ended {:?} after the start", ended - started);
            assert_eq!(polls.len(), 4, "the Steam client was not polled each time");
            assert!(
                polls[2] <= ended && ended < polls[3],
                "the session did not end at the last poll that saw Steam"
            );
        }

        /// The page reloads the library on `game-finished`, so the session's
        /// minutes must be in the state by then.
        #[test]
        fn game_finished_follows_the_play_time() {
            use tauri::Listener as _;

            let fixture = one_game("event_order");
            let app = fixture.app.handle().clone();
            let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
            let record = std::sync::Arc::clone(&seen);
            let handle = app.clone();
            app.listen_any("game-finished", move |_| {
                record.lock().push(minutes_of(&handle));
            });
            let token = super::super::reserve(&app.state::<AppState>(), "g1").unwrap();
            let started = super::link(&app, "g1", token).unwrap();

            finish_session(&app, "g1", token, started + Duration::from_mins(30));

            assert_eq!(
                *seen.lock(),
                [30],
                "the page reloads before the play time is added"
            );
        }

        #[test]
        fn a_failed_play_time_save_reaches_the_page() {
            let saves = failing_saves(
                "play_time",
                r#"{"games":[{"id":"g1","name":"Foo","source":"manual"}],"settings":{}}"#,
            );
            let app = saves.app.handle();
            let token = super::super::reserve(&app.state::<AppState>(), "g1").unwrap();
            let started = super::link(app, "g1", token).unwrap();

            finish_session(app, "g1", token, started + Duration::from_hours(1));

            let reported = saves.reported.recv_timeout(Duration::from_secs(5));
            println!("reported: {reported:?}");
            assert!(
                reported.is_ok_and(
                    |message| message.starts_with("The play time of Foo could not be saved: ")
                ),
                "the page was not told the play time was not saved"
            );
            assert!(app.state::<AppState>().active_sessions.lock().is_empty());
        }
    }
}
