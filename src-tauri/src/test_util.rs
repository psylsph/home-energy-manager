//! Shared helpers for tests that need settings-backed state.
//! Tests must never resolve or touch the live `~/.givenergy-local` directory.

#![cfg(test)]

use std::sync::OnceLock;

/// Serialises every in-process test that changes the process-global config
/// override. Both sync and async tests use the same lock.
fn config_dir_mutex() -> &'static parking_lot::Mutex<()> {
    static MUTEX: OnceLock<parking_lot::Mutex<()>> = OnceLock::new();
    MUTEX.get_or_init(|| parking_lot::Mutex::new(()))
}

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

/// Run a synchronous test body with an isolated config directory.
pub fn with_isolated_config_dir<T>(body: impl FnOnce() -> T) -> T {
    let _isolation = IsolationGuard::enter();
    body()
}

/// Run an asynchronous test body with an isolated config directory.
pub async fn with_isolated_config_dir_async<F, Fut>(body: F) -> Fut::Output
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future,
{
    let _isolation = IsolationGuard::enter();
    body().await
}

/// Owns the process-global override and restores the caller's prior value on
/// every exit path, including panic unwinding.
struct IsolationGuard {
    _lock: parking_lot::MutexGuard<'static, ()>,
    dir: Option<std::path::PathBuf>,
    previous_config_dir: Option<std::ffi::OsString>,
}

impl IsolationGuard {
    fn enter() -> Self {
        let lock = config_dir_mutex().lock();
        let dir = make_temp_dir();
        std::fs::create_dir_all(&dir).expect("create isolated test config directory");
        let previous_config_dir = std::env::var_os("GIVENERGY_LOCAL_CONFIG_DIR");
        std::env::set_var("GIVENERGY_LOCAL_CONFIG_DIR", &dir);
        Self {
            _lock: lock,
            dir: Some(dir),
            previous_config_dir,
        }
    }
}

impl Drop for IsolationGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous_config_dir.take() {
            std::env::set_var("GIVENERGY_LOCAL_CONFIG_DIR", previous);
        } else {
            std::env::remove_var("GIVENERGY_LOCAL_CONFIG_DIR");
        }
        if let Some(dir) = self.dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;

    #[test]
    fn sync_helper_uses_temp_dir_and_restores_caller_override() {
        let previous = std::env::var_os("GIVENERGY_LOCAL_CONFIG_DIR");
        with_isolated_config_dir(|| {
            let inside = crate::settings::Settings::settings_dir();
            assert!(inside.starts_with(std::env::temp_dir()));
            assert_eq!(inside, crate::settings::Settings::settings_dir());
        });
        assert_eq!(std::env::var_os("GIVENERGY_LOCAL_CONFIG_DIR"), previous);
    }

    #[tokio::test]
    async fn async_helper_uses_temp_dir_and_restores_caller_override() {
        let previous = std::env::var_os("GIVENERGY_LOCAL_CONFIG_DIR");
        let result = with_isolated_config_dir_async(|| async {
            let inside = crate::settings::Settings::settings_dir();
            assert!(inside.starts_with(std::env::temp_dir()));
            42_u32
        })
        .await;
        assert_eq!(result, 42);
        assert_eq!(std::env::var_os("GIVENERGY_LOCAL_CONFIG_DIR"), previous);
    }

    #[test]
    fn sync_helper_restores_override_after_panic() {
        let previous = std::env::var_os("GIVENERGY_LOCAL_CONFIG_DIR");
        let result = std::panic::catch_unwind(|| {
            with_isolated_config_dir(|| {
                assert!(crate::settings::Settings::settings_dir().starts_with(std::env::temp_dir()));
                panic!("intentional isolation test panic");
            })
        });
        assert!(result.is_err());
        assert_eq!(std::env::var_os("GIVENERGY_LOCAL_CONFIG_DIR"), previous);
    }

    #[tokio::test]
    async fn async_helper_restores_override_after_panic() {
        let previous = std::env::var_os("GIVENERGY_LOCAL_CONFIG_DIR");
        let result = std::panic::AssertUnwindSafe(with_isolated_config_dir_async(|| async {
            assert!(crate::settings::Settings::settings_dir().starts_with(std::env::temp_dir()));
            panic!("intentional async isolation test panic");
        }))
        .catch_unwind()
        .await;
        assert!(result.is_err());
        assert_eq!(std::env::var_os("GIVENERGY_LOCAL_CONFIG_DIR"), previous);
    }
}
