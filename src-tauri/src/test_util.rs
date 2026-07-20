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

fn restore_config_dir(previous: Option<std::ffi::OsString>) {
    if let Some(previous) = previous {
        std::env::set_var("GIVENERGY_LOCAL_CONFIG_DIR", previous);
    } else {
        std::env::remove_var("GIVENERGY_LOCAL_CONFIG_DIR");
    }
}

impl Drop for IsolationGuard {
    fn drop(&mut self) {
        restore_config_dir(self.previous_config_dir.take());
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
    fn sync_helper_uses_and_removes_temp_dir() {
        let inside = with_isolated_config_dir(|| {
            let inside = crate::settings::Settings::settings_dir();
            assert!(inside.starts_with(std::env::temp_dir()));
            assert_eq!(inside, crate::settings::Settings::settings_dir());
            inside
        });
        assert!(!inside.exists());
    }

    #[tokio::test]
    async fn async_helper_uses_and_removes_temp_dir() {
        let inside = with_isolated_config_dir_async(|| async {
            let inside = crate::settings::Settings::settings_dir();
            assert!(inside.starts_with(std::env::temp_dir()));
            inside
        })
        .await;
        assert!(!inside.exists());
    }

    #[test]
    fn sync_helper_cleans_up_after_panic() {
        let inside = std::sync::Mutex::new(None);
        let result = std::panic::catch_unwind(|| {
            with_isolated_config_dir(|| {
                let dir = crate::settings::Settings::settings_dir();
                assert!(dir.starts_with(std::env::temp_dir()));
                *inside.lock().unwrap() = Some(dir);
                panic!("intentional isolation test panic");
            })
        });
        assert!(result.is_err());
        assert!(!inside.lock().unwrap().as_ref().unwrap().exists());
    }

    #[tokio::test]
    async fn async_helper_cleans_up_after_panic() {
        let inside = std::sync::Mutex::new(None);
        let result = std::panic::AssertUnwindSafe(with_isolated_config_dir_async(|| async {
            let dir = crate::settings::Settings::settings_dir();
            assert!(dir.starts_with(std::env::temp_dir()));
            *inside.lock().unwrap() = Some(dir);
            panic!("intentional async isolation test panic");
        }))
        .catch_unwind()
        .await;
        assert!(result.is_err());
        assert!(!inside.lock().unwrap().as_ref().unwrap().exists());
    }

    #[test]
    fn restore_config_dir_reinstates_caller_override() {
        let _lock = config_dir_mutex().lock();
        let original = std::env::var_os("GIVENERGY_LOCAL_CONFIG_DIR");
        let caller = make_temp_dir();
        restore_config_dir(Some(caller.clone().into_os_string()));
        assert_eq!(
            std::env::var_os("GIVENERGY_LOCAL_CONFIG_DIR"),
            Some(caller.into_os_string())
        );
        restore_config_dir(original);
    }

    #[test]
    fn restore_config_dir_clears_missing_override() {
        let _lock = config_dir_mutex().lock();
        let original = std::env::var_os("GIVENERGY_LOCAL_CONFIG_DIR");
        std::env::set_var("GIVENERGY_LOCAL_CONFIG_DIR", make_temp_dir());
        restore_config_dir(None);
        assert!(std::env::var_os("GIVENERGY_LOCAL_CONFIG_DIR").is_none());
        restore_config_dir(original);
    }
}
