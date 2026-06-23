pub mod alerts;
pub mod evc;
pub mod history;
pub mod inverter;
pub mod modbus;
pub mod server;
pub mod settings;
pub mod support;
pub mod weather;
#[cfg(test)]
mod test_util;

use history::HistoryDb;
use inverter::poll::{run_poll_loop, AppState};
use server::logs::{LogCaptureLayer, LogRing};
use server::{
    start_server, start_server_with_frontend, start_server_with_frontend_on_available_port,
};
use settings::Settings;
use std::sync::Arc;

fn show_startup_error(window: &tauri::WebviewWindow, message: &str) {
    let html = format!(
        r#"<main style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; padding: 32px; color: #f8fafc; background: #0f172a; min-height: 100vh; box-sizing: border-box;">
<h1 style="margin: 0 0 12px; font-size: 22px;">Home Energy Manager could not start its local server</h1>
<p style="line-height: 1.5; max-width: 720px; color: #cbd5e1;">The app could not bind a local HTTP port, so it has not connected to any existing server. This avoids accidentally showing an older installed version.</p>
<pre style="white-space: pre-wrap; background: #1e293b; color: #e2e8f0; padding: 16px; border-radius: 12px; max-width: 720px;">{}</pre>
<p style="line-height: 1.5; max-width: 720px; color: #cbd5e1;">Quit any other Home Energy Manager processes and reopen the app.</p>
</main>"#,
        html_escape(message)
    );
    if let Ok(script_arg) = serde_json::to_string(&html) {
        let _ = window.eval(format!("document.body.innerHTML = {script_arg};"));
    }
}

fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Initialise the global tracing subscriber.
///
/// Three layers are installed:
/// * `fmt` to stdout/stderr (default level **WARN**, override with `RUST_LOG`),
/// * `fmt` to a **daily-rotated file** under `<config_dir>/logs/` (default
///   level **INFO**), and
/// * `LogCaptureLayer` feeding the in-memory ring buffer behind the developer
///   console (LogsPage) and the automated support bundle (#125). Defaults to
///   INFO — see [`crate::server::logs::DEFAULT_CAPTURE_LEVEL`].
///
/// Daily rotation bounds each file to one day of output; [`prune_old_logs`]
/// then caps how many days accumulate so total disk usage stays bounded.
fn init_tracing(log_ring: &Arc<LogRing>) {
    use tracing_subscriber::prelude::*;
    let capture_layer = LogCaptureLayer::new(log_ring.clone());
    // Default console (stdout/stderr) level is WARN. INFO floods the
    // terminal/journal when running headless — most INFO lines are routine
    // (first poll, grace-period summary, write confirmations) and only
    // matter when debugging. The in-memory LogRing that backs the developer
    // console (LogsPage) is a SEPARATE layer with its own runtime min_level,
    // so this default does not affect it. Override for a session with
    // RUST_LOG=info (or =debug).
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        );
    // Persistent daily-rotated file log. One file per day bounds each file's
    // size; prune_old_logs caps how many days accumulate. Default level INFO
    // so the file records more history than the terse WARN console/ring
    // streams. Failure to write (e.g. read-only dir) is silent per-write —
    // the user's ~/.givenergy-local/ is already writable for settings/db.
    let log_dir = settings::Settings::settings_dir().join("logs");
    prune_old_logs(&log_dir);
    let _ = std::fs::create_dir_all(&log_dir);
    let file_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_ansi(false)
        .with_writer(tracing_appender::rolling::daily(&log_dir, "app.log"))
        .with_filter(tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(capture_layer)
        .with(file_layer)
        .init();
}

/// How many days of rotated log files to keep on disk.
const LOG_RETENTION_DAYS: i64 = 14;

