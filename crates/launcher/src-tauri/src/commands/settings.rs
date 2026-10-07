use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_opener::OpenerExt;

use crate::models::LauncherSettings;
use crate::state::AppState;

#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "a Tauri command receives its arguments by value"
)]
pub(crate) fn get_settings(state: State<'_, AppState>) -> LauncherSettings {
    let launcher = state.launcher.lock();
    launcher.settings.clone()
}

/// Labels of the overlay hotkeys that failed to register at startup, in
/// registration order. Empty when all of them registered.
#[derive(Debug, Default)]
pub(crate) struct HotkeyStatus(pub parking_lot::Mutex<Vec<String>>);

/// The hotkeys that could not be registered; empty means all are ready.
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "a Tauri command receives its arguments by value"
)]
pub(crate) fn hotkey_status(status: State<'_, HotkeyStatus>) -> Vec<String> {
    status.0.lock().clone()
}

/// Why the library file is read-only this run, or `None` when it is writable.
/// The main window asks before deciding whether to scan.
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "a Tauri command receives its arguments by value"
)]
pub(crate) fn state_health(state: State<'_, AppState>) -> Option<String> {
    state.load_error().map(str::to_owned)
}

#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "a Tauri command receives its arguments by value"
)]
pub(crate) fn update_settings(
    settings: LauncherSettings,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    // Refuse before the autostart entry changes: `save()` would fail anyway in
    // read-only mode, but only after the OS entry had been rewritten.
    if let Some(reason) = state.load_error() {
        return Err(reason.to_owned());
    }
    let model = settings.gemini_model.trim();
    if !model.is_empty() {
        crate::ai::validate_gemini_model(model)?;
    }
    let launch_on_startup = settings.launch_on_startup;
    {
        let mut launcher = state.launcher.lock();
        let merged = merge_settings(&launcher.settings, settings);
        launcher.settings = merged;
    }

    // Sync autostart with OS. Log failures: a registry write blocked by policy
    // or AV would otherwise leave the toggle showing a state the OS does not
    // actually have, with nothing anywhere to explain it.
    let autostart = app.autolaunch();
    let synced = if launch_on_startup {
        autostart.enable()
    } else {
        autostart.disable()
    };
    if let Err(e) = synced {
        tracing::warn!("Failed to set launch-on-startup to {launch_on_startup}: {e}");
    }

    let saved = state.save();
    // The Gemini model may have changed, and with it every model label.
    crate::ai::notify_providers_changed(&app);
    saved
}

/// What Save stores: the fields the Settings modal owns, from `incoming`, with
/// the model trimmed. Every other field is written only by its own command, so
/// Save keeps the stored value -- a provider picked in the overlay while
/// Settings is open is not reverted. The destructure names every field: a new
/// one does not compile until its writer is chosen here.
fn merge_settings(stored: &LauncherSettings, incoming: LauncherSettings) -> LauncherSettings {
    let LauncherSettings {
        scan_on_startup,
        minimize_to_tray,
        launch_on_startup,
        // Written only by `set_active_provider`.
        active_provider: _,
        gemini_model,
        // Written only by `set_hints_first`.
        hints_first: _,
    } = incoming;
    LauncherSettings {
        scan_on_startup,
        minimize_to_tray,
        launch_on_startup,
        gemini_model: gemini_model.trim().to_owned(),
        ..stored.clone()
    }
}

/// Remember the overlay's hints-first switch. Refused in read-only mode before
/// anything changes; the overlay then keeps the choice for this session only.
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "a Tauri command receives its arguments by value"
)]
pub(crate) fn set_hints_first(on: bool, state: State<'_, AppState>) -> Result<(), String> {
    store_hints_first(&state, on)
}

fn store_hints_first(state: &AppState, on: bool) -> Result<(), String> {
    state.edit_settings(|settings| settings.hints_first = on)
}

/// The longest link that is opened, in bytes.
const MAX_LINK_BYTES: usize = 2048;

/// `raw` as an https web address fit for the browser, or why it is not.
///
/// Refuses every scheme but https, IP literals, this computer and one-word
/// (local network) hosts, and addresses carrying credentials. The result is the
/// parsed serialization, so what reaches the browser is percent-encoded. No
/// reason contains the input.
pub(crate) fn checked_link(raw: &str) -> Result<tauri::Url, &'static str> {
    if raw.len() > MAX_LINK_BYTES {
        return Err("The link is too long.");
    }
    let url = tauri::Url::parse(raw).map_err(|_| "The link is not a web address.")?;
    if url.scheme() != "https" {
        return Err("Only https links can be opened.");
    }
    let Some(domain) = url.domain() else {
        return Err("Links to IP addresses are not opened.");
    };
    let name = domain.trim_end_matches('.');
    if !name.contains('.') || name == "localhost" || name.ends_with(".localhost") {
        return Err("Links to this computer or to one-word hosts are not opened.");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Links carrying a user name or password are not opened.");
    }
    Ok(url)
}

