//! Integration tests for the WebSocket keepalive probe (issue #274).
//!
//! A quiet-but-live client must NOT be disconnected: when the 30s keepalive
//! fires, the server sends a Ping and keeps the connection open when the
//! client's stack answers (browsers answer Pings automatically per RFC
//! 6455). These tests drive the real `/ws` route through a real TCP socket
//! with `tokio-tungstenite`, because the in-process oneshot harness cannot
//! perform the HTTP upgrade.
//!
//! Timing: the keepalive is 30s, too slow for a test to wait out. Instead
//! these tests assert the observable contract at test speed — a client that
//! stays connected and only ever answers Pings receives broadcasts
//! indefinitely and is never dropped — by streaming for a window shorter
//! than the keepalive would need. The probe itself (send Ping, grace
//! window, drop on silence) is exercised by keeping a connection open with
//! NO traffic beyond the automatic Pong for longer than the server's
//! keepalive; if the server still delivered a broadcast afterwards, the
//! probe correctly kept the connection alive.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use givenergy_local::inverter::poll::{AppState, PollMessage};
use givenergy_local::server::create_router;
use tokio_tungstenite::tungstenite::Message;

/// Bind the production router on an ephemeral port and return its address.
async fn spawn_server(state: Arc<AppState>) -> std::net::SocketAddr {
    let app = create_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    addr
}

/// How long to hold the connection with no client-originated traffic.
/// Must exceed the server's 30s keepalive so the probe path runs.
const QUIET_WINDOW: Duration = Duration::from_secs(35);

#[tokio::test]
async fn quiet_client_survives_keepalive_probe_and_still_receives_broadcasts() {
    let config_guard = test_config_isolation::enter();
    let state = Arc::new(AppState::new());
    let addr = spawn_server(state.clone()).await;

    let (ws, _resp) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap();
    let (mut sink, mut stream) = ws.split();

    // Client never sends anything. tungstenite answers the server's Ping
    // with a Pong automatically (same as a browser stack), which is exactly
    // the reporter's scenario in issue #274: an idle tab on a remote link.

    // Hold the connection quiet for longer than the 30s keepalive. Incoming
    // Pings from the server are answered by tungstenite's auto-pong in the
    // stream half; we just must not consume-and-drop the stream. Splitting
    // means the pong handling needs manual forwarding, so drive it in a task.
    let driver = tokio::spawn(async move {
        // Answer any server Ping with a Pong, ignore everything else, and
        // surface the first broadcast snapshot after the quiet window.
        let mut saw_first_after_quiet;
        loop {
            let Some(Ok(msg)) = stream.next().await else { return false };
            match msg {
                Message::Ping(payload) => {
                    let _ = sink.send(Message::Pong(payload)).await;
                }
                Message::Text(t) => {
                    let is_snapshot = serde_json::from_str::<serde_json::Value>(&t)
                        .ok()
                        .and_then(|v| v.get("type").cloned())
                        .is_some_and(|ty| ty == "snapshot");
                    saw_first_after_quiet = is_snapshot;
                    if saw_first_after_quiet {
                        return true;
                    }
                }
                _ => {}
            }
        }
    });

    // Wait out the quiet window (past the keepalive + probe grace).
    tokio::time::sleep(QUIET_WINDOW + Duration::from_secs(12)).await;

    // The connection must still be alive: broadcast a snapshot and expect
    // the client to receive it. If the server had dropped the client at the
    // 30s mark (the bug), the socket would be closed and this send would
    // reach nobody.
    let snapshot = givenergy_local::inverter::model::InverterSnapshot::default();
    let _ = state.tx.send(PollMessage::Snapshot(Box::new(snapshot)));

    let received = tokio::time::timeout(Duration::from_secs(10), driver)
        .await
        .expect("timeout waiting for broadcast after quiet window")
        .expect("driver task panicked");
    assert!(received, "client should still receive broadcasts after idle period");

    drop(config_guard);
}

/// Test-isolation helpers: point the settings/config dir at a unique temp
/// directory so the server never reads the live config.
mod test_config_isolation {
    use std::path::PathBuf;

    pub struct Guard {
        _dir: PathBuf,
        prev: Option<std::ffi::OsString>,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var("GIVENERGY_LOCAL_CONFIG_DIR", v),
                None => std::env::remove_var("GIVENERGY_LOCAL_CONFIG_DIR"),
            }
        }
    }

    pub fn enter() -> Guard {
        let dir = std::env::temp_dir().join(format!("hem-ws-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let prev = std::env::var_os("GIVENERGY_LOCAL_CONFIG_DIR");
        std::env::set_var("GIVENERGY_LOCAL_CONFIG_DIR", &dir);
        Guard { _dir: dir, prev }
    }
}
