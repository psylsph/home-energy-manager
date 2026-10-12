//! WebSocket real-time data stream.
//!
//! Manages WebSocket connections that broadcast live inverter
//! data updates to connected frontend clients.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::ConnectInfo;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::inverter::model::InverterSnapshot;
use crate::inverter::poll::{AppState, PollMessage};

/// Timing knobs for the WebSocket idle probe. Production uses the defaults;
/// tests can shorten both windows and advance Tokio's clock without waiting
/// in real time.
#[derive(Debug, Clone, Copy)]
pub struct KeepaliveConfig {
    pub interval: Duration,
    pub probe_grace: Duration,
}

impl Default for KeepaliveConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(30),
            probe_grace: Duration::from_secs(10),
        }
    }
}

// ---------------------------------------------------------------------------
// Connected clients tracker
// ---------------------------------------------------------------------------

/// Tracks connected WebSocket clients by their peer address.
/// Each client gets a unique incrementing ID.
pub struct ConnectedClients {
    next_id: u64,
    clients: HashMap<u64, SocketAddr>,
}

impl Default for ConnectedClients {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectedClients {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            clients: HashMap::new(),
        }
    }

    /// Register a new client. Returns the assigned ID.
    pub fn add(&mut self, peer: SocketAddr) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.clients.insert(id, peer);
        id
    }

    /// Remove a client by ID.
    pub fn remove(&mut self, id: u64) {
        self.clients.remove(&id);
    }

    /// Get the list of connected client addresses.
    pub fn list(&self) -> Vec<SocketAddr> {
        self.clients.values().copied().collect()
    }

    /// Get the count of connected clients.
    pub fn count(&self) -> usize {
        self.clients.len()
    }
}

/// HTTP upgrade handler for WebSocket connections.
///
/// The client should connect to `ws://<host>:<port>/ws`.
/// On connection the server immediately sends the latest snapshot
/// (if available), then streams all broadcast messages in real time.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    connect_info: ConnectInfo<std::net::SocketAddr>,
) -> impl IntoResponse {
    let peer = connect_info.0;

    // Limit concurrent WebSocket connections to prevent resource exhaustion.
    const MAX_WS_CLIENTS: usize = 32;
    if state.connected_clients.lock().count() >= MAX_WS_CLIENTS {
        tracing::warn!(
            "WebSocket connection from {peer} rejected — {MAX_WS_CLIENTS} clients already connected"
        );
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Too many WebSocket connections",
        )
            .into_response();
    }

    let keepalive = *state.ws_keepalive.lock().await;
    ws.on_upgrade(move |socket| handle_ws(socket, state, peer, keepalive))
        .into_response()
}

/// Probe a quiet WebSocket connection before dropping it (issue #274).
///
/// Returns `true` when the connection is proven alive (any frame arrived
/// within the grace window), `false` when it stayed silent — half-open,
/// safe to disconnect. A failed Ping *send* means the socket is already
/// broken, so it also returns `false`.
async fn probe_alive(
    socket: &mut WebSocket,
    peer: std::net::SocketAddr,
    probe_grace: Duration,
) -> bool {
    tracing::debug!("WebSocket client {peer} quiet for keepalive — probing with Ping");
    if socket.send(Message::Ping(vec![].into())).await.is_err() {
        tracing::debug!("WebSocket client {peer} probe send failed — disconnecting");
        return false;
    }
    tokio::select! {
        msg = socket.recv() => {
            match msg {
                // Pong (or any other frame) proves the connection is live.
                // Close/None/error genuinely ended it.
                Some(Ok(Message::Close(_))) | None => false,
                Some(Ok(_)) => true,
                Some(Err(e)) => {
                    tracing::debug!("WebSocket probe error from {peer}: {e}");
                    false
                }
            }
        }
        _ = tokio::time::sleep(probe_grace) => {
            tracing::debug!(
                "WebSocket client {peer} timed out — no response to probe — disconnecting"
            );
            false
        }
    }
}

/// Why the first frames could not be delivered to a new client.
#[derive(Debug, PartialEq, Eq)]
enum InitialSendError<E> {
    Failed(E),
    TimedOut,
}

