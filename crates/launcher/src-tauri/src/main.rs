#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![deny(clippy::all, clippy::pedantic)]
#![allow(
    clippy::module_name_repetitions,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc
)]

mod ai;
mod commands;
mod discovery;
mod models;
mod overlay;
mod overlay_capture;
mod process_watch;
mod state;

use ai::AiState;
use overlay::OverlayState;
use state::AppState;
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::TrayIconBuilder,
    Manager,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

#[allow(clippy::too_many_lines)] // Tauri builder + setup is one long, linear wiring.
fn main() {
    // Overlay toggle hotkey (Ctrl+Alt+G): a modifier chord, not a bare F-key, so
    // it does not collide with common in-game bindings.
    let toggle = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyG);
    let translate = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyT);
    let quick_ask = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyA);

    tauri::Builder::default()
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
        .setup(move |app| {
            let app_dir = app
                .path()
                .app_data_dir()
                .expect("Failed to get app data dir");
            std::fs::create_dir_all(&app_dir).expect("Failed to create app data directory");

            let log_file = std::fs::File::create(app_dir.join("launcher.log"))
                .expect("Failed to create log file");
            tracing_subscriber::fmt()
                .with_writer(std::sync::Mutex::new(log_file))
                .with_ansi(false)
                .init();

            let state_path = app_dir.join("launcher-state.json");
            let app_state = AppState::load(state_path);

            // Apply launch_on_startup from saved settings
            let autostart = app.autolaunch();
            let should_autostart = app_state.launcher.lock().settings.launch_on_startup;
            if should_autostart {
                let _ = autostart.enable();
            } else {
                let _ = autostart.disable();
            }

            // Register the overlay hotkeys (log + continue on conflict).
            for shortcut in [toggle, translate, quick_ask] {
                if let Err(e) = app.global_shortcut().register(shortcut) {
                    tracing::warn!("hotkey registration failed: {e}");
                }
            }

            // Detect CLI provider availability off the main thread (probing the
            // claude/codex binaries can take a moment, especially via WSL).
            let detect_handle = app.handle().clone();
            std::thread::spawn(move || {
                let claude = ai::detect_cli("claude");
                let codex = ai::detect_cli("codex");
                let codex_workdir = ai::ensure_codex_workdir(codex);
                tracing::info!("CLI availability -- claude: {claude:?}, codex: {codex:?}");
                detect_handle.state::<AiState>().set_cli(ai::CliConfig {
                    claude,
                    codex,
                    codex_workdir,
                });
            });

            // Build system tray (always present, shown/hidden based on setting)
            let show = MenuItemBuilder::with_id("show", "Show").build(app)?;
            let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
            let menu = MenuBuilder::new(app).items(&[&show, &quit]).build()?;

            TrayIconBuilder::new()
                .icon(app.default_window_icon().cloned().expect("No app icon"))
                .tooltip("AI Game Companion")
                .menu(&menu)
                .on_menu_event(|app, event| {
                    match event.id().as_ref() {
                        "show" => {
                            if let Some(window) = app.get_webview_window("main") {
                                let _ = window.show();
                                let _ = window.unminimize();
                                let _ = window.set_focus();
                            }
                        }
                        "quit" => {
                            std::process::exit(0);
                        }
                        _ => {}
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::DoubleClick { .. } = event {
                        if let Some(window) = tray.app_handle().get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.unminimize();
                            let _ = window.set_focus();
                        }
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
                    let _ = window.hide();
                    return;
                }
                let state = window.state::<AppState>();
                let minimize_to_tray = state.launcher.lock().settings.minimize_to_tray;
                if minimize_to_tray {
                    // Hide to tray instead of closing.
                    api.prevent_close();
                    let _ = window.hide();
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
            commands::games::open_game_config,
            commands::games::open_game_logs,
            commands::settings::get_settings,
            commands::settings::update_settings,
            commands::ai::ask_sage,
            commands::ai::cancel_sage,
            commands::ai::available_providers,
            commands::ai::set_active_provider,
            commands::ai::translate_screen,
            overlay::capture_game,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");

    // Tauri's event loop has exited (all windows closed). Force-terminate so no
    // background thread keeps the process alive.
    std::process::exit(0);
}
