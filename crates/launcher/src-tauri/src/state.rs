use std::path::PathBuf;
use std::sync::Mutex;

use crate::models::LauncherState;

pub struct AppState {
    pub launcher: Mutex<LauncherState>,
    pub state_path: PathBuf,
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
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let state = self.launcher.lock().map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(&*state).map_err(|e| e.to_string())?;
        std::fs::write(&self.state_path, json).map_err(|e| e.to_string())
    }
}
