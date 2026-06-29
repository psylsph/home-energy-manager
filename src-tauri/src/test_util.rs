//! Shared test utilities.
//!
//! Currently only contains helpers to run tests against an ephemeral
//! `~/.givenergy-local/`-shaped config directory without polluting the
//! user's real settings file.

#![cfg(test)]

use std::path::Path;
use std::sync::OnceLock;

/// Process-wide mutex that serialises ALL tests touching
/// `GIVENERGY_LOCAL_CONFIG_DIR`. We use a `parking_lot::Mutex` (not
/// `tokio::sync::Mutex`) because the helper must work from BOTH:
///   - plain `#[test]` functions (no Tokio runtime in scope), and
///   - `#[tokio::test]` functions (Tokio runtime in scope).
///
/// `tokio::sync::Mutex::blocking_lock()` from a non-async context
/// spawns a fresh single-threaded runtime per call, which can race
/// against a sibling `#[tokio::test]`'s async `lock().await` — in
/// practice that race manifests as one test's body running while
/// another's teardown is mid-flight, clobbering `GIVENERGY_LOCAL_CONFIG_DIR`
/// between two `Settings::settings_dir()` calls inside a single body.
/// `parking_lot::Mutex` blocks the OS thread cleanly with no runtime
/// involvement, so the env-var lifecycle is fully serialised.
fn config_dir_mutex() -> &'static parking_lot::Mutex<()> {
    static M: OnceLock<parking_lot::Mutex<()>> = OnceLock::new();
    M.get_or_init(|| parking_lot::Mutex::new(()))
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
///
/// Panic safety: the env-var override is owned by an RAII guard that
/// resets on `Drop`, so a panic inside `body` (sync or async) still
/// clears `GIVENERGY_LOCAL_CONFIG_DIR` and removes the temp dir
/// before the test process moves on.
pub fn with_isolated_config_dir<T>(body: impl FnOnce() -> T) -> T {
    let _isolation = IsolationGuard::enter();
    body()
}

/// Async variant of [`with_isolated_config_dir`]. Keeps the env var alive
/// until the returned future completes. Uses the same `parking_lot`
/// mutex so a `#[tokio::test]` body can't race against a sibling
/// plain `#[test]` body's set/remove cycle.
pub async fn with_isolated_config_dir_async<F, Fut>(body: F) -> Fut::Output
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future,
{
    let _isolation = IsolationGuard::enter();
    body().await
}

/// RAII guard that owns the global env-var mutex + the temp dir it
/// created. `Drop` runs in any unwind path (panic, early return, etc.)
/// so the env var is always cleared and the temp dir is always swept,
/// even if a test body bails out unexpectedly. This is the property
/// the AGENTS.md isolation contract depends on — a panicking test
/// must NOT leave `GIVENERGY_LOCAL_CONFIG_DIR` set for the next test.
struct IsolationGuard {
    /// Hold the lock for the entire lifetime of the guard. Dropping
    /// the guard releases the lock AFTER the env-var reset, so a
    /// sibling test's body can't observe a transient "lock held +
    /// env var cleared" state.
    _lock: parking_lot::MutexGuard<'static, ()>,
    /// Temp dir to remove on drop. We `take()` it in `Drop` so a
    /// panic during the body doesn't leave the directory around.
    dir: Option<std::path::PathBuf>,
}

impl IsolationGuard {
    fn enter() -> Self {
        let lock = config_dir_mutex().lock();
        let dir = make_temp_dir();
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("GIVENERGY_LOCAL_CONFIG_DIR", &dir);
        Self {
            _lock: lock,
            dir: Some(dir),
        }
    }
}

