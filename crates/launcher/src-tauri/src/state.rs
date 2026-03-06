use std::collections::HashMap;
use std::path::PathBuf;

use parking_lot::Mutex;
use tauri_plugin_shell::process::CommandChild;

use crate::models::LauncherState;

pub struct AppState {
    pub launcher: Mutex<LauncherState>,
    pub state_path: PathBuf,
    pub active_injectors: Mutex<HashMap<String, CommandChild>>,
}

impl AppState {
    pub fn load(state_path: PathBuf) -> Self {
        let launcher = if state_path.exists() {
            match std::fs::read_to_string(&state_path) {
                Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
                Err(_) => LauncherState::default(),
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
