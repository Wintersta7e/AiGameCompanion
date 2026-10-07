//! External (no-injection) overlay companion: foreground-game detection and the
//! show/focus/hide state machine driven by the global toggle hotkey.
//!
//! The Win32 specifics compile only on Windows; on other hosts (the launcher's
//! pure-logic tests run on Linux) the helpers degrade to no-ops so the crate
//! still builds.

use std::collections::HashSet;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::models::{Game, GameSource};
use crate::placement::{self, MonitorArea, Placed, Rect};
use crate::state::AppState;

/// What a linked request may tell a provider about the game, besides its name.
/// Rust-only: never sent to the page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Identity {
    /// The exe path could not be read.
    #[default]
    Unknown,
    /// A library game: its id, and its app id when it is a Steam game.
    Library {
        game_id: String,
        steam_app_id: Option<u32>,
    },
    /// Any other window: the product name its exe file carries, when that says
    /// more than the file name.
    Program { product_name: Option<String> },
}

/// Snapshot of the foreground game window at the moment the overlay was opened.
#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct GameInfo {
    /// Native window handle, stored as i64 so it crosses the serde/IPC boundary.
    pub(crate) hwnd: i64,
    pub(crate) pid: u32,
    pub(crate) exe: String,
    /// For local display only: a window title never reaches a provider.
    pub(crate) title: String,
    /// The displayed name, sanitised: the library game's name, else the exe
    /// file stem. Empty when the exe path could not be read. One input to the
    /// identity block a linked request carries.
    pub(crate) name: String,
    /// Whether requests may use this window: a library game, or a window the
    /// user linked this session.
    pub(crate) linked: bool,
    /// What a linked request may say about the game; never sent to the page.
    #[serde(skip)]
    pub(crate) identity: Identity,
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

/// Hide the overlay and hand focus back to the stored target. Every hide of
/// the overlay window -- the hotkey, its close control, Alt+F4 -- goes through
/// here.
pub(crate) fn hide(app: &AppHandle) {
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
                let classification = app.try_state::<AppState>().map_or_else(
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
                game.name = classification.name;
                game.linked = classification.linked;
                game.identity = classification.identity;
                // Read from the exe file, with no lock held.
                if let Identity::Program { product_name } = &mut game.identity {
                    *product_name =
                        program_product_name(exe_product_name(&game.exe).as_deref(), &game.name);
                }
                *state.game.lock() = Some(game.clone());
            }
            Some(game)
        }
        None => live_game(app),
    };
    show_placed(app, &overlay, game.as_ref());
    // A null payload tells the overlay UI "no game detected".
    crate::util::log_if_err(
        "emit overlay-status",
        app.emit_to("overlay", "overlay-status", game),
    );
}

/// Show and focus the overlay, first placed for `game` when a reference rect
/// is found. Position before size: a move onto a monitor of another DPI keeps
/// the logical size, which the size then overrides. A read-back that differs
/// from the target is applied once more, never in a loop.
fn show_placed(app: &AppHandle, overlay: &tauri::WebviewWindow, game: Option<&GameInfo>) {
    let target = placement_target(app, game);
    if let Some((_, _, placed)) = &target {
        apply_rect(overlay, placed.rect);
    }
    crate::util::log_if_err("show overlay", overlay.show());
    crate::util::log_if_err("focus overlay", overlay.set_focus());
    raise_topmost(overlay);
    let Some((_, scale, placed)) = target else {
        return;
    };
    let mut applied = read_rect(overlay);
    if applied != Some(placed.rect) {
        tracing::debug!(
            "Overlay read back at {applied:?}, not {:?}; placing it once more",
            placed.rect
        );
        apply_rect(overlay, placed.rect);
        applied = read_rect(overlay);
    }
    let applied = applied.unwrap_or(placed.rect);
    tracing::info!(
        "Overlay placed {},{} {}x{} ({}) scale {scale}",
        applied.x,
        applied.y,
        applied.w,
        applied.h,
        placed.from.label()
    );
}

