use tauri::Manager;
use tauri::State;
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_shell::ShellExt;

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
    let state = app.state::<AppState>();

    // Get game from state
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
        if let Some(source_id) = &game.source_id {
            if source_id.chars().all(|c| c.is_ascii_digit()) && !source_id.is_empty() {
                let url = format!("steam://rungameid/{source_id}");
                app.opener().open_url(&url, None::<&str>).map_err(|e| format!("Failed to launch: {e}"))?;
            } else {
                return Err(format!("Invalid source_id: {source_id}"));
            }
        }
    } else {
        // For non-Steam games, launch via exe_path directly
        if let Some(exe_path) = &game.exe_path {
            app.opener().open_path(exe_path, None::<&str>).map_err(|e| format!("Failed to launch: {e}"))?;
        } else {
            return Err(format!("No executable path for game: {}", game.name));
        }
    }

    // Resolve exe name on demand if not cached
    let mut exe_name = game.exe_name.clone();
    if exe_name.is_empty() {
        if let Some(dir) = &game.install_dir {
            let (resolved_name, resolved_path) =
                discovery::steam::resolve_game_exe(std::path::Path::new(dir));
            exe_name = resolved_name;
            // Cache the resolved exe for next time
            let mut launcher = state.launcher.lock();
            if let Some(g) = launcher.games.iter_mut().find(|g| g.id == game_id) {
                g.exe_name.clone_from(&exe_name);
                g.exe_path = resolved_path;
            }
            drop(launcher);
            let _ = state.save();
        }
    }

    // Validate exe_name before passing to sidecar
    if !exe_name.is_empty() {
        if exe_name.contains("--") || exe_name.contains('/') || exe_name.contains('\\') {
            return Err(format!("Invalid exe name: {exe_name}"));
        }

        let shell = app.shell();
        let sidecar = shell
            .sidecar("injector")
            .map_err(|e| format!("Sidecar error: {e}"))?
            .args(["--process", &exe_name, "--timeout", "30"]);

        let (_rx, child) = sidecar
            .spawn()
            .map_err(|e| format!("Failed to spawn injector: {e}"))?;
        state.active_injectors.lock().insert(game_id.clone(), child);
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

    Ok("injecting".to_string())
}

#[tauri::command]
pub fn open_game_config(game_id: String, app: tauri::AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let install_dir = {
        let launcher = state.launcher.lock();
        launcher
            .games
            .iter()
            .find(|g| g.id == game_id)
            .and_then(|g| g.install_dir.clone())
            .ok_or_else(|| format!("No install dir for game: {game_id}"))?
    };
    let config_path = std::path::Path::new(&install_dir).join("config.toml");
    if config_path.exists() {
        app.opener()
            .open_path(config_path.to_string_lossy().as_ref(), None::<&str>)
            .map_err(|e| format!("Failed to open config: {e}"))
    } else {
        Err(format!("Config not found: {}", config_path.display()))
    }
}

#[tauri::command]
pub fn open_game_logs(game_id: String, app: tauri::AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let install_dir = {
        let launcher = state.launcher.lock();
        launcher
            .games
            .iter()
            .find(|g| g.id == game_id)
            .and_then(|g| g.install_dir.clone())
            .ok_or_else(|| format!("No install dir for game: {game_id}"))?
    };
    let log_path = std::path::Path::new(&install_dir).join("companion.log");
    if log_path.exists() {
        app.opener()
            .open_path(log_path.to_string_lossy().as_ref(), None::<&str>)
            .map_err(|e| format!("Failed to open log: {e}"))
    } else {
        Err(format!("Log not found: {}", log_path.display()))
    }
}
