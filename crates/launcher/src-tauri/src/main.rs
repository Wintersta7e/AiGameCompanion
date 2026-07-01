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
mod proxy;
mod state;

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
    let handler_toggle = toggle;

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, shortcut, event| {
                    if shortcut == &handler_toggle && event.state() == ShortcutState::Pressed {
                        overlay::toggle(app);
                    }
                })
                .build(),
        )
        .manage(OverlayState::default())
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

            // Register the overlay toggle hotkey (log + continue on conflict).
            if let Err(e) = app.global_shortcut().register(toggle) {
                tracing::warn!("overlay toggle hotkey registration failed: {e}");
            }

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
                            crate::proxy::cleanup_port_file();
                            // app.exit() may not kill the proxy thread, so force it.
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
            proxy::spawn_proxy_thread();
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
                    // Real close: clean up and force exit so the proxy thread
                    // doesn't keep the process alive.
                    crate::proxy::cleanup_port_file();
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
            overlay::ask_sage,
            overlay::capture_game,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");

    // Tauri's event loop has exited (all windows closed). The proxy thread
    // blocks on `pending().await` and will never return on its own, so clean
    // up and force-terminate all threads.
    proxy::cleanup_port_file();
    std::process::exit(0);
}
