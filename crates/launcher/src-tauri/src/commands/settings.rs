use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_opener::OpenerExt;

use crate::models::LauncherSettings;
use crate::state::AppState;

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn get_settings(state: State<'_, AppState>) -> LauncherSettings {
    let launcher = state.launcher.lock();
    launcher.settings.clone()
}

/// Labels of the overlay hotkeys that failed to register at startup, in
/// registration order. Empty when all of them registered.
#[derive(Debug, Default)]
pub(crate) struct HotkeyStatus(pub parking_lot::Mutex<Vec<String>>);

/// The hotkeys that could not be registered; empty means all are ready.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn hotkey_status(status: State<'_, HotkeyStatus>) -> Vec<String> {
    status.0.lock().clone()
}

/// Why the library file is read-only this run, or `None` when it is writable.
/// The main window asks before deciding whether to scan.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn state_health(state: State<'_, AppState>) -> Option<String> {
    state.load_error().map(str::to_owned)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn update_settings(
    settings: LauncherSettings,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    // Refuse before the autostart entry changes: `save()` would fail anyway in
    // read-only mode, but only after the OS entry had been rewritten.
    if let Some(reason) = state.load_error() {
        return Err(reason.to_owned());
    }
    let model = settings.gemini_model.trim();
    if !model.is_empty() {
        crate::ai::validate_gemini_model(model)?;
    }
    let launch_on_startup = settings.launch_on_startup;
    {
        let mut launcher = state.launcher.lock();
        let merged = merge_settings(&launcher.settings, settings);
        launcher.settings = merged;
    }

    // Sync autostart with OS. Log failures: a registry write blocked by policy
    // or AV would otherwise leave the toggle showing a state the OS does not
    // actually have, with nothing anywhere to explain it.
    let autostart = app.autolaunch();
    let synced = if launch_on_startup {
        autostart.enable()
    } else {
        autostart.disable()
    };
    if let Err(e) = synced {
        tracing::warn!("Failed to set launch-on-startup to {launch_on_startup}: {e}");
    }

    let saved = state.save();
    // The Gemini model may have changed, and with it every model label.
    crate::ai::notify_providers_changed(&app);
    saved
}

/// What Save stores: the modal's values with the model trimmed, except the
/// provider. Only `set_active_provider` writes that, so a provider picked in the
/// overlay while Settings is open is not reverted by Save.
fn merge_settings(stored: &LauncherSettings, incoming: LauncherSettings) -> LauncherSettings {
    let mut merged = incoming;
    merged.active_provider.clone_from(&stored.active_provider);
    merged.gemini_model = merged.gemini_model.trim().to_owned();
    merged
}

/// Open an https URL in the default browser (Settings "Get a key" / docs links).
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn open_url(app: AppHandle, url: String) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err("Only https links can be opened.".to_owned());
    }
    app.opener()
        .open_url(&url, None::<&str>)
        .map_err(|e| format!("Failed to open link: {e}"))
}

/// Open the launcher's data folder (state + logs live here).
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn open_config_folder(app: AppHandle) -> Result<(), String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Cannot determine data folder: {e}"))?;
    app.opener()
        .open_path(dir.to_string_lossy().as_ref(), None::<&str>)
        .map_err(|e| format!("Failed to open folder: {e}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::print_stdout, reason = "the scans print what they counted")]

    use super::merge_settings;
    use crate::models::LauncherSettings;

    #[test]
    fn no_config_button_command() {
        let needle = concat!("open_game", "_config");
        let (files, count) = crate::util::count_in_sources(needle, None);
        println!("scanned {files} files, found {count} occurrence(s) of {needle}");
        assert!(files > 0, "the source scan found no files");
        assert_eq!(
            count, 0,
            "nothing may open a config.toml the app does not ship"
        );
    }

    #[test]
    fn no_error_sends_users_to_config_toml() {
        for needle in [
            concat!("Check ", "config.toml"),
            concat!("in ", "config.toml"),
        ] {
            let (files, count) = crate::util::count_in_sources(needle, None);
            println!("scanned {files} files, found {count} occurrence(s) of {needle}");
            assert!(files > 0, "the source scan found no files");
            assert_eq!(
                count, 0,
                "settings live in the Settings window, not config.toml"
            );
        }
    }

    #[test]
    fn save_keeps_the_stored_provider() {
        let stored = LauncherSettings {
            active_provider: "claude".to_owned(),
            ..LauncherSettings::default()
        };
        let incoming = LauncherSettings {
            scan_on_startup: false,
            minimize_to_tray: false,
            launch_on_startup: true,
            active_provider: "gemini".to_owned(),
            gemini_model: " gemini-3.8-flash ".to_owned(),
        };
        let merged = merge_settings(&stored, incoming);
        assert_eq!(merged.active_provider, "claude");
        assert_eq!(merged.gemini_model, "gemini-3.8-flash");
        assert!(!merged.scan_on_startup);
        assert!(!merged.minimize_to_tray);
        assert!(merged.launch_on_startup);
    }
}
