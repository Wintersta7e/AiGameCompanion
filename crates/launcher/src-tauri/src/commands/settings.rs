use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_opener::OpenerExt;

use crate::models::LauncherSettings;
use crate::state::AppState;

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
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
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn hotkey_status(status: State<'_, HotkeyStatus>) -> Vec<String> {
    status.0.lock().clone()
}

/// Why the library file is read-only this run, or `None` when it is writable.
/// The main window asks before deciding whether to scan.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn state_health(state: State<'_, AppState>) -> Option<String> {
    state.load_error().map(str::to_owned)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
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

/// What Save stores: the modal's values with the model trimmed, except the
/// provider. Only `set_active_provider` writes that, so a provider picked in the
/// overlay while Settings is open is not reverted by Save.
fn merge_settings(stored: &LauncherSettings, incoming: LauncherSettings) -> LauncherSettings {
    let mut merged = incoming;
    merged.active_provider.clone_from(&stored.active_provider);
    merged.gemini_model = merged.gemini_model.trim().to_owned();
    merged
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
#[allow(clippy::needless_pass_by_value)]
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
#[allow(clippy::needless_pass_by_value)]
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
        };
        let merged = merge_settings(&stored, incoming);
        assert_eq!(merged.active_provider, "claude");
        assert_eq!(merged.gemini_model, "gemini-3.8-flash");
        assert!(!merged.scan_on_startup);
        assert!(!merged.minimize_to_tray);
        assert!(merged.launch_on_startup);
    }
}
