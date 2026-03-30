#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![deny(clippy::all, clippy::pedantic)]
#![allow(clippy::module_name_repetitions, clippy::missing_errors_doc, clippy::missing_panics_doc)]

mod commands;
mod discovery;
mod models;
mod proxy;
mod state;

use state::AppState;
use tauri::{
    Manager,
    menu::{MenuBuilder, MenuItemBuilder},
    tray::TrayIconBuilder,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            let app_dir = app.path().app_data_dir().expect("Failed to get app data dir");
            std::fs::create_dir_all(&app_dir).expect("Failed to create app data directory");

            let log_file =
                std::fs::File::create(app_dir.join("launcher.log")).expect("Failed to create log file");
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
                            app.exit(0);
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

            // Start the proxy server on a separate tokio runtime.
            // The server runs forever, so the thread blocks on a pending future
            // after the server spawns its listener task.
            std::thread::spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("Failed to create proxy runtime");
                rt.block_on(async {
                    match proxy::start_proxy().await {
                        Ok(addr) => {
                            tracing::info!("Proxy server started on {addr}");
                            // Keep the runtime alive so the spawned server task continues
                            std::future::pending::<()>().await;
                        }
                        Err(e) => tracing::error!("Failed to start proxy: {e}"),
                    }
                });
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            // Minimize to tray: intercept close and hide instead of quitting
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<AppState>();
                let minimize_to_tray = state.launcher.lock().settings.minimize_to_tray;
                if minimize_to_tray {
                    api.prevent_close();
                    let _ = window.hide();
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
