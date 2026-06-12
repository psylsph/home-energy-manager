//! Shared test utilities.
//!
//! Currently only contains helpers to run tests against an ephemeral
//! `~/.givenergy-local/`-shaped config directory without polluting the
//! user's real settings file.

#![cfg(test)]

use std::sync::OnceLock;

/// Global mutex that serializes all tests touching `GIVENERGY_LOCAL_CONFIG_DIR`.
/// Uses `tokio::sync::Mutex` so async tests can hold the guard across `.await`.
fn config_dir_mutex() -> &'static tokio::sync::Mutex<()> {
    static M: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    M.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Return a unique temp path for one test invocation.
fn make_temp_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "givenergy-local-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

/// Run `body` against an isolated config dir. Holds a global mutex so tests
/// across modules don't race on the env var.
///
/// This is the **sync** flavour — for async tests (tokio), call
/// [`with_isolated_config_dir_async`] instead so the guard lives across `.await`.
pub fn with_isolated_config_dir<T>(body: impl FnOnce() -> T) -> T {
    // block on the tokio mutex for sync test compatibility
    let _guard = config_dir_mutex().blocking_lock();
    let tmp = make_temp_dir();
    let _ = std::fs::create_dir_all(&tmp);
    std::env::set_var("GIVENERGY_LOCAL_CONFIG_DIR", &tmp);
    let result = body();
    std::env::remove_var("GIVENERGY_LOCAL_CONFIG_DIR");
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// Async variant of [`with_isolated_config_dir`]. Keeps the env var alive
/// until the returned future completes.
pub async fn with_isolated_config_dir_async<F, Fut>(body: F) -> Fut::Output
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future,
{
    let _guard = config_dir_mutex().lock().await;
    let tmp = make_temp_dir();
    let _ = std::fs::create_dir_all(&tmp);
    std::env::set_var("GIVENERGY_LOCAL_CONFIG_DIR", &tmp);
    let result = body().await;
    std::env::remove_var("GIVENERGY_LOCAL_CONFIG_DIR");
    let _ = std::fs::remove_dir_all(&tmp);
    result
}
