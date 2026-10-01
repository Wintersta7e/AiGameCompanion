//! External (no-injection) overlay companion: foreground-game detection and the
//! show/focus/hide state machine driven by the global toggle hotkey.
//!
//! The Win32 specifics compile only on Windows; on other hosts (the launcher's
//! pure-logic tests run on Linux) the helpers degrade to no-ops so the crate
//! still builds.

use std::collections::HashSet;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::models::Game;
use crate::state::AppState;

/// Snapshot of the foreground game window at the moment the overlay was opened.
#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct GameInfo {
    /// Native window handle, stored as i64 so it crosses the serde/IPC boundary.
    pub(crate) hwnd: i64,
    pub(crate) pid: u32,
    pub(crate) exe: String,
    /// For local display only: a window title never reaches a provider.
    pub(crate) title: String,
    /// The name a provider may see: the library game's name, else the exe file
    /// stem. Empty when the exe path could not be read.
    pub(crate) name: String,
    /// Whether requests may use this window: a library game, or a window the
    /// user linked this session.
    pub(crate) linked: bool,
}

/// Remembers the game window that had focus before the overlay was shown, so
/// focus can be handed back when the overlay hides.
#[derive(Default)]
pub(crate) struct OverlayState {
    pub(crate) game: parking_lot::Mutex<Option<GameInfo>>,
    /// (pid, exe path) of every window the user linked this launcher session.
    pub(crate) user_linked: parking_lot::Mutex<HashSet<(u32, String)>>,
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
        Some(mut game) => {
            if let Some(state) = app.try_state::<OverlayState>() {
                // One lock at a time: the linked pairs are copied out before
                // the library is read, and the slot is written last.
                let user_linked = state.user_linked.lock().clone();
                let (name, linked) = app.try_state::<AppState>().map_or_else(
                    || classify(&game.exe, game.pid, &[], &user_linked),
                    |app_state| {
                        classify(
                            &game.exe,
                            game.pid,
                            &app_state.launcher.lock().games,
                            &user_linked,
                        )
                    },
                );
                game.name = name;
                game.linked = linked;
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

/// Name the detected window and decide whether requests may use it: a library
/// game is linked and named by the library; any other window is named by its
/// exe stem and linked only if the user linked this (pid, exe) this session. A
/// window whose exe path could not be read is never linked.
fn classify(
    exe: &str,
    pid: u32,
    games: &[Game],
    user_linked: &HashSet<(u32, String)>,
) -> (String, bool) {
    if exe.is_empty() {
        return (String::new(), false);
    }
    if let Some(game) = library_match(exe, games) {
        return (game.name.clone(), true);
    }
    (exe_stem(exe), user_linked.contains(&(pid, exe.to_owned())))
}

/// The library game `exe` belongs to: its `exe_path` equals `exe`, or `exe`
/// lies under its `install_dir`. An `exe_path` match wins; of several matching
/// directories the longest wins.
fn library_match<'a>(exe: &str, games: &'a [Game]) -> Option<&'a Game> {
    let exe = normalize_path(exe);
    if exe.is_empty() {
        return None;
    }
    let by_exe = games.iter().find(|game| {
        game.exe_path
            .as_deref()
            .is_some_and(|path| normalize_path(path) == exe)
    });
    if by_exe.is_some() {
        return by_exe;
    }
    games
        .iter()
        .filter_map(|game| {
            let dir = normalize_path(game.install_dir.as_deref()?);
            let under = is_matchable_dir(&dir)
                && exe
                    .strip_prefix(dir.as_str())
                    .is_some_and(|rest| rest.starts_with('\\'));
            under.then_some((dir.len(), game))
        })
        .max_by_key(|&(len, _)| len)
        .map(|(_, game)| game)
}

/// The form paths are compared in: lower-cased, `/` read as `\`, trailing
/// separators removed.
fn normalize_path(path: &str) -> String {
    path.to_lowercase()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_owned()
}

/// Whether a normalised directory is deep enough to own a game: never empty, a
/// drive root (`c:`) or a share root (`\\server\share`), which would claim
/// every program on that drive or share.
fn is_matchable_dir(dir: &str) -> bool {
    let components = dir.split('\\').filter(|part| !part.is_empty()).count();
    let root_components = if dir.starts_with("\\\\") { 2 } else { 1 };
    components > root_components
}

/// The exe's file name with a trailing `.exe` removed, case-insensitively.
fn exe_stem(exe: &str) -> String {
    let file = exe.rsplit(['\\', '/']).next().unwrap_or_default();
    let stem = file
        .len()
        .checked_sub(".exe".len())
        .filter(|&cut| {
            file.get(cut..)
                .is_some_and(|ext| ext.eq_ignore_ascii_case(".exe"))
        })
        .and_then(|cut| file.get(..cut))
        .unwrap_or(file);
    stem.to_owned()
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
pub(crate) fn is_live_window(hwnd: i64, pid: u32) -> bool {
    imp::is_live_window(hwnd, pid)
}

#[cfg(not(windows))]
pub(crate) const fn is_live_window(_hwnd: i64, _pid: u32) -> bool {
    false
}

/// The stored target, but only if its handle still names a live window of the
/// process we recorded. Clears the slot otherwise, so a handle Windows reused
/// for another process is never screenshotted, uploaded or handed focus. It
/// cannot tell two windows of that same process apart.
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

/// The stored target, revalidated, when requests may use it. Every provider
/// request and every capture gets its target here.
pub(crate) fn linked_game(app: &AppHandle) -> Option<GameInfo> {
    live_game(app).filter(|game| game.linked)
}

/// Whether the stored target may be linked by a control that displayed `hwnd`
/// and `pid`: it must still be that window, and its exe must be known.
fn may_link(stored: Option<&GameInfo>, hwnd: i64, pid: u32) -> bool {
    stored.is_some_and(|game| game.hwnd == hwnd && game.pid == pid && !game.exe.is_empty())
}

/// Link the displayed window's app instance (pid + exe) for the rest of this
/// launcher session. The overlay's link control is the only caller: a click
/// there is the user's consent, so any other window is refused. Returns the
/// linked target, or `None` when nothing was linked -- including when the
/// stored target is no longer the window the overlay showed.
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects the handle and the calling window by value"
)]
pub(crate) fn link_game(
    app: AppHandle,
    window: tauri::WebviewWindow,
    hwnd: i64,
    pid: u32,
) -> Option<GameInfo> {
    if window.label() != "overlay" {
        return None;
    }
    let game = live_game(&app)?;
    if !may_link(Some(&game), hwnd, pid) {
        return None;
    }
    let state = app.try_state::<OverlayState>()?;
    state.user_linked.lock().insert((game.pid, game.exe));
    let mut slot = state.game.lock();
    let linked = slot
        .as_mut()
        .filter(|stored| stored.hwnd == hwnd && stored.pid == pid)
        .map(|stored| {
            stored.linked = true;
            stored.clone()
        });
    drop(slot);
    if linked.is_some() {
        tracing::info!("Linked window {hwnd} (pid {pid}) for this session");
    }
    linked
}