impl Drop for IsolationGuard {
    fn drop(&mut self) {
        // Reset the env var FIRST so a sibling test that races to
        // acquire the lock doesn't see the old override.
        std::env::remove_var("GIVENERGY_LOCAL_CONFIG_DIR");
        if let Some(dir) = self.dir.take() {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// True iff `path` resolves under the user's real home directory (the one
/// used by the production `Settings::settings_dir()` when no override is
/// set). Tests use this to prove they never write to production paths.
fn is_under_user_home(path: &Path) -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    // canonicalize best-effort; on missing files just compare components.
    let path_norm = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf());
    let home_norm = home
        .canonicalize()
        .unwrap_or_else(|_| home.clone());
    path_norm.starts_with(&home_norm)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;

    /// Sanity-check that [`with_isolated_config_dir`] does what its docs
    /// claim: redirects `Settings::settings_dir()` to a per-test temp
    /// directory and reverts to the real home afterwards. Without this
    /// guarantee, a future refactor that broke isolation would only
    /// surface as production settings being clobbered mid-test-run —
    /// which is the exact failure mode the AGENTS.md guidance exists
    /// to prevent.
    #[test]
    fn with_isolated_config_dir_points_settings_away_from_production() {
        // Capture the production path the test would have hit had isolation
        // failed. We do this BEFORE setting the env var so we compare against
        // the real resolution, not a post-override one.
        std::env::remove_var("GIVENERGY_LOCAL_CONFIG_DIR");
        let production_path = crate::settings::Settings::settings_dir();

        with_isolated_config_dir(|| {
            let test_path = crate::settings::Settings::settings_dir();

            // The test path must not match the production path. If they did,
            // isolation would be broken — the test would be writing to the
            // user's real ~/.givenergy-local/.
            assert_ne!(
                test_path, production_path,
                "isolated config dir collapsed onto production path; AGENTS.md isolation broken"
            );

            // The test path must live somewhere that ISN'T the user's home
            // directory tree (so even if canonicalize fails the comparison
            // is meaningful). Our helper uses std::env::temp_dir() which is
            // /tmp on Linux, so this should always hold.
            assert!(
                !is_under_user_home(&test_path),
                "test settings dir {test_path:?} is unexpectedly inside the user's home"
            );

            // And the temp dir must be unique-per-invocation — no two runs
            // can ever share a config dir.
            let test_path2 = crate::settings::Settings::settings_dir();
            assert_eq!(
                test_path, test_path2,
                "settings_dir() should be stable across calls within one isolated run"
            );
        });

        // After the closure returns the env var is cleared. A fresh
        // settings_dir() call must land on the production path again —
        // otherwise the next test in the process would inherit our
        // isolation state.
        let after_path = crate::settings::Settings::settings_dir();
        assert_eq!(
            after_path, production_path,
            "env var not cleaned up; future tests could inherit isolation state"
        );
    }

    /// Companion to the sync version: prove the async flavour cleans up
    /// the env var once the body completes normally. Panic safety of
    /// both flavours is now provided by the `IsolationGuard`'s `Drop`
    /// impl (see `IsolationGuard`), which fires during unwinding so a
    /// panicking body still resets the env var and removes the temp
    /// dir before the next test runs.
    #[tokio::test]
    async fn with_isolated_config_dir_async_cleans_up_after_normal_return() {
        std::env::remove_var("GIVENERGY_LOCAL_CONFIG_DIR");
        let production_path = crate::settings::Settings::settings_dir();

        let result = with_isolated_config_dir_async(|| async {
            // Inside the async body, isolation must be active.
            let inside = crate::settings::Settings::settings_dir();
            assert_ne!(inside, production_path, "isolation inactive inside async body");
            assert!(!is_under_user_home(&inside));
            42_u32
        })
        .await;
        assert_eq!(result, 42, "body result must be returned verbatim");

        let after_path = crate::settings::Settings::settings_dir();
        assert_eq!(
            after_path, production_path,
            "env var not cleaned up; future tests could inherit isolation state"
        );
        assert!(
            std::env::var_os("GIVENERGY_LOCAL_CONFIG_DIR").is_none(),
            "env var should be cleared after the async body completes"
        );
    }

    /// Prove that the RAII guard in `IsolationGuard::Drop` resets the
    /// env var even when the test body panics. Without this property
    /// a panicking test would leave `GIVENERGY_LOCAL_CONFIG_DIR`
    /// pointing at a now-deleted temp dir, and the next test would
    /// fail in confusing ways (or worse, accidentally clobber
    /// production settings).
    #[test]
    fn isolation_survives_panicking_sync_body() {
        std::env::remove_var("GIVENERGY_LOCAL_CONFIG_DIR");
        let production_path = crate::settings::Settings::settings_dir();

        let result = std::panic::catch_unwind(|| {
            with_isolated_config_dir(|| {
                // Confirm isolation is active before we panic.
                let inside = crate::settings::Settings::settings_dir();
                assert_ne!(inside, production_path);
                panic!("intentional test panic for isolation guard verification");
            })
        });
        assert!(result.is_err(), "body should have panicked");

        // After the unwind, the env var MUST be cleared and a fresh
        // settings_dir() call must resolve back to the production path.
        assert!(
            std::env::var_os("GIVENERGY_LOCAL_CONFIG_DIR").is_none(),
            "env var leaked across a panic; next test in the process could clobber production settings"
        );
        let after = crate::settings::Settings::settings_dir();
        assert_eq!(
            after, production_path,
            "settings_dir() must resolve back to production after a panic"
        );
    }

    /// Same property for the async flavour: a panic inside the awaited
    /// future must still trigger the guard's `Drop` and reset the env
    /// var. We use `catch_unwind` to swallow the panic and assert on
    /// the post-conditions.
    #[tokio::test]
    async fn isolation_survives_panicking_async_body() {
        std::env::remove_var("GIVENERGY_LOCAL_CONFIG_DIR");
        let production_path = crate::settings::Settings::settings_dir();

        // Build a future that panics inside with_isolated_config_dir_async.
        // We invoke it directly inside `catch_unwind` since futures
        // themselves are not `UnwindSafe`, but `std::panic::AssertUnwindSafe`
        // lets us assert on the panic without inspecting the result.
        let join = std::panic::AssertUnwindSafe(with_isolated_config_dir_async(|| async {
            let inside = crate::settings::Settings::settings_dir();
            assert_ne!(inside, production_path);
            panic!("intentional test panic in async body");
        }))
        .catch_unwind()
        .await;
        assert!(join.is_err(), "async body should have panicked");

        assert!(
            std::env::var_os("GIVENERGY_LOCAL_CONFIG_DIR").is_none(),
            "env var leaked across an async panic"
        );
        let after = crate::settings::Settings::settings_dir();
        assert_eq!(after, production_path);
    }
}
