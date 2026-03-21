use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt;

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
