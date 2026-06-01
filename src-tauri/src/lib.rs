pub mod history;
pub mod inverter;
pub mod modbus;
pub mod server;
pub mod settings;

use history::HistoryDb;
use inverter::poll::{run_poll_loop, AppState};
use server::logs::{LogCaptureLayer, LogRing};
use server::{start_server, start_server_with_frontend};
use settings::Settings;
use std::sync::Arc;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    use tauri::Manager;

    tauri::Builder::default()
        .setup(|app| {
            // Set up tracing with log capture layer for developer console
            let log_ring = Arc::new(LogRing::new(2000));
            {
                use tracing_subscriber::prelude::*;
                let capture_layer = LogCaptureLayer::new(log_ring.clone());
                let fmt_layer = tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .with_filter(
                        tracing_subscriber::EnvFilter::try_from_default_env()
                            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
                    );
                tracing_subscriber::registry()
                    .with(fmt_layer)
                    .with(capture_layer)
                    .init();
            }

            if cfg!(debug_assertions) {
                let _ = app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                );
            }

            // Load persisted settings (or use defaults)
            let app_settings = Settings::load();
            tracing::info!(
                "Loaded settings: host={}, serial={}",
                app_settings.host,
                app_settings.serial
            );

            // Create shared app state with log ring
            let state = Arc::new(AppState::with_log_ring(log_ring));
            {
                // Apply saved settings to poll settings
                let mut ps = state.settings.blocking_lock();
                ps.host = app_settings.host.clone();
                ps.port = app_settings.port;
                ps.serial = app_settings.serial.clone();
                ps.interval_secs = app_settings.poll_interval;
            }

            // Apply saved auto-winter config
            {
                let mut aw = state.auto_winter_config.blocking_lock();
                aw.enabled = app_settings.auto_winter_enabled;
                aw.cold_threshold = app_settings.auto_winter_cold_threshold;
                aw.recovery_threshold = app_settings.auto_winter_recovery_threshold;
                aw.target_soc = app_settings.auto_winter_target_soc;
                aw.debounce_readings = app_settings.auto_winter_debounce_readings;
            }

            // Load persisted auto-winter saved values (original register
            // values captured before winter mode activated).
            {
                let mut saved = state.auto_winter_saved.blocking_lock();
                if let (Some(enable_target), Some(target_soc)) = (
                    app_settings.auto_winter_saved_enable_target,
                    app_settings.auto_winter_saved_target_soc,
                ) {
                    *saved = Some(crate::inverter::poll::AutoWinterSaved {
                        enable_charge_target: enable_target,
                        target_soc: target_soc as u8,
                    });
                    tracing::info!(
                        "Restored auto-winter saved state: enable={}, target_soc={}",
                        enable_target, target_soc,
                    );
                }
            }

            // Open history database
            let config_dir = crate::settings::Settings::settings_dir();
            let db_path = config_dir.join("history.db");
            let history_db = match HistoryDb::open(&db_path) {
                Ok(db) => Arc::new(db),
                Err(e) => {
                    tracing::error!("Failed to open history database: {e}");
                    return Ok(());
                }
            };
            {
                let mut h = state.history.blocking_lock();
                *h = Some(history_db.clone());
            }

            // Spawn the HTTP server on LAN interface.
            let http_port = app_settings.http_port;
            let server_state = state.clone();
            if cfg!(debug_assertions) {
                // Dev mode: Vite serves the frontend on :5173 for the Tauri
                // window (hot-reload). Axum also serves the built frontend
                // from dist/ so LAN devices can access the dashboard.
                let dist_dir = std::path::PathBuf::from("../dist")
                    .canonicalize()
                    .unwrap_or_else(|_| std::path::PathBuf::from("dist"));
                tracing::info!("Dev frontend path: {}", dist_dir.display());
                tauri::async_runtime::spawn(async move {
                    start_server_with_frontend(server_state, "0.0.0.0", http_port, dist_dir.to_string_lossy().to_string()).await;
                });
            } else {
                // Production: serve the frontend from Axum too so that
                // the Tauri window is same-origin with the API/WebSocket.
                // The dist files are bundled as Tauri resources and land at
                // {resource_dir}/dist/. Fall back gracefully if the bundle
                // path can't be resolved (e.g. running outside LaunchServices).
                let dist_dir = app
                    .path()
                    .resource_dir()
                    .map(|d| d.join("dist").to_string_lossy().to_string())
                    .unwrap_or_else(|e| {
                        tracing::warn!("Could not resolve resource dir ({e}); trying relative to executable fallback");
                        std::env::current_exe()
                            .ok()
                            .and_then(|exe| {
                                let d = exe.parent()?.join("..").join("Resources").join("dist");
                                if d.join("index.html").exists() { Some(d) } else { None }
                            })
                            .or_else(|| {
                                let d = std::path::PathBuf::from("dist");
                                if d.join("index.html").exists() { Some(d) } else { None }
                            })
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|| {
                                // Last resort: try current_exe's Resources/dist
                                let exe = std::env::current_exe().ok().unwrap_or_default();
                                exe.parent()
                                    .map(|p| p.join("..").join("Resources").join("dist"))
                                    .unwrap_or_else(|| std::path::PathBuf::from("dist"))
                                    .to_string_lossy()
                                    .to_string()
                            })
                    });
                tracing::info!("Production frontend path: {}", dist_dir);

                tauri::async_runtime::spawn(async move {
                    start_server_with_frontend(server_state, "0.0.0.0", http_port, dist_dir).await;
                });

                // Give the server a moment to bind, then navigate the
                // Tauri window away from the asset protocol to the Axum
                // origin (same-origin for fetch + WebSocket).
                std::thread::sleep(std::time::Duration::from_millis(300));
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.eval(
                        format!("window.location.replace('http://127.0.0.1:{}')", http_port).as_str(),
                    );
                }
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