#[cfg(windows)]
mod imp {
    #![expect(
        unsafe_code,
        reason = "this module wraps the Win32 window and process calls"
    )]

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
        // SAFETY: GetForegroundWindow takes no arguments; it may return null,
        // which is checked before the handle is used.
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            return None;
        }
        let mut pid = 0u32;
        // SAFETY: `hwnd` is the non-null handle Windows just returned, and
        // `pid` is a local that outlives the call.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut pid)) };
        if pid == 0 || pid == self_pid {
            return None;
        }
        let exe = exe_path(pid).unwrap_or_default();
        let mut buf = [0u16; 512];
        // SAFETY: `hwnd` is the handle checked above, and `buf` is owned by
        // this frame for the whole call.
        let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
        let title = String::from_utf16_lossy(
            buf.get(..usize::try_from(n).unwrap_or(0))
                .unwrap_or_default(),
        );
        Some(GameInfo {
            hwnd: hwnd.0 as i64,
            pid,
            exe,
            title,
            ..Default::default()
        })
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
        buf.get(..len as usize).map(String::from_utf16_lossy)
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
        let handle = to_hwnd(hwnd);
        // SAFETY: `handle` is a plain window handle; IsWindow tolerates a stale
        // or recycled value, which is exactly what this check is for.
        let alive = unsafe { IsWindow(Some(handle)) }.as_bool();
        if !alive {
            return false;
        }
        let mut current = 0u32;
        // SAFETY: a stale handle only makes the call fail and leave `current`
        // at 0, and `current` is a local that outlives the call.
        unsafe { GetWindowThreadProcessId(handle, Some(&raw mut current)) };
        current != 0 && current == pid
    }

    pub(super) fn focus_window(hwnd: i64) {
        let handle = to_hwnd(hwnd);
        // SAFETY: IsIconic takes the handle by value and reports a stale one
        // through its return value; a stale handle is not UB.
        let minimized = unsafe { IsIconic(handle) }.as_bool();
        // A minimized target is not restored by SetForegroundWindow alone.
        if minimized {
            // SAFETY: ShowWindow takes the handle by value and reports failure
            // through its return value; a stale handle is not UB.
            let _ = unsafe { ShowWindow(handle, SW_RESTORE) };
        }
        // SAFETY: SetForegroundWindow takes the handle by value and reports
        // failure through its return value, logged below.
        let focused = unsafe { SetForegroundWindow(handle) }.as_bool();
        if !focused {
            tracing::warn!("SetForegroundWindow failed for hwnd {hwnd}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The overlay's hand-written `GameInfo` type must carry every field Rust
    /// sends, with the same JSON type; a field only TypeScript has must be
    /// optional, since Rust never sends it.
    #[test]
    fn overlay_game_info_type_mirrors_rust() {
        let source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/lib/components/Overlay.svelte"
        ));
        let typescript = crate::util::ts_fields(source, "type GameInfo = {");
        let rust = crate::util::json_fields(&serde_json::to_value(GameInfo::default()).unwrap());
        println!("Rust:       {rust:?}");
        println!("TypeScript: {typescript:?}");
        assert!(
            !typescript.is_empty(),
            "no GameInfo type found in Overlay.svelte"
        );

        let missing: Vec<String> = rust
            .iter()
            .filter(|(key, ty)| {
                !typescript
                    .iter()
                    .any(|(name, ts_ty, _)| name == key && ts_ty == ty)
            })
            .map(|(key, ty)| format!("{key}: {ty}"))
            .collect();
        let extra_required: Vec<&str> = typescript
            .iter()
            .filter(|(name, _, optional)| !optional && !rust.iter().any(|(key, _)| key == name))
            .map(|(name, _, _)| name.as_str())
            .collect();
        assert!(
            missing.is_empty() && extra_required.is_empty(),
            "missing or mistyped in TypeScript: {missing:?}; not sent by Rust but required: {extra_required:?}"
        );
    }

    fn game(name: &str, install_dir: &str, exe_path: Option<&str>) -> Game {
        Game {
            name: name.to_owned(),
            install_dir: Some(install_dir.to_owned()),
            exe_path: exe_path.map(str::to_owned),
            ..Default::default()
        }
    }

    fn matched<'a>(exe: &str, games: &'a [Game]) -> Option<&'a str> {
        library_match(exe, games).map(|game| game.name.as_str())
    }

    #[test]
    fn library_match_cases() {
        let foo = [game("Foo", r"C:\Games\Foo", None)];
        assert_eq!(matched(r"C:\Games\Foo\bin\foo.exe", &foo), Some("Foo"));
        assert_eq!(matched(r"c:/games/FOO/bin/Foo.EXE", &foo), Some("Foo"));
        let trailing = [game("Foo", r"C:\Games\Foo\", None)];
        assert_eq!(matched(r"C:\Games\Foo\foo.exe", &trailing), Some("Foo"));
        // A sibling whose name merely starts the same is not "under" it.
        assert_eq!(matched(r"C:\Games\FooBar\x.exe", &foo), None);

        let by_exe = [game("Foo", "", Some(r"D:\Elsewhere\foo.exe"))];
        assert_eq!(matched(r"d:/elsewhere/FOO.exe", &by_exe), Some("Foo"));

        for (root, exe) in [
            (r"C:\", r"C:\Games\Foo\foo.exe"),
            ("C:", r"C:\Games\Foo\foo.exe"),
            ("", r"C:\Games\Foo\foo.exe"),
            (r"\\server\share", r"\\server\share\Foo\foo.exe"),
        ] {
            assert_eq!(matched(exe, &[game("Root", root, None)]), None, "{root:?}");
        }

        let nested = [
            game("Outer", r"C:\Games", None),
            game("Inner", r"C:\Games\Foo", None),
        ];
        assert_eq!(matched(r"C:\Games\Foo\foo.exe", &nested), Some("Inner"));
        let reversed = [
            game("Inner", r"C:\Games\Foo", None),
            game("Outer", r"C:\Games", None),
        ];
        assert_eq!(matched(r"C:\Games\Foo\foo.exe", &reversed), Some("Inner"));

        assert_eq!(matched("", &[game("Foo", r"C:\Games\Foo", Some(""))]), None);
    }

    #[test]
    fn classify_cases() {
        let library = [game("Real Name", r"C:\Games\Foo", None)];
        let nothing_linked = HashSet::new();
        assert_eq!(
            classify(r"C:\Games\Foo\bin\foo.exe", 7, &library, &nothing_linked),
            ("Real Name".to_owned(), true)
        );
        assert_eq!(
            classify(r"C:\a\b\Game.EXE", 7, &library, &nothing_linked),
            ("Game".to_owned(), false)
        );

        let linked = HashSet::from([(7, r"C:\a\b\Game.EXE".to_owned())]);
        assert_eq!(
            classify(r"C:\a\b\Game.EXE", 7, &library, &linked),
            ("Game".to_owned(), true)
        );
        assert_eq!(
            classify(r"C:\a\b\Other.exe", 7, &library, &linked),
            ("Other".to_owned(), false)
        );
        assert_eq!(
            classify(r"C:\a\b\Game.EXE", 8, &library, &linked),
            ("Game".to_owned(), false)
        );

        let linked_empty = HashSet::from([(7, String::new())]);
        assert_eq!(
            classify("", 7, &library, &linked_empty),
            (String::new(), false)
        );
    }

    #[test]
    fn link_requires_the_displayed_window() {
        let stored = GameInfo {
            hwnd: 42,
            pid: 7,
            exe: r"C:\a\b\Game.exe".into(),
            ..Default::default()
        };
        assert!(may_link(Some(&stored), 42, 7));
        assert!(!may_link(Some(&stored), 43, 7), "a different window");
        assert!(!may_link(Some(&stored), 42, 8), "a different process");
        assert!(!may_link(None, 42, 7), "nothing stored");
        let unidentified = GameInfo {
            exe: String::new(),
            ..stored
        };
        assert!(!may_link(Some(&unidentified), 42, 7), "an unreadable exe");
    }

    #[test]
    fn stored_target_is_read_only_through_overlay() {
        let needle = concat!(".game", ".lock()");
        let (files, count) = crate::util::count_in_sources(needle, Some("overlay.rs"));
        println!("scanned {files} files, found {count} occurrence(s) of {needle}");
        assert!(files > 0, "the source scan found no files");
        assert_eq!(count, 0, "the stored target is read through overlay.rs");
    }
}
