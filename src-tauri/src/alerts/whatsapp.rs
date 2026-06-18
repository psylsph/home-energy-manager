//! Uses a rusqlite-backed persistent store so pairing and Signal sessions
//! survive restarts without needing the upstream whatsapp-rust-sqlite-storage
//! crate (which pulls in diesel+r2d2 and conflicts with the project's
//! rusqlite). The database lives at `~/.givenergy-local/whatsapp-store.db`.

use std::sync::Arc;
use tokio::sync::Mutex;

use whatsapp_rust::bot::Bot;
use whatsapp_rust::store::traits::Backend;
use whatsapp_rust::types::events::Event;
use whatsapp_rust::waproto::whatsapp as wa;
use whatsapp_rust::{Client, Jid, TokioRuntime};
use whatsapp_rust_tokio_transport::TokioWebSocketTransportFactory;
use whatsapp_rust_ureq_http_client::UreqHttpClient;

/// Current WhatsApp pairing state, surfaced to the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum PairingState {
    Idle,
    WaitingForScan(String),
    Paired,
    Error(String),
}

/// Shared WhatsApp bot state.
pub struct WhatsAppState {
    pub pairing_state: Arc<Mutex<PairingState>>,
    client: Arc<Mutex<Option<Arc<Client>>>>,
    paired_jid: Arc<Mutex<Option<Jid>>>,
    db_path: Option<std::path::PathBuf>,
}

impl WhatsAppState {
    pub fn new(db_path: Option<std::path::PathBuf>) -> Self {
        Self {
            pairing_state: Arc::new(Mutex::new(PairingState::Idle)),
            client: Arc::new(Mutex::new(None)),
            paired_jid: Arc::new(Mutex::new(None)),
            db_path,
        }
    }

    /// Start the WhatsApp bot. Returns immediately; the bot runs in background.
    ///
    /// The library kills the bot when all QR codes for a session expire
    /// (disconnect sets is_running=false). We wrap the bot in a restart loop
    /// that re-launches it until pairing succeeds.
    pub async fn start(&self) {
        let pairing_state = self.pairing_state.clone();
        let client_slot = self.client.clone();
        let paired_jid = self.paired_jid.clone();
        let db_path = self.db_path.clone();

        tokio::spawn(async move {
            loop {
                if matches!(*pairing_state.lock().await, PairingState::Paired) {
                    break;
                }

                tracing::info!("WhatsApp: starting bot (QR pairing)");

                let backend: Arc<dyn Backend> = if let Some(ref path) = db_path {
                    match crate::alerts::whatsapp_store::SqliteBackend::open(path) {
                        Ok(b) => Arc::new(b),
                        Err(e) => {
                            tracing::warn!("WhatsApp: failed to open store at {path:?}: {e}, falling back to in-memory");
                            Arc::new(wacore::store::InMemoryBackend::new())
                        }
                    }
                } else {
                    Arc::new(wacore::store::InMemoryBackend::new())
                };

                let bot = Bot::builder()
                    .with_backend(backend)
                    .with_transport_factory(TokioWebSocketTransportFactory::new())
                    .with_http_client(UreqHttpClient::new())
                    .with_runtime(TokioRuntime)
                    .on_event({
                        let pairing_state = pairing_state.clone();
                        let paired_jid = paired_jid.clone();
                        let client_slot = client_slot.clone();
                        let db_path = db_path.clone();
                        move |event, client| {
                            let pairing_state = pairing_state.clone();
                            let paired_jid = paired_jid.clone();
                            let client_slot = client_slot.clone();
                            let db_path = db_path.clone();
                            async move {
                                match &*event {
                                    Event::PairingQrCode { code, .. } => {
                                        tracing::info!("WhatsApp: QR code generated");
                                        *pairing_state.lock().await =
                                            PairingState::WaitingForScan(code.clone());
                                    }
                                    Event::Connected(_) => {
                                        tracing::info!("WhatsApp: connected");
                                        *client_slot.lock().await = Some(client.clone());
                                        if let Some(jid) = client.get_pn().await {
                                            tracing::info!("WhatsApp: paired as {jid}");
                                            *paired_jid.lock().await = Some(jid);
                                        }
                                        *pairing_state.lock().await = PairingState::Paired;
                                    }
                                    Event::Disconnected(_) => {
                                        tracing::warn!("WhatsApp: disconnected");
                                        if !matches!(
                                            *pairing_state.lock().await,
                                            PairingState::Paired
                                        ) {
                                            *pairing_state.lock().await = PairingState::Idle;
                                        }
                                    }
                                    Event::LoggedOut(_) => {
                                        tracing::warn!("WhatsApp: logged out");
                                        *pairing_state.lock().await = PairingState::Idle;
                                        *client_slot.lock().await = None;
                                        *paired_jid.lock().await = None;
                                        // Stale session data is invalid — delete
                                        // so next start does a fresh pairing.
                                        if let Some(path) = db_path.as_ref() {
                                            let _ = std::fs::remove_file(path);
                                        }
                                    }
                                    Event::Receipt(receipt) => {
                                        let msg_ids = receipt.message_ids.join(", ");
                                        tracing::warn!(
                                            "WhatsApp: receipt {:?} for message(s) {}",
                                            receipt.r#type,
                                            msg_ids
                                        );
                                    }
                                    _ => {}
                                }
                            }
                        }
                    })
                    .build()
                    .await;

                let mut bot = match bot {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!("WhatsApp bot build failed: {e}");
                        *pairing_state.lock().await =
                            PairingState::Error(format!("Build: {e}"));
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        continue;
                    }
                };

                let bot_handle = match bot.run().await {
                    Ok(h) => h,
                    Err(e) => {
                        tracing::warn!("WhatsApp bot run failed: {e}");
                        *pairing_state.lock().await =
                            PairingState::Error(format!("Run: {e}"));
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        continue;
                    }
                };

                // Block until the bot stops (QR expired, disconnected, etc.)
                let _ = bot_handle.await;

                if matches!(*pairing_state.lock().await, PairingState::Paired) {
                    tracing::info!("WhatsApp: bot stopped after pairing, exiting loop");
                    break;
                }

                tracing::warn!("WhatsApp: bot stopped (QR expired), restarting in 3s...");
                *pairing_state.lock().await = PairingState::Idle;
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        });
    }

