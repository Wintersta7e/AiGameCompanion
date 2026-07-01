use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_opener::OpenerExt;

use crate::models::LauncherSettings;
use crate::state::AppState;

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn get_settings(state: State<'_, AppState>) -> LauncherSettings {
    let launcher = state.launcher.lock();
    launcher.settings.clone()
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn update_settings(
    settings: LauncherSettings,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    let launch_on_startup = settings.launch_on_startup;
    {
        let mut launcher = state.launcher.lock();
        launcher.settings = settings;
    }

    // Sync autostart with OS
    let autostart = app.autolaunch();
    if launch_on_startup {
        let _ = autostart.enable();
    } else {
        let _ = autostart.disable();
    }

    state.save()
}

/// Open an https URL in the default browser (Settings "Get a key" / docs links).
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn open_url(app: AppHandle, url: String) -> Result<(), String> {
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
pub fn open_config_folder(app: AppHandle) -> Result<(), String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Cannot determine data folder: {e}"))?;
    app.opener()
        .open_path(dir.to_string_lossy().as_ref(), None::<&str>)
        .map_err(|e| format!("Failed to open folder: {e}"))
}
