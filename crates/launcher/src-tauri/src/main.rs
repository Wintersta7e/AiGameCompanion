#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod discovery;
mod models;
mod state;

use state::AppState;

fn main() {
    let state_path = std::env::current_exe()
        .expect("Failed to get exe path")
        .parent()
        .expect("Failed to get exe dir")
        .join("launcher-state.json");

    let app_state = AppState::load(state_path);

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::games::get_games,
            commands::games::scan_games,
            commands::games::launch_game,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
