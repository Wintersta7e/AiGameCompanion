use tauri::Manager;
use tauri::State;
use tauri_plugin_shell::ShellExt;

use crate::discovery;
use crate::models::{Game, GameSource};
use crate::state::AppState;

#[tauri::command]
pub fn get_games(state: State<'_, AppState>) -> Vec<Game> {
    let launcher = state.launcher.lock().expect("Failed to lock state");
    launcher.games.clone()
}

#[tauri::command]
pub fn scan_games(state: State<'_, AppState>) -> Vec<Game> {
    let mut steam_games = discovery::steam::discover_steam_games();

    let mut launcher = state.launcher.lock().expect("Failed to lock state");
    // Merge: preserve play_time and last_played from existing state
    for new_game in &mut steam_games {
        if let Some(existing) = launcher.games.iter().find(|g| g.id == new_game.id) {
            new_game.last_played.clone_from(&existing.last_played);
            new_game.play_time_minutes = existing.play_time_minutes;
        }
    }
    launcher.games = steam_games;
    let games = launcher.games.clone();
    drop(launcher);
    let _ = state.save();
    games
}

#[tauri::command]
pub async fn launch_game(game_id: String, app: tauri::AppHandle) -> Result<String, String> {
    let state = app.state::<AppState>();

    // Get game from state
    let game = {
        let launcher = state.launcher.lock().map_err(|e| e.to_string())?;
        launcher
            .games
            .iter()
            .find(|g| g.id == game_id)
            .cloned()
            .ok_or_else(|| format!("Game not found: {game_id}"))?
    };

    // Launch via Steam protocol URL for Steam games
    if game.source == GameSource::Steam {
        if let Some(source_id) = &game.source_id {
            let url = format!("steam://rungameid/{source_id}");
            open::that(&url).map_err(|e| format!("Failed to launch: {e}"))?;
        }
    }

    // If we have an exe_name, spawn the injector sidecar to wait for it and inject
    if !game.exe_name.is_empty() {
        let shell = app.shell();
        let sidecar = shell
            .sidecar("injector")
            .map_err(|e| format!("Sidecar error: {e}"))?
            .args(["--process", &game.exe_name, "--timeout", "30"]);

        let (_rx, _child) = sidecar
            .spawn()
            .map_err(|e| format!("Failed to spawn injector: {e}"))?;
    }

    // Update last_played timestamp
    {
        let mut launcher = state.launcher.lock().map_err(|e| e.to_string())?;
        if let Some(g) = launcher.games.iter_mut().find(|g| g.id == game_id) {
            g.last_played = Some(chrono::Local::now().to_rfc3339());
        }
    }
    let _ = state.save();

    Ok("injecting".to_string())
}