/// Open an https web address in the default browser once `checked_link`
/// accepts it. Neither the log nor the returned error ever contains the link.
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "a Tauri command receives its arguments by value"
)]
pub(crate) fn open_url(app: AppHandle, url: String) -> Result<(), String> {
    let checked = checked_link(&url).map_err(|reason| {
        tracing::warn!("Link not opened: {reason}");
        reason.to_owned()
    })?;
    app.opener()
        .open_url(checked.as_str(), None::<&str>)
        .map_err(|e| {
            let error = e.to_string().replace(checked.as_str(), "[link]");
            tracing::warn!("Link could not be opened: {error}");
            "The link could not be opened.".to_owned()
        })
}

/// Open the launcher's data folder (state + logs live here).
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "a Tauri command receives its arguments by value"
)]
pub(crate) fn open_config_folder(app: AppHandle) -> Result<(), String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Cannot determine data folder: {e}"))?;
    app.opener()
        .open_path(dir.to_string_lossy().as_ref(), None::<&str>)
        .map_err(|e| format!("Failed to open folder: {e}"))
}

#[cfg(test)]
mod tests {
    use super::{checked_link, merge_settings};
    use crate::models::LauncherSettings;

    #[test]
    fn checked_link_table() {
        let shown = |row: &str| row.chars().take(60).collect::<String>();
        let prefix = "https://example.com/";
        let longest = format!("{prefix}{}", "a".repeat(2048 - prefix.len()));
        let too_long = format!("{longest}a");
        let mut wrong = Vec::new();

        let accepted = [
            "https://example.com/x",
            "https://example.com:8443/",
            "https://www.google.com/search?q=a%20b",
            longest.as_str(),
        ];
        for row in accepted {
            match checked_link(row) {
                Ok(url) if url.as_str() == row => println!("accepted {}", shown(row)),
                other => wrong.push(format!(
                    "{} should pass unchanged: {:?}",
                    shown(row),
                    other.map(|url| shown(url.as_str()))
                )),
            }
        }

        let rejected = [
            "http://example.com/",
            "javascript:alert(1)",
            "data:,x",
            "file:///C:/x",
            "https://user:pw@example.com/",
            "https://127.0.0.1/",
            "https://127.1/",
            "https://0x7f.1/",
            "https://[::1]/",
            "https://localhost/",
            "https://localhost./",
            "https://a.localhost/",
            "https://tauri.localhost/",
            "https://router/",
            too_long.as_str(),
            "not a url",
        ];
        let mut old_check_passed = 0;
        for row in rejected {
            match checked_link(row) {
                Err(reason) if !reason.contains(row) => {
                    println!("rejected {}: {reason}", shown(row));
                }
                other => wrong.push(format!(
                    "{} should be refused: {:?}",
                    shown(row),
                    other.map(|url| shown(url.as_str()))
                )),
            }
            if row.starts_with("https://") {
                old_check_passed += 1;
            }
        }

        let odd = r#"https://example.com/a b"c?q=x" --flag y"#;
        match checked_link(odd) {
            Ok(url) if !url.as_str().contains([' ', '"']) => println!("serialized as {url}"),
            other => wrong.push(format!("{odd} should serialize encoded: {other:?}")),
        }

        println!(
            "rejected {} rows; {old_check_passed} are https:// rows the old prefix check accepted",
            rejected.len()
        );
        assert_eq!(old_check_passed, 11);
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[test]
    fn no_config_button_command() {
        let needle = concat!("open_game", "_config");
        let (files, count) = crate::util::count_in_sources(needle, None);
        println!("scanned {files} files, found {count} occurrence(s) of {needle}");
        assert!(files > 0, "the source scan found no files");
        assert_eq!(
            count, 0,
            "nothing may open a config.toml the app does not ship"
        );
    }

    #[test]
    fn no_error_sends_users_to_config_toml() {
        for needle in [
            concat!("Check ", "config.toml"),
            concat!("in ", "config.toml"),
        ] {
            let (files, count) = crate::util::count_in_sources(needle, None);
            println!("scanned {files} files, found {count} occurrence(s) of {needle}");
            assert!(files > 0, "the source scan found no files");
            assert_eq!(
                count, 0,
                "settings live in the Settings window, not config.toml"
            );
        }
    }

    #[test]
    fn save_keeps_the_stored_provider() {
        let stored = LauncherSettings {
            active_provider: "claude".to_owned(),
            ..LauncherSettings::default()
        };
        let incoming = LauncherSettings {
            scan_on_startup: false,
            minimize_to_tray: false,
            launch_on_startup: true,
            active_provider: "gemini".to_owned(),
            gemini_model: " gemini-3.8-flash ".to_owned(),
            hints_first: true,
        };
        let merged = merge_settings(&stored, incoming);
        assert_eq!(merged.active_provider, "claude");
        assert_eq!(merged.gemini_model, "gemini-3.8-flash");
        assert!(!merged.scan_on_startup);
        assert!(!merged.minimize_to_tray);
        assert!(merged.launch_on_startup);
    }

    #[test]
    fn save_keeps_hints_first() {
        for stored in [false, true] {
            let merged = merge_settings(
                &LauncherSettings {
                    hints_first: stored,
                    ..LauncherSettings::default()
                },
                LauncherSettings {
                    hints_first: !stored,
                    ..LauncherSettings::default()
                },
            );
            println!(
                "stored {stored}, Save sends {}: {} kept",
                !stored, merged.hints_first
            );
            assert_eq!(merged.hints_first, stored, "Save changed hints first");
        }
    }

    /// A state file path of this test's own: process id plus label, with
    /// nothing left there from an earlier run.
    fn temp_state_path(label: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("aigc_settings_{}_{label}.json", std::process::id()));
        remove(&path);
        path
    }