/// Send one frame to a freshly connected client, giving up after `limit`.
///
/// A half-open client can park a send until TCP gives up, which would keep
/// `handle_ws` from ever reaching the keepalive loop that evicts dead
/// connections, so the first frames get the same grace as the idle probe.
async fn send_bounded<E, Fut>(limit: Duration, send: Fut) -> Result<(), InitialSendError<E>>
where
    Fut: std::future::Future<Output = Result<(), E>>,
{
    match tokio::time::timeout(limit, send).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(InitialSendError::Failed(e)),
        Err(_) => Err(InitialSendError::TimedOut),
    }
}

/// Serialise an outgoing frame, logging and returning `None` on failure so
/// the caller skips it instead of sending an empty frame.
fn encode_frame<T: serde::Serialize>(msg: &T) -> Option<String> {
    match serde_json::to_string(msg) {
        Ok(text) => Some(text),
        Err(e) => {
            tracing::warn!("Failed to serialise initial WebSocket frame: {}", e);
            None
        }
    }
}

/// Send the latest snapshot (if any) to a freshly connected client.
///
/// The `latest_snapshot` guard is held only long enough to clone the
/// snapshot; serialisation and the send happen after it drops (issue #365).
/// A slow or half-open client can park `send` until TCP gives up, and
/// holding the lock across that would stall the poll loop's publish and
/// every API handler that reads the snapshot. The send itself is bounded by
/// `limit` (see [`send_bounded`]). A snapshot that fails to serialise is
/// logged and skipped rather than sent as an empty frame the client cannot
/// parse; the next broadcast will carry the data.
async fn send_initial_snapshot<E, F, Fut>(
    latest_snapshot: &tokio::sync::Mutex<Option<InverterSnapshot>>,
    limit: Duration,
    send: F,
) -> Result<(), InitialSendError<E>>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<(), E>>,
{
    let snapshot = latest_snapshot.lock().await.clone();
    let Some(snapshot) = snapshot else {
        return Ok(());
    };
    let Some(msg) = encode_frame(&PollMessage::Snapshot(Box::new(snapshot))) else {
        return Ok(());
    };
    send_bounded(limit, send(msg)).await
}

