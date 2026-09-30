#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai;
mod commands;
mod discovery;
mod models;
#[cfg(test)]
mod npm_lock_policy;
mod overlay;
mod overlay_capture;
mod process_watch;
mod secrets;
mod state;
mod trust;
mod util;

use ai::AiState;
use commands::settings::HotkeyStatus;
use overlay::OverlayState;
use state::AppState;
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::TrayIconBuilder,
    Manager,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// The overlay hotkeys in registration order -- Ctrl+Shift plus the key: toggle,
/// translate, quick ask -- with the label shown when one cannot be registered.
/// Modifier chords, not bare F-keys, and not Ctrl+Alt (which equals `AltGr` on
/// international keyboards).
const HOTKEYS: [(Code, &str); 3] = [
    (Code::KeyG, "Ctrl+Shift+G"),
    (Code::KeyT, "Ctrl+Shift+T"),
    (Code::KeyA, "Ctrl+Shift+A"),
];

/// Bring the main launcher window to the foreground (restore + focus).
fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        util::log_if_err("show main window", window.show());
        util::log_if_err("unminimize main window", window.unminimize());
        util::log_if_err("focus main window", window.set_focus());
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Tauri builder + setup is one long, linear wiring"
)]
#[expect(
    clippy::allow_attributes,
    reason = "version-dependent: clippy::exit fires here on the pinned toolchain but not on newer ones; \
              the toolchain bump that makes it uniform deletes this and the allow below"
)]
#[allow(
    clippy::exit,
    reason = "background threads (CLI detection, global-shortcut) keep the process alive unless it force-exits; \
              allow, not expect: newer clippy no longer lints exit inside main"
)]
#[expect(
    clippy::print_stderr,
    reason = "these two sites run before the tracing logger is initialised"
)]
fn main() {
    let [toggle, translate, quick_ask] =
        HOTKEYS.map(|(code, _)| Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), code));

    let context = tauri::generate_context!();
    // Tauri loads `devUrl` exactly when it is built without `custom-protocol`,
    // whatever the profile, so that is when the dev server's pages are the app's.
    let dev_url = if tauri::is_dev() {
        context.config().build.dev_url.clone()
    } else {
        None
    };

    let run_result = tauri::Builder::default()
        // Must be registered first. Two instances would otherwise share one
        // state file with only a per-process lock: their temp-file writes and
        // renames interleave, one publishes the other's snapshot or a truncated
        // file, and the next start treats it as corrupt and resets to defaults
        // -- every game, playtime total and setting gone. `minimize_to_tray`
        // defaults to true, so "closed the window, relaunched from the
        // shortcut" is the normal way to end up here.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main_window(app);
        }))
        .plugin(trust::navigation_guard(dev_url))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, shortcut, event| {
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    if shortcut == &toggle {
                        overlay::toggle(app);
                    } else if shortcut == &translate {
                        overlay::trigger(app, "translate-request");
                    } else if shortcut == &quick_ask {
                        overlay::trigger(app, "quick-ask");
                    }
                })
                .build(),
        )
        .manage(OverlayState::default())
        .manage(AiState::default())
        .manage(HotkeyStatus::default())
        .setup(move |app| {
            // Under `windows_subsystem = "windows"` there is no console and no
            // dialog, so panicking here kills the launcher with no visible
            // output at all. An unwritable %APPDATA% (roaming profile issues,
            // disk full, AV lock) costs logging, not the whole app.
            let app_dir = app.path().app_data_dir().map_err(|e| {
                format!("Failed to resolve the app data directory: {e}. Cannot continue.")
            })?;
            if let Err(e) = std::fs::create_dir_all(&app_dir) {
                eprintln!("Failed to create {}: {e}", app_dir.display());
            }

            match std::fs::File::create(app_dir.join("launcher.log")) {
                Ok(log_file) => tracing_subscriber::fmt()
                    .with_writer(std::sync::Mutex::new(log_file))
                    .with_ansi(false)
                    .init(),
                Err(e) => {
                    eprintln!("Failed to open launcher.log: {e}; logging to stderr");
                    tracing_subscriber::fmt().with_ansi(false).init();
                }
            }

            // A Codex screenshot left behind by a crash, a killed process or a
            // failed delete is removed at the next start.
            if let Some(dir) = ai::shots_dir(app.handle()) {
                let removed = ai::sweep_shots(&dir);
                if removed > 0 {
                    tracing::info!("Removed {removed} leftover screenshot file(s)");
                }
            }

            let state_path = app_dir.join("launcher-state.json");
            let app_state = AppState::load(state_path);

            // Apply launch_on_startup from saved settings. Not in read-only
            // mode: the settings are then defaults, not the user's, and applying
            // them would delete an autostart entry the user asked for.
            if app_state.load_error().is_some() {
                tracing::warn!("Launcher state is read-only; leaving autostart unchanged");
            } else {
                let autostart = app.autolaunch();
                let should_autostart = app_state.launcher.lock().settings.launch_on_startup;
                if should_autostart {
                    util::log_if_err("enable autostart", autostart.enable());
                } else if let Err(err) = autostart.disable() {
                    // Disabling when no registry entry exists is the normal case
                    // on a fresh install, so this is not worth a warning on every
                    // start.
                    tracing::debug!("disable autostart: {err}");
                }
            }

            // Register the overlay hotkeys. A failure is logged and recorded, so
            // the main window can say which chord is unavailable.
            let hotkey_status = app.state::<HotkeyStatus>();
            for (shortcut, (_, label)) in [toggle, translate, quick_ask].into_iter().zip(HOTKEYS) {
                if let Err(e) = app.global_shortcut().register(shortcut) {
                    tracing::warn!("hotkey registration failed for {label}: {e}");
                    hotkey_status.0.lock().push(label.to_owned());
                }
            }

            // Detect CLI provider availability off the main thread (probing the
            // claude/codex binaries can take a moment, especially via WSL).
            let detect_handle = app.handle().clone();
            std::thread::spawn(move || {
                let cfg = ai::detect_all();
                tracing::info!(
                    "CLI availability -- claude: {:?}, codex: {:?}",
                    cfg.claude,
                    cfg.codex
                );
                detect_handle.state::<AiState>().set_cli(cfg);
                ai::notify_providers_changed(&detect_handle);
            });

            // Build system tray (always present, shown/hidden based on setting)
            let show = MenuItemBuilder::with_id("show", "Show").build(app)?;
            let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
            let menu = MenuBuilder::new(app).items(&[&show, &quit]).build()?;

            TrayIconBuilder::new()
                .icon(
                    app.default_window_icon()
                        .cloned()
                        .ok_or("the bundled window icon is missing")?,
                )
                .tooltip("AI Game Companion")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => show_main_window(app),
                    "quit" => std::process::exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::DoubleClick { .. } = event {
                        show_main_window(tray.app_handle());
                    }
                })
                .build(app)?;

            app.manage(app_state);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // The overlay window only hides; the main window drives the
                // launcher's tray / exit behaviour.
                if window.label() == "overlay" {
                    api.prevent_close();
                    util::log_if_err("hide overlay window", window.hide());
                    return;
                }
                let state = window.state::<AppState>();
                let minimize_to_tray = state.launcher.lock().settings.minimize_to_tray;
                if minimize_to_tray {
                    // Hide to tray instead of closing.
                    api.prevent_close();
                    util::log_if_err("hide main window", window.hide());
                } else {
                    // Real close: force exit so any background threads (CLI
                    // detection, global-shortcut) do not keep the process alive.
                    std::process::exit(0);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::games::get_games,
            commands::games::scan_games,
            commands::games::launch_game,
            commands::games::open_game_logs,
            commands::settings::get_settings,
            commands::settings::update_settings,
            commands::settings::state_health,
            commands::settings::hotkey_status,
            commands::settings::open_url,
            commands::settings::open_config_folder,
            commands::ai::ask_sage,
            commands::ai::cancel_sage,
            commands::ai::available_providers,
            commands::ai::set_active_provider,
            commands::ai::translate_screen,
            commands::ai::set_gemini_key,
            commands::ai::recheck_clis,
            overlay::hide_overlay,
            overlay::link_game,
        ])
        .run(context);

    let code = match run_result {
        Ok(()) => 0,
        Err(err) => {
            tracing::error!("the Tauri runtime exited with an error: {err}");
            1
        }
    };

    // Tauri's event loop has exited (all windows closed). Force-terminate so no
    // background thread keeps the process alive.
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::HOTKEYS;
    use tauri_plugin_global_shortcut::Code;

    #[test]
    fn hotkey_labels_are_the_documented_chords() {
        let labels: Vec<&str> = HOTKEYS.iter().map(|(_, label)| *label).collect();
        assert_eq!(labels, ["Ctrl+Shift+G", "Ctrl+Shift+T", "Ctrl+Shift+A"]);
        let codes: Vec<Code> = HOTKEYS.iter().map(|(code, _)| *code).collect();
        assert_eq!(codes, [Code::KeyG, Code::KeyT, Code::KeyA]);
    }
}
