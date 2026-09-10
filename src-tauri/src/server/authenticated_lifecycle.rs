//! Owned lifecycle for the authenticated external API listener.
//!
//! The listener is a long-lived task, but its configuration (credential
//! presence, bind address, port, CORS origins) changes through settings.
//! This manager owns the running task so a configuration change can stop
//! the old listener (gracefully draining in-flight requests) before the
//! replacement starts — and so a failed bind rolls back deterministically
//! instead of leaving a stale or half-configured listener behind.
//!
//! Transaction policy (remote-API hardening U2):
//!
//! 1. Stop the current listener, if any, and wait for it to release its
//!    socket.
//! 2. Bind the new configuration. On failure the previous configuration is
//!    re-bound as a rollback, and the error is returned to the caller (the
//!    settings handler reverts the persisted change).
//! 3. Serve until the next `apply` or `shutdown`.
//!
//! Binding happens *before* the success reply, so once `apply` returns
//! `Ok` the listener is accepting connections.

use std::net::IpAddr;
use std::sync::Arc;

use tokio::sync::watch;

use crate::inverter::poll::AppState;

/// Listener configuration snapshot applied atomically by [`AuthenticatedLifecycle::apply`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedConfig {
    pub bind_ip: IpAddr,
    pub port: u16,
    /// Exact browser origins allowed by CORS; empty = no CORS headers.
    pub allowed_origins: Vec<String>,
}

struct RunningListener {
    config: AuthenticatedConfig,
    shutdown: watch::Sender<bool>,
    handle: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
pub struct AuthenticatedLifecycle {
    running: tokio::sync::Mutex<Option<RunningListener>>,
}

impl AuthenticatedLifecycle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a listener task is currently owned by this manager.
    pub async fn is_running(&self) -> bool {
        self.running.lock().await.is_some()
    }

    /// Converge the running listener to `desired` (`None` = stopped).
    ///
    /// Returns `Err` (with the previous configuration re-bound) if the new
    /// configuration cannot bind. Always safe to call repeatedly with the
    /// same configuration: the caller decides when a change happened.
    pub async fn apply(
        &self,
        state: Arc<AppState>,
        desired: Option<AuthenticatedConfig>,
    ) -> Result<(), String> {
        // Always stop the old listener first: if the port is unchanged the
        // new bind could never succeed while the old task holds the socket.
        let previous = self.stop_current().await;
        if previous.is_some() {
            Self::record(
                &state,
                crate::server::audit::AuditEvent {
                    kind: "listener_stopped",
                    actor: None,
                    source: None,
                    method: None,
                    path: None,
                    outcome: "ok",
                    detail: None,
                },
            );
        }

        let Some(config) = desired else {
            return Ok(());
        };

        match self.start(state.clone(), &config).await {
            Ok(()) => {
                Self::record(
                    &state,
                    crate::server::audit::AuditEvent {
                        kind: "listener_started",
                        actor: None,
                        source: Some(config.bind_ip.to_string()),
                        method: None,
                        path: None,
                        outcome: "ok",
                        detail: Some(format!("port {}", config.port)),
                    },
                );
                Ok(())
            }
            Err(bind_error) => {
                Self::record(
                    &state,
                    crate::server::audit::AuditEvent {
                        kind: "listener_bind_failed",
                        actor: None,
                        source: Some(config.bind_ip.to_string()),
                        method: None,
                        path: None,
                        outcome: "error",
                        detail: Some(bind_error.clone()),
                    },
                );
                // Roll back: the previous configuration was healthy, so the
                // owner's integrations keep working while settings show the
                // failure. (If even the rollback bind fails — e.g. the port
                // vanished from the machine — surface the combined error.)
                if let Some(previous) = previous {
                    if let Err(rollback_error) = self.start(state, &previous).await {
                        return Err(format!(
                            "{bind_error}; rollback to the previous listener also failed: \
                             {rollback_error}"
                        ));
                    }
                }
                Err(bind_error)
            }
        }
    }

    /// Fail-open audit for listener transitions.
    fn record(state: &Arc<AppState>, event: crate::server::audit::AuditEvent) {
        if let Err(e) = state.audit.record(event) {
            tracing::warn!("Audit write failed: {e}");
        }
    }

    /// Stop the listener (if any) and wait for the socket to be released.
    pub async fn shutdown(&self) {
        self.stop_current().await;
    }

    /// Stop the listener (if any) and wait for the socket to be released.
    /// Returns the configuration that was running, if known.
    async fn stop_current(&self) -> Option<AuthenticatedConfig> {
        let running = self.running.lock().await.take()?;
        let _ = running.shutdown.send(true);
        let _ = running.handle.await;
        Some(running.config)
    }