/// Inner WebSocket loop — runs for the lifetime of a single connection.
async fn handle_ws(
    mut socket: WebSocket,
    state: Arc<AppState>,
    peer: std::net::SocketAddr,
    keepalive: KeepaliveConfig,
) {
    // Register this client
    let client_id = state.connected_clients.lock().add(peer);
    tracing::debug!("WebSocket client connected: {}", peer);

    // Subscribe to the broadcast channel *before* sending the initial
    // snapshot so we don't miss any updates between the two operations.
    let mut rx = state.tx.subscribe();

    // Send the current connection state immediately on connect.
    {
        let cs = state.connection_state.lock().await.clone();
        let cs_val = state.connected_since.lock().ok().and_then(|guard| *guard);
        let connected_since_epoch_ms = cs_val
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64);
        let cs_str = serde_json::json!({
            "type": "connection",
            "state": cs,
            "host": state.settings.lock().await.host.clone(),
            "connected_since_epoch_ms": connected_since_epoch_ms,
        });
        let sent = send_bounded(
            keepalive.probe_grace,
            socket.send(Message::Text(cs_str.to_string().into())),
        )
        .await;
        if sent.is_err() {
            state.connected_clients.lock().remove(client_id);
            return;
        }
    }

    // Send the current snapshot immediately on connect (if available).
    let sent = send_initial_snapshot(&state.latest_snapshot, keepalive.probe_grace, |msg| async {
        socket.send(Message::Text(msg.into())).await
    })
    .await;
    if sent.is_err() {
        // Client disconnected immediately, or never took the frame.
        state.connected_clients.lock().remove(client_id);
        return;
    }

    // Detect half-open connections: if the client hasn't sent any message
    // (or pong) for the configured interval, consider the connection dead. Tokio's
    // broadcast recv has no timeout, so we use select! to race it against
    // a sleep or incoming WebSocket frame. Receiving any frame (including
    // Pong from the client's WebSocket stack) proves the connection is
    // still alive. If the client disconnects, `recv().await` returns None.
    loop {
        tokio::select! {
            // Incoming WebSocket message from client (close, pong, or error).
            // Any message proves the connection is alive. None = disconnect.
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {
                        // Pong or any other frame — connection alive, continue.
                    }
                    Some(Err(e)) => {
                        tracing::debug!("WebSocket error from {peer}: {e}");
                        break;
                    }
                }
            }
            // Broadcast message from the poll loop.
            broadcast = rx.recv() => {
                match broadcast {
                    Ok(poll_msg) => {
                        let text = match serde_json::to_string(&poll_msg) {
                            Ok(t) => t,
                            Err(e) => {
                                tracing::warn!("Failed to serialise WebSocket message: {}", e);
                                continue;
                            }
                        };
                        if socket.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                        let notice = serde_json::json!({
                            "type": "lagged",
                            "count": count,
                        });
                        let text = notice.to_string();
                        if socket.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            // Timeout — no broadcast and no client message. The client may
            // simply be quiet (browsers never send unsolicited frames, and
            // snapshot broadcasts may be less frequent than this timeout on
            // slow poll intervals or remote links). RFC 6455 requires every
            // WebSocket stack to answer a Ping with a Pong automatically,
            // so probe before dropping: send a Ping and allow a short
            // grace window for ANY frame. A live client's stack replies
            // even if the page is idle (issue #274); a half-open
            // connection stays silent and is disconnected here.
            _ = tokio::time::sleep(keepalive.interval) => {
                if !probe_alive(&mut socket, peer, keepalive.probe_grace).await {
                    break;
                }
            }
        }
    }

    // Unregister this client
    state.connected_clients.lock().remove(client_id);
    tracing::debug!("WebSocket client disconnected: {}", peer);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, SocketAddr};
    use std::str::FromStr;

    fn peer(a: &str) -> SocketAddr {
        SocketAddr::new(IpAddr::from_str(a).unwrap(), 12345)
    }

    #[test]
    fn new_is_empty() {
        let clients = ConnectedClients::new();
        assert_eq!(clients.count(), 0);
        assert!(clients.list().is_empty());
    }

    #[test]
    fn default_matches_new() {
        let clients = ConnectedClients::default();
        assert_eq!(clients.count(), 0);
    }

    #[test]
    fn add_assigns_monotonic_ids_starting_at_one() {
        let mut clients = ConnectedClients::new();
        let id1 = clients.add(peer("10.0.0.1"));
        let id2 = clients.add(peer("10.0.0.2"));
        let id3 = clients.add(peer("10.0.0.3"));
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
        assert_eq!(clients.count(), 3);
    }

    #[test]
    fn add_stores_peer_address() {
        let mut clients = ConnectedClients::new();
        let p1 = peer("192.168.1.10");
        let p2 = peer("192.168.1.11");
        clients.add(p1);
        clients.add(p2);
        let mut list = clients.list();
        list.sort();
        assert_eq!(list, vec![p1, p2]);
    }

    #[test]
    fn remove_drops_client() {
        let mut clients = ConnectedClients::new();
        let id1 = clients.add(peer("10.0.0.1"));
        let id2 = clients.add(peer("10.0.0.2"));
        clients.remove(id1);
        assert_eq!(clients.count(), 1);
        let mut list = clients.list();
        list.sort();
        assert_eq!(list, vec![peer("10.0.0.2")]);
        // Removing the same id twice is a no-op.
        clients.remove(id1);
        assert_eq!(clients.count(), 1);
        clients.remove(id2);
        assert_eq!(clients.count(), 0);
    }

    #[test]
    fn remove_unknown_id_is_safe() {
        let mut clients = ConnectedClients::new();
        clients.remove(9999);
        assert_eq!(clients.count(), 0);
    }

    #[test]
    fn ids_never_reused_after_removal() {
        // The next_id counter is monotonic; even after a remove, the
        // next add() must return a fresh id. The frontend uses the id
        // only for tracing; this test pins the contract so a future
        // refactor doesn't accidentally reset the counter.
        let mut clients = ConnectedClients::new();
        let id1 = clients.add(peer("10.0.0.1"));
        clients.remove(id1);
        let id2 = clients.add(peer("10.0.0.2"));
        assert_ne!(id1, id2);
        assert!(id2 > id1);
    }

    #[test]
    fn many_clients_does_not_panic() {
        let mut clients = ConnectedClients::new();
        for i in 0..100u8 {
            clients.add(peer(&format!("10.0.0.{i}")));
        }
        assert_eq!(clients.count(), 100);
        assert_eq!(clients.list().len(), 100);
    }

    #[tokio::test]
    async fn initial_snapshot_send_does_not_hold_the_snapshot_lock() {
        // A client whose send never completes (half-open connection) must
        // not stop another task taking the lock. Needs a concurrent task:
        // a sequential test can never observe the guard held across send.
        let latest = Arc::new(tokio::sync::Mutex::new(Some(InverterSnapshot::default())));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let sender = {
            let latest = Arc::clone(&latest);
            tokio::spawn(async move {
                let _ = send_initial_snapshot(&latest, Duration::from_secs(3600), |_msg| async {
                    let _ = entered_tx.send(());
                    std::future::pending::<Result<(), ()>>().await
                })
                .await;
            })
        };
        entered_rx.await.expect("send was never reached");

        let writer = tokio::time::timeout(Duration::from_secs(1), async {
            *latest.lock().await = None;
        })
        .await;
        sender.abort();
        assert!(
            writer.is_ok(),
            "snapshot lock stayed held while the initial send was blocked"
        );
    }

    #[tokio::test]
    async fn initial_snapshot_sends_serialised_snapshot() {
        let latest = tokio::sync::Mutex::new(Some(InverterSnapshot::default()));
        let mut sent = None;
        let res: Result<(), InitialSendError<()>> =
            send_initial_snapshot(&latest, Duration::from_secs(1), |msg| {
                sent = Some(msg);
                async { Ok(()) }
            })
            .await;
        assert!(res.is_ok());
        let v: serde_json::Value = serde_json::from_str(&sent.expect("nothing sent")).unwrap();
        assert_eq!(v["type"], "snapshot");
    }

    #[tokio::test]
    async fn initial_snapshot_skips_send_when_none_available() {
        let latest = tokio::sync::Mutex::new(None);
        let res: Result<(), InitialSendError<()>> =
            send_initial_snapshot(&latest, Duration::from_secs(1), |_msg| async {
                panic!("must not send without a snapshot")
            })
            .await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn initial_snapshot_propagates_send_error() {
        let latest = tokio::sync::Mutex::new(Some(InverterSnapshot::default()));
        let res = send_initial_snapshot(&latest, Duration::from_secs(1), |_msg| async {
            Err("closed")
        })
        .await;
        assert_eq!(res, Err(InitialSendError::Failed("closed")));
    }

    #[tokio::test(start_paused = true)]
    async fn initial_snapshot_send_gives_up_on_a_stalled_client() {
        let latest = tokio::sync::Mutex::new(Some(InverterSnapshot::default()));
        let res = send_initial_snapshot(&latest, Duration::from_secs(10), |_msg| {
            std::future::pending::<Result<(), ()>>()
        })
        .await;
        assert_eq!(res, Err(InitialSendError::TimedOut));
    }

    #[test]
    fn encode_frame_returns_none_when_serialisation_fails() {
        struct Unserialisable;
        impl serde::Serialize for Unserialisable {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("boom"))
            }
        }
        assert_eq!(encode_frame(&Unserialisable), None);
        assert_eq!(encode_frame(&"ok").as_deref(), Some("\"ok\""));
    }

    #[tokio::test(start_paused = true)]
    async fn send_bounded_passes_through_fast_sends() {
        let res: Result<(), InitialSendError<()>> =
            send_bounded(Duration::from_secs(10), async { Ok(()) }).await;
        assert_eq!(res, Ok(()));
    }
}
