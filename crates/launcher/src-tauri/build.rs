use tauri_build::{AppManifest, Attributes};

/// The names in `generate_handler!` in `src/main.rs`. A command missing from
/// either list, or from the capability of the window that calls it, is refused
/// at runtime.
const APP_COMMANDS: &[&str] = &[
    "get_games",
    "scan_games",
    "launch_game",
    "open_game_logs",
    "get_settings",
    "update_settings",
    "state_health",
    "hotkey_status",
    "open_url",
    "open_config_folder",
    "set_hints_first",
    "ask_sage",
    "cancel_sage",
    "available_providers",
    "set_active_provider",
    "translate_screen",
    "set_gemini_key",
    "recheck_clis",
    "hide_overlay",
    "link_game",
];

fn main() -> Result<(), String> {
    tauri_build::try_build(
        Attributes::new().app_manifest(AppManifest::new().commands(APP_COMMANDS)),
    )
    .map_err(|e| format!("{e:#}"))
}