fn apply_rect(overlay: &tauri::WebviewWindow, rect: Rect) {
    crate::util::log_if_err(
        "move overlay",
        overlay.set_position(tauri::PhysicalPosition::new(rect.x, rect.y)),
    );
    crate::util::log_if_err(
        "size overlay",
        overlay.set_size(tauri::PhysicalSize::new(rect.w, rect.h)),
    );
}

/// The overlay's outer position and inner size: what `apply_rect` sets.
fn read_rect(overlay: &tauri::WebviewWindow) -> Option<Rect> {
    let position = overlay.outer_position().ok()?;
    let size = overlay.inner_size().ok()?;
    Some(Rect {
        x: position.x,
        y: position.y,
        w: size.width,
        h: size.height,
    })
}

fn monitor_area(monitor: &tauri::Monitor) -> MonitorArea {
    let work = monitor.work_area();
    MonitorArea {
        work: Rect {
            x: work.position.x,
            y: work.position.y,
            w: work.size.width,
            h: work.size.height,
        },
        scale: monitor.scale_factor(),
    }
}

/// The reference rect for `game`, its scale and where to put the panel.
/// `None` when no monitor or reference rect is known: the panel then keeps its
/// last spot.
fn placement_target(app: &AppHandle, game: Option<&GameInfo>) -> Option<(Rect, f64, Placed)> {
    let monitors: Vec<MonitorArea> = match app.available_monitors() {
        Ok(listed) if !listed.is_empty() => listed.iter().map(monitor_area).collect(),
        outcome => {
            let why = outcome.map_or_else(|err| err.to_string(), |_| "none listed".to_owned());
            tracing::debug!("Overlay not placed: no monitor ({why})");
            return None;
        }
    };
    let monitor_at = |(x, y): (f64, f64)| {
        app.monitor_from_point(x, y)
            .ok()
            .flatten()
            .map(|monitor| monitor_area(&monitor))
    };
    let client = game.and_then(|game| client_rect(game.hwnd));
    let at_centre = client.and_then(|client| monitor_at(client.centre()));
    let at_cursor = app
        .cursor_position()
        .ok()
        .and_then(|cursor| monitor_at((cursor.x, cursor.y)));
    let Some((r, r_scale)) = placement::reference(client, at_centre, at_cursor) else {
        tracing::debug!("Overlay not placed: no reference rect");
        return None;
    };
    // One lock hold, copied out.
    let saved = app
        .try_state::<AppState>()
        .and_then(|state| state.launcher.lock().overlay_placement);
    Some((
        r,
        r_scale,
        placement::place(saved.as_ref(), r, r_scale, &monitors),
    ))
}

/// What `classify` decides about a detected window.
#[derive(Debug, PartialEq, Eq)]
struct Classification {
    name: String,
    linked: bool,
    identity: Identity,
}

/// Name the detected window, decide whether requests may use it and what they
/// may say about it: a library game is linked, named by the library and
/// identified by its id and Steam app id; any other window is named by its exe
/// stem, identified as a program and linked only if the user linked this
/// (pid, exe) this session. A window whose exe path could not be read is never
/// linked and has no identity.
fn classify(
    exe: &str,
    pid: u32,
    games: &[Game],
    user_linked: &HashSet<(u32, String)>,
) -> Classification {
    if exe.is_empty() {
        return Classification {
            name: String::new(),
            linked: false,
            identity: Identity::Unknown,
        };
    }
    if let Some(game) = library_match(exe, games) {
        return Classification {
            name: identity_text(&game.name),
            linked: true,
            identity: Identity::Library {
                game_id: game.id.clone(),
                steam_app_id: steam_app_id(game),
            },
        };
    }
    Classification {
        name: identity_text(&exe_stem(exe)),
        linked: user_linked.contains(&(pid, exe.to_owned())),
        identity: Identity::Program { product_name: None },
    }
}

/// A Steam game's app id: its `source_id` when that is 1-10 ASCII digits
/// naming a non-zero `u32`. `None` for any other game.
fn steam_app_id(game: &Game) -> Option<u32> {
    let id = game.source_id.as_deref()?;
    let digits = (1..=10).contains(&id.len()) && id.bytes().all(|byte| byte.is_ascii_digit());
    if game.source != GameSource::Steam || !digits {
        return None;
    }
    id.parse::<u32>().ok().filter(|&app_id| app_id > 0)
}