    /// Send a message to a recipient phone number.
    ///
    /// `recipient_phone` must be digits only in international format (e.g.
    /// "34684770005"). It is converted to a JID (`{phone}@s.whatsapp.net`).
    /// The recipient must be different from the linked account — WhatsApp does
    /// not reliably deliver messages from a linked device to its own account.
    pub async fn send_message(&self, recipient_phone: &str, text: &str) -> Result<(), String> {
        if recipient_phone.is_empty() {
            return Err("No WhatsApp recipient configured".to_string());
        }

        let client_guard = self.client.lock().await;
        let client = client_guard
            .as_ref()
            .ok_or_else(|| "WhatsApp not connected".to_string())?
            .clone();
        drop(client_guard);

        // Ensure the client is fully ready before attempting to send.
        if let Err(e) = client
            .wait_for_connected(std::time::Duration::from_secs(30))
            .await
        {
            tracing::warn!("WhatsApp: not ready: {e}");
            return Err(format!("Not ready: {e}"));
        }

        let jid: Jid = format!("{recipient_phone}@s.whatsapp.net")
            .parse()
            .map_err(|e| format!("Invalid recipient phone '{recipient_phone}': {e}"))?;

        // Retry loop: the first send may trigger pre-key exchange/device-list
        // sync as a side effect. Subsequent attempts use the established
        // sessions. This handles the "session not found" scenario where
        // the initial sync hasn't completed by the time we first send.
        for attempt in 0..3 {
            if attempt > 0 {
                tracing::warn!("WhatsApp: retry {attempt} sending to {jid}");
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }

            match client
                .send_message(
                    jid.clone(),
                    wa::Message {
                        conversation: Some(text.to_string()),
                        ..Default::default()
                    },
                )
                .await
            {
                Ok(result) => {
                    tracing::warn!(
                        "WhatsApp: message accepted by server (message_id={}, to={})",
                        result.message_id, result.to
                    );
                    // Listen for delivery receipt — the bot's event handler
                    // will log the actual delivery status via Event::Receipt.
                    tracing::info!(
                        "WhatsApp: {jid} <- message queued for delivery (mid={})",
                        result.message_id
                    );
                    return Ok(());
                }
                Err(e) => {
                    if attempt == 2 {
                        tracing::error!("WhatsApp: send failed after 3 attempts: {e}");
                        return Err(format!("Send: {e}"));
                    }
                    tracing::warn!("WhatsApp: send attempt {attempt} failed, retrying: {e}");
                }
            }
        }

        Ok(())
    }
}

impl Default for WhatsAppState {
    fn default() -> Self {
        Self::new(None)
    }
}
