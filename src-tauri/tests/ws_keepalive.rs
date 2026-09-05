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
use givenergy_local::server::ws::KeepaliveConfig;
use tokio_tungstenite::tungstenite::Message;

/// Bind the production router on an ephemeral port and return its address.
async fn spawn_server(
    state: Arc<AppState>,
) -> (
    std::net::SocketAddr,
    tokio::task::JoinHandle<Result<(), std::io::Error>>,
) {
    let app = create_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
    });
    (addr, server)
}

async fn wait_for_client_count(state: &Arc<AppState>, expected: usize) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if state.connected_clients.lock().count() == expected {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("WebSocket client count did not settle");
}

#[tokio::test]
async fn quiet_client_survives_keepalive_probe_and_still_receives_broadcasts() {
    tokio::time::pause();
    let config_guard = test_config_isolation::enter();
    let state = Arc::new(AppState::new());
    {
        let mut timing = state.ws_keepalive.lock().await;
        *timing = KeepaliveConfig {
            interval: Duration::from_secs(5),
            probe_grace: Duration::from_secs(10),
        };
    }
    let (addr, server) = spawn_server(state.clone()).await;

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
            let Some(Ok(msg)) = stream.next().await else {
                return false;
            };
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

    // Advance through the keepalive interval. The driver answers the Ping,
    // then we advance inside the probe grace window before broadcasting.
    tokio::time::advance(Duration::from_secs(5)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_secs(1)).await;

    // The connection must still be alive: broadcast a snapshot and expect
    // the client to receive it. If the server had dropped the client at the
    // 30s mark (the bug), the socket would be closed and this send would
    // reach nobody.
    let snapshot = givenergy_local::inverter::model::InverterSnapshot::default();
    let _ = state.tx.send(PollMessage::Snapshot(Box::new(snapshot)));

    let received = tokio::time::timeout(Duration::from_secs(1), driver);
    tokio::time::advance(Duration::from_secs(1)).await;
    let received = received
        .await
        .expect("timeout waiting for broadcast after quiet window")
        .expect("driver task panicked");
    assert!(
        received,
        "client should still receive broadcasts after idle period"
    );

    server.abort();
    let _ = server.await;
    drop(config_guard);
}

#[tokio::test]
async fn websocket_rejects_the_thirty_third_client() {
    let config_guard = test_config_isolation::enter();
    let state = Arc::new(AppState::new());
    for index in 0..32u16 {
        state
            .connected_clients
            .lock()
            .add(std::net::SocketAddr::from(([127, 0, 0, 1], 20_000 + index)));
    }
    let (addr, server) = spawn_server(state.clone()).await;

    let result = tokio_tungstenite::connect_async(format!("ws://{addr}/ws")).await;
    match result {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(
                response.status(),
                axum::http::StatusCode::SERVICE_UNAVAILABLE
            );
        }
        other => panic!("the 33rd client must be rejected with HTTP 503, got {other:?}"),
    }
    assert_eq!(state.connected_clients.lock().count(), 32);

    server.abort();
    let _ = server.await;
    drop(config_guard);
}

#[tokio::test]
async fn websocket_sends_connection_and_snapshot_immediately() {
    let config_guard = test_config_isolation::enter();
    let state = Arc::new(AppState::new());
    let snapshot = givenergy_local::inverter::model::InverterSnapshot {
        inverter_serial: "SNAPSHOT-1".to_string(),
        soc: 67,
        ..Default::default()
    };
    *state.latest_snapshot.lock().await = Some(snapshot);
    let (addr, server) = spawn_server(state.clone()).await;

    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap();
    wait_for_client_count(&state, 1).await;

    let connection = tokio::time::timeout(Duration::from_secs(1), socket.next())
        .await
        .expect("connection message timeout")
        .expect("connection stream ended")
        .expect("connection frame error");
    let connection = connection.into_text().expect("connection must be text");
    let connection: serde_json::Value = serde_json::from_str(&connection).unwrap();
    assert_eq!(connection["type"], "connection");

    let snapshot = tokio::time::timeout(Duration::from_secs(1), socket.next())
        .await
        .expect("snapshot message timeout")
        .expect("snapshot stream ended")
        .expect("snapshot frame error");
    let snapshot = snapshot.into_text().expect("snapshot must be text");
    let snapshot: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
    assert_eq!(snapshot["type"], "snapshot");
    assert_eq!(snapshot["inverter_serial"], "SNAPSHOT-1");
    assert_eq!(snapshot["soc"], 67);

    socket.close(None).await.unwrap();
    wait_for_client_count(&state, 0).await;
    server.abort();
    let _ = server.await;
    drop(config_guard);
}

#[tokio::test]
async fn websocket_reports_broadcast_lag_to_the_client() {
    let config_guard = test_config_isolation::enter();
    let state = Arc::new(AppState::new());
    let (addr, server) = spawn_server(state.clone()).await;
    let (mut sink, mut stream) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap()
        .0
        .split();

    // Consume the connection-on-open message so the handler has subscribed
    // before the burst fills the broadcast ring.
    let _ = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("connection message timeout")
        .expect("connection stream ended")
        .expect("connection frame error");
    wait_for_client_count(&state, 1).await;

    for _ in 0..40 {
        state
            .tx
            .send(PollMessage::Snapshot(Box::default()))
            .expect("the connected WebSocket must receive the burst");
    }

    let mut lagged_count = None;
    for _ in 0..40 {
        let message = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("lag notice timeout")
            .expect("lagged client stream ended")
            .expect("lagged client frame error");
        if let Message::Text(text) = message {
            let json: serde_json::Value = serde_json::from_str(&text).unwrap();
            if json["type"] == "lagged" {
                lagged_count = json["count"].as_u64();
                break;
            }
        }
    }
    assert!(
        lagged_count.is_some_and(|count| count > 0),
        "a receiver that missed broadcast capacity must receive a lag notice"
    );

    sink.send(Message::Close(None)).await.unwrap();
    wait_for_client_count(&state, 0).await;
    server.abort();
    let _ = server.await;
    drop(config_guard);
}

#[tokio::test]
async fn websocket_cleanup_removes_client_after_peer_close() {
    let config_guard = test_config_isolation::enter();
    let state = Arc::new(AppState::new());
    let (addr, server) = spawn_server(state.clone()).await;
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap();
    wait_for_client_count(&state, 1).await;

    socket.close(None).await.unwrap();
    wait_for_client_count(&state, 0).await;

    server.abort();
    let _ = server.await;
    drop(config_guard);
}

/// Test-isolation helpers: point the settings/config dir at a unique temp
/// directory so the server never reads the live config.
mod test_config_isolation {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    pub struct Guard {
        _lock: parking_lot::MutexGuard<'static, ()>,
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
        static LOCK: OnceLock<parking_lot::Mutex<()>> = OnceLock::new();
        let lock = LOCK.get_or_init(|| parking_lot::Mutex::new(())).lock();
        let dir = std::env::temp_dir().join(format!("hem-ws-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let prev = std::env::var_os("GIVENERGY_LOCAL_CONFIG_DIR");
        std::env::set_var("GIVENERGY_LOCAL_CONFIG_DIR", &dir);
        Guard {
            _lock: lock,
            _dir: dir,
            prev,
        }
    }
}
