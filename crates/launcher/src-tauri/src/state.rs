use std::collections::HashSet;
use std::path::PathBuf;

use parking_lot::Mutex;

use crate::models::LauncherState;

pub struct AppState {
    pub launcher: Mutex<LauncherState>,
    pub state_path: PathBuf,
    /// Game ids with an active play session (a running process being watched).
    /// Guards against launching the same game twice.
    pub active_sessions: Mutex<HashSet<String>>,
    /// Serializes `save()` so the watcher thread and command threads cannot
    /// interleave writes to the shared temp file.
    save_lock: Mutex<()>,
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
            active_sessions: Mutex::new(HashSet::new()),
            save_lock: Mutex::new(()),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        // Serialize concurrent saves so they cannot clobber each other's temp file.
        let _write = self.save_lock.lock();
        // Clone state and drop lock before file I/O
        let state = self.launcher.lock().clone();
        let json = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
        // Atomic write: write to temp file, then rename
        let tmp_path = self.state_path.with_extension("json.tmp");
        std::fs::write(&tmp_path, json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp_path, &self.state_path).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Game, GameSource};
    use std::path::Path;

    /// Unique temp path per test (process id + label) so parallel tests
    /// never collide on the same file. Cleans any leftover first.
    fn temp_state_path(label: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "aigc_launcher_state_{}_{label}.json",
            std::process::id()
        ));
        cleanup(&p);
        p
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("json.tmp"));
        let _ = std::fs::remove_file(path.with_extension("json.bak"));
    }

    #[test]
    fn load_returns_default_when_file_absent() {
        let path = temp_state_path("absent");
        let app = AppState::load(path.clone());
        let st = app.launcher.lock();
        assert!(st.games.is_empty());
        assert!(
            st.settings.scan_on_startup,
            "default scan_on_startup is true"
        );
        drop(st);
        cleanup(&path);
    }

    #[test]
    fn save_then_load_round_trips_games() {
        let path = temp_state_path("round_trip");
        let app = AppState::load(path.clone());
        app.launcher.lock().games.push(Game {
            id: "g1".to_owned(),
            name: "Test Game".to_owned(),
            ..Default::default()
        });
        app.save().unwrap();

        let reloaded = AppState::load(path.clone());
        let st = reloaded.launcher.lock();
        assert_eq!(st.games.len(), 1);
        assert_eq!(st.games[0].id, "g1");
        assert_eq!(st.games[0].name, "Test Game");
        drop(st);
        cleanup(&path);
    }

    #[test]
    fn load_fills_defaults_for_fields_missing_in_old_json() {
        let path = temp_state_path("schema_evo");
        // Older on-disk shape: the game lacks play_time_minutes/source and the
        // settings object lacks launch_on_startup. Game and LauncherSettings
        // are #[serde(default)], so missing fields fall back to defaults rather
        // than wiping the whole state.
        let json = r#"{"games":[{"id":"g1","name":"Old","exe_name":"g.exe"}],"settings":{"scan_on_startup":false}}"#;
        std::fs::write(&path, json).unwrap();

        let app = AppState::load(path.clone());
        let st = app.launcher.lock();
        assert_eq!(st.games.len(), 1);
        assert_eq!(st.games[0].play_time_minutes, 0); // defaulted
        assert_eq!(st.games[0].source, GameSource::Manual); // defaulted
        assert!(!st.settings.scan_on_startup); // explicit value preserved
        assert!(!st.settings.launch_on_startup); // defaulted to false
        assert!(st.settings.minimize_to_tray); // defaulted to true
        drop(st);
        cleanup(&path);
    }

    #[test]
    fn corrupt_state_falls_back_to_default_and_backs_up() {
        let path = temp_state_path("corrupt");
        std::fs::write(&path, "{ this is not valid json").unwrap();

        let app = AppState::load(path.clone());
        assert!(
            app.launcher.lock().games.is_empty(),
            "corrupt state resets to default"
        );

        let backup = path.with_extension("json.bak");
        assert!(
            backup.exists(),
            "corrupt state must be backed up before reset"
        );
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            "{ this is not valid json"
        );
        cleanup(&path);
    }

    #[test]
    fn recovers_state_from_leftover_tmp_when_main_missing() {
        let path = temp_state_path("tmp_recovery");
        let tmp = path.with_extension("json.tmp");
        // Simulate an interrupted atomic write: only the .tmp survived.
        let json = r#"{"games":[{"id":"recovered","name":"R","exe_name":"r.exe"}],"settings":{}}"#;
        std::fs::write(&tmp, json).unwrap();

        let app = AppState::load(path.clone());
        assert!(
            path.exists(),
            "tmp should be promoted to the main state file"
        );
        assert!(!tmp.exists(), "tmp should be renamed away after recovery");
        assert_eq!(app.launcher.lock().games[0].id, "recovered");
        cleanup(&path);
    }

    #[test]
    fn save_is_atomic_and_leaves_no_tmp() {
        let path = temp_state_path("atomic");
        let app = AppState::load(path.clone());
        app.launcher.lock().games.push(Game {
            id: "x".to_owned(),
            ..Default::default()
        });
        app.save().unwrap();

        assert!(path.exists());
        assert!(
            !path.with_extension("json.tmp").exists(),
            "atomic save must not leave a .tmp behind"
        );
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&contents).is_ok());
        cleanup(&path);
    }
}