// ---------------------------------------------------------------------------
// Headless server mode (no Tauri window)
// ---------------------------------------------------------------------------

/// Parse a `--port <N>` argument from the CLI args.
fn parse_port(args: &[String]) -> u16 {
    for i in 0..args.len() {
        if args[i] == "--port" && i + 1 < args.len() {
            if let Ok(p) = args[i + 1].parse::<u16>() {
                return p;
            }
        }
    }
    7337
}

/// Parse a `--dist <path>` argument from the CLI args.
fn parse_dist(args: &[String]) -> Option<String> {
    for i in 0..args.len() {
        if args[i] == "--dist" && i + 1 < args.len() {
            return Some(args[i + 1].clone());
        }
    }
    None
}

/// Resolve the frontend dist directory for headless mode.
///
/// Search order:
/// 1. `--dist <path>` CLI argument
/// 2. `./dist/` relative to the current working directory
/// 3. `<exe_dir>/dist/` relative to the binary location
/// 4. `/usr/share/givenergy-local/dist/` system path
fn resolve_dist_dir(args: &[String]) -> Option<String> {
    if let Some(path) = parse_dist(args) {
        if std::path::Path::new(&path).exists() {
            return Some(path);
        }
        tracing::warn!("--dist path does not exist: {path}");
    }

    let candidates: Vec<std::path::PathBuf> = vec![
        std::path::PathBuf::from("dist"),
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|p| p.join("dist")))
            .unwrap_or_default(),
        std::path::PathBuf::from("/usr/share/givenergy-local/dist"),
    ];

    for candidate in candidates {
        if candidate.join("index.html").exists() {
            let path = candidate.to_string_lossy().to_string();
            tracing::info!("Found frontend dist at: {path}");
            return Some(path);
        }
    }

    None
}

/// Run the server in headless mode — no Tauri window, just the
/// Axum HTTP/WS server and the Modbus polling loop.
///
/// Usage: `givenergy-local --headless [--port 7337] [--dist /path/to/dist]`
pub fn run_headless(args: &[String]) {
    // Set up tracing with log capture
    let log_ring = Arc::new(LogRing::new(2000));
    {
        use tracing_subscriber::prelude::*;
        let capture_layer = LogCaptureLayer::new(log_ring.clone());
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_target(false)
            .with_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
            );
        tracing_subscriber::registry()
            .with(fmt_layer)
            .with(capture_layer)
            .init();
    }

    let cli_port = parse_port(args);
    // Load settings
    let app_settings = Settings::load();
    // CLI --port overrides settings; settings overrides default 7337
    let port = if cli_port != 7337 || args.iter().any(|a| a == "--port") {
        cli_port // explicit CLI override
    } else {
        app_settings.http_port
    };
    tracing::info!("GivEnergy Local starting in headless mode on port {port}");
    tracing::info!(
        "Loaded settings: host={}, serial={}",
        app_settings.host,
        app_settings.serial
    );

    // Create tokio runtime
    let rt = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");

    rt.block_on(async {
        // Create shared app state with log ring
        let state = Arc::new(AppState::with_log_ring(log_ring));
        {
            let mut ps = state.settings.lock().await;
            ps.host = app_settings.host.clone();
            ps.port = app_settings.port;
            ps.serial = app_settings.serial.clone();
            ps.interval_secs = app_settings.poll_interval;
        }

        // Apply saved auto-winter config
        {
            let mut aw = state.auto_winter_config.lock().await;
            aw.enabled = app_settings.auto_winter_enabled;
            aw.cold_threshold = app_settings.auto_winter_cold_threshold;
            aw.recovery_threshold = app_settings.auto_winter_recovery_threshold;
            aw.target_soc = app_settings.auto_winter_target_soc;
            aw.debounce_readings = app_settings.auto_winter_debounce_readings;
        }

        // Load persisted auto-winter saved values
        {
            let mut saved = state.auto_winter_saved.lock().await;
            if let (Some(enable_target), Some(target_soc)) = (
                app_settings.auto_winter_saved_enable_target,
                app_settings.auto_winter_saved_target_soc,
            ) {
                *saved = Some(crate::inverter::poll::AutoWinterSaved {
                    enable_charge_target: enable_target,
                    target_soc: target_soc as u8,
                });
                tracing::info!(
                    "Restored auto-winter saved state: enable={}, target_soc={}",
                    enable_target, target_soc,
                );
            }
        }

        // Open history database
        let config_dir = crate::settings::Settings::settings_dir();
        let db_path = config_dir.join("history.db");
        let history_db = match HistoryDb::open(&db_path) {
            Ok(db) => Arc::new(db),
            Err(e) => {
                tracing::error!("Failed to open history database: {e}");
                return;
            }
        };
        {
            let mut h = state.history.lock().await;
            *h = Some(history_db);
        }

        // Spawn the poll loop
        let poll_state = state.clone();
        tokio::spawn(async move {
            run_poll_loop(poll_state).await;
        });

        // Start the HTTP server
        let server_state = state.clone();
        match resolve_dist_dir(args) {
            Some(dist_dir) => {
                tracing::info!("Serving frontend from: {dist_dir}");
                start_server_with_frontend(server_state, "0.0.0.0", port, dist_dir).await;
            }
            None => {
                tracing::warn!(
                    "No frontend dist directory found. Running API-only mode. \
                     Specify --dist <path> or place dist/ next to the binary."
                );
                start_server(server_state, "0.0.0.0", port).await;
            }
        }
    });
}