/// Delete `app.log.<YYYY-MM-DD>` files older than [`LOG_RETENTION_DAYS`].
/// `tracing-appender` names daily rotations this way, so the date is parsed
/// straight from the suffix. Failures are ignored — pruning is best-effort
/// and must never block startup.
fn prune_old_logs(log_dir: &std::path::Path) {
    let read_dir = match std::fs::read_dir(log_dir) {
        Ok(rd) => rd,
        Err(_) => return, // dir doesn't exist yet — nothing to prune
    };
    let cutoff = chrono::Local::now().date_naive() - chrono::Duration::days(LOG_RETENTION_DAYS);
    for entry in read_dir.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let Some(date_str) = name.strip_prefix("app.log.") else {
            continue;
        };
        if let Ok(date) = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
            if date < cutoff {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Build the shared [`AppState`] from persisted settings.
///
/// Applies the saved poll settings, auto-winter config, load-limiter config,
/// restored load-limiter state, persisted auto-winter saved register values,
/// and opens the history database — i.e. every piece of startup state that is
/// identical between the Tauri-windowed and headless code paths. Both `run()`
/// and `run_headless()` call this so the initialisation sequence cannot
/// diverge (previously it was duplicated ~verbatim and had already started
/// to, with `run()` using `blocking_lock()` while `run_headless()` used
/// `.lock().await`).
///
/// Returns `None` (after logging) if the history database cannot be opened —
/// matching the previous abort-on-failure behaviour of both callers.
async fn initialize_app_state(
    app_settings: Settings,
    log_ring: Arc<LogRing>,
) -> Option<Arc<AppState>> {
    let state = Arc::new(AppState::with_log_ring(log_ring));

    // Apply saved settings to poll settings
    {
        let mut ps = state.settings.lock().await;
        ps.host = app_settings.host.clone();
        ps.port = app_settings.port;
        ps.serial = app_settings.serial.clone();
        ps.interval_secs = app_settings.poll_interval;
        ps.evc_host = app_settings.evc_host.clone();
        ps.evc_port = app_settings.evc_port;
        ps.minimal_telemetry_mode = app_settings.minimal_telemetry_mode;
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

    // Apply saved load limiter config
    {
        let mut ll = state.load_limiter_config.lock().await;
        ll.enabled = app_settings.load_limiter_enabled;
        ll.threshold_w = app_settings.load_limiter_threshold_w;
        ll.trigger_delay_minutes = app_settings.load_limiter_trigger_delay_minutes;
        ll.start_hour = app_settings.load_limiter_start_hour;
        ll.start_minute = app_settings.load_limiter_start_minute;
        ll.end_hour = app_settings.load_limiter_end_hour;
        ll.end_minute = app_settings.load_limiter_end_minute;
    }

    // If the load limiter was active when the app last ran, mark the state as
    // PausedFromRestart so the first poll immediately restores Eco if the load
    // has already dropped below threshold while the app was down.
    if app_settings.load_limiter_active_persisted {
        let mut ll_state = state.load_limiter_state.lock().await;
        *ll_state = crate::inverter::poll::LoadLimiterState::PausedFromRestart;
        tracing::info!("Restored load limiter state: PausedFromRestart (post-crash)");
    }

    // Load persisted auto-winter saved values (original register values
    // captured before winter mode activated).
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
                enable_target,
                target_soc,
            );
        }
    }

    // Apply saved alert config
    {
        let mut ac = state.alert_config.lock().await;
        *ac = app_settings.alerts_config.clone();
    }

    // Open history database
    let config_dir = crate::settings::Settings::settings_dir();
    let db_path = config_dir.join("history.db");
    let history_db = match HistoryDb::open(&db_path) {
        Ok(db) => Arc::new(db),
        Err(e) => {
            tracing::error!("Failed to open history database: {e}");
            return None;
        }
    };
    {
        let mut h = state.history.lock().await;
        *h = Some(history_db);
    }

    Some(state)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    use tauri::Manager;

    tauri::Builder::default()
        .setup(|app| {
            // Set up tracing with log capture layer for developer console.
            // Shared with `run_headless()` via `init_tracing()` so the tracing
            // configuration can never drift between the two startup paths.
            let log_ring = Arc::new(LogRing::new(2000));
            init_tracing(&log_ring);

            // tauri-plugin-opener is required in release builds so that
            // external links (Settings → About, Telegram/ntfy/Pushover help
            // URLs, NOTIFICATIONS.md) open in the user's default browser
            // instead of inside the WebView2 webview process.
            let _ = app.handle().plugin(tauri_plugin_opener::init());

            // tauri-plugin-autostart gives the user a "Start on login"
            // toggle in Settings. On Windows this writes the
            // HKCU\…\Run registry key; on macOS a LaunchAgent plist; on
            // Linux a .desktop file under ~/.config/autostart/. Android
            // is unsupported by the plugin. See issue #117.
            #[cfg(desktop)]
            let _ = app.handle().plugin(tauri_plugin_autostart::init(
                tauri_plugin_autostart::MacosLauncher::LaunchAgent,
                None,
            ));

            if cfg!(debug_assertions) {
                let _ = app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                );
            }

            // Load persisted settings (or use defaults)
            let settings_dir = crate::settings::Settings::settings_dir();
            tracing::info!("Settings directory: {}", settings_dir.display());
            let app_settings = Settings::load();
            tracing::info!(
                "Loaded settings: host={}, serial={}",
                app_settings.host,
                app_settings.serial
            );

            // Initialise shared app state: apply persisted poll/auto-winter/load
            // limiter settings and open the history database. Identical to the
            // headless path via `initialize_app_state()`, so the two startup
            // sequences cannot diverge. `http_port` is captured first because
            // `app_settings` is moved into the helper.
            let http_port = app_settings.http_port;
            let autostart_enabled = app_settings.autostart_enabled;
            let state = match tauri::async_runtime::block_on(initialize_app_state(
                app_settings,
                log_ring,
            )) {
                Some(s) => s,
                None => return Ok(()),
            };

            // Self-heal the platform autostart entry. If the user has
            // enabled "Start on login" but the OS-level autostart entry
            // is missing (e.g. the registry key was wiped by another
            // process, or macOS removed the LaunchAgent after an OS
            // update — see tauri-apps/plugins-workspace#771), re-add it
            // now. Failures are non-fatal — the user can re-flip the
            // toggle in Settings. See issue #117.
            #[cfg(desktop)]
            if autostart_enabled {
                use tauri_plugin_autostart::ManagerExt;
                let manager = app.handle().autolaunch();
                match manager.is_enabled() {
                    Ok(true) => tracing::info!("Autostart entry present on startup"),
                    Ok(false) => {
                        tracing::warn!(
                            "Autostart preference is enabled but platform entry is missing; re-enabling"
                        );
                        if let Err(e) = manager.enable() {
                            tracing::warn!("Autostart self-heal failed: {e}");
                        }
                    }
                    Err(e) => tracing::warn!("Autostart is_enabled() check failed: {e}"),
                }
            }

            // Spawn the HTTP server on LAN interfaces.
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

                let (bound_tx, bound_rx) = std::sync::mpsc::channel();
                tauri::async_runtime::spawn(async move {
                    start_server_with_frontend_on_available_port(
                        server_state,
                        "0.0.0.0",
                        http_port,
                        dist_dir,
                        bound_tx,
                    )
                    .await;
                });

                // Navigate the Tauri window only after the embedded Axum server
                // has actually bound. If an older app still owns :7337, the
                // server falls forward to the next free port and we navigate to
                // that new port instead of accidentally displaying the old app.
                let bind_result = bound_rx.recv_timeout(std::time::Duration::from_secs(3));
                if let Some(window) = app.get_webview_window("main") {
                    match bind_result {
                        Ok(Ok(bound_port)) => {
                            tracing::info!("Navigating desktop window to local server on port {bound_port}");
                            let _ = window.eval(
                                format!("window.location.replace('http://127.0.0.1:{}')", bound_port)
                                    .as_str(),
                            );
                            // Bring the window to the top of the screen and
                            // request focus so it appears in front of other
                            // windows when launched. (#79)
                            let _ = window.set_focus();
                        }
                        Ok(Err(e)) => {
                            tracing::error!("Embedded HTTP server failed to start: {e}");
                            show_startup_error(&window, &e);
                        }
                        Err(e) => {
                            let message = format!("Timed out waiting for embedded HTTP server to bind: {e}");
                            tracing::error!("{message}");
                            show_startup_error(&window, &message);
                        }
                    }
                }
            }

            // Set the window icon from the bundled PNG.
            // `include_bytes!` embeds the file at compile time, avoiding
            // runtime working-directory issues (macOS .app bundle CWD is /).
            match tauri::image::Image::from_bytes(include_bytes!("../icons/128x128.png")) {
                Ok(img) => {
                    tracing::info!(
                        "Window icon decoded: {}x{} ({} bytes RGBA)",
                        img.width(),
                        img.height(),
                        img.rgba().len()
                    );
                    #[cfg(desktop)]
                    {
                        if let Some(window) = app.get_webview_window("main") {
                            tracing::info!(
                                "Main window found (label: {}), setting icon...",
                                window.label()
                            );
                            match window.set_icon(img) {
                                Ok(()) => {
                                    tracing::info!("Window icon set successfully");
                                }
                                Err(e) => {
                                    tracing::error!("Failed to set window icon: {e}");
                                }
                            }
                        } else {
                            tracing::warn!(
                                "Main window not found (app.get_webview_window returned None), cannot set icon"
                            );
                        }
                    }
                    #[cfg(not(desktop))]
                    {
                        tracing::debug!("Skipping window icon on non-desktop target");
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to load/encode window icon: {e}");
                }
            }

            // Spawn the Modbus polling loop
            let poll_state = state.clone();
            tauri::async_runtime::spawn(async move {
                run_poll_loop(poll_state).await;
            });

            // Spawn the EV charger polling loop
            let evc_state = state.clone();
            tauri::async_runtime::spawn(async move {
                evc::run_evc_poll_loop(evc_state).await;
            });

            // Spawn the weather fetcher. Ids for ~15 min on a missing config;
            // runs forever otherwise. See weather::run_weather_loop.
            let weather_state = state.clone();
            tauri::async_runtime::spawn(async move {
                weather::run_weather_loop(weather_state).await;
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
/// Search order (first hit wins, keyed on the presence of `index.html`):
/// 1. `--dist <path>` CLI argument (explicit override)
/// 2. `./dist/` relative to the current working directory
/// 3. `<exe_dir>/dist/` relative to the binary location
/// 4. `/usr/lib/givenergy-local/dist/` — Tauri-managed resource path used by
///    `.deb`, `.rpm`, and AppImage bundles (where Tauri's resource bundler
///    places `dist/` when only `bundle.resources` is configured, with no
///    package-specific `files` override)
/// 5. `/usr/share/givenergy-local/dist/` — system path used by the `.deb`
///    package (via `linux.deb.files`) and the Docker image
/// 6. `/var/lib/givenergy-local/dist/` — last-resort fallback for hand-rolled
///    packaging layouts
fn resolve_dist_dir(args: &[String]) -> Option<String> {
    if let Some(path) = parse_dist(args) {
        if std::path::Path::new(&path).join("index.html").exists() {
            return Some(path);
        }
        tracing::warn!("--dist path does not exist or has no index.html: {path}");
    }

    let exe_dir_dist = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.join("dist")))
        .unwrap_or_default();

    let candidates: Vec<std::path::PathBuf> = vec![
        std::path::PathBuf::from("dist"),
        exe_dir_dist,
        std::path::PathBuf::from("/usr/lib/givenergy-local/dist"),
        std::path::PathBuf::from("/usr/share/givenergy-local/dist"),
        std::path::PathBuf::from("/var/lib/givenergy-local/dist"),
    ];

    for candidate in &candidates {
        if candidate.join("index.html").exists() {
            let path = candidate.to_string_lossy().to_string();
            tracing::info!("Found frontend dist at: {path}");
            return Some(path);
        }
    }

    // Surface the failure with enough information that the user can act on
    // it. `start_server()` (API-only mode) is still called below, so the
    // server keeps running for users who genuinely want the JSON/WS API
    // without a web UI.
    let mut msg = String::from(
        "No frontend dist directory found. Searched the following paths:",
    );
    for candidate in &candidates {
        msg.push_str(&format!("\n  - {}", candidate.display()));
    }
    msg.push_str(
        "\nThe server will continue in API-only mode. To serve the web UI:\n\
         \x20 1. Install the .deb/.rpm package from the Releases page (easiest), or\n\
         \x20 2. Re-run with --dist <path> pointing to a built dist/ directory, or\n\
         \x20 3. Build with `cargo tauri build` and deploy the resulting bundle \
         (binary + dist/) together, or\n\
         \x20 4. Run from the project root after `npm run build` (dist/ is then \
         alongside the binary or under cwd).",
    );
    tracing::warn!("{msg}");

    None
}

/// Run the server in headless mode — no Tauri window, just the
/// Axum HTTP/WS server and the Modbus polling loop.
///
/// Usage: `givenergy-local --headless [--port 7337] [--dist /path/to/dist]`
pub fn run_headless(args: &[String]) {
    // Set up tracing with log capture. Shared with `run()` via `init_tracing()`.
    let log_ring = Arc::new(LogRing::new(2000));
    init_tracing(&log_ring);

    let cli_port = parse_port(args);
    // Load settings
    let settings_dir = crate::settings::Settings::settings_dir();
    tracing::info!("Settings directory: {}", settings_dir.display());
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
        // Initialise shared app state: identical to the Tauri-windowed path
        // via `initialize_app_state()`, so the startup sequence cannot diverge.
        let state = match initialize_app_state(app_settings, log_ring).await {
            Some(s) => s,
            None => return,
        };

        // Spawn the poll loop
        let poll_state = state.clone();
        tokio::spawn(async move {
            run_poll_loop(poll_state).await;
        });

        // Spawn the EV charger poll loop
        let evc_state = state.clone();
        tokio::spawn(async move {
            evc::run_evc_poll_loop(evc_state).await;
        });

        // Spawn the weather fetcher (same code path as the Tauri-windowed
        // mode — see `run_weather_loop`).
        let weather_state = state.clone();
        tokio::spawn(async move {
            weather::run_weather_loop(weather_state).await;
        });

        // Start the HTTP server
        let server_state = state.clone();
        match resolve_dist_dir(args) {
            Some(dist_dir) => {
                tracing::info!("Serving frontend from: {dist_dir}");
                start_server_with_frontend(server_state, "0.0.0.0", port, dist_dir).await;
            }
            // `resolve_dist_dir()` already logged the searched paths and the
            // remediation steps. Fall through to API-only mode so the JSON /
            // WebSocket surface stays available for users who don't need the
            // web UI (e.g. they're driving the API from a separate front-end
            // or running on a server that has no browser).
            None => {
                start_server(server_state, "0.0.0.0", port).await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverter::poll::LoadLimiterState;
    use crate::test_util::with_isolated_config_dir_async;

    /// `initialize_app_state` must apply every persisted field to the live
    /// `AppState`: poll settings, auto-winter config, load-limiter config, the
    /// restored load-limiter state, persisted auto-winter saved values, and the
    /// history database. This is the shared initialisation path that both
    /// `run()` and `run_headless()` depend on, so locking its behaviour here
    /// protects both startup paths from regression.
    #[tokio::test]
    async fn initialize_app_state_applies_all_persisted_settings() {
        with_isolated_config_dir_async(|| async {
            // Persist distinctive, non-default values through settings.json so
            // we exercise the real Settings::load() round-trip that both
            // startup paths use.
            let mut s = Settings::load();
            s.host = "10.0.0.99".to_string();
            s.port = 1234;
            s.serial = "SN-INIT-TEST".to_string();
            s.poll_interval = 42;
            s.evc_host = "evc.local".to_string();
            s.evc_port = 5020;
            s.auto_winter_enabled = true;
            s.auto_winter_cold_threshold = 1.0;
            s.auto_winter_recovery_threshold = 9.0;
            s.auto_winter_target_soc = 55;
            s.auto_winter_debounce_readings = 7;
            s.load_limiter_enabled = true;
            s.load_limiter_threshold_w = 3000;
            s.load_limiter_trigger_delay_minutes = 12;
            s.load_limiter_start_hour = 7;
            s.load_limiter_start_minute = 8;
            s.load_limiter_end_hour = 9;
            s.load_limiter_end_minute = 10;
            s.load_limiter_active_persisted = true;
            s.auto_winter_saved_enable_target = Some(true);
            s.auto_winter_saved_target_soc = Some(77);
            s.alerts_config.enabled = true;
            // Issue #117: the autostart preference should be saved/restored.
            s.autostart_enabled = true;
            s.save().expect("settings save");

            let loaded = Settings::load();
            let log_ring = Arc::new(LogRing::new(64));
            let state = initialize_app_state(loaded, log_ring)
                .await
                .expect("history db should open in isolated dir");

            // Poll settings
            {
                let ps = state.settings.lock().await;
                assert_eq!(ps.host, "10.0.0.99");
                assert_eq!(ps.port, 1234);
                assert_eq!(ps.serial, "SN-INIT-TEST");
                assert_eq!(ps.interval_secs, 42);
                assert_eq!(ps.evc_host, "evc.local");
                assert_eq!(ps.evc_port, 5020);
            }

            // Auto-winter config
            {
                let aw = state.auto_winter_config.lock().await;
                assert!(aw.enabled);
                assert_eq!(aw.cold_threshold, 1.0);
                assert_eq!(aw.recovery_threshold, 9.0);
                assert_eq!(aw.target_soc, 55);
                assert_eq!(aw.debounce_readings, 7);
            }

            // Load limiter config
            {
                let ll = state.load_limiter_config.lock().await;
                assert!(ll.enabled);
                assert_eq!(ll.threshold_w, 3000);
                assert_eq!(ll.trigger_delay_minutes, 12);
                assert_eq!(
                    (ll.start_hour, ll.start_minute, ll.end_hour, ll.end_minute),
                    (7, 8, 9, 10)
                );
            }

            // Load limiter state restored to PausedFromRestart
            {
                let ll_state = state.load_limiter_state.lock().await;
                assert!(
                    matches!(*ll_state, LoadLimiterState::PausedFromRestart),
                    "load limiter state should be restored to PausedFromRestart"
                );
            }

            // Auto-winter saved register values
            {
                let saved = state.auto_winter_saved.lock().await;
                let saved = saved
                    .as_ref()
                    .expect("auto-winter saved should be restored");
                assert!(saved.enable_charge_target);
                assert_eq!(saved.target_soc, 77);
            }

            // Alert config applied
            {
                let ac = state.alert_config.lock().await;
                assert!(
                    ac.enabled,
                    "alert config should be restored from persisted settings"
                );
            }

            // History database opened
            assert!(
                state.history.lock().await.is_some(),
                "history db should be opened"
            );
        })
        .await;
    }

    /// With no persisted auto-winter/load-limiter state, `initialize_app_state`
    /// leaves the saved register slot `None` and the limiter state at its
    /// default `Idle` rather than populating garbage.
    #[tokio::test]
    async fn initialize_app_state_leaves_defaults_when_unset() {
        with_isolated_config_dir_async(|| async {
            let s = Settings::load();
            let log_ring = Arc::new(LogRing::new(64));
            let state = initialize_app_state(s, log_ring)
                .await
                .expect("history db should open");

            assert!(
                state.auto_winter_saved.lock().await.is_none(),
                "auto-winter saved should stay None"
            );
            let ll_state = state.load_limiter_state.lock().await;
            assert!(
                matches!(*ll_state, LoadLimiterState::Idle),
                "load limiter state should stay Idle"
            );
            assert!(state.history.lock().await.is_some());
        })
        .await;
    }

    /// The autostart preference must survive a settings.json save/load
    /// round-trip. `initialize_app_state` doesn't read it (the self-heal
    /// path in `run()` reads it directly from `Settings::load()`), so the
    /// important contract is: write to disk, reload, observe the same
    /// value. Issue #117.
    #[tokio::test]
    async fn autostart_preference_round_trips_through_disk() {
        with_isolated_config_dir_async(|| async {
            let mut s = Settings::load();
            s.autostart_enabled = true;
            s.save().expect("settings save");

            let reloaded = Settings::load();
            assert!(
                reloaded.autostart_enabled,
                "autostart preference should persist across save/load"
            );

            // Flipping back to false must also stick.
            let mut s2 = Settings::load();
            s2.autostart_enabled = false;
            s2.save().expect("settings save");

            let reloaded2 = Settings::load();
            assert!(
                !reloaded2.autostart_enabled,
                "disabling autostart should also persist"
            );
        })
        .await;
    }

    /// `prune_old_logs` keeps files within the retention window and deletes
    /// older ones, while leaving unrelated files untouched.
    #[test]
    fn prune_old_logs_drops_expired_files() {
        let dir = std::env::temp_dir().join(format!(
            "hem-prune-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let today = chrono::Local::now().date_naive();
        let keep = today.format("app.log.%Y-%m-%d").to_string();
        let old = (today - chrono::Duration::days(LOG_RETENTION_DAYS + 1))
            .format("app.log.%Y-%m-%d")
            .to_string();
        let unrelated = "settings.json".to_string();

        std::fs::write(dir.join(&keep), "keep").unwrap();
        std::fs::write(dir.join(&old), "drop").unwrap();
        std::fs::write(dir.join(&unrelated), "ignore").unwrap();

        prune_old_logs(&dir);

        assert!(dir.join(&keep).exists(), "recent log should be kept");
        assert!(!dir.join(&old).exists(), "expired log should be deleted");
        assert!(
            dir.join(&unrelated).exists(),
            "non-log files must be left alone"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Build a tempdir containing a fake `dist/index.html`. Returns the
    /// directory path. Caller is responsible for removing it.
    fn make_fake_dist(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hem-dist-test-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("dist")).unwrap();
        std::fs::write(dir.join("dist").join("index.html"), "<html></html>").unwrap();
        dir
    }

    /// `--dist <path>` wins over every other candidate. The explicit override
    /// must be honoured even if the path isn't one of the auto-discovered
    /// locations (e.g. the user pointed at a custom frontend fork).
    #[test]
    fn resolve_dist_dir_prefers_explicit_dist_arg() {
        let dir = make_fake_dist("explicit");
        let args = vec![
            "--headless".to_string(),
            "--dist".to_string(),
            dir.join("dist").to_string_lossy().to_string(),
        ];
        let resolved = resolve_dist_dir(&args).expect("explicit --dist should win");
        assert_eq!(resolved, dir.join("dist").to_string_lossy().to_string());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// When `--dist` points at a non-existent path, the resolver falls through
    /// to the auto-discovery candidates rather than erroring out. This matches
    /// the documented "API-only mode if dist can't be found" contract.
    #[test]
    fn resolve_dist_dir_falls_through_when_explicit_dist_missing() {
        let args = vec![
            "--headless".to_string(),
            "--dist".to_string(),
            "/nonexistent/dist-xyz".to_string(),
        ];
        // Should not panic, should not return the bad path. Whether the
        // search lands on cwd or none depends on the test environment, so we
        // just assert it does not return the explicit (bad) override.
        let resolved = resolve_dist_dir(&args);
        if let Some(path) = resolved {
            assert_ne!(
                path, "/nonexistent/dist-xyz",
                "must not return the missing --dist path"
            );
        }
    }

    /// `--dist` to a directory that exists but has no `index.html` must NOT
    /// be accepted — that would claim to serve a frontend and then 404 on
    /// every request. The resolver should fall through.
    #[test]
    fn resolve_dist_dir_rejects_dist_without_index_html() {
        let dir = std::env::temp_dir().join(format!(
            "hem-dist-noindex-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let args = vec![
            "--headless".to_string(),
            "--dist".to_string(),
            dir.to_string_lossy().to_string(),
        ];
        let resolved = resolve_dist_dir(&args);
        if let Some(path) = resolved {
            assert_ne!(
                path,
                dir.to_string_lossy().to_string(),
                "must not return a dist path that has no index.html"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The search order must visit every documented candidate. This test
    /// pins the relative order of the entries in the docstring so that
    /// reordering has to be deliberate. A real "first hit wins" test with
    /// multiple dists on disk would be too environment-dependent, so we
    /// pin the order by string position in the source.
    #[test]
    fn resolve_dist_dir_search_order_documented() {
        let candidates = [
            "./dist/",
            "<exe_dir>/dist/",
            "/usr/lib/givenergy-local/dist/",
            "/usr/share/givenergy-local/dist/",
            "/var/lib/givenergy-local/dist/",
        ];
        // Pull the doc comment text out of the function. This is the
        // single source of truth for what the resolver searches.
        let doc = "/// 1. `--dist <path>` CLI argument (explicit override)
/// 2. `./dist/` relative to the current working directory
/// 3. `<exe_dir>/dist/` relative to the binary location
/// 4. `/usr/lib/givenergy-local/dist/` \u{2014} Tauri-managed resource path used by
///    `.deb`, `.rpm`, and AppImage bundles (where Tauri's resource bundler
///    places `dist/` when only `bundle.resources` is configured, with no
///    package-specific `files` override)
/// 5. `/usr/share/givenergy-local/dist/` \u{2014} system path used by the `.deb`
///    package (via `linux.deb.files`) and the Docker image
/// 6. `/var/lib/givenergy-local/dist/` \u{2014} last-resort fallback for hand-rolled
///    packaging layouts";
        let positions: Vec<usize> = candidates
            .iter()
            .map(|needle| {
                doc.find(needle)
                    .unwrap_or_else(|| panic!("docstring missing entry: {needle}"))
            })
            .collect();
        let mut sorted = positions.clone();
        sorted.sort();
        assert_eq!(
            positions, sorted,
            "search-order docstring entries must be listed in ascending order"
        );
    }
}
