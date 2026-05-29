pub mod inverter;
pub mod modbus;
pub mod server;
pub mod settings;

use inverter::poll::{run_poll_loop, AppState};
use server::{start_server, start_server_with_frontend};
use settings::Settings;
use std::sync::Arc;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            // Load persisted settings (or use defaults)
            let app_settings = Settings::load();
            tracing::info!(
                "Loaded settings: host={}, serial={}",
                app_settings.host,
                app_settings.serial
            );

            // Create shared app state
            let state = Arc::new(AppState::new());
            {
                // Apply saved settings to poll settings
                let mut ps = state.settings.blocking_lock();
                ps.host = app_settings.host.clone();
                ps.port = app_settings.port;
                ps.serial = app_settings.serial.clone();
                ps.interval_secs = app_settings.poll_interval;
            }

            // Spawn the HTTP server on LAN interface, port 7337.
            // In production, also serve frontend files so the Tauri window
            // can load from http://127.0.0.1:7337 (same-origin, avoids
            // Windows WebView2 mixed-content blocking).
            let server_state = state.clone();
            if cfg!(debug_assertions) {
                tauri::async_runtime::spawn(async move {
                    start_server(server_state, "0.0.0.0", 7337).await;
                });
            } else {
                // dist/ is relative to src-tauri/ — goes up one level
                let dist_dir = "../dist".to_string();
                tauri::async_runtime::spawn(async move {
                    start_server_with_frontend(server_state, "0.0.0.0", 7337, &dist_dir).await;
                });
            }

            // Spawn the Modbus polling loop
            let poll_state = state.clone();
            tauri::async_runtime::spawn(async move {
                run_poll_loop(poll_state).await;
            });

            // In production, navigate the window to the Axum server
            // (same-origin — avoids Windows WebView2 mixed-content blocking)
            if !cfg!(debug_assertions) {
                std::thread::sleep(std::time::Duration::from_millis(300));
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.eval(
                        "window.location.replace('http://127.0.0.1:7337')",
                    );
                }
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
