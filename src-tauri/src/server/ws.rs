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
        let _ = socket.send(Message::Text(cs_str.to_string().into())).await;
    }

    // Send the current snapshot immediately on connect (if available).
    if let Some(snapshot) = state.latest_snapshot.lock().await.as_ref() {
        let msg = serde_json::to_string(&PollMessage::Snapshot(Box::new(snapshot.clone())))
            .unwrap_or_default();
        if socket.send(Message::Text(msg.into())).await.is_err() {
            // Client disconnected immediately.
            state.connected_clients.lock().remove(client_id);
            return;
        }
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
}