/// The product name a program's file carries, kept only when it says more than
/// the exe stem: sanitised, non-empty and not the stem in another case.
fn program_product_name(raw: Option<&str>, stem: &str) -> Option<String> {
    let name = identity_text(raw?);
    (!name.is_empty() && name.to_lowercase() != stem.to_lowercase()).then_some(name)
}

#[cfg(windows)]
fn exe_product_name(exe: &str) -> Option<String> {
    imp::exe_product_name(exe)
}

#[cfg(not(windows))]
const fn exe_product_name(_exe: &str) -> Option<String> {
    None
}

/// The file name of the process whose frame windows show Store apps.
#[cfg(any(windows, test))]
const FRAME_HOST_EXE: &str = "applicationframehost.exe";
/// The window class of a Store app's own window inside its host frame.
#[cfg(any(windows, test))]
const HOSTED_APP_CLASS: &str = "Windows.UI.Core.CoreWindow";

/// Whether `exe` is the Store app window host, compared by file name.
#[cfg(any(windows, test))]
fn is_frame_host(exe: &str) -> bool {
    exe.rsplit(['\\', '/'])
        .next()
        .is_some_and(|file| file.eq_ignore_ascii_case(FRAME_HOST_EXE))
}

/// The process of the app a host frame shows: the first child window of the
/// hosted-app class owned by a process other than the host. `None` when the
/// frame holds no such window, as when the app is minimised or suspended.
#[cfg(any(windows, test))]
fn hosted_pid(host_pid: u32, children: &[(String, u32)]) -> Option<u32> {
    children
        .iter()
        .find(|(class, pid)| class == HOSTED_APP_CLASS && *pid != 0 && *pid != host_pid)
        .map(|&(_, pid)| pid)
}

/// Whether a window owned by process `owner` still belongs to `pid`: `pid`
/// owns it, or it is the host frame showing `pid`'s app. `children` lists the
/// window's child windows; it runs only when the two processes differ.
#[cfg(any(windows, test))]
fn owned_by(owner: u32, pid: u32, children: impl FnOnce() -> Vec<(String, u32)>) -> bool {
    owner != 0 && (owner == pid || hosted_pid(owner, &children()) == Some(pid))
}

/// The longest name kept, in `char`s; a longer one is cut to end in an ellipsis.
const MAX_IDENTITY_CHARS: usize = 128;

/// A third-party name (library name, exe stem, product name) fit to show and
/// to send: whitespace becomes a space, control and invisible characters are
/// dropped, runs of spaces collapse, and the result is trimmed and at most
/// `MAX_IDENTITY_CHARS` long. Idempotent. Quotes and angle brackets stay: the
/// identity block escapes them.
pub(crate) fn identity_text(s: &str) -> String {
    let visible: String = s
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|&c| {
            !c.is_control()
                && !matches!(
                    u32::from(c),
                    0xAD | 0x34F
                        | 0x61C
                        | 0x115F..=0x1160
                        | 0x17B4..=0x17B5
                        | 0x180B..=0x180F
                        | 0x200B..=0x200F
                        | 0x202A..=0x202E
                        | 0x2060..=0x206F
                        | 0x3164
                        | 0xFE00..=0xFE0F
                        | 0xFEFF
                        | 0xFFA0
                        | 0xFFF0..=0xFFFB
                        | 0x1D173..=0x1D17A
                        | 0xE0000..=0xE0FFF
                )
        })
        .collect();
    let words: Vec<&str> = visible.split(' ').filter(|word| !word.is_empty()).collect();
    let text = words.join(" ");
    if text.chars().count() <= MAX_IDENTITY_CHARS {
        return text;
    }
    let mut cut: String = text.chars().take(MAX_IDENTITY_CHARS - 1).collect();
    cut.push('\u{2026}');
    cut
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

/// A window's client area in screen pixels; `None` when it is minimised, its
/// frame cannot be read or the area is empty. The crate's one reader of a
/// window's frame.
#[cfg(windows)]
pub(crate) fn client_rect(hwnd: i64) -> Option<Rect> {
    imp::client_rect(hwnd)
}

