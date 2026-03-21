use tauri::Emitter;
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

    // Guard: prevent duplicate injection for the same game
    if state.active_injectors.lock().contains_key(&game_id) {
        return Err("Injector already active for this game".to_string());
    }

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
        let source_id = game.source_id.as_ref()
            .ok_or_else(|| format!("Missing Steam app ID for game: {}", game.name))?;
        if source_id.chars().all(|c| c.is_ascii_digit()) && !source_id.is_empty() {
            let url = format!("steam://rungameid/{source_id}");
            app.opener().open_url(&url, None::<&str>).map_err(|e| format!("Failed to launch: {e}"))?;
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
            if let Err(e) = state.save() {
                tracing::warn!("Failed to cache resolved exe: {e}");
            }
        }
    }

    // Validate exe_name before passing to sidecar (must match capability regex: ^[\w.-]+\.exe$)
    if !exe_name.is_empty() {
        let has_exe_ext = std::path::Path::new(&exe_name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
        let is_valid = has_exe_ext
            && exe_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-');
        if !is_valid {
            return Err(format!("Invalid exe name: {exe_name}"));
        }

        let shell = app.shell();
        let sidecar = shell
            .sidecar("injector")
            .map_err(|e| format!("Sidecar error: {e}"))?
            .args(["--process", &exe_name, "--timeout", "30"]);

        let (mut rx, child) = sidecar
            .spawn()
            .map_err(|e| format!("Failed to spawn injector: {e}"))?;
        state.active_injectors.lock().insert(game_id.clone(), child);

        // Listen for sidecar exit and emit event to frontend
        let app_handle = app.clone();
        let gid = game_id.clone();
        tauri::async_runtime::spawn(async move {
            use tauri_plugin_shell::process::CommandEvent;
            while let Some(event) = rx.recv().await {
                match event {
                    CommandEvent::Terminated(_) | CommandEvent::Error(_) => {
                        let _ = app_handle.emit("injector-finished", &gid);
                        let s = app_handle.state::<AppState>();
                        s.active_injectors.lock().remove(&gid);
                        break;
                    }
                    _ => {}
                }
            }
        });
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

/// Resolve the directory containing the overlay DLL (config.toml + companion.log live here).
fn overlay_dir(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    let state = app.state::<AppState>();
    let custom = state.launcher.lock().settings.overlay_dll_path.clone();
    if let Some(ref dll_path) = custom {
        let p = std::path::Path::new(dll_path);
        if let Some(parent) = p.parent() {
            return Ok(parent.to_path_buf());
        }
    }
    // Default: same directory as the launcher exe
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
        .ok_or_else(|| "Cannot determine overlay directory".to_string())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn open_game_config(app: tauri::AppHandle) -> Result<(), String> {
    let dir = overlay_dir(&app)?;
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
    let dir = overlay_dir(&app)?;
    let log_path = dir.join("companion.log");
    if log_path.exists() {
        app.opener()
            .open_path(log_path.to_string_lossy().as_ref(), None::<&str>)
            .map_err(|e| format!("Failed to open log: {e}"))
    } else {
        Err(format!("companion.log not found in {}", dir.display()))
    }
}
