use tauri::Emitter;
use tauri::Manager;
use tauri::State;
use tauri_plugin_opener::OpenerExt;

use crate::discovery;
use crate::models::{Game, GameSource};
use crate::state::AppState;

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn get_games(state: State<'_, AppState>) -> Vec<Game> {
    let launcher = state.launcher.lock();
    launcher.games.clone()
}

#[tauri::command]
pub async fn scan_games(state: State<'_, AppState>) -> Result<Vec<Game>, String> {
    tracing::info!("scan_games: starting Steam discovery");
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let result = discovery::steam::discover_steam_games();
        let _ = tx.send(result);
    });
    let mut steam_games = rx.await.map_err(|e| format!("Scan task failed: {e}"))?;
    tracing::info!("scan_games: found {} games", steam_games.len());

    let mut launcher = state.launcher.lock();
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
    if let Err(e) = state.save() {
        tracing::error!("Failed to save state: {e}");
    }
    Ok(games)
}

#[tauri::command]
pub async fn launch_game(game_id: String, app: tauri::AppHandle) -> Result<String, String> {
    // Reserve the session slot atomically (guard + insert) so two rapid launches
    // cannot both start the same game.
    {
        let state = app.state::<AppState>();
        let mut sessions = state.active_sessions.lock();
        if sessions.contains(&game_id) {
            return Err("This game is already running".to_string());
        }
        sessions.insert(game_id.clone());
    }

    match do_launch(&app, &game_id) {
        Ok(()) => Ok("launching".to_string()),
        Err(e) => {
            // Release the reservation so the game can be launched again.
            app.state::<AppState>()
                .active_sessions
                .lock()
                .remove(&game_id);
            Err(e)
        }
    }
}

/// Launch the game and attach the playtime watcher. The caller has already
/// reserved `game_id` in `active_sessions`; on `Err` the caller releases it, and
/// on the no-watcher path this releases it after emitting a terminal event.
fn do_launch(app: &tauri::AppHandle, game_id: &str) -> Result<(), String> {
    let state = app.state::<AppState>();

    let game = {
        let launcher = state.launcher.lock();
        launcher
            .games
            .iter()
            .find(|g| g.id == game_id)
            .cloned()
            .ok_or_else(|| format!("Game not found: {game_id}"))?
    };

    // Launch via Steam protocol URL for Steam games
    if game.source == GameSource::Steam {
        let source_id = game
            .source_id
            .as_ref()
            .ok_or_else(|| format!("Missing Steam app ID for game: {}", game.name))?;
        if source_id.chars().all(|c| c.is_ascii_digit()) && !source_id.is_empty() {
            let url = format!("steam://rungameid/{source_id}");
            app.opener()
                .open_url(&url, None::<&str>)
                .map_err(|e| format!("Failed to launch: {e}"))?;
        } else {
            return Err(format!("Invalid source_id: {source_id}"));
        }
    } else {
        // For non-Steam games, launch via exe_path directly
        if let Some(exe_path) = &game.exe_path {
            let path = std::path::Path::new(exe_path);
            // Validate the exe path exists and has an .exe extension
            if !path.exists() {
                return Err(format!("Executable not found: {exe_path}"));
            }
            if path.extension().and_then(|e| e.to_str()) != Some("exe") {
                return Err(format!("Invalid executable: {exe_path}"));
            }
            app.opener()
                .open_path(exe_path, None::<&str>)
                .map_err(|e| format!("Failed to launch: {e}"))?;
        } else {
            return Err(format!("No executable path for game: {}", game.name));
        }
    }

    // Attach the session watcher. Steam games are watched via Steam's own
    // running-flag (keyed by appid): authoritative, and no exe guessing. The
    // launch branch above already validated the appid is present + all-digits.
    if game.source == GameSource::Steam {
        let app_id = game.source_id.clone().unwrap_or_default();
        crate::process_watch::spawn_steam_watch(app.clone(), game_id.to_owned(), app_id);
    } else {
        // Non-Steam: watch by executable name, resolving it on demand if needed.
        let mut exe_name = game.exe_name.clone();
        if exe_name.is_empty() {
            if let Some(dir) = &game.install_dir {
                let (resolved_name, resolved_path) =
                    discovery::steam::resolve_game_exe(std::path::Path::new(dir));
                exe_name = resolved_name;
                // Cache the resolved exe for next time.
                let mut launcher = state.launcher.lock();
                if let Some(g) = launcher.games.iter_mut().find(|g| g.id == game_id) {
                    g.exe_name.clone_from(&exe_name);
                    g.exe_path = resolved_path;
                }
                drop(launcher);
                if let Err(e) = state.save() {
                    tracing::warn!("Failed to cache resolved exe: {e}");
                }
            }
        }
        // No process name to watch -- reset to idle (the game did launch).
        if exe_name.is_empty() {
            let _ = app.emit("game-finished", game_id);
            state.active_sessions.lock().remove(game_id);
        } else {
            crate::process_watch::spawn_game_watch(app.clone(), game_id.to_owned(), exe_name);
        }
    }

    // Update last_played timestamp
    {
        let mut launcher = state.launcher.lock();
        if let Some(g) = launcher.games.iter_mut().find(|g| g.id == game_id) {
            g.last_played = Some(chrono::Local::now().to_rfc3339());
        }
    }
    if let Err(e) = state.save() {
        tracing::error!("Failed to save state: {e}");
    }

    Ok(())
}

/// Resolve the directory where the companion's `config.toml` lives (next to the
/// launcher executable).
fn companion_dir() -> Result<std::path::PathBuf, String> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
        .ok_or_else(|| "Cannot determine companion directory".to_string())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn open_game_config(app: tauri::AppHandle) -> Result<(), String> {
    let dir = companion_dir()?;
    let config_path = dir.join("config.toml");
    if config_path.exists() {
        app.opener()
            .open_path(config_path.to_string_lossy().as_ref(), None::<&str>)
            .map_err(|e| format!("Failed to open config: {e}"))
    } else {
        Err(format!("config.toml not found in {}", dir.display()))
    }
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn open_game_logs(app: tauri::AppHandle) -> Result<(), String> {
    let log_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Cannot determine log directory: {e}"))?;
    let log_path = log_dir.join("launcher.log");
    if log_path.exists() {
        app.opener()
            .open_path(log_path.to_string_lossy().as_ref(), None::<&str>)
            .map_err(|e| format!("Failed to open log: {e}"))
    } else {
        Err(format!("launcher.log not found in {}", log_dir.display()))
    }
}