#[cfg(not(windows))]
pub(crate) const fn client_rect(_hwnd: i64) -> Option<Rect> {
    None
}

/// Put the overlay back on top of the topmost windows without activating it,
/// once per show: a focus request Windows refused leaves a topmost game above
/// the panel. Setting always-on-top again cannot do this, since re-setting a
/// set flag changes nothing.
#[cfg(windows)]
fn raise_topmost(overlay: &tauri::WebviewWindow) {
    let raised = overlay
        .hwnd()
        .map_err(|err| err.to_string())
        .and_then(|hwnd| imp::raise_topmost(hwnd.0 as i64).map_err(|err| err.to_string()));
    crate::util::log_if_err("raise overlay", raised);
}

#[cfg(not(windows))]
const fn raise_topmost(_overlay: &tauri::WebviewWindow) {}

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
    use crate::placement::Rect;
    use windows::core::{BOOL, PCWSTR, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumChildWindows, GetClassNameW, GetForegroundWindow, GetWindowInfo, GetWindowTextW,
        GetWindowThreadProcessId, IsIconic, IsWindow, SetForegroundWindow, SetWindowPos,
        ShowWindow, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_RESTORE, WINDOWINFO,
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
        let mut exe = exe_path(pid).unwrap_or_default();
        // A Store app's window belongs to the host process that draws its
        // frame, so every Store app would look like that one program. Name the
        // app the frame shows instead; the frame stays the window captured and
        // focused. With no app inside (minimised, suspended) it is unknown.
        if super::is_frame_host(&exe) {
            match super::hosted_pid(pid, &child_windows(hwnd)) {
                Some(app) => {
                    pid = app;
                    exe = exe_path(app).unwrap_or_default();
                }
                None => exe = String::new(),
            }
        }
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

    /// The `ProductName` in `exe`'s version resource: for the first language
    /// the file lists, else for US English in Unicode. `None` on any failure.
    pub(super) fn exe_product_name(exe: &str) -> Option<String> {
        let path = wide(exe);
        // SAFETY: `path` is a NUL-terminated UTF-16 string that outlives the
        // call.
        let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(path.as_ptr()), None) };
        if size == 0 {
            return None;
        }
        let mut block = vec![0u8; usize::try_from(size).ok()?];
        // SAFETY: `block` holds exactly the `size` bytes the call may write,
        // and `path` is as above.
        unsafe {
            GetFileVersionInfoW(PCWSTR(path.as_ptr()), None, size, block.as_mut_ptr().cast())
        }
        .ok()?;
        let listed = version_value(&block, r"\VarFileInfo\Translation").and_then(|(bytes, len)| {
            let [lang_lo, lang_hi, page_lo, page_hi]: [u8; 4] =
                bytes.get(..4.min(len))?.try_into().ok()?;
            Some(format!(
                "{:04x}{:04x}",
                u16::from_le_bytes([lang_lo, lang_hi]),
                u16::from_le_bytes([page_lo, page_hi])
            ))
        });
        listed
            .into_iter()
            .chain(std::iter::once("040904b0".to_owned()))
            .find_map(|code| {
                let (bytes, len) =
                    version_value(&block, &format!(r"\StringFileInfo\{code}\ProductName"))?;
                let units: Vec<u16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .take(len)
                    .map(|&pair| u16::from_le_bytes(pair))
                    .take_while(|&unit| unit != 0)
                    .collect();
                Some(String::from_utf16_lossy(&units))
            })
    }

    /// The value `VerQueryValueW` finds at `sub_block` in a version block: its
    /// bytes from where the value starts to the end of `block`, and its length
    /// as the call reports it (bytes for a binary value, UTF-16 units for a
    /// string). The value must lie inside `block`; it is read with `.get()`.
    fn version_value<'a>(block: &'a [u8], sub_block: &str) -> Option<(&'a [u8], usize)> {
        let name = wide(sub_block);
        let mut value: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut len = 0u32;
        // SAFETY: `block` is a version block GetFileVersionInfoW filled and
        // outlives the call, `name` is NUL-terminated, and `value`/`len` are
        // locals the call writes.
        let found = unsafe {
            VerQueryValueW(
                block.as_ptr().cast(),
                PCWSTR(name.as_ptr()),
                &raw mut value,
                &raw mut len,
            )
        }
        .as_bool();
        if !found || value.is_null() {
            return None;
        }
        let start = value.addr().checked_sub(block.as_ptr().addr())?;
        Some((block.get(start..)?, usize::try_from(len).ok()?))
    }

    /// `s` as a NUL-terminated UTF-16 string, as the wide Win32 calls take it.
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn to_hwnd(hwnd: i64) -> HWND {
        HWND(usize::try_from(hwnd).unwrap_or(0) as *mut core::ffi::c_void)
    }

    /// `hwnd`'s client area in screen coordinates, which are physical pixels
    /// in this per-monitor DPI aware process. `None` when the window is
    /// minimised, cannot be read or has an empty client area.
    pub(super) fn client_rect(hwnd: i64) -> Option<Rect> {
        let handle = to_hwnd(hwnd);
        // SAFETY: IsIconic takes the handle by value and reports a stale one
        // through its return value; a stale handle is not UB.
        if unsafe { IsIconic(handle) }.as_bool() {
            return None;
        }
        let mut info = WINDOWINFO {
            cbSize: u32::try_from(size_of::<WINDOWINFO>()).ok()?,
            ..Default::default()
        };
        // SAFETY: `info` is a local with `cbSize` set as the call requires,
        // and outlives the call; a stale handle only makes the call fail.
        unsafe { GetWindowInfo(handle, &raw mut info) }.ok()?;
        let client = info.rcClient;
        Rect::from_edges(
            i64::from(client.left),
            i64::from(client.top),
            i64::from(client.right),
            i64::from(client.bottom),
        )
    }

    /// Move `hwnd` to the top of the topmost band, keeping its position and
    /// size and without activating it.
    pub(super) fn raise_topmost(hwnd: i64) -> windows::core::Result<()> {
        // SAFETY: SetWindowPos takes both handles by value and reports a stale
        // one through its result; no pointer is passed.
        unsafe {
            SetWindowPos(
                to_hwnd(hwnd),
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
        }
    }

    /// Whether `hwnd` is still a live window owned by `pid`, or the Store app
    /// host frame still showing `pid`'s app.
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
        super::owned_by(current, pid, || child_windows(handle))
    }

    /// (class name, owning pid) of each child window of `parent`.
    fn child_windows(parent: HWND) -> Vec<(String, u32)> {
        let mut found: Vec<(String, u32)> = Vec::new();
        // SAFETY: `push_child` runs only during this call and is handed
        // `found`, which outlives the call and is not touched by anything
        // else meanwhile. A stale `parent` only makes the call enumerate
        // nothing.
        let _ = unsafe {
            EnumChildWindows(
                Some(parent),
                Some(push_child),
                LPARAM(&raw mut found as isize),
            )
        };
        found
    }

    /// `EnumChildWindows` callback: appends the window's class name and owning
    /// pid to the `Vec` that `lparam` points at, and keeps enumerating.
    unsafe extern "system" fn push_child(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the `&mut Vec` `child_windows` passed, alive and
        // unaliased for the whole enumeration.
        let found = unsafe { &mut *(lparam.0 as *mut Vec<(String, u32)>) };
        let mut class = [0u16; 256];
        // SAFETY: `hwnd` is a window Windows is enumerating, and `class` is a
        // local buffer the call writes at most its length into.
        let len = unsafe { GetClassNameW(hwnd, &mut class) };
        let mut pid = 0u32;
        // SAFETY: as above for `hwnd`; `pid` is a local that outlives the call.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut pid)) };
        let name = class
            .get(..usize::try_from(len).unwrap_or(0))
            .map_or_default(String::from_utf16_lossy);
        found.push((name, pid));
        BOOL::from(true)
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

    /// A Steam library game installed under `C:\Games\Foo`.
    fn steam_game(id: &str, source_id: &str) -> Game {
        Game {
            id: id.to_owned(),
            source: GameSource::Steam,
            source_id: Some(source_id.to_owned()),
            ..game("Real Name", r"C:\Games\Foo", None)
        }
    }

    fn program(name: &str, linked: bool) -> Classification {
        Classification {
            name: name.to_owned(),
            linked,
            identity: Identity::Program { product_name: None },
        }
    }

    #[test]
    fn classify_cases() {
        let library = [steam_game("steam_123450", "123450")];
        let nothing_linked = HashSet::new();
        assert_eq!(
            classify(r"C:\Games\Foo\bin\foo.exe", 7, &library, &nothing_linked),
            Classification {
                name: "Real Name".to_owned(),
                linked: true,
                identity: Identity::Library {
                    game_id: "steam_123450".to_owned(),
                    steam_app_id: Some(123_450),
                },
            }
        );
        for (source_id, app_id) in [
            ("62a", None),
            ("", None),
            ("-1", None),
            ("+620", None),
            ("0", None),
            ("4294967296", None),
            ("4294967295", Some(u32::MAX)),
        ] {
            let found = classify(
                r"C:\Games\Foo\foo.exe",
                7,
                &[steam_game("steam_x", source_id)],
                &nothing_linked,
            );
            println!("Steam source_id {source_id:?}: {:?}", found.identity);
            assert_eq!(
                found.identity,
                Identity::Library {
                    game_id: "steam_x".to_owned(),
                    steam_app_id: app_id,
                },
                "{source_id:?}"
            );
        }
        let not_steam = Game {
            id: "manual_1".to_owned(),
            source_id: Some("123450".to_owned()),
            ..game("Real Name", r"C:\Games\Foo", None)
        };
        assert_eq!(
            classify(r"C:\Games\Foo\foo.exe", 7, &[not_steam], &nothing_linked).identity,
            Identity::Library {
                game_id: "manual_1".to_owned(),
                steam_app_id: None,
            }
        );
        assert_eq!(
            classify(r"C:\a\b\Game.EXE", 7, &library, &nothing_linked),
            program("Game", false)
        );

        let linked = HashSet::from([(7, r"C:\a\b\Game.EXE".to_owned())]);
        assert_eq!(
            classify(r"C:\a\b\Game.EXE", 7, &library, &linked),
            program("Game", true)
        );
        assert_eq!(
            classify(r"C:\a\b\Other.exe", 7, &library, &linked),
            program("Other", false)
        );
        assert_eq!(
            classify(r"C:\a\b\Game.EXE", 8, &library, &linked),
            program("Game", false)
        );

        let linked_empty = HashSet::from([(7, String::new())]);
        assert_eq!(
            classify("", 7, &library, &linked_empty),
            Classification {
                name: String::new(),
                linked: false,
                identity: Identity::Unknown,
            }
        );

        let reversed = [game("Real\u{202e}Name", r"C:\Games\Foo", None)];
        assert_eq!(
            classify(r"C:\Games\Foo\foo.exe", 7, &reversed, &nothing_linked).name,
            "RealName"
        );
    }

    #[test]
    fn game_info_sends_no_identity() {
        let identified = GameInfo {
            identity: Identity::Program {
                product_name: Some("Foo Studio Game".to_owned()),
            },
            ..GameInfo::default()
        };
        for target in [GameInfo::default(), identified] {
            let value = serde_json::to_value(target).unwrap();
            let mut keys: Vec<&str> = value
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            println!("GameInfo keys: {keys:?}");
            assert_eq!(keys, ["exe", "hwnd", "linked", "name", "pid", "title"]);
        }
    }

    #[test]
    fn page_never_builds_a_game_key() {
        let needles = [
            concat!("'library", ":'"),
            concat!("\"library", ":\""),
            concat!("'exe", ":'"),
            concat!("\"exe", ":\""),
            concat!("`library", ":"),
            concat!("`exe", ":"),
        ];
        let scan = crate::util::count_in_frontend(&needles);
        println!("{} files scanned", scan.files.len());
        for (needle, hits) in needles.iter().zip(&scan.hits) {
            println!("{needle}: {} {hits:?}", hits.len());
        }
        assert!(
            scan.hits.iter().all(Vec::is_empty),
            "the page builds or parses a game key"
        );
    }

    #[test]
    fn identity_text_cases() {
        let dropped = [
            '\u{ad}',
            '\u{34f}',
            '\u{61c}',
            '\u{115f}',
            '\u{17b4}',
            '\u{180e}',
            '\u{200b}',
            '\u{202e}',
            '\u{2060}',
            '\u{3164}',
            '\u{fe0f}',
            '\u{feff}',
            '\u{ffa0}',
            '\u{fff0}',
            '\u{1d173}',
            '\u{e0001}',
            '\u{1b}',
        ];
        let mut cases: Vec<(String, String)> = dropped
            .iter()
            .map(|c| (format!("Foo{c}Bar"), "FooBar".to_owned()))
            .collect();
        for space in ["\n", "\r\n", "\t", "\u{2028}", "\u{85}", "  "] {
            cases.push((format!("Foo{space}Bar"), "Foo Bar".to_owned()));
        }
        cases.push((
            format!("  {}  ", "a".repeat(300)),
            format!("{}\u{2026}", "a".repeat(127)),
        ));
        for unchanged in [
            "Foo \"Bar\"",
            r"Foo\Bar",
            "</game_context>",
            "Foo's Quest: Part II\u{2122}",
            "\u{30b2}\u{30fc}\u{30e0}\u{ff01}",
            "\u{30c6}\u{30b9}\u{30c8}\u{ff1a}\u{4e8c}",
        ] {
            cases.push((unchanged.to_owned(), unchanged.to_owned()));
        }
        let mut wrong = Vec::new();
        for (input, expected) in &cases {
            let output = identity_text(input);
            println!("{input:?} -> {output:?}");
            if output != *expected {
                wrong.push(format!("{input:?} -> {output:?}, expected {expected:?}"));
            }
            if identity_text(&output) != output {
                wrong.push(format!("{output:?} changes when sanitised again"));
            }
        }
        println!("{} cases", cases.len());
        assert_eq!(identity_text(&"a".repeat(300)).chars().count(), 128);
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[test]
    fn product_name_kept_only_when_informative() {
        for (raw, stem, expected) in [
            (Some("FOO"), "foo", None),
            (Some("foo"), "Foo", None),
            (Some(" \t "), "foo", None),
            (None, "foo", None),
            (Some("Foo Studio Game"), "foo", Some("Foo Studio Game")),
            (
                Some("Foo\u{200b} Studio\nGame"),
                "foo",
                Some("Foo Studio Game"),
            ),
        ] {
            let kept = program_product_name(raw, stem);
            println!("{raw:?} beside the stem {stem:?} -> {kept:?}");
            assert_eq!(kept.as_deref(), expected, "{raw:?} beside {stem:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn product_name_read_from_the_file() {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
        let read = |path: &str| {
            let start = std::time::Instant::now();
            let name = exe_product_name(path);
            println!("{path}: {name:?} in {:?}", start.elapsed());
            name
        };
        let notepad = read(&format!(r"{root}\System32\notepad.exe"));
        assert!(
            notepad
                .as_deref()
                .is_some_and(|name| name.contains("Windows")),
            "{notepad:?}"
        );
        assert_eq!(read(&format!(r"{root}\System32\no-such-file.exe")), None);

        // A file with no version resource: not a program at all.
        let plain =
            std::env::temp_dir().join(format!("aigc_no_version_{}.exe", std::process::id()));
        std::fs::write(&plain, b"not a program").unwrap();
        let read_plain = read(&plain.to_string_lossy());
        std::fs::remove_file(&plain).unwrap();
        assert_eq!(read_plain, None);

        // The build embeds the app's own version resource in every binary of
        // this crate, this test binary included.
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let this = std::env::current_exe().unwrap();
        assert_eq!(
            read(&this.to_string_lossy()).as_deref(),
            config["productName"].as_str()
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn product_name_stub_reads_nothing() {
        assert_eq!(exe_product_name(r"C:\Windows\System32\notepad.exe"), None);
    }

    #[test]
    fn frame_host_resolves_to_the_hosted_app() {
        for (exe, expected) in [
            (r"C:\Windows\System32\ApplicationFrameHost.exe", true),
            (r"c:\windows\system32\APPLICATIONFRAMEHOST.EXE", true),
            (r"C:\x\NotApplicationFrameHost.exe", false),
            (r"C:\x\foo.exe", false),
            ("", false),
        ] {
            println!("is_frame_host({exe:?}) = {}", is_frame_host(exe));
            assert_eq!(is_frame_host(exe), expected, "{exe:?}");
        }

        let child = |class: &str, pid: u32| (class.to_owned(), pid);
        let app = |pid: u32| child("Windows.UI.Core.CoreWindow", pid);
        let frame = [
            child("ApplicationFrameTitleBarWindow", 10),
            child("ApplicationFrameInputSinkWindow", 10),
        ];
        for (children, expected) in [
            (vec![app(20)], Some(20)),
            (vec![app(10)], None),
            (frame.to_vec(), None),
            (Vec::new(), None),
            (vec![app(0)], None),
            ([frame.to_vec(), vec![app(20), app(30)]].concat(), Some(20)),
        ] {
            let found = hosted_pid(10, &children);
            println!("hosted_pid(10, {children:?}) = {found:?}");
            assert_eq!(found, expected, "{children:?}");
        }

        let owned = [
            (
                "its own process",
                owned_by(7, 7, || panic!("not a host frame")),
            ),
            (
                "the app its frame shows",
                owned_by(10, 20, || vec![app(20)]),
            ),
            ("another app", !owned_by(10, 30, || vec![app(20)])),
            ("no owner", !owned_by(0, 20, || vec![app(20)])),
        ];
        for (case, passed) in owned {
            println!("owned_by, {case}: {}", if passed { "ok" } else { "WRONG" });
            assert!(passed, "owned_by, {case}");
        }
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

    /// Every hide of the overlay window goes through `hide`, which hands focus
    /// back to the game; the only other hide is the main window's tray hide.
    #[test]
    fn every_overlay_hide_goes_through_overlay_hide() {
        let needle = concat!(".hide", "())");
        let counts = crate::util::count_in_sources_by_file(needle);
        let hits: Vec<(std::path::PathBuf, usize)> = counts
            .iter()
            .filter(|(_, count)| *count > 0)
            .cloned()
            .collect();
        let total: usize = counts.iter().map(|(_, count)| count).sum();
        println!(
            "{} files scanned; {needle}: {total} in {hits:?}",
            counts.len()
        );
        assert!(!counts.is_empty(), "the source scan found no files");
        assert_eq!(total, 2, "hides outside overlay::hide: {hits:?}");
        assert_eq!(
            hits,
            [("main.rs".into(), 1), ("overlay.rs".into(), 1)],
            "one hide in overlay::hide, one for the main window"
        );
    }

    /// The panel is raised in one place, without taking activation; setting
    /// always-on-top again would be a no-op, so it never stands in for that.
    /// The minimum size is set once, at setup.
    #[test]
    fn overlay_is_raised_once_without_activating() {
        let in_file = |needle: &str| -> (usize, Vec<(std::path::PathBuf, usize)>) {
            let counts = crate::util::count_in_sources_by_file(needle);
            assert!(!counts.is_empty(), "the source scan found no files");
            let hits: Vec<(std::path::PathBuf, usize)> = counts
                .iter()
                .filter(|(_, count)| *count > 0)
                .cloned()
                .collect();
            println!("{} files scanned; {needle}: {hits:?}", counts.len());
            (counts.len(), hits)
        };
        let (files, raise) = in_file(concat!("SetWindow", "Pos("));
        assert!(files > 0, "the source scan found no files");
        assert_eq!(
            raise,
            [("overlay.rs".into(), 1)],
            "one raise, in overlay.rs"
        );
        let (_, no_activate) = in_file(concat!("SWP_NO", "ACTIVATE"));
        assert!(
            no_activate.iter().any(|(file, count)| file.as_path()
                == std::path::Path::new("overlay.rs")
                && *count >= 1),
            "the raise does not take activation: {no_activate:?}"
        );
        let (_, on_top) = in_file(concat!("set_always", "_on_top("));
        assert!(on_top.is_empty(), "always-on-top is set again: {on_top:?}");
        let (_, min_size) = in_file(concat!("set_min", "_size("));
        assert_eq!(
            min_size,
            [("main.rs".into(), 1)],
            "one minimum size, set at setup in main.rs"
        );
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
