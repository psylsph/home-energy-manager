//! WebSocket real-time data stream.
//!
//! Manages WebSocket connections that broadcast live inverter
//! data updates to connected frontend clients.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::ConnectInfo;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::inverter::poll::{AppState, PollMessage};

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

    ws.on_upgrade(move |socket| handle_ws(socket, state, peer))
        .into_response()
}

/// Inner WebSocket loop — runs for the lifetime of a single connection.
async fn handle_ws(mut socket: WebSocket, state: Arc<AppState>, peer: std::net::SocketAddr) {
    // Register this client
    let client_id = state.connected_clients.lock().add(peer);
    tracing::debug!("WebSocket client connected: {}", peer);

    // Subscribe to the broadcast channel *before* sending the initial
    // snapshot so we don't miss any updates between the two operations.
    let mut rx = state.tx.subscribe();

    // Send the current connection state immediately on connect.
    {
        let cs = state.connection_state.lock().await.clone();
        let cs_str = serde_json::json!({
            "type": "connection",
            "state": cs,
            "host": state.settings.lock().await.host.clone(),
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
    // (or pong) for 30 seconds, consider the connection dead. Tokio's
    // broadcast recv has no timeout, so we use select! to race it against
    // a sleep or incoming WebSocket frame. Receiving any frame (including
    // Pong from the client's WebSocket stack) proves the connection is
    // still alive. If the client disconnects, `recv().await` returns None.
    let keepalive = tokio::time::Duration::from_secs(30);

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
            // Timeout — no broadcast and no client message. Treat as
            // half-open connection and disconnect.
            _ = tokio::time::sleep(keepalive) => {
                tracing::debug!(
                    "WebSocket client {peer} timed out after {keepalive:?} — disconnecting"
                );
                break;
            }
        }
    }

    // Unregister this client
    state.connected_clients.lock().remove(client_id);
    tracing::debug!("WebSocket client disconnected: {}", peer);
}
