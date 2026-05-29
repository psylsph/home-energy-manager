pub mod inverter;
pub mod modbus;
pub mod server;
pub mod settings;

use inverter::poll::{run_poll_loop, AppState};
use server::start_server;
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

            // Determine frontend serving strategy:
            // - Dev mode: Vite serves frontend at localhost:5173
            // - Production: Axum serves embedded frontend at 127.0.0.1:7337
            //   (avoids mixed-content blocking on Windows where WebView2
            //   uses https://tauri.localhost and blocks http:// fetches)
            let frontend_dir = if cfg!(debug_assertions) {
                None
            } else {
                // In a Tauri bundle, the dist/ dir is alongside the executable
                // because of bundle > resources config
                let exe = std::env::current_exe().unwrap_or_default();
                let exe_dir = exe.parent().unwrap_or(std::path::Path::new("."));
                let dist = exe_dir.join("dist");
                if dist.exists() {
                    tracing::info!("Serving frontend from: {}", dist.display());
                    Some(dist.to_string_lossy().to_string())
                } else {
                    tracing::warn!("Frontend dist/ not found at {} — API-only mode", dist.display());
                    None
                }
            };

            // Spawn the HTTP server
            let server_state = state.clone();
            let server_fe_dir = frontend_dir.clone();
            tauri::async_runtime::spawn(async move {
                start_server(
                    server_state,
                    "0.0.0.0",
                    7337,
                    server_fe_dir.as_deref(),
                )
                .await;
            });

            // In production, navigate the window to the Axum server
            // (same-origin — no mixed-content issues)
            if !cfg!(debug_assertions) {
                let window = app.get_webview_window("main").expect("main window not found");
                // Small delay to let the server bind
                std::thread::sleep(std::time::Duration::from_millis(200));
                let _ = window.eval("window.location.href='http://127.0.0.1:7337'");
            }

            // Spawn the Modbus polling loop
            let poll_state = state.clone();
            tauri::async_runtime::spawn(async move {
                run_poll_loop(poll_state).await;
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
