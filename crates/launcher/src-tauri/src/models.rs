use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GameSource {
    Steam,
    Epic,
    Gog,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Game {
    pub id: String,
    pub name: String,
    pub source: GameSource,
    pub source_id: Option<String>,
    pub exe_name: String,
    pub exe_path: Option<String>,
    pub install_dir: Option<String>,
    pub cover_art_path: Option<String>,
    pub last_played: Option<String>,
    pub play_time_minutes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LauncherSettings {
    pub overlay_dll_path: Option<String>,
    pub scan_on_startup: bool,
    pub minimize_to_tray: bool,
    pub launch_on_startup: bool,
}

impl Default for LauncherSettings {
    fn default() -> Self {
        Self {
            overlay_dll_path: None,
            scan_on_startup: true,
            minimize_to_tray: true,
            launch_on_startup: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LauncherState {
    pub games: Vec<Game>,
    pub settings: LauncherSettings,
}
