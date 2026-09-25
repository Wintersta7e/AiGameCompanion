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
    let launch_on_startup = settings.launch_on_startup;
    {
        let mut launcher = state.launcher.lock();
        launcher.settings = settings;
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

    state.save()
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
