use tauri::State;

use crate::discovery;
use crate::models::Game;
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