    /// Bind and spawn the listener for `config`. Resolves once the socket is
    /// bound (or fails), so the caller sees a listening — or failed —
    /// service, never a half-started one.
    async fn start(
        &self,
        state: Arc<AppState>,
        config: &AuthenticatedConfig,
    ) -> Result<(), String> {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (bound_tx, bound_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
        let handle = tokio::spawn(super::start_authenticated_server(
            state,
            config.bind_ip.to_string(),
            config.port,
            config.allowed_origins.clone(),
            shutdown_rx,
            bound_tx,
        ));
        match bound_rx.await {
            Ok(Ok(())) => {
                self.running.lock().await.replace(RunningListener {
                    config: config.clone(),
                    shutdown: shutdown_tx,
                    handle,
                });
                Ok(())
            }
            Ok(Err(message)) => {
                // The task exits on its own after a bind failure.
                let _ = handle.await;
                Err(message)
            }
            Err(_) => {
                // The task died before reporting (panic); join to clean up.
                let _ = handle.await;
                Err("authenticated API listener task ended unexpectedly".to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverter::model::{DeviceType, InverterSnapshot};
    use std::io::{Read, Write};
    use std::time::Duration;

    /// Reserve an OS-assigned free port, release it, and hand the number
    /// back for the listener under test.
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    async fn state_with_credential() -> Arc<AppState> {
        let mut settings = crate::settings::Settings::load();
        settings.api_credential =
            Some(crate::settings::ApiCredential::from_secret("lifecycle-key"));
        settings.save().unwrap();
        let state = Arc::new(AppState::new());
        *state.latest_snapshot.lock().await = Some(InverterSnapshot {
            timestamp: chrono::Utc::now().timestamp(),
            device_type: DeviceType::ACCoupled,
            ..Default::default()
        });
        state
    }

    /// Minimal authenticated GET over a raw socket; returns the status line.
    fn http_get_status(addr: std::net::SocketAddr, token: &str) -> String {
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(
            stream,
            "GET /api/control/status HTTP/1.1\r\nHost: {addr}\r\n\
             Authorization: Bearer {token}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        response.lines().next().unwrap_or_default().to_string()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn apply_starts_and_shuts_down_a_real_listener() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_credential().await;
            let lifecycle = AuthenticatedLifecycle::new();
            let port = free_port();
            let config = AuthenticatedConfig {
                bind_ip: "127.0.0.1".parse().unwrap(),
                port,
                allowed_origins: Vec::new(),
            };

            lifecycle
                .apply(state.clone(), Some(config.clone()))
                .await
                .unwrap();
            assert!(lifecycle.is_running().await);

            let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
            let status = http_get_status(addr, "lifecycle-key");
            assert!(
                status.starts_with("HTTP/1.1 200"),
                "authenticated request must succeed on the applied listener, got {status:?}"
            );

            lifecycle.shutdown().await;
            assert!(!lifecycle.is_running().await);

            // The socket must be released after shutdown.
            std::thread::sleep(Duration::from_millis(50));
            assert!(
                std::net::TcpListener::bind(addr).is_ok(),
                "port {port} must be released after shutdown"
            );
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn apply_replaces_the_listener_on_port_change() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_credential().await;
            let lifecycle = AuthenticatedLifecycle::new();
            let first = AuthenticatedConfig {
                bind_ip: "127.0.0.1".parse().unwrap(),
                port: free_port(),
                allowed_origins: Vec::new(),
            };
            let second = AuthenticatedConfig {
                port: free_port(),
                ..first.clone()
            };

            lifecycle
                .apply(state.clone(), Some(first.clone()))
                .await
                .unwrap();
            lifecycle
                .apply(state.clone(), Some(second.clone()))
                .await
                .unwrap();
            assert!(lifecycle.is_running().await);

            let second_addr: std::net::SocketAddr =
                format!("127.0.0.1:{}", second.port).parse().unwrap();
            let status = http_get_status(second_addr, "lifecycle-key");
            assert!(
                status.starts_with("HTTP/1.1 200"),
                "replacement listener must answer on the new port, got {status:?}"
            );

            lifecycle.shutdown().await;
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn apply_rolls_back_to_the_previous_listener_when_the_new_bind_fails() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_credential().await;
            let lifecycle = AuthenticatedLifecycle::new();

            // Occupy the candidate port with an unrelated socket.
            let blocker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let blocked_port = blocker.local_addr().unwrap().port();

            let healthy = AuthenticatedConfig {
                bind_ip: "127.0.0.1".parse().unwrap(),
                port: free_port(),
                allowed_origins: Vec::new(),
            };
            lifecycle
                .apply(state.clone(), Some(healthy.clone()))
                .await
                .unwrap();

            let broken = AuthenticatedConfig {
                port: blocked_port,
                ..healthy.clone()
            };
            let error = lifecycle
                .apply(state.clone(), Some(broken))
                .await
                .expect_err("binding an occupied port must fail");
            assert!(error.contains("Failed to bind"), "error: {error}");

            // The previous listener is still serving after the rolled-back apply.
            let addr: std::net::SocketAddr = format!("127.0.0.1:{}", healthy.port).parse().unwrap();
            let status = http_get_status(addr, "lifecycle-key");
            assert!(
                status.starts_with("HTTP/1.1 200"),
                "previous listener must survive a failed rebind, got {status:?}"
            );

            lifecycle.shutdown().await;
            drop(blocker);
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn apply_none_stops_the_listener() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_credential().await;
            let lifecycle = AuthenticatedLifecycle::new();
            let config = AuthenticatedConfig {
                bind_ip: "127.0.0.1".parse().unwrap(),
                port: free_port(),
                allowed_origins: Vec::new(),
            };
            lifecycle.apply(state.clone(), Some(config)).await.unwrap();
            lifecycle.apply(state, None).await.unwrap();
            assert!(!lifecycle.is_running().await);
        })
        .await;
    }
}
