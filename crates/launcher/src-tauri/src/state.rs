use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use parking_lot::Mutex;
use tauri_plugin_shell::process::CommandChild;

use crate::models::LauncherState;

/// Tracks an active injection session for play time accounting.
/// The `CommandChild` is held alive so the sidecar process isn't killed on drop.
#[allow(dead_code)]
pub struct ActiveSession {
    pub child: CommandChild,
    pub started_at: Instant,
}

pub struct AppState {
    pub launcher: Mutex<LauncherState>,
    pub state_path: PathBuf,
    pub active_injectors: Mutex<HashMap<String, ActiveSession>>,
}

impl AppState {
    pub fn load(state_path: PathBuf) -> Self {
        // Recover from interrupted atomic write (tmp file left behind)
        let tmp_path = state_path.with_extension("json.tmp");
        if !state_path.exists() && tmp_path.exists() {
            let _ = std::fs::rename(&tmp_path, &state_path);
        }

        let launcher = if state_path.exists() {
            match std::fs::read_to_string(&state_path) {
                Ok(contents) => match serde_json::from_str(&contents) {
                    Ok(state) => state,
                    Err(e) => {
                        tracing::error!("Corrupt launcher state at {}: {e}", state_path.display());
                        // Backup before overwriting with defaults
                        let backup = state_path.with_extension("json.bak");
                        let _ = std::fs::copy(&state_path, &backup);
                        LauncherState::default()
                    }
                },
                Err(e) => {
                    tracing::error!("Failed to read launcher state: {e}");
                    LauncherState::default()
                }
            }
        } else {
            LauncherState::default()
        };
        Self {
            launcher: Mutex::new(launcher),
            state_path,
            active_injectors: Mutex::new(HashMap::new()),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        // Clone state and drop lock before file I/O
        let state = self.launcher.lock().clone();
        let json = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
        // Atomic write: write to temp file, then rename
        let tmp_path = self.state_path.with_extension("json.tmp");
        std::fs::write(&tmp_path, json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp_path, &self.state_path).map_err(|e| e.to_string())
    }
}
