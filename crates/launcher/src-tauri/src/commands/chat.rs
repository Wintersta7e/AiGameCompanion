//! The overlay's saved chat.

use tauri::{AppHandle, Manager};

use crate::chat_store::{self, ChatStore, ChatView};
use crate::overlay::{self, GameInfo};
use crate::state::AppState;

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
    // Without the launcher state nothing may be written.
    let mode = app
        .try_state::<AppState>()
        .map_or(chat_store::StoreMode { read_only: true }, |state| {
            chat_store::store_mode(&state)
        });
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
