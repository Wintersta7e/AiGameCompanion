//! The overlay's saved chat.

use tauri::{AppHandle, Emitter, Manager};

use crate::chat_store::{self, ChatStore, ChatView, StoreMode};
use crate::overlay::{self, GameInfo};
use crate::state::AppState;
use crate::util;

/// What the overlay shows when clearing the chat did not run to its end.
const CLEAR_FAILED: &str = "The chat could not be cleared.";

/// This run's store mode. Without the launcher state nothing may be written.
pub(crate) fn mode_of(app: &AppHandle) -> StoreMode {
    app.try_state::<AppState>()
        .map_or(StoreMode { read_only: true }, |state| {
            chat_store::store_mode(&state)
        })
}

/// The key of the chat the overlay may show for the window it displayed as
/// `hwnd`/`pid`: only when the stored target is still that window and linked.
fn shown_key(target: Option<&GameInfo>, hwnd: i64, pid: u32) -> Option<String> {
    target
        .filter(|game| game.hwnd == hwnd && game.pid == pid)
        .and_then(overlay::game_key)
}

/// The saved chat of the linked window the overlay shows, which then counts as
/// the chat on screen. `None` -- and no chat on screen -- for any other window
/// than the overlay, a window that is not linked, or one replaced while the
/// chat was read.
#[tauri::command]
pub(crate) async fn load_chat(
    app: AppHandle,
    window: tauri::WebviewWindow,
    hwnd: i64,
    pid: u32,
) -> Option<ChatView> {
    if window.label() != "overlay" {
        return None;
    }
    let store = app.try_state::<ChatStore>()?;
    let Some(key) = shown_key(overlay::linked_game(&app).as_ref(), hwnd, pid) else {
        store.hide();
        return None;
    };
    let mode = mode_of(&app);
    let worker = app.clone();
    let loading = key.clone();
    let loaded = tauri::async_runtime::spawn_blocking(move || {
        if let Some(store) = worker.try_state::<ChatStore>() {
            store.load(&loading, mode);
        }
    })
    .await;
    let still_shown = shown_key(overlay::linked_game(&app).as_ref(), hwnd, pid);
    if loaded.is_err() || still_shown.as_deref() != Some(key.as_str()) {
        store.hide();
        return None;
    }
    Some(store.show(&key, mode))
}

/// New chat: empty the linked game's chat on screen and delete its saved
/// copy. Refused when the game, or the chat on screen, changed since the
/// page's `display_generation`.
#[tauri::command]
pub(crate) async fn clear_chat(
    app: AppHandle,
    display_generation: u64,
) -> Result<ChatView, String> {
    let key = overlay::linked_game(&app)
        .as_ref()
        .and_then(overlay::game_key);
    let mode = mode_of(&app);
    tauri::async_runtime::spawn_blocking(move || {
        app.try_state::<ChatStore>().map_or_else(
            || Err(CLEAR_FAILED.to_owned()),
            |store| store.clear(key.as_deref(), display_generation, mode),
        )
    })
    .await
    .unwrap_or_else(|_| Err(CLEAR_FAILED.to_owned()))
}

/// Settings: delete every game's saved chat. The overlay is told, as the
/// chat it shows may be gone, unless nothing could be deleted.
#[tauri::command]
pub(crate) async fn delete_saved_chats(app: AppHandle) -> Result<u32, String> {
    let mode = mode_of(&app);
    let worker = app.clone();
    let deleted = tauri::async_runtime::spawn_blocking(move || {
        worker
            .try_state::<ChatStore>()
            .map_or(Ok(0), |store| store.delete_all(mode))
    })
    .await
    .unwrap_or_else(|_| Err(chat_store::DELETE_PARTIAL.to_owned()));
    if !mode.read_only {
        util::log_if_err(
            "Telling the overlay the saved chats changed",
            app.emit_to("overlay", "saved-chats-changed", ()),
        );
    }
    deleted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(linked: bool) -> GameInfo {
        GameInfo {
            hwnd: 42,
            pid: 7,
            exe: r"C:\Games\Foo\foo.exe".to_owned(),
            name: "Real Name".to_owned(),
            linked,
            game_key: r"exe:c:\games\foo\foo.exe".to_owned(),
            ..GameInfo::default()
        }
    }

    #[test]
    fn load_needs_the_shown_linked_window() {
        let linked = shown(true);
        let unlinked = shown(false);
        let rows = [
            ("no target", None, 42, 7, None),
            ("an unlinked target", Some(&unlinked), 42, 7, None),
            ("another window", Some(&linked), 43, 7, None),
            ("another process", Some(&linked), 42, 8, None),
            (
                "the linked window shown",
                Some(&linked),
                42,
                7,
                Some(r"exe:c:\games\foo\foo.exe"),
            ),
        ];
        for (name, target, hwnd, pid, expected) in rows {
            let key = shown_key(target, hwnd, pid);
            println!("{name}: {}", if key.is_some() { "a key" } else { "none" });
            assert_eq!(key.as_deref(), expected, "{name}");
        }
    }
}
