// Module-wide because serde's generated code for `LauncherState` names the
// map type too, outside any item an attribute here could cover.
#![expect(
    clippy::zero_sized_map_values,
    reason = "GamePrefs has no field until the first per-game setting joins it"
)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
// Deserialize via String so an unrecognised source (a state file written by a
// newer build, then opened by an older one) degrades to `Manual` instead of
// failing the whole file and resetting the user's library.
#[serde(rename_all = "snake_case", from = "String")]
pub(crate) enum GameSource {
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
pub(crate) struct Game {
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
pub(crate) struct LauncherSettings {
    pub scan_on_startup: bool,
    pub minimize_to_tray: bool,
    pub launch_on_startup: bool,
    /// Overlay AI provider selection ("gemini" / "claude" / "openai"); empty
    /// until the user picks one, when the first available provider is shown.
    pub active_provider: String,
    /// Gemini model chosen in Settings; empty = the default model.
    pub gemini_model: String,
}

impl Default for LauncherSettings {
    fn default() -> Self {
        Self {
            scan_on_startup: true,
            minimize_to_tray: true,
            launch_on_startup: false,
            active_provider: String::new(),
            gemini_model: String::new(),
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

/// Per-game values the user sets, keyed by a game key. Never sent to the page:
/// each value reaches it only through its own command.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct GamePrefs {}

/// Load `game_prefs` one entry at a time, like `games`: an unreadable entry is
/// dropped, and a value that is not an object loads as empty. The warning never
/// names the key, which may hold a local path.
fn game_prefs_lenient<'de, D>(deserializer: D) -> Result<BTreeMap<String, GamePrefs>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let serde_json::Value::Object(raw) = serde_json::Value::deserialize(deserializer)? else {
        tracing::warn!("Dropping unreadable per-game preferences: not an object");
        return Ok(BTreeMap::new());
    };
    Ok(raw
        .into_iter()
        .filter_map(
            |(key, value)| match serde_json::from_value::<GamePrefs>(value) {
                Ok(prefs) => Some((key, prefs)),
                Err(e) => {
                    tracing::warn!("Dropping an unreadable per-game preferences entry: {e}");
                    None
                }
            },
        )
        .collect())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
// `#[serde(default)]` here as well as on the inner structs: without it, adding
// any new top-level field makes every existing state file fail to parse, which
// resets the library on upgrade.
#[serde(default)]
pub(crate) struct LauncherState {
    #[serde(deserialize_with = "games_lenient")]
    pub games: Vec<Game>,
    pub settings: LauncherSettings,
    /// Kept beside the library rather than on `Game`: a scan rebuilds Steam
    /// games and would erase a value stored on one.
    #[serde(deserialize_with = "game_prefs_lenient")]
    pub game_prefs: BTreeMap<String, GamePrefs>,
}

#[cfg(test)]
mod tests {
    use super::LauncherSettings;
    use std::path::Path;

    /// The page's one `LauncherSettings` type carries every field Rust sends,
    /// with the same JSON type, and nothing Rust lacks: Save sends the object
    /// back, and serde would drop an unknown field without a word.
    #[test]
    fn launcher_settings_type_mirrors_rust() {
        let source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/lib/settings.ts"
        ));
        let typescript =
            crate::util::ts_fields(source, concat!("export interface Launcher", "Settings {"));
        let rust =
            crate::util::json_fields(&serde_json::to_value(LauncherSettings::default()).unwrap());
        println!("Rust:       {rust:?}");
        println!("TypeScript: {typescript:?}");
        assert!(
            !rust.is_empty() && !typescript.is_empty(),
            "no LauncherSettings fields found"
        );
        let missing: Vec<String> = rust
            .iter()
            .filter(|(key, ty)| {
                !typescript
                    .iter()
                    .any(|(name, ts_ty, _)| name == key && ts_ty == ty)
            })
            .map(|(key, ty)| format!("{key}: {ty}"))
            .collect();
        let extra: Vec<&str> = typescript
            .iter()
            .filter(|(name, _, _)| !rust.iter().any(|(key, _)| key == name))
            .map(|(name, _, _)| name.as_str())
            .collect();
        println!(
            "checked {} of {} fields",
            rust.len() - missing.len(),
            rust.len()
        );
        assert!(
            missing.is_empty() && extra.is_empty(),
            "missing or mistyped in TypeScript: {missing:?}; not in Rust: {extra:?}"
        );

        let scan = crate::util::count_in_frontend(&[
            concat!("interface Launcher", "Settings"),
            concat!("type Launcher", "Settings"),
            concat!("'get_", "settings'"),
            concat!("invoke<Launcher", "Settings>('get_settings')"),
        ]);
        println!("{} files scanned", scan.files.len());
        for (needle, hits) in ["interface", "type", "get_settings", "typed get_settings"]
            .iter()
            .zip(&scan.hits)
        {
            println!("{needle}: {} {hits:?}", hits.len());
        }
        let declarations: Vec<&Path> = scan.hits[0]
            .iter()
            .chain(&scan.hits[1])
            .map(|(file, _)| file.as_path())
            .collect();
        assert_eq!(
            declarations,
            [Path::new("src/lib/settings.ts")],
            "LauncherSettings is declared once, in src/lib/settings.ts"
        );
        assert!(!scan.hits[2].is_empty(), "no get_settings call found");
        assert_eq!(
            scan.hits[2].len(),
            scan.hits[3].len(),
            "every get_settings call is typed with LauncherSettings"
        );
    }

    /// A scan rebuilds every Steam `Game` and carries over only its play
    /// record, so a value the user sets on a game would be erased there.
    #[test]
    fn game_holds_no_per_game_value() {
        let value = serde_json::to_value(super::Game::default()).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        println!("Game keys: {keys:?}");
        let mut expected = [
            "id",
            "name",
            "source",
            "source_id",
            "exe_name",
            "exe_path",
            "install_dir",
            "cover_art_path",
            "last_played",
            "play_time_minutes",
        ];
        expected.sort_unstable();
        assert_eq!(
            keys, expected,
            "a per-game value the user sets belongs in LauncherState.game_prefs, not on Game: a scan rebuilds Steam games"
        );
    }

    #[test]
    fn gemini_model_defaults_and_round_trips() {
        let old: LauncherSettings =
            serde_json::from_str(r#"{"scan_on_startup":false,"active_provider":"claude"}"#)
                .expect("settings saved before the model field load");
        assert_eq!(old.gemini_model, "");
        assert_eq!(old.active_provider, "claude");
        assert!(!old.scan_on_startup);

        let chosen: LauncherSettings =
            serde_json::from_str(r#"{"gemini_model":"gemini-3.8-flash"}"#)
                .expect("settings with a model load");
        assert_eq!(chosen.gemini_model, "gemini-3.8-flash");
        let value = serde_json::to_value(&chosen).expect("settings serialise");
        assert_eq!(value["gemini_model"], "gemini-3.8-flash");
    }

    #[test]
    fn new_install_has_no_saved_provider() {
        assert_eq!(LauncherSettings::default().active_provider, "");
        let without: LauncherSettings = serde_json::from_str(r#"{"scan_on_startup":false}"#)
            .expect("settings without a provider load");
        assert_eq!(without.active_provider, "");
        let saved: LauncherSettings = serde_json::from_str(r#"{"active_provider":"gemini"}"#)
            .expect("settings with a provider load");
        assert_eq!(saved.active_provider, "gemini", "a stored choice is kept");
    }
}