    /// Remove the state file (or the directory standing in for it) and its
    /// temp file, where they exist.
    fn remove(path: &std::path::Path) {
        for path in [path.to_path_buf(), path.with_extension("json.tmp")] {
            if path.is_dir() {
                std::fs::remove_dir(&path).unwrap();
            } else if path.exists() {
                std::fs::remove_file(&path).unwrap();
            }
        }
    }

    #[test]
    fn hints_first_setter_refuses_read_only_first() {
        // A directory where the file should be cannot be read: read-only.
        let path = temp_state_path("hints_read_only");
        std::fs::create_dir(&path).unwrap();
        let state = crate::state::AppState::load(path.clone());
        let refused = super::store_hints_first(&state, false);
        println!("read-only: {refused:?}");
        assert_eq!(refused, Err(state.load_error().unwrap().to_owned()));
        assert!(
            state.launcher.lock().settings.hints_first,
            "a refused write changed memory"
        );
        assert!(!path.with_extension("json.tmp").exists());
        remove(&path);

        let path = temp_state_path("hints_writable");
        let state = crate::state::AppState::load(path.clone());
        assert_eq!(super::store_hints_first(&state, false), Ok(()));
        assert!(!state.launcher.lock().settings.hints_first);
        let file = std::fs::read_to_string(&path).unwrap();
        let stored = file.contains("\"hints_first\": false");
        println!("writable: the file holds \"hints_first\": false: {stored}");
        assert!(stored, "{file}");
        assert_eq!(
            super::store_hints_first(&state, false),
            Ok(()),
            "a repeated write is refused"
        );
        remove(&path);
    }

    /// Save takes the modal's fields from what it sends and keeps every other
    /// field as stored, and its destructure names every field, so a new one
    /// does not compile until its writer is chosen.
    #[test]
    fn merge_settings_owners() {
        let stored = LauncherSettings {
            scan_on_startup: true,
            minimize_to_tray: true,
            launch_on_startup: false,
            active_provider: "claude".to_owned(),
            gemini_model: "gemini-y".to_owned(),
            hints_first: false,
        };
        let incoming = LauncherSettings {
            scan_on_startup: false,
            minimize_to_tray: false,
            launch_on_startup: true,
            active_provider: "gemini".to_owned(),
            gemini_model: " gemini-x ".to_owned(),
            hints_first: true,
        };
        // (field, whether the modal writes it); every other writer is a setter.
        let owners = [
            ("scan_on_startup", true),
            ("minimize_to_tray", true),
            ("launch_on_startup", true),
            ("gemini_model", true),
            ("active_provider", false),
            ("hints_first", false),
        ];
        let stored_value = serde_json::to_value(&stored).unwrap();
        let mut incoming_value = serde_json::to_value(&incoming).unwrap();
        let merged = serde_json::to_value(merge_settings(&stored, incoming)).unwrap();
        incoming_value["gemini_model"] = "gemini-x".into();
        let mut wrong = Vec::new();
        for (field, modal) in owners {
            let expected = if modal {
                &incoming_value[field]
            } else {
                &stored_value[field]
            };
            println!("{field}: merged {}, expected {expected}", merged[field]);
            if stored_value[field] == incoming_value[field] {
                wrong.push(format!("{field}: the fixture does not tell the two apart"));
            } else if merged[field] != *expected {
                wrong.push(format!(
                    "{field}: merged {}, expected {expected}",
                    merged[field]
                ));
            }
        }
        let defaults = serde_json::to_value(LauncherSettings::default()).unwrap();
        let mut keys: Vec<&str> = defaults
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let mut table: Vec<&str> = owners.iter().map(|(field, _)| *field).collect();
        keys.sort_unstable();
        table.sort_unstable();
        println!("checked {} of {} fields", table.len(), keys.len());
        assert_eq!(table, keys, "the owner table must list every setting");
        assert!(wrong.is_empty(), "{wrong:#?}");

        let source = include_str!("settings.rs");
        let destructure = source
            .split_once(concat!("let Launcher", "Settings {"))
            .and_then(|(_, rest)| rest.split_once("} = incoming;"))
            .map(|(fields, _)| fields);
        println!("merge_settings destructures: {destructure:?}");
        let destructure = destructure.expect("merge_settings destructures incoming by name");
        assert!(
            !destructure.contains(".."),
            "the destructure must name every field, without `..`"
        );
    }
}
