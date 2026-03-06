use tauri::State;

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
pub fn update_settings(settings: LauncherSettings, state: State<'_, AppState>) -> Result<(), String> {
    {
        let mut launcher = state.launcher.lock();
        launcher.settings = settings;
    }
    state.save()
}
