use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
// Deserialize via String so an unrecognised source (a state file written by a
// newer build, then opened by an older one) degrades to `Manual` instead of
// failing the whole file and resetting the user's library.
#[serde(rename_all = "snake_case", from = "String")]
pub enum GameSource {
    Steam,
    Epic,
    Gog,
    #[default]
    Manual,
}

impl From<String> for GameSource {
    fn from(value: String) -> Self {
        match value.as_str() {
            "steam" => Self::Steam,
            "epic" => Self::Epic,
            "gog" => Self::Gog,
            other => {
                if other != "manual" {
                    tracing::warn!("Unknown game source {other:?}, treating as manual");
                }
                Self::Manual
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
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
#[serde(default)]
pub struct LauncherSettings {
    pub scan_on_startup: bool,
    pub minimize_to_tray: bool,
    pub launch_on_startup: bool,
    /// Overlay AI provider selection ("gemini" / "claude" / "openai").
    pub active_provider: String,
}

impl Default for LauncherSettings {
    fn default() -> Self {
        Self {
            scan_on_startup: true,
            minimize_to_tray: true,
            launch_on_startup: false,
            active_provider: "gemini".to_owned(),
        }
    }
}

/// Drop individual unreadable game entries rather than failing the whole file.
/// Without this, one bad field value (a hand edit, a type change between
/// versions) costs the user their entire library.
fn games_lenient<'de, D>(deserializer: D) -> Result<Vec<Game>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<serde_json::Value>::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .filter_map(|value| match serde_json::from_value::<Game>(value) {
            Ok(game) => Some(game),
            Err(e) => {
                tracing::warn!("Dropping unreadable game entry: {e}");
                None
            }
        })
        .collect())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
// `#[serde(default)]` here as well as on the inner structs: without it, adding
// any new top-level field makes every existing state file fail to parse, which
// resets the library on upgrade.
#[serde(default)]
pub struct LauncherState {
    #[serde(deserialize_with = "games_lenient")]
    pub games: Vec<Game>,
    pub settings: LauncherSettings,
}
