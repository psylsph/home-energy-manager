//! Modbus TCP client.
//!
//! Manages the TCP connection to the inverter, sending requests
//! and reading responses with configurable timeouts and retries.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::TcpStream;
use tokio::sync::{oneshot, Mutex};

use super::framer::{self, DecodedFrame, RegisterType};
use super::registers::{RegisterBlock, STANDARD_POLL_BLOCKS, STANDARD_POLL_BLOCKS_3PH};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum response frame size we are willing to accept (bytes).
const MAX_RESPONSE_SIZE: usize = 4096;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur during Modbus client operations.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("Not connected")]
    NotConnected,

    #[error("Already connected")]
    AlreadyConnected,

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Send failed: {0}")]
    SendFailed(String),

    #[error("Receive failed: {0}")]
    ReceiveFailed(String),

    #[error("Timeout")]
    Timeout,

    #[error("Frame error: {0}")]
    FrameError(String),

    #[error("Invalid response: {0}")]
    InvalidResponse(String),
}

impl ClientError {
    /// Errors that mean the TCP session is no longer usable.
    pub fn is_connection_lost(&self) -> bool {
        matches!(
            self,
            ClientError::NotConnected | ClientError::SendFailed(_) | ClientError::ReceiveFailed(_)
        )
    }
}

// ---------------------------------------------------------------------------
// Block read result
// ---------------------------------------------------------------------------

/// Result of reading a single register block.
#[derive(Debug)]
pub struct BlockRead {
    /// The block descriptor that was read.
    pub block: &'static RegisterBlock,
    /// Raw register values (big-endian u16 words).
    pub data: Vec<u16>,
}

// ---------------------------------------------------------------------------
// Model-aware poll ordering
// ---------------------------------------------------------------------------

fn model_specific_blocks_in_poll_order(
    device_type: &crate::inverter::model::DeviceType,
) -> Vec<&'static RegisterBlock> {
    let mut blocks = Vec::new();

    // Three-phase telemetry/daily counters are dashboard-critical. Read them
    // before optional holding-register config/schedule blocks so an unsupported
    // or slow optional block cannot starve Status-page data for this cycle.
    if device_type.needs_three_phase_input_blocks() {
        blocks.extend(super::registers::THREE_PHASE_INPUT_BLOCKS.iter());
    }

    // Gateway aggregation telemetry (IR 1600-1859) — all live measurements for
    // the Gateway live here; the standard IR 0-59 / IR 1000-1414 ranges are
    // unmapped on the Gateway. Read before optional config blocks.
    if device_type.needs_gateway_input_blocks() {
        blocks.extend(super::registers::GATEWAY_INPUT_BLOCKS.iter());
    }

    blocks.extend(device_type.extra_poll_blocks().iter());
    blocks
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Manages a single TCP connection to a GivEnergy inverter dongle.
/// Content-based key for matching Modbus responses to pending requests.
///
/// Mirrors givenergy-modbus's `shape_hash()` — both requests and responses
/// produce the same key from (slave, function, base_register, count), so the
/// consumer task can route each incoming frame to the correct waiting caller.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub(crate) struct ResponseKey {
    pub(crate) slave: u8,
    pub(crate) function: u8,
    pub(crate) base_register: u16,
    pub(crate) count: u16,
}

impl ResponseKey {
    pub(crate) fn from_request(slave: u8, function: u8, start: u16, count: u16) -> Self {
        Self {
            slave,
            function,
            base_register: start,
            count,
        }
    }

    pub(crate) fn from_response(frame: &DecodedFrame) -> Option<Self> {
        // Read response payload: serial(10) + base_register(2) + count(2) + data(...)
        if frame.payload.len() < 14 {
            return None;
        }
        let base_register = u16::from_be_bytes([frame.payload[10], frame.payload[11]]);
        // For write responses (function 0x06), payload[12..14] is the register
        // value, not a count. Writes always target a single register.
        let count = if (frame.function & 0x7F) == 0x06 {
            1
        } else {
            u16::from_be_bytes([frame.payload[12], frame.payload[13]])
        };
        // Mask off the 0x80 exception bit so exception responses (function |
        // 0x80) match the pending request key created with the bare function
        // code. The exception is detected later in send_and_await_response
        // by checking frame.function >= 0x80 on the decoded frame — which
        // still carries the original (unmasked) function code.
        Some(Self {
            slave: frame.slave,
            function: frame.function & 0x7F,
            base_register,
            count,
        })
    }
}

/// Manages a single TCP connection to a GivEnergy inverter dongle.
pub struct ModbusClient {
    /// Hostname or IP address of the data adapter.
    host: String,
    /// TCP port (typically 8899 for GivEnergy).
    port: u16,
    /// Data adapter serial number (up to 10 Latin-1 characters).
    /// May be empty for auto-discovery — the dongle's serial is extracted
    /// from the first response frame header (bytes 8-17).
    serial: String,
    /// Whether the serial was auto-discovered from a response.
    serial_discovered: bool,
    /// Whether the auto-discovered serial is suspect (extracted from a
    /// truncated/partial frame). When true, the serial should NOT be used
    /// for subsequent requests — some dongle firmware versions stop
    /// responding once the serial is set.
    serial_suspect: bool,
    /// Modbus slave address used for operational inverter reads.
    ///
    /// GivTCP/givenergy-modbus use 0x11 for initial detection/canonical reads,
    /// switching to 0x31 for AC-coupled and Gen1 Hybrid models. Battery BMS
    /// reads still explicitly target 0x32/0x33+ via `read_registers_at_slave`.
    slave: u8,
    /// Write half of the split TCP stream, shared with the consumer task so
    /// it can answer dongle heartbeat requests without a writer of its own.
    /// `None` when disconnected.
    ///
    /// Wrapped in `Arc<Mutex<…>>` because writes to a single TCP stream must
    /// be serialised — the consumer answers heartbeats (~every 3 min) while the
    /// client issues request frames, and concurrent `write_all` calls would
    /// interleave. `tokio::sync::Mutex` is used because `write_all` is awaited
    /// while the lock is held.
    writer: Option<Arc<Mutex<OwnedWriteHalf>>>,
    /// Timeout for individual read/write operations.
    timeout: Duration,
    /// Inter-request delay between consecutive Modbus reads.
    inter_request_delay: Duration,
    /// Pending response futures, keyed by content hash (slave+func+base+count).
    /// The consumer task resolves these when a matching frame arrives.
    pending: Arc<Mutex<HashMap<ResponseKey, oneshot::Sender<DecodedFrame>>>>,
    /// Set to `true` when connected, `false` on disconnect or consumer error.
    connected: Arc<AtomicBool>,
    /// Instant the consumer task last received a complete frame from the
    /// dongle. Drives the inactivity watchdog in the poll loop: a "zombie"
    /// dongle (TCP alive, Modbus hung) keeps the socket open and keepalives
    /// passing but never delivers a frame, so this timestamp goes stale.
    /// `None` until the first frame arrives in a session.
    last_rx_instant: Arc<std::sync::Mutex<Option<std::time::Instant>>>,
    /// Handle to the background consumer task.
    consumer_handle: Option<tokio::task::JoinHandle<()>>,
}

impl ModbusClient {
    /// Create a new client that will connect to `host:port`.
    ///
    /// If `serial` is empty the client will auto-discover the dongle serial
    /// from the first response frame header and use it for all subsequent
    /// requests. This mirrors how GivTCP works — only the IP is needed.
    pub fn new(host: &str, port: u16, serial: &str) -> Self {
        Self {
            host: host.to_string(),
            port,
            serial: serial.to_string(),
            serial_discovered: false,
            serial_suspect: false,
            slave: 0x11, // canonical GivEnergy inverter address for detection
            writer: None,
            timeout: Duration::from_secs(5),
            inter_request_delay: Self::INTER_REQUEST_DELAY_DEFAULT,
            pending: Arc::new(Mutex::new(HashMap::new())),
            connected: Arc::new(AtomicBool::new(false)),
            last_rx_instant: Arc::new(std::sync::Mutex::new(None)),
            consumer_handle: None,
        }
    }

    /// Return the current serial (may be empty if not yet discovered).
    pub fn serial(&self) -> &str {
        &self.serial
    }

    /// Return whether the serial was auto-discovered from a response.
    pub fn serial_was_discovered(&self) -> bool {
        self.serial_discovered
    }

    /// Return whether the auto-discovered serial is suspect (extracted from
    /// a truncated or partial frame). When true, the serial was not set and
    /// empty serial will be used for all requests.
    pub fn serial_is_suspect(&self) -> bool {
        self.serial_suspect
    }

    /// Update the serial (e.g. after auto-discovery).
    pub fn set_serial(&mut self, serial: String) {
        self.serial = serial;
    }

    /// Set the Modbus slave address used for operational inverter reads.
    pub fn set_slave(&mut self, slave: u8) {
        self.slave = slave;
    }

    /// Return the current Modbus slave address used for operational reads.
    pub fn slave_address(&self) -> u8 {
        self.slave
    }

    /// Set the I/O timeout for individual read/write operations.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Set the inter-request delay between consecutive Modbus reads.
    ///
    /// Three-phase models read 15+ blocks per cycle and need a longer delay
    /// (250ms, matching the givenergy-modbus reference library) to avoid
    /// overwhelming the dongle's slow processor. Single-phase models use
    /// the default 150ms.
    pub fn set_inter_request_delay(&mut self, delay: Duration) {
        self.inter_request_delay = delay;
    }

    /// Timeout for a single-register liveness probe — well inside any healthy
    /// dongle's ~50 ms response time, so a silent ("zombie") dongle fails in
    /// one round-trip instead of cascading through the full block-read retry
    /// sequence (~minutes).
    pub const LIVENESS_TIMEOUT: Duration = Duration::from_secs(3);

    /// Cheap single-register read used to confirm the dongle is actually
    /// answering Modbus before committing to a full multi-block poll.
    ///
    /// Holding register 0 is present on every GivEnergy model and is read in
    /// every standard poll, so *any* response — data or a Modbus exception —
    /// proves liveness. A "zombie" dongle (TCP connected, Modbus processor
    /// hung) times out here in ~3 s. Uses exactly one attempt (no retry loop)
    /// and treats a Modbus exception as success (the dongle decoded and
    /// answered the request).
    pub async fn liveness_probe(&mut self) -> Result<(), ClientError> {
        let saved_timeout = self.timeout;
        self.timeout = Self::LIVENESS_TIMEOUT;
        let request =
            framer::build_read_request(&self.serial, self.slave, RegisterType::Holding, 0, 1);
        let key = ResponseKey::from_request(
            self.slave,
            RegisterType::Holding.function_code(),
            0,
            1,
        );
        let result = self.send_and_await_response(request, key).await;
        self.timeout = saved_timeout;
        match result {
            // A decoded read response or even a Modbus exception means the
            // dongle is alive and answering — the probe's only goal.
            Ok(_) | Err(ClientError::InvalidResponse(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Connect to the inverter. Returns `Err(ClientError::AlreadyConnected)` if
    /// a connection is already open.
    pub async fn connect(&mut self) -> Result<(), ClientError> {
        if self.connected.load(Ordering::SeqCst) {
            return Err(ClientError::AlreadyConnected);
        }

        let addr = format!("{}:{}", self.host, self.port);
        tracing::info!("Connecting to {addr} ...",);
        let stream = tokio::time::timeout(self.timeout, TcpStream::connect(&addr))
            .await
            .map_err(|_| ClientError::Timeout)?
            .map_err(|e| ClientError::ConnectionFailed(e.to_string()))?;

        // Set TCP_NODELAY to minimise latency (disable Nagle's algorithm).
        stream
            .set_nodelay(true)
            .map_err(|e| ClientError::ConnectionFailed(e.to_string()))?;

        // Enable TCP keepalive so that dead connections (dongle power-cycled,
        // network change, etc.) are detected within ~15 seconds rather than
        // hanging until the per-read timeout expires.
        let keepalive = socket2::SockRef::from(&stream);
        // 3 probes × 5 s interval after 10 s idle ⇒ a genuinely dead peer
        // (power-cycled, network gone) is detected in ~25 s rather than the
        // OS default of ~10 s + 9×5 s = 55 s. Note: keepalive CANNOT detect
        // a "zombie" dongle (TCP stack alive, Modbus hung) — the application
        // layer watchdog (`last_rx_instant`) handles that case.
        let ka_conf = socket2::TcpKeepalive::new()
            .with_time(std::time::Duration::from_secs(10))
            .with_interval(std::time::Duration::from_secs(5))
            .with_retries(3);
        if let Err(e) = keepalive.set_tcp_keepalive(&ka_conf) {
            tracing::debug!("Failed to set TCP keepalive: {e} (non-fatal)");
        }

        // Log connection details for diagnostics.
        let local = stream
            .local_addr()
            .map(|a| a.to_string())
            .unwrap_or_default();
        let peer = stream
            .peer_addr()
            .map(|a| a.to_string())
            .unwrap_or_default();
        tracing::info!("TCP connected: local={local}, peer={peer}, nodelay=true, keepalive=10s");

        // Split the stream so the consumer reads and the client writes
        // independently without lock contention on the TCP stream. The writer
        // is shared (via Arc<Mutex>) with the consumer task so it can answer
        // dongle heartbeat requests — without a response the dongle closes the
        // socket after 3 missed heartbeats (~9 min).
        let (reader, writer) = stream.into_split();
        let writer = Arc::new(Mutex::new(writer));
        self.writer = Some(writer.clone());

        // Set connected BEFORE spawning the consumer task so it doesn't
        // see the default `false` and exit immediately.
        self.connected.store(true, Ordering::SeqCst);

        let pending = self.pending.clone();
        let connected = self.connected.clone();
        let last_rx = self.last_rx_instant.clone();
        let timeout = self.timeout;
        self.consumer_handle = Some(tokio::spawn(async move {
            Self::consumer_task(reader, writer, pending, connected, last_rx, timeout).await;
        }));

        Ok(())
    }

    /// Disconnect gracefully.
    pub async fn disconnect(&mut self) {
        self.connected.store(false, Ordering::SeqCst);

        // Abort background tasks and await completion so the consumer releases
        // its shared writer clone before we drop our own — guaranteeing the
        // write half is closed and the TCP connection tears down promptly.
        if let Some(h) = self.consumer_handle.take() {
            h.abort();
            // `abort()` cancels the task at its next await point; awaiting the
            // JoinHandle waits for that cancellation to finish so the task's
            // locals (including its writer Arc clone) are dropped first.
            let _ = h.await;
        }

        // Drop our writer clone — the last Arc reference now that the consumer
        // has finished, so the write half closes the TCP connection.
        self.writer = None;
    }

    /// Check if the client is currently connected.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// Time elapsed since the consumer task last received a complete frame
    /// from the dongle, or `None` if no frame has arrived yet this session.
    ///
    /// Used by the poll loop's inactivity watchdog to detect a "zombie"
    /// dongle whose TCP stack is alive (so `connect()` and keepalives
    /// succeed) but whose Modbus application processor has hung (so no
    /// frame ever arrives). Such a connection is invisible to TCP keepalive.
    pub fn last_activity_age(&self) -> Option<Duration> {
        self.last_rx_instant
            .lock()
            .unwrap()
            .as_ref()
            .map(|t| t.elapsed())
    }

    // -----------------------------------------------------------------------
    // Core I/O helpers
    // -----------------------------------------------------------------------

    /// Send a raw frame to the dongle via the TCP writer.
    #[allow(dead_code)]
    async fn send_raw(&mut self, frame: &[u8]) -> Result<(), ClientError> {
        let writer = self.writer.as_ref().ok_or(ClientError::NotConnected)?;
        let mut writer = writer.lock().await;
        tokio::time::timeout(self.timeout, writer.write_all(frame))
            .await
            .map_err(|_| ClientError::Timeout)?
            .map_err(|e| ClientError::SendFailed(e.to_string()))?;
        Ok(())
    }

    /// Background task: reads ALL incoming frames from the TCP stream and
    /// routes them to the correct pending future by content key
    /// (slave + function + base_register + count).
    ///
    /// Responses that don't match any pending future (e.g. battery BMS at
    /// 0x35 responding when the inverter at 0x11 was addressed) are silently
    /// dropped — no caller is waiting for them.
    async fn consumer_task(
        mut reader: tokio::net::tcp::OwnedReadHalf,
        writer: Arc<Mutex<OwnedWriteHalf>>,
        pending: Arc<Mutex<HashMap<ResponseKey, oneshot::Sender<DecodedFrame>>>>,
        connected: Arc<AtomicBool>,
        last_rx: Arc<std::sync::Mutex<Option<std::time::Instant>>>,
        timeout: Duration,
    ) {
        // Read buffer that persists across frames so a stray byte or
        // mis-sized frame doesn't permanently desync the stream.
        let mut read_buf = Vec::new();

        loop {
            if !connected.load(Ordering::SeqCst) {
                break;
            }

            let frame = match Self::read_one_frame(&mut reader, &mut read_buf, timeout).await {
                Ok(f) => f,
                Err(ClientError::Timeout) => {
                    // No data arrived yet — loop and try again.
                    continue;
                }
                Err(e) => {
                    // Non-timeout error (RST, EOF, decode failure): the TCP
                    // session is unusable. Log explicitly so a silently-exited
                    // consumer (which previously broke here with no message)
                    // is visible in diagnostics.
                    tracing::warn!(
                        error = %e,
                        "Modbus reader: connection lost, exiting consumer task"
                    );
                    break;
                }
            };

            // A complete frame arrived — the dongle's Modbus layer is alive.
            // Stamp the inactivity watchdog so the poll loop can distinguish a
            // responsive dongle from a "zombie" (TCP up, Modbus hung).
            *last_rx.lock().unwrap() = Some(std::time::Instant::now());

            // Heartbeat — the dongle sends one (~every 3 min) and closes the
            // socket after 3 unanswered ones (~9 min). Echo the request back so
            // the connection stays alive. Best-effort: a send failure means the
            // socket is already tearing down, so we exit the consumer.
            if framer::is_heartbeat_request(&frame) {
                let response = framer::build_heartbeat_response(&frame);
                let mut w = writer.lock().await;
                if let Err(e) = w.write_all(&response).await {
                    tracing::warn!("Failed to send heartbeat response: {e}");
                    break;
                }
                tracing::trace!("Sent heartbeat response ({} bytes)", response.len());
                continue;
            }

            // Decode and route by content key.
            match framer::decode_frame(&frame) {
                Ok(decoded) => {
                    // Modbus exception responses carry the 0x80 bit in the
                    // function code. Normal responses match via exact content
                    // key (slave + function + base_register + count).
                    // Exception responses have a shorter payload (serial(10)
                    // + error_code(1) = 11 bytes vs ≥14 for normal), so
                    // from_response returns None — route them manually by
                    // matching on (slave, function & 0x7F).
                    if let Some(key) = ResponseKey::from_response(&decoded) {
                        let mut map = pending.lock().await;
                        if let Some(tx) = map.remove(&key) {
                            let _ = tx.send(decoded);
                        }
                        // No matching future -> stale frame, silently dropped.
                    } else if decoded.function >= 0x80 && decoded.payload.len() >= 11 {
                        // Exception frame — scan pending futures for a match
                        // on the masked function code. O(n) per exception,
                        // but exceptions are rare in normal operation.
                        let masked = decoded.function & 0x7F;
                        let mut map = pending.lock().await;
                        // Collect matching keys to avoid borrow issues.
                        let matching: Vec<ResponseKey> = map
                            .keys()
                            .filter(|k| k.slave == decoded.slave && k.function == masked)
                            .cloned()
                            .collect();
                        for key in &matching {
                            if let Some(tx) = map.remove(key) {
                                let _ = tx.send(decoded.clone());
                            }
                        }
                    } else {
                        tracing::debug!(
                            "Consumer: unrouteable frame (function 0x{:02X}, payload {} bytes)",
                            decoded.function,
                            decoded.payload.len()
                        );
                    }
                }
                Err(e) => {
                    tracing::debug!("Consumer: decode error: {e}");
                }
            }
        }

        connected.store(false, Ordering::SeqCst);
    }

    /// Read one complete GivEnergy frame (MBAP header + body) from TCP.
    ///
    /// Uses a persistent read buffer (`buf`) so stray bytes or mis-sized
    /// frames don't permanently desync the stream. Scans for the start marker
    /// `0x59590001`, discards leading garbage, validates the length field,
    /// and detects implausibly-close next-frame markers (a sign the current
    /// candidate frame is corrupt/invalid). Mirrors givenergy-modbus's framer
    /// resync logic.
    async fn read_one_frame(
        reader: &mut tokio::net::tcp::OwnedReadHalf,
        buf: &mut Vec<u8>,
        timeout: Duration,
    ) -> Result<Vec<u8>, ClientError> {
        /// Start-of-frame marker: bytes 0-3 of every GivEnergy frame.
        const HEADER_START: [u8; 4] = [0x59, 0x59, 0x00, 0x01];
        /// Minimum plausible frame size (heartbeat: 6-byte header + 2-byte body).
        const MIN_FRAME_LEN: usize = 8;

        loop {
            // --- Scan buffer for the start marker ---
            if let Some(pos) = buf.windows(4).position(|w| w == HEADER_START) {
                if pos > 0 {
                    // Garbage before the marker — discard it.
                    tracing::debug!(
                        "Discarding {} bytes of leading garbage before frame header",
                        pos
                    );
                    buf.drain(..pos);
                }
                // marker at buf[0] (or after drain) — good, proceed
            } else {
                // No marker found. Keep only HEADER_START.len()-1 bytes so a
                // split marker across reads can be recovered (the framer's
                // sliding-window approach). Discard the rest.
                let keep = HEADER_START.len() - 1;
                if buf.len() > keep {
                    let discarded = buf.len() - keep;
                    if discarded > 100 {
                        let prefix_hex = buf[..buf.len().min(16)]
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect::<Vec<_>>()
                            .join(" ");
                        tracing::debug!(
                                "No frame header in {} byte buffer, discarding {} bytes (first bytes: {})",
                                buf.len(),
                                discarded,
                                prefix_hex
                            );
                    } else {
                        tracing::debug!(
                            "No frame header in {} byte buffer, discarding {} bytes",
                            buf.len(),
                            discarded
                        );
                    }
                    buf.drain(..buf.len() - keep);
                }
                // Read more data from the stream.
                Self::fill_read_buf(reader, buf, timeout).await?;
                continue;
            }

            // --- We have the marker at buf[0..4]. Enough for the header? ---
            if buf.len() < 6 {
                Self::fill_read_buf(reader, buf, timeout).await?;
                continue;
            }

            // Read the length from bytes 4-5 and validate.
            let length = u16::from_be_bytes([buf[4], buf[5]]) as usize;
            let total_frame_len = 6 + length;

            if !(2..=MAX_RESPONSE_SIZE).contains(&length) {
                // Bogus length — the current marker is probably a false
                // positive (stray 0x59590001 bytes in garbage). Discard the
                // first byte and re-scan.
                tracing::debug!(
                    "Implausible frame length {length} (max {MAX_RESPONSE_SIZE}) — advancing past false marker"
                );
                buf.drain(..1);
                continue;
            }

            // --- Implausibly-close next-frame check (corruption guard) ---
            // If there's another marker within < total_frame_len (and < 18
            // bytes from the first), the current frame is almost certainly
            // corrupt. Skip forward to the next marker.
            if let Some(next) = buf[1..].windows(4).position(|w| w == HEADER_START) {
                let next_offset = next + 1;
                if next_offset < total_frame_len && next_offset < MIN_FRAME_LEN.max(18) {
                    tracing::warn!(
                        "Next frame marker only {} bytes in — current frame likely corrupt, skipping ahead",
                        next_offset
                    );
                    buf.drain(..next_offset);
                    continue;
                }
            }

            // --- Do we have the complete frame? ---
            if buf.len() < total_frame_len {
                Self::fill_read_buf_demand(reader, buf, total_frame_len, timeout).await?;
                continue;
            }

            // --- Extract the frame ---
            let frame = buf[..total_frame_len].to_vec();
            buf.drain(..total_frame_len);
            return Ok(frame);
        }
    }

    /// Fill the read buffer with up to 4 KiB of new data from the TCP stream.
    async fn fill_read_buf(
        reader: &mut tokio::net::tcp::OwnedReadHalf,
        buf: &mut Vec<u8>,
        timeout: Duration,
    ) -> Result<(), ClientError> {
        let start = buf.len();
        let chunk = 4096;
        buf.resize(start + chunk, 0);
        let n = tokio::time::timeout(timeout, reader.read(&mut buf[start..]))
            .await
            .map_err(|_| ClientError::Timeout)?
            .map_err(|e| ClientError::ReceiveFailed(format!("TCP read: {e}")))?;
        if n == 0 {
            return Err(ClientError::NotConnected);
        }
        tracing::trace!("Modbus reader: received {n} bytes");
        buf.truncate(start + n);
        Ok(())
    }

    /// Fill the read buffer until it contains at least `needed` bytes.
    async fn fill_read_buf_demand(
        reader: &mut tokio::net::tcp::OwnedReadHalf,
        buf: &mut Vec<u8>,
        needed: usize,
        timeout: Duration,
    ) -> Result<(), ClientError> {
        let start = buf.len();
        if start >= needed {
            return Ok(());
        }
        let missing = needed - start;
        buf.resize(needed, 0);
        let mut read = 0usize;
        while read < missing {
            let n = tokio::time::timeout(timeout, reader.read(&mut buf[start + read..]))
                .await
                .map_err(|_| ClientError::Timeout)?
                .map_err(|e| ClientError::ReceiveFailed(format!("TCP read: {e}")))?;
            if n == 0 {
                return Err(ClientError::NotConnected);
            }
            read += n;
        }
        buf.truncate(start + read);
        Ok(())
    }

    /// Send a frame via the TCP writer and wait for a matching response via
    /// the consumer task's pending futures.
    async fn send_and_await_response(
        &mut self,
        frame: Vec<u8>,
        key: ResponseKey,
    ) -> Result<DecodedFrame, ClientError> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(key.clone(), tx);

        // Send the frame via the TCP writer.
        let writer = self.writer.as_ref().ok_or(ClientError::NotConnected)?;
        let mut writer = writer.lock().await;
        let send_result = tokio::time::timeout(self.timeout, writer.write_all(&frame)).await;
        if let Err(e) = send_result
            .map_err(|_| ClientError::Timeout)
            .and_then(|r| r.map_err(|e| ClientError::SendFailed(e.to_string())))
        {
            drop(writer);
            self.pending.lock().await.remove(&key);
            return Err(e);
        }
        drop(writer); // release the writer lock before awaiting the response

        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(frame)) => {
                if frame.function >= 0x80 {
                    // GivEnergy transparent protocol embeds the 10-byte
                    // inverter serial in ALL inner payloads, including
                    // exception responses. The real exception code follows
                    // the serial at byte offset 10. Fall back to byte 0
                    // for frames that omit the serial (e.g. test mocks).
                    let code = frame
                        .payload
                        .get(10)
                        .copied()
                        .or_else(|| frame.payload.first().copied())
                        .unwrap_or(0);
                    return Err(ClientError::InvalidResponse(format!(
                        "Modbus exception: function 0x{:02X}, code {}",
                        frame.function, code
                    )));
                }
                Ok(frame)
            }
            Ok(Err(_)) => {
                self.pending.lock().await.remove(&key);
                Err(ClientError::NotConnected)
            }
            Err(_) => {
                self.pending.lock().await.remove(&key);
                Err(ClientError::Timeout)
            }
        }
    }

    // -----------------------------------------------------------------------
    // Register operations
    // -----------------------------------------------------------------------

    /// Maximum number of registers to request in a single Modbus read.
    /// The GivEnergy WiFi/Ethernet dongle has a limited frame buffer and will
    /// return fewer registers than requested if this is exceeded.
    const MAX_REGISTERS_PER_READ: u16 = 60;

    /// Inter-request delay to avoid overwhelming the GivEnergy dongle.
    /// The dongle has a very slow processor and limited frame buffer.
    /// The givenergy-modbus reference library uses 250ms; we default to
    /// 150ms for single-phase models and increase to 250ms for three-phase
    /// (which reads 15+ blocks per cycle).
    pub const INTER_REQUEST_DELAY_DEFAULT: Duration = Duration::from_millis(150);
    /// Inter-request delay for three-phase models (more blocks per cycle).
    pub const INTER_REQUEST_DELAY_3PH: Duration = Duration::from_millis(250);

    /// Read a block of registers (input or holding).
    ///
    /// If `count` exceeds [`MAX_REGISTERS_PER_READ`], the read is split into
    /// multiple sub-requests and the results are concatenated.
    pub async fn read_registers(
        &mut self,
        register_type: RegisterType,
        start: u16,
        count: u16,
    ) -> Result<Vec<u16>, ClientError> {
        let mut all_values = Vec::with_capacity(count as usize);
        let mut offset: u16 = 0;

        while offset < count {
            let remaining = count - offset;
            let chunk_size = remaining.min(Self::MAX_REGISTERS_PER_READ);
            let chunk_start = start + offset;

            // Pause between chunks to let the dongle catch up
            if offset > 0 {
                tokio::time::sleep(self.inter_request_delay).await;
            }

            let chunk_values = self
                .read_registers_raw(register_type, chunk_start, chunk_size)
                .await?;
            all_values.extend_from_slice(&chunk_values);

            // If the dongle returned fewer registers than requested, pad with zeros
            let returned = chunk_values.len() as u16;
            if returned < chunk_size {
                tracing::debug!(
                    "Partial read at {}+{}: got {}/{} registers, padding with zeros",
                    start,
                    offset,
                    returned,
                    chunk_size
                );
                all_values.resize(all_values.len() + (chunk_size - returned) as usize, 0);
            }

            offset += chunk_size;
        }

        // Truncate to the originally requested count (safety)
        all_values.truncate(count as usize);
        Ok(all_values)
    }

    /// Probe a single register block at a specific slave address with a
    /// short timeout and no retries.
    ///
    /// Designed for meter/CT discovery where the probed slave likely doesn't
    /// exist — a single attempt with a tight timeout avoids blocking the
    /// startup sequence for tens of seconds per non-existent address.
    ///
    /// Temporarily overrides `self.timeout` for the duration of the call,
    /// then restores it. Uses exactly one attempt (no stale-retry loop).
    pub async fn probe_registers_at_slave(
        &mut self,
        slave: u8,
        register_type: RegisterType,
        start: u16,
        count: u16,
        probe_timeout: Duration,
    ) -> Result<Vec<u16>, ClientError> {
        let original_slave = self.slave;
        let original_timeout = self.timeout;
        self.slave = slave;
        self.timeout = probe_timeout;

        let request =
            framer::build_read_request(&self.serial, self.slave, register_type, start, count);
        let expected_fc = register_type.function_code();
        let key = ResponseKey::from_request(self.slave, expected_fc, start, count);

        let result = match self.send_and_await_response(request, key).await {
            Ok(decoded) => Self::parse_register_response(&decoded, count),
            Err(e) => Err(e),
        };

        self.slave = original_slave;
        self.timeout = original_timeout;
        result
    }

    /// Read registers at a specific slave address, without mutating `self.slave`.
    ///
    /// Used for battery BMS probing where we need to address different
    /// slave addresses (0x32–0x37) without affecting the model-specific slave
    /// used by the main poll cycle.
    pub async fn read_registers_at_slave(
        &mut self,
        slave: u8,
        register_type: RegisterType,
        start: u16,
        count: u16,
    ) -> Result<Vec<u16>, ClientError> {
        let original = self.slave;
        self.slave = slave;
        let result = self.read_registers(register_type, start, count).await;
        self.slave = original;
        result
    }

    /// Maximum number of retries when a stale response is received.
    const MAX_STALE_RETRIES: u8 = 4;

    /// Internal: read a single chunk of registers (no splitting).
    ///
    /// Tolerates the dongle returning fewer registers than requested — common
    /// on GivEnergy WiFi/Ethernet dongles with limited frame buffers.
    ///
    /// Also tolerates stale responses from the dongle — if the function code
    /// doesn't match, the response is from a previous request that arrived
    /// late. In that case we discard it and retry.
    async fn read_registers_raw(
        &mut self,
        register_type: RegisterType,
        start: u16,
        count: u16,
    ) -> Result<Vec<u16>, ClientError> {
        let expected_fc = register_type.function_code();

        for attempt in 0..=Self::MAX_STALE_RETRIES {
            let request =
                framer::build_read_request(&self.serial, self.slave, register_type, start, count);
            let key = ResponseKey::from_request(self.slave, expected_fc, start, count);

            if attempt == 0 {
                tracing::debug!(
                    "Reading {} {}..{} ({} regs) from slave 0x{:02X}",
                    if matches!(register_type, RegisterType::Input) {
                        "IR"
                    } else {
                        "HR"
                    },
                    start,
                    start + count - 1,
                    count,
                    self.slave,
                );
            }

            match self.send_and_await_response(request, key).await {
                Ok(decoded) => {
                    return Self::parse_register_response(&decoded, count);
                }
                Err(ClientError::Timeout) => {
                    // A pure timeout (no bytes at all within the generous
                    // per-request window). Distinguish two cases:
                    //
                    // 1. Stale frame in buffer: the consumer received a
                    //    wrong-key frame (updating `last_rx_instant`) but
                    //    our real response hasn't arrived yet. Retrying
                    //    gives the dongle a chance to send the correct
                    //    response. Retry up to MAX_STALE_RETRIES times.
                    //
                    // 2. Zombie dongle: no bytes at all (last_rx_instant
                    //    is stale or None). Re-sending identical requests
                    //    only multiplies the wait — fail fast so the poll
                    //    loop's connection-lost / inactivity-watchdog /
                    //    back-off logic can take over.
                    let recently_active = self
                        .last_activity_age()
                        .is_some_and(|age| age < Duration::from_secs(10));
                    if recently_active && attempt < Self::MAX_STALE_RETRIES {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        continue;
                    }
                    return Err(ClientError::Timeout);
                }
                Err(ClientError::InvalidResponse(msg))
                    if attempt < Self::MAX_STALE_RETRIES && msg.contains("code 67") =>
                {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
                err @ Err(ClientError::NotConnected) | err @ Err(ClientError::SendFailed(_)) => {
                    // Dead connection — fail fast instead of retrying 4× with 500ms delays.
                    return Err(match err {
                        Err(e) => e,
                        _ => unreachable!(),
                    });
                }
                Err(_) if attempt < Self::MAX_STALE_RETRIES => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }

        Err(ClientError::InvalidResponse(
            "exhausted retries".to_string(),
        ))
    }

    /// Extract the base register and returned register count from a decoded
    /// read response frame.
    fn response_register_metadata(decoded: &DecodedFrame) -> Result<(u16, u16), ClientError> {
        let payload = &decoded.payload;
        const INVERTER_SERIAL_LEN: usize = 10;
        const MIN_INNER_LEN: usize = INVERTER_SERIAL_LEN + 4;

        if payload.len() < MIN_INNER_LEN {
            return Err(ClientError::InvalidResponse(format!(
                "response payload too short: need at least {} bytes for inner header, got {}",
                MIN_INNER_LEN,
                payload.len()
            )));
        }

        let inner = &payload[INVERTER_SERIAL_LEN..];
        let resp_base_register = u16::from_be_bytes([inner[0], inner[1]]);
        let resp_register_count = u16::from_be_bytes([inner[2], inner[3]]);
        Ok((resp_base_register, resp_register_count))
    }

    /// Parse register values from a decoded read response frame.
    fn parse_register_response(
        decoded: &DecodedFrame,
        count: u16,
    ) -> Result<Vec<u16>, ClientError> {
        let payload = &decoded.payload;
        let (_, resp_register_count) = Self::response_register_metadata(decoded)?;
        const INVERTER_SERIAL_LEN: usize = 10;

        let inner = &payload[INVERTER_SERIAL_LEN..];
        let resp_register_count = resp_register_count as usize;

        let reg_data = &inner[4..];
        let max_values = count as usize;
        let actual_count = resp_register_count.min(max_values).min(reg_data.len() / 2);

        let mut values = Vec::with_capacity(actual_count);
        for chunk in reg_data.chunks_exact(2).take(actual_count) {
            values.push(u16::from_be_bytes([chunk[0], chunk[1]]));
        }

        Ok(values)
    }

    /// Write a single holding register (transparent function code 6).
    ///
    /// Per the givenergy-modbus reference library, writes use:
    ///   - Transparent function code **6** (Write Single Register), NOT 0x10
    ///   - Device address **0x11** (inverter setup address), NOT 0x32
    ///   - One register per request (no batching)
    ///
    /// The CRC/check field is the normal transparent-frame CRC over
    /// `device_address + function_code + register + value`, appended by
    /// [`framer::encode_frame`]. Do not append a second write-specific CRC:
    /// real dongles silently ignore those malformed 36-byte write frames.
    ///
    /// Handles stale read responses and dongle-busy exceptions (code 67)
    /// with automatic retries.
    pub async fn write_register(&mut self, register: u16, value: u16) -> Result<(), ClientError> {
        let inner_function: u8 = 6;
        let request = Self::build_write_register_request(&self.serial, register, value);
        let key = ResponseKey::from_request(0x11, inner_function, register, 1);

        let max_attempts: u8 = 6;

        for attempt in 0..max_attempts {
            match self
                .send_and_await_response(request.clone(), key.clone())
                .await
            {
                Ok(decoded) => {
                    if decoded.payload.len() < 14 {
                        return Err(ClientError::InvalidResponse(format!(
                            "write response payload too short: need at least 14 bytes, got {}",
                            decoded.payload.len()
                        )));
                    }
                    let inner = &decoded.payload[10..];
                    let resp_register = u16::from_be_bytes([inner[0], inner[1]]);
                    let _resp_value = u16::from_be_bytes([inner[2], inner[3]]);
                    if resp_register != register {
                        if attempt + 1 < max_attempts {
                            tracing::debug!(
                                "Write at {register} got stale ack (reg {resp_register}), retrying"
                            );
                            tokio::time::sleep(Duration::from_millis(500)).await;
                            continue;
                        }
                        return Err(ClientError::InvalidResponse(format!(
                            "write acknowledgment mismatch: register {} vs {}",
                            resp_register, register
                        )));
                    }
                    tracing::debug!("Write ack: register {register} = {value} (0x{value:04X})");
                    return Ok(());
                }
                Err(ClientError::InvalidResponse(msg)) if msg.contains("code 67") => {
                    if attempt + 1 < max_attempts {
                        tracing::debug!(
                            "Write at {register} got exception 67 (busy), retrying ({}/{})",
                            attempt + 1,
                            max_attempts
                        );
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        continue;
                    }
                    tracing::warn!(
                        "Write at {register} got exception 67 after {max_attempts} retries — treating as acknowledged"
                    );
                    return Ok(());
                }
                Err(ClientError::Timeout) if attempt + 1 < max_attempts => {
                    tracing::debug!(
                        "Write at {register} timed out, retrying ({}/{})",
                        attempt + 1,
                        max_attempts
                    );
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }

        Err(ClientError::InvalidResponse(
            "exhausted write retries".to_string(),
        ))
    }

    /// Build a single-register write request frame.
    ///
    /// The payload passed to `encode_frame` is only register + value. The
    /// transparent-protocol CRC/check is appended by `encode_frame` over the
    /// full inner PDU (`0x11 0x06 register value`), matching givenergy-modbus.
    fn build_write_register_request(serial: &str, register: u16, value: u16) -> Vec<u8> {
        let mut payload = Vec::with_capacity(4);
        payload.extend_from_slice(&register.to_be_bytes());
        payload.extend_from_slice(&value.to_be_bytes());
        framer::encode_frame(serial, 0x11, 0x06, &payload)
    }

    /// Read a set of poll blocks, returning raw data per block.
    ///
    /// Iterates over the provided blocks and issues a read request for each
    /// one. If any block fails the entire operation fails.
    pub async fn read_blocks(
        &mut self,
        blocks: &'static [RegisterBlock],
    ) -> Result<Vec<BlockRead>, ClientError> {
        let mut results = Vec::with_capacity(blocks.len());

        for (i, block) in blocks.iter().enumerate() {
            // Pause between blocks to let the dongle catch up
            if i > 0 {
                tokio::time::sleep(self.inter_request_delay).await;
            }

            let reg_type = match block.register_type {
                super::registers::RegisterType::Input => RegisterType::Input,
                super::registers::RegisterType::Holding => RegisterType::Holding,
            };

            let t0 = std::time::Instant::now();
            match self
                .read_registers(reg_type, block.start, block.count)
                .await
            {
                Ok(data) => {
                    tracing::debug!(
                        block = block.name,
                        start = block.start,
                        count = block.count,
                        received = data.len(),
                        elapsed_ms = t0.elapsed().as_millis() as u64,
                        "Block read OK"
                    );
                    results.push(BlockRead { block, data });
                }
                Err(e) => {
                    tracing::debug!(
                        block = block.name,
                        start = block.start,
                        count = block.count,
                        elapsed_ms = t0.elapsed().as_millis() as u64,
                        error = %e,
                        "Block read FAILED"
                    );
                    return Err(e);
                }
            }
        }

        Ok(results)
    }

    /// Read all standard poll blocks, returning raw data per block.
    ///
    /// Iterates over [`STANDARD_POLL_BLOCKS`] and issues a read request for
    /// each one. If any block fails the entire operation fails.
    pub async fn read_all_standard(&mut self) -> Result<Vec<BlockRead>, ClientError> {
        self.read_blocks(STANDARD_POLL_BLOCKS).await
    }

    /// Read standard blocks plus any model-specific extended blocks.
    ///
    /// If `device_type` is provided, reads the extra blocks appropriate for
    /// that inverter model (e.g. HR 240-299 for Gen3, HR 300-359 for AC).
    /// If extra blocks fail, they're silently skipped — the standard blocks
    /// are still returned. This is because extended blocks may not be
    /// supported by all firmware versions of a given model.
    ///
    /// When `minimal_telemetry` is true, the optional model-specific blocks
    /// (extended slots, AC config, three-phase config, meter/gateway input
    /// banks) are skipped entirely. This trades some UI detail (slots 3-10,
    /// AC limits) for reduced per-cycle timeout exposure on chronically
    /// unstable dongles. Standard blocks (always needed) are always read.
    pub async fn read_all_with_extras(
        &mut self,
        device_type: Option<&crate::inverter::model::DeviceType>,
        minimal_telemetry: bool,
    ) -> Result<Vec<BlockRead>, ClientError> {
        // Three-phase models read all real-time telemetry from the
        // IR(1000-1414) range, making input_0_59 and input_180_181
        // redundant. The Gateway likewise reads all telemetry from its own
        // IR(1600-1859) aggregation bank. Both use the lean HR-only standard
        // set to save ~300 ms per cycle and reduce timeout exposure. On the
        // first poll (device_type is None) the full STANDARD_POLL_BLOCKS set
        // is used so that model detection (from HR(0)) can proceed.
        let standard_blocks = if device_type.is_some_and(|dt| {
            dt.needs_three_phase_input_blocks() || dt.needs_gateway_input_blocks()
        }) {
            STANDARD_POLL_BLOCKS_3PH
        } else {
            STANDARD_POLL_BLOCKS
        };
        let mut results = self.read_blocks(standard_blocks).await?;

        if let Some(dt) = device_type {
            if minimal_telemetry {
                tracing::debug!(
                    "Minimal telemetry mode - skipping optional model-specific blocks"
                );
            } else {
                for block in model_specific_blocks_in_poll_order(dt) {
                    // Pause between blocks to let the dongle catch up.
                    tokio::time::sleep(self.inter_request_delay).await;

                    let reg_type = match block.register_type {
                        super::registers::RegisterType::Input => RegisterType::Input,
                        super::registers::RegisterType::Holding => RegisterType::Holding,
                    };

                    let t0 = std::time::Instant::now();
                    match self
                        .read_registers(reg_type, block.start, block.count)
                        .await
                    {
                        Ok(data) => {
                            tracing::debug!(
                                block = block.name,
                                start = block.start,
                                count = block.count,
                                received = data.len(),
                                elapsed_ms = t0.elapsed().as_millis() as u64,
                                "Model-specific block read OK"
                            );
                            results.push(BlockRead { block, data });
                        }
                        Err(e) => {
                            tracing::debug!(
                                block = block.name,
                                error = %e,
                                "Model-specific block read skipped (non-fatal)"
                            );
                            // Continue — model-specific blocks are optional.
                        }
                    }
                }
            }
        }

        Ok(results)
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    #![allow(dead_code)]
    use super::super::framer::HEADER_SIZE;
    use super::*;

    // -----------------------------------------------------------------------
    // Parsing register data from raw payload bytes
    // -----------------------------------------------------------------------

    /// Helper to simulate the payload-decode logic that `read_registers`
    /// performs: given a byte-count byte followed by big-endian u16 pairs,
    /// extract the register values.
    fn parse_read_payload(payload: &[u8], expected_count: u16) -> Result<Vec<u16>, ClientError> {
        if payload.is_empty() {
            return Err(ClientError::InvalidResponse(
                "read response payload is empty".to_string(),
            ));
        }

        let byte_count = payload[0] as usize;
        let expected_bytes = expected_count as usize * 2;

        if byte_count != expected_bytes {
            return Err(ClientError::InvalidResponse(format!(
                "byte count mismatch: header says {}, expected {}",
                byte_count, expected_bytes
            )));
        }

        if payload.len() < 1 + expected_bytes {
            return Err(ClientError::InvalidResponse(format!(
                "payload too short: got {} bytes, need {}",
                payload.len(),
                1 + expected_bytes
            )));
        }

        let data_bytes = &payload[1..=expected_bytes];
        let mut values = Vec::with_capacity(expected_count as usize);
        for chunk in data_bytes.chunks_exact(2) {
            values.push(u16::from_be_bytes([chunk[0], chunk[1]]));
        }

        Ok(values)
    }

    #[test]
    fn parse_single_register() {
        // byte_count=2, one register: 0x1234
        let payload = vec![0x02, 0x12, 0x34];
        let values = parse_read_payload(&payload, 1).unwrap();
        assert_eq!(values, vec![0x1234]);
    }

    #[test]
    fn parse_multiple_registers() {
        // byte_count=4, two registers: 0xABCD, 0xEF01
        let payload = vec![0x04, 0xAB, 0xCD, 0xEF, 0x01];
        let values = parse_read_payload(&payload, 2).unwrap();
        assert_eq!(values, vec![0xABCD, 0xEF01]);
    }

    #[test]
    fn parse_zero_registers() {
        let payload = vec![0x00];
        let values = parse_read_payload(&payload, 0).unwrap();
        assert!(values.is_empty());
    }

    fn decoded_read_response(slave: u8, function: u8, base: u16, count: u16) -> DecodedFrame {
        let mut payload = Vec::new();
        payload.extend_from_slice(b"INV1234567");
        payload.extend_from_slice(&base.to_be_bytes());
        payload.extend_from_slice(&count.to_be_bytes());
        for i in 0..count {
            payload.extend_from_slice(&(base + i).to_be_bytes());
        }
        DecodedFrame {
            slave,
            function,
            payload,
        }
    }

    #[test]
    fn response_register_metadata_reads_base_and_count() {
        let decoded = decoded_read_response(0x32, 0x03, 80, 20);
        let metadata = ModbusClient::response_register_metadata(&decoded).unwrap();
        assert_eq!(metadata, (80, 20));
    }

    #[test]
    fn write_register_request_has_single_transparent_crc() {
        let frame = ModbusClient::build_write_register_request("WG2301G167", 27, 1);

        // 6-byte MBAP + 28-byte transparent payload. The earlier malformed
        // implementation appended an extra write-specific CRC and produced a
        // 36-byte frame with MBAP len 30, which real dongles ignored.
        assert_eq!(frame.len(), 34);
        assert_eq!(u16::from_be_bytes([frame[4], frame[5]]), 28);
        assert_eq!(&frame[26..32], &[0x11, 0x06, 0x00, 0x1B, 0x00, 0x01]);

        let expected_crc = framer::crc16_modbus(&frame[26..32]);
        assert_eq!(&frame[32..34], &expected_crc.to_le_bytes());

        let decoded = framer::decode_frame(&frame).unwrap();
        assert_eq!(decoded.slave, 0x11);
        assert_eq!(decoded.function, 0x06);
        assert_eq!(decoded.payload, vec![0x00, 0x1B, 0x00, 0x01]);
    }

    #[test]
    fn parse_register_response_uses_returned_count() {
        let decoded = decoded_read_response(0x32, 0x03, 80, 2);
        let values = ModbusClient::parse_register_response(&decoded, 20).unwrap();
        assert_eq!(values, vec![80, 81]);
    }

    #[test]
    fn parse_sixty_registers() {
        let mut payload = vec![120u8]; // byte count = 60 * 2
        for i in 0u16..60 {
            payload.extend_from_slice(&i.to_be_bytes());
        }
        let values = parse_read_payload(&payload, 60).unwrap();
        assert_eq!(values.len(), 60);
        for (i, &v) in values.iter().enumerate() {
            assert_eq!(v, i as u16);
        }
    }

    #[test]
    fn parse_empty_payload_is_error() {
        let result = parse_read_payload(&[], 1);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ClientError::InvalidResponse(_)));
    }

    #[test]
    fn parse_wrong_byte_count_is_error() {
        // byte_count says 4 but we expect 1 register (2 bytes)
        let payload = vec![0x04, 0x12, 0x34];
        let result = parse_read_payload(&payload, 1);
        assert!(result.is_err());
    }

    #[test]
    fn parse_truncated_payload_is_error() {
        // byte_count says 4 but only 3 bytes follow
        let payload = vec![0x04, 0x12, 0x34];
        let result = parse_read_payload(&payload, 2);
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // Error variant construction
    // -----------------------------------------------------------------------

    #[test]
    fn error_variants_construct_and_format() {
        let e = ClientError::NotConnected;
        assert_eq!(format!("{e}"), "Not connected");

        let e = ClientError::AlreadyConnected;
        assert_eq!(format!("{e}"), "Already connected");

        let e = ClientError::ConnectionFailed("refused".to_string());
        assert!(format!("{e}").contains("refused"));

        let e = ClientError::SendFailed("broken pipe".to_string());
        assert!(format!("{e}").contains("broken pipe"));

        let e = ClientError::ReceiveFailed("eof".to_string());
        assert!(format!("{e}").contains("eof"));

        let e = ClientError::Timeout;
        assert_eq!(format!("{e}"), "Timeout");

        let e = ClientError::FrameError("bad crc".to_string());
        assert!(format!("{e}").contains("bad crc"));

        let e = ClientError::InvalidResponse("unexpected".to_string());
        assert!(format!("{e}").contains("unexpected"));
    }

    // -----------------------------------------------------------------------
    // Client construction & state
    // -----------------------------------------------------------------------

    #[test]
    fn new_client_is_disconnected() {
        let client = ModbusClient::new("192.168.1.100", 8899, "SA12345678");
        assert!(!client.is_connected());
        assert_eq!(client.host, "192.168.1.100");
        assert_eq!(client.port, 8899);
        assert_eq!(client.serial, "SA12345678");
    }

    #[test]
    fn set_slave_changes_address() {
        let mut client = ModbusClient::new("10.0.0.1", 8899, "SN0001");
        assert_eq!(client.slave, 0x11);
        assert_eq!(client.slave_address(), 0x11);
        client.set_slave(0x31);
        assert_eq!(client.slave, 0x31);
        assert_eq!(client.slave_address(), 0x31);
    }

    #[test]
    fn set_timeout_changes_duration() {
        let mut client = ModbusClient::new("10.0.0.1", 8899, "SN0001");
        let new_timeout = Duration::from_secs(10);
        client.set_timeout(new_timeout);
        assert_eq!(client.timeout, new_timeout);
    }

    // -----------------------------------------------------------------------
    // Register type conversion
    // -----------------------------------------------------------------------

    #[test]
    fn register_type_function_codes_match() {
        assert_eq!(RegisterType::Holding.function_code(), 0x03);
        assert_eq!(RegisterType::Input.function_code(), 0x04);
    }

    #[test]
    fn standard_poll_blocks_are_accessible() {
        assert_eq!(STANDARD_POLL_BLOCKS.len(), 4);
        assert_eq!(STANDARD_POLL_BLOCKS[0].name, "input_0_59");
        // Input 180 block now reads IR(180-183) to include the Gen1-authoritative
        // alternative daily battery energy registers.
        assert_eq!(STANDARD_POLL_BLOCKS[3].start, 180);
        assert_eq!(STANDARD_POLL_BLOCKS[3].count, 4);
    }

    #[test]
    fn three_phase_standard_poll_blocks_omit_redundant_inputs() {
        assert_eq!(STANDARD_POLL_BLOCKS_3PH.len(), 2);
        assert_eq!(STANDARD_POLL_BLOCKS_3PH[0].name, "holding_0_59");
        assert_eq!(STANDARD_POLL_BLOCKS_3PH[1].name, "holding_60_119");
        // No input register blocks — telemetry comes from IR 1000+
        assert!(STANDARD_POLL_BLOCKS_3PH
            .iter()
            .all(|b| b.register_type != super::super::registers::RegisterType::Input));
    }

    #[test]
    fn inter_request_delay_is_adjustable() {
        let mut client = ModbusClient::new("10.0.0.1", 8899, "SN0001");
        assert_eq!(
            client.inter_request_delay,
            ModbusClient::INTER_REQUEST_DELAY_DEFAULT
        );
        client.set_inter_request_delay(ModbusClient::INTER_REQUEST_DELAY_3PH);
        assert_eq!(
            client.inter_request_delay,
            ModbusClient::INTER_REQUEST_DELAY_3PH
        );
    }

    #[test]
    fn three_phase_model_specific_poll_order_reads_dashboard_inputs_first() {
        use crate::inverter::model::DeviceType;

        let blocks = model_specific_blocks_in_poll_order(&DeviceType::ThreePhase);
        let names: Vec<&str> = blocks.iter().map(|b| b.name).collect();

        assert!(names.starts_with(&[
            "input_1000_1059",
            "input_1060_1119",
            "input_1120_1179",
            "input_1180_1239",
            "input_1240_1299",
            "input_1300_1359",
            "input_1360_1413",
        ]));
        assert!(names.iter().any(|name| *name == "holding_240_299"));
        assert!(names.iter().any(|name| *name == "holding_1080_1124"));
    }

    #[test]
    fn gateway_model_specific_poll_order_reads_gateway_blocks_first() {
        use crate::inverter::model::DeviceType;

        let blocks = model_specific_blocks_in_poll_order(&DeviceType::Gateway);
        let names: Vec<&str> = blocks.iter().map(|b| b.name).collect();

        // The gateway aggregation blocks are dashboard-critical and should
        // appear before optional HR config/schedule blocks.
        assert!(names.starts_with(&[
            "input_1600_1659",
            "input_1660_1719",
            "input_1720_1779",
            "input_1780_1830",
            "input_1831_1859",
        ]));
        // Should also poll three-phase config (HR 1080-1124) for write registers
        // and extended slots (HR 240-299) for slots 3-10.
        assert!(names.iter().any(|name| *name == "holding_240_299"));
        assert!(names.iter().any(|name| *name == "holding_1080_1124"));
    }

    #[test]
    fn gateway_read_all_with_extras_uses_lean_standard_blocks() {
        use crate::inverter::model::DeviceType;

        // For Gateway, read_all_with_extras should use STANDARD_POLL_BLOCKS_3PH
        // (HR-only) rather than the full set, because IR 0-59 and IR 180-239
        // are unmapped on the Gateway.
        // We can't easily test the runtime switch, but we can verify the
        // condition function produces the right result.
        assert!(DeviceType::Gateway.needs_gateway_input_blocks());
        assert!(!DeviceType::Gateway.needs_three_phase_input_blocks());

        // Both three-phase and gateway should trigger the lean block selection
        // via the condition in read_all_with_extras.
        let condition_3ph = DeviceType::ThreePhase.needs_three_phase_input_blocks()
            || DeviceType::ThreePhase.needs_gateway_input_blocks();
        assert!(condition_3ph);

        let condition_gw = DeviceType::Gateway.needs_three_phase_input_blocks()
            || DeviceType::Gateway.needs_gateway_input_blocks();
        assert!(condition_gw);
    }
    #[test]
    fn ac_coupled_model_specific_poll_order_still_reads_ac_config() {
        use crate::inverter::model::DeviceType;

        let blocks = model_specific_blocks_in_poll_order(&DeviceType::ACCoupled);
        let names: Vec<&str> = blocks.iter().map(|b| b.name).collect();

        assert_eq!(names, vec!["holding_300_359"]);
    }

    // =======================================================================
    // Mock GivEnergy dongle server
    // =======================================================================
    //
    // Simulates the real dongle's behavior over TCP — returns configurable
    // response sequences per-request so tests can exercise the full retry
    // and stale-frame logic.

    use std::sync::Arc;
    use tokio::net::TcpListener;

    /// Programmed response for a single request.
    enum MockResponse {
        /// Send this frame as-is.
        Raw(Vec<u8>),
        /// Simulate a read response with the given slave/function/base/data.
        ReadResponse {
            slave: u8,
            function: u8,
            base: u16,
            data: Vec<u16>,
        },
        /// Simulate a Modbus exception (function code with high bit set).
        Exception {
            slave: u8,
            function: u8, // will be OR'd with 0x80
            code: u8,
        },
    }

    impl MockResponse {
        fn encode(&self) -> Vec<u8> {
            match self {
                MockResponse::Raw(frame) => frame.clone(),
                MockResponse::ReadResponse {
                    slave,
                    function,
                    base,
                    data,
                } => build_read_response(*slave, *function, *base, data),
                MockResponse::Exception {
                    slave,
                    function,
                    code,
                } => build_exception_response(*slave, *function | 0x80, *code),
            }
        }
    }

    /// Build a GivEnergy-wrapped read response frame.
    ///
    /// Uses the real `encode_frame` from the framer so the format matches
    /// what the client expects exactly.
    fn build_read_response(slave: u8, function: u8, base_register: u16, data: &[u16]) -> Vec<u8> {
        // Build the inner Modbus-style payload: serial(10) + base(2) + count(2) + register_data
        let mut payload = Vec::new();
        payload.extend_from_slice(b"TEST123456"); // 10-byte serial
        payload.extend_from_slice(&base_register.to_be_bytes());
        payload.extend_from_slice(&(data.len() as u16).to_be_bytes());
        for val in data {
            payload.extend_from_slice(&val.to_be_bytes());
        }
        // `encode_frame` wraps with GivEnergy header + CRC
        crate::modbus::framer::encode_frame("TEST123456", slave, function, &payload)
    }

    /// Build a GivEnergy-wrapped Modbus exception response.
    fn build_exception_response(slave: u8, function_with_error: u8, code: u8) -> Vec<u8> {
        // Real GivEnergy dongles embed the 10-byte serial in ALL transparent
        // responses, including exceptions. The exception code follows the
        // serial prefix (byte offset 10 in the inner payload).
        // Note: real exception frames omit base_register and register_count,
        // so from_response() returns None for these — the consumer task
        // has a fallback scan by (slave, function & 0x7F).
        let mut payload = Vec::with_capacity(11);
        payload.extend_from_slice(b"TEST123456"); // 10-byte serial prefix
        payload.push(code);
        crate::modbus::framer::encode_frame("TEST123456", slave, function_with_error, &payload)
    }

    /// Parse an incoming GivEnergy frame to extract the inner request details.
    /// Returns (slave, function, payload) where payload for a read request
    /// is the start register (2 bytes) + count (2 bytes).
    fn parse_request(data: &[u8]) -> Option<(u8, u8, Vec<u8>)> {
        if data.len() < HEADER_SIZE + 4 {
            return None;
        }
        // Skip 26-byte header: txn(2)+proto(2)+len(2)+unit(1)+func(1)+serial(10)+padding(8)
        let inner_start = HEADER_SIZE;
        let inner = &data[inner_start..];
        if inner.len() < 4 {
            return None;
        }
        let slave = inner[0];
        let function = inner[1];
        // Inner payload starts after slave(1)+func(1), before CRC (last 2 bytes)
        let payload_end = inner.len() - 2; // exclude CRC
        let payload = inner[2..payload_end].to_vec();
        Some((slave, function, payload))
    }

    /// Run a mock server that replies to each request with the next response
    /// from `responses` (cycling if there are fewer responses than requests).
    async fn run_mock_server(listener: TcpListener, responses: Arc<Vec<MockResponse>>) {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut idx = 0usize;

        loop {
            // Read the 6-byte MBAP header.
            let mut header = [0u8; 6];
            if tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut header))
                .await
                .is_err()
            {
                break;
            }
            let length = u16::from_be_bytes([header[4], header[5]]) as usize;

            // Read the body (may need multiple reads).
            let mut rest = vec![0u8; length];
            let mut read = 0usize;
            while read < length {
                match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut rest[read..]))
                    .await
                {
                    Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                    Ok(Ok(n)) => read += n,
                }
            }
            if read < length {
                break; // incomplete frame
            }

            // Respond
            let response = &responses[idx % responses.len()];
            idx += 1;

            let frame = response.encode();
            if stream.write_all(&frame).await.is_err() {
                break;
            }
        }
    }

    // =======================================================================
    // Producer/consumer client
    // =======================================================================
    //
    // The tests below exercise a producer/consumer-style client that matches
    // responses by content (slave + function + register range) rather than
    // sequential position. This mirrors how givenergy-modbus works.
    //
    // Infrastructure:
    //
    //   ResponseKey = (slave, function, base_register, count)
    //   pending: HashMap<ResponseKey, oneshot::Sender<DecodedFrame>>
    //
    //   Consumer task: reads all incoming frames, routes by key
    //   Producer task: dequeues from tx_queue, writes with jitter
    //
    // The production implementation will be in ModbusClient. These tests
    // validate the pattern before the refactor.

    /// Content-based response key for matching responses to pending requests.
    ///
    /// Mirrors givenergy-modbus's `shape_hash()` concept — a response matches
    /// a request when slave, function code, and register range all agree.
    #[derive(Debug, Clone, Hash, Eq, PartialEq)]
    struct ResponseKey {
        slave: u8,
        function: u8,
        base_register: u16,
        count: u16,
    }

    impl ResponseKey {
        fn from_read_request(slave: u8, function: u8, start: u16, count: u16) -> Self {
            Self {
                slave,
                function,
                base_register: start,
                count,
            }
        }

        fn from_decoded_frame(frame: &DecodedFrame, payload: &[u8]) -> Option<Self> {
            // Read response payload: byte_count(1) + data(byte_count)
            // Extract base register and count from the original request context
            // is not available here. Instead, we derive from the data array
            // location — for a read response, the payload contains register
            // values starting at some base.
            //
            // Since we can't know the base_register from the response alone,
            // this is a best-effort match. In the real implementation, the
            // pending map is keyed by the request, not derived from the response.
            if payload.len() < 3 {
                return None;
            }
            let byte_count = payload[0] as usize;
            if payload.len() < 1 + byte_count {
                return None;
            }
            let register_count = (byte_count / 2) as u16;
            Some(Self {
                slave: frame.slave,
                function: frame.function,
                base_register: 0, // unknown from response alone
                count: register_count,
            })
        }
    }

    /// Extract the expected response key from an outgoing read request frame.
    /// Returns None for non-read frames (writes, etc.).
    fn read_request_key(frame: &[u8]) -> Option<ResponseKey> {
        let (slave, function, payload) = parse_request(frame)?;
        if function != 0x03 && function != 0x04 {
            return None;
        }
        if payload.len() < 4 {
            return None;
        }
        let start = u16::from_be_bytes([payload[0], payload[1]]);
        let count = u16::from_be_bytes([payload[2], payload[3]]);
        Some(ResponseKey::from_read_request(
            slave, function, start, count,
        ))
    }

    /// Extract the key from a decoded response frame for matching.
    fn response_frame_key(frame: &DecodedFrame) -> Option<ResponseKey> {
        // For a read response (function 0x03/0x04): payload = byte_count + data
        if frame.function != 0x03 && frame.function != 0x04 {
            return None;
        }
        if frame.payload.is_empty() {
            return None;
        }
        let byte_count = frame.payload[0] as usize;
        let register_count = (byte_count / 2) as u16;
        Some(ResponseKey {
            slave: frame.slave,
            function: frame.function,
            base_register: 0, // unknown — matched by slave+func+count in practice
            count: register_count,
        })
    }

    fn dummy_register_values(start: u16, count: u16) -> Vec<u16> {
        (start..start + count).collect()
    }

    // =======================================================================
    // Integration tests — simulate real dongle behavior over TCP
    // =======================================================================

    /// Helper: start a mock server and connect a client to it.
    /// Returns (port, server_handle, client).
    async fn setup_client_with_server(
        responses: Vec<MockResponse>,
    ) -> (u16, tokio::task::JoinHandle<()>, ModbusClient) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let responses = Arc::new(responses);
        let server_handle = tokio::spawn(async move {
            run_mock_server(listener, responses).await;
        });

        let mut client = ModbusClient::new("127.0.0.1", port, "TEST123456");
        client.set_timeout(Duration::from_millis(500));
        client.connect().await.unwrap();

        (port, server_handle, client)
    }

    #[tokio::test]
    async fn happy_path_single_read() {
        // Server returns correct IR 0-19 response
        let data: Vec<u16> = (0..20).collect();
        let responses = vec![MockResponse::ReadResponse {
            slave: 0x11,
            function: 0x04,
            base: 0,
            data,
        }];

        let (_port, server, mut client) = setup_client_with_server(responses).await;

        let result = client
            .read_registers(RegisterType::Input, 0, 20)
            .await
            .unwrap();

        assert_eq!(result.len(), 20);
        assert_eq!(result[0], 0);
        assert_eq!(result[19], 19);

        server.await.unwrap();
    }

    #[tokio::test]
    async fn wrong_slave_response_triggers_retry_and_succeeds() {
        // First response: wrong slave (battery 0x35 instead of inverter 0x11)
        // Second response: correct
        let wrong_data: Vec<u16> = (100..120).collect(); // data, but from wrong device
        let correct_data: Vec<u16> = (0..20).collect();

        let responses = vec![
            MockResponse::ReadResponse {
                slave: 0x35,
                function: 0x04,
                base: 200,
                data: wrong_data,
            },
            MockResponse::ReadResponse {
                slave: 0x11,
                function: 0x04,
                base: 0,
                data: correct_data,
            },
        ];

        let (_port, server, mut client) = setup_client_with_server(responses).await;

        let result = client
            .read_registers(RegisterType::Input, 0, 20)
            .await
            .unwrap();

        assert_eq!(result.len(), 20);
        assert_eq!(result[0], 0);
        assert_eq!(result[19], 19);

        server.await.unwrap();
    }

    #[tokio::test]
    async fn wrong_slave_all_retries_exhausted_returns_error() {
        // All 5 responses (4 stale retries + 1 initial) have wrong slave
        let wrong_data: Vec<u16> = (100..120).collect();
        let responses = vec![MockResponse::ReadResponse {
            slave: 0x35,
            function: 0x04,
            base: 200,
            data: wrong_data,
        }];

        let (_port, server, mut client) = setup_client_with_server(responses).await;

        let err = client
            .read_registers(RegisterType::Input, 0, 20)
            .await
            .unwrap_err();

        assert!(
            matches!(&err, ClientError::Timeout),
            "Expected timeout after all retries exhausted, got: {}",
            err
        );

        server.await.unwrap();
    }

    #[tokio::test]
    async fn code_67_busy_triggers_retry_and_succeeds() {
        let data: Vec<u16> = (0..20).collect();
        let responses = vec![
            MockResponse::Exception {
                slave: 0x11,
                function: 0x04,
                code: 67,
            },
            MockResponse::ReadResponse {
                slave: 0x11,
                function: 0x04,
                base: 0,
                data,
            },
        ];

        let (_port, server, mut client) = setup_client_with_server(responses).await;

        // With the producer/consumer pattern, code 67 is caught in
        // read_registers_raw's retry loop. It retries and gets the
        // correct response.
        let result = client.read_registers(RegisterType::Input, 0, 20).await;
        assert!(
            result.is_ok(),
            "Expected success after code 67 retry, got: {:?}",
            result
        );

        server.await.unwrap();
    }

    #[tokio::test]
    async fn stale_frame_behind_correct_frame_in_buffer() {
        // The dongle has TWO responses buffered: a stale response from a
        // previous request (wrong register range) and the correct response.
        // The correct response is behind the stale one.
        //
        // With sequential read: first read gets the stale frame, retries
        // and drains → second read gets the correct one.

        let correct_data: Vec<u16> = (0..20).collect();
        // Stale response for a DIFFERENT request (base 200 instead of 0)
        let stale_data: Vec<u16> = (200..220).collect();

        let responses = vec![
            MockResponse::ReadResponse {
                slave: 0x11,
                function: 0x04,
                base: 200,
                data: stale_data,
            },
            MockResponse::ReadResponse {
                slave: 0x11,
                function: 0x04,
                base: 0,
                data: correct_data,
            },
        ];

        let (_port, server, mut client) = setup_client_with_server(responses).await;

        let result = client
            .read_registers(RegisterType::Input, 0, 20)
            .await
            .unwrap();

        // Should get the correct data after the stale frame is consumed
        assert_eq!(result.len(), 20);
        assert_eq!(result[0], 0);

        server.await.unwrap();
    }

    #[tokio::test]
    async fn multiple_wrong_slave_frames_in_buffer_then_correct() {
        // Battery modules at 0x34, 0x35 respond before inverter at 0x11.
        // All three responses are buffered in the dongle's TCP output.
        let correct_data: Vec<u16> = (0..20).collect();

        let responses = vec![
            MockResponse::ReadResponse {
                slave: 0x34,
                function: 0x04,
                base: 100,
                data: (100..120).collect(),
            },
            MockResponse::ReadResponse {
                slave: 0x35,
                function: 0x04,
                base: 200,
                data: (200..220).collect(),
            },
            MockResponse::ReadResponse {
                slave: 0x11,
                function: 0x04,
                base: 0,
                data: correct_data,
            },
        ];

        let (_port, server, mut client) = setup_client_with_server(responses).await;

        let result = client
            .read_registers(RegisterType::Input, 0, 20)
            .await
            .unwrap();

        assert_eq!(result.len(), 20);
        assert_eq!(result[0], 0);

        server.await.unwrap();
    }

    #[tokio::test]
    async fn code_67_followed_by_multiple_wrong_slave_then_correct() {
        // The worst case: dongle busy, then battery module responses,
        // then finally the correct inverter response.
        let correct_data: Vec<u16> = (0..20).collect();

        let responses = vec![
            MockResponse::Exception {
                slave: 0x11,
                function: 0x04,
                code: 67,
            },
            MockResponse::ReadResponse {
                slave: 0x34,
                function: 0x04,
                base: 100,
                data: (100..120).collect(),
            },
            MockResponse::ReadResponse {
                slave: 0x35,
                function: 0x04,
                base: 200,
                data: (200..220).collect(),
            },
            MockResponse::ReadResponse {
                slave: 0x11,
                function: 0x04,
                base: 0,
                data: correct_data,
            },
        ];

        let (_port, server, mut client) = setup_client_with_server(responses).await;

        // Producer/consumer: code 67 triggers retry, wrong-slave frames are
        // dropped, correct response is received.
        let result = client.read_registers(RegisterType::Input, 0, 20).await;
        assert!(
            result.is_ok(),
            "Expected success after code 67 + wrong-slave + correct, got: {:?}",
            result
        );

        server.await.unwrap();
    }

    #[tokio::test]
    async fn heartbeat_is_absorbed_by_consumer() {
        // The dongle sends a heartbeat before the response. The consumer must
        // answer it (echo) so the connection stays alive, and transparently
        // route the subsequent read response to the caller.

        // Build a minimal heartbeat frame.
        let mut heartbeat = vec![0x59, 0x59, 0x00, 0x01, 0x00, 0x00, 0x01, 0x01];
        let len = 2u16; // minimum: unit(1) + function(1)
        heartbeat[4..6].copy_from_slice(&len.to_be_bytes());

        let correct_data: Vec<u16> = (0..20).collect();

        let responses = vec![
            // Heartbeat request from dongle — consumer echoes it back.
            MockResponse::Raw(heartbeat.clone()),
            // Correct read response (sent after the mock reads our echo).
            MockResponse::ReadResponse {
                slave: 0x11,
                function: 0x04,
                base: 0,
                data: correct_data,
            },
        ];

        let (_port, server, mut client) = setup_client_with_server(responses).await;

        let result = client.read_registers(RegisterType::Input, 0, 20).await;

        // The consumer echoed the heartbeat, so the mock advanced to the read
        // response and the call succeeds. Without the echo, this would time
        // out (the mock would block waiting for the client's second frame).
        assert!(
            result.is_ok(),
            "heartbeat not answered — read failed: {result:?}"
        );
        assert_eq!(result.unwrap().len(), 20);

        server.await.unwrap();
    }

    #[tokio::test]
    async fn consumer_answers_heartbeat_with_echo() {
        // Regression test for the dropped-heartbeat bug: the consumer must
        // echo each heartbeat request back to the dongle, otherwise the
        // socket is torn down after 3 missed heartbeats (~9 min). This uses
        // a raw server (not the response-driven mock) to capture the exact
        // bytes the client sends in response to an unsolicited heartbeat.
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        // Heartbeat request frame the dongle sends (0x5959 MBAP + function 0x01).
        let mut heartbeat = vec![0x59, 0x59, 0x00, 0x01, 0x00, 0x00, 0x01, 0x01];
        let len = 2u16; // minimum: unit(1) + function(1)
        heartbeat[4..6].copy_from_slice(&len.to_be_bytes());
        let expected_echo = heartbeat.clone();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            // Dongle sends the heartbeat request.
            stream.write_all(&heartbeat).await.unwrap();
            // Read back whatever the client sends in response.
            let mut echo = vec![0u8; heartbeat.len()];
            stream.read_exact(&mut echo).await.unwrap();
            echo
        });

        let mut client = ModbusClient::new("127.0.0.1", port, "TEST123456");
        client.set_timeout(Duration::from_millis(500));
        client.connect().await.unwrap();

        // The consumer runs in the background and should echo the heartbeat.
        // No read is issued from the client, so the only outbound bytes are the
        // heartbeat response. Bound the wait so a missing echo fails fast
        // instead of hanging.
        let echo = tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("server timed out — consumer did not echo the heartbeat")
            .expect("server task panicked");
        assert_eq!(
            echo, expected_echo,
            "consumer must echo the heartbeat back byte-for-byte"
        );

        client.disconnect().await;
    }

    #[tokio::test]
    async fn split_register_read_reassembles_correctly() {
        // Reading 60 registers as a single request.
        let data: Vec<u16> = (0..60).collect();

        let responses = vec![MockResponse::ReadResponse {
            slave: 0x11,
            function: 0x04,
            base: 0,
            data,
        }];

        let (_port, server, mut client) = setup_client_with_server(responses).await;

        let result = client
            .read_registers(RegisterType::Input, 0, 60)
            .await
            .unwrap();

        assert_eq!(result.len(), 60);
        assert_eq!(result[0], 0);
        assert_eq!(result[30], 30);
        assert_eq!(result[59], 59);

        server.await.unwrap();
    }

    #[tokio::test]
    async fn read_blocks_returns_all_standard_blocks() {
        // Each 60-register block is read as a single request.
        let responses = vec![
            // input_0_59: IR 0-59
            MockResponse::ReadResponse {
                slave: 0x11,
                function: 0x04,
                base: 0,
                data: (0..60).collect(),
            },
            // holding_0_59: HR 0-59
            MockResponse::ReadResponse {
                slave: 0x11,
                function: 0x03,
                base: 0,
                data: (100..160).collect(),
            },
            // holding_60_119: HR 60-119
            MockResponse::ReadResponse {
                slave: 0x11,
                function: 0x03,
                base: 60,
                data: (200..260).collect(),
            },
            // input_180_181: IR 180-183 (4 registers: lifetime totals plus
            // Gen1-authoritative alternative daily battery energy)
            MockResponse::ReadResponse {
                slave: 0x11,
                function: 0x04,
                base: 180,
                data: (500..504).collect(),
            },
        ];

        let (_port, server, mut client) = setup_client_with_server(responses).await;

        let blocks = client.read_blocks(STANDARD_POLL_BLOCKS).await.unwrap();
        assert_eq!(blocks.len(), 4);
        assert_eq!(blocks[0].block.name, "input_0_59");
        assert_eq!(blocks[0].data.len(), 60);
        assert_eq!(blocks[0].data[0], 0);
        assert_eq!(blocks[1].block.name, "holding_0_59");
        assert_eq!(blocks[1].data[0], 100);
        assert_eq!(blocks[3].block.name, "input_180_181");
        assert_eq!(blocks[3].data.len(), 4);

        server.await.unwrap();
    }

    #[tokio::test]
    async fn read_blocks_fails_on_first_standard_block_error() {
        // First block fails — entire read_blocks should fail
        let responses = vec![MockResponse::Exception {
            slave: 0x11,
            function: 0x04,
            code: 67,
        }];

        let (_port, server, mut client) = setup_client_with_server(responses).await;

        let result = client.read_blocks(STANDARD_POLL_BLOCKS).await;
        assert!(
            result.is_err(),
            "read_blocks should fail when first block errors"
        );

        server.await.unwrap();
    }

    #[tokio::test]
    async fn write_register_sends_correct_frame() {
        // Write response: serial(10) + register(2) + value(2) = 14 bytes
        let mut payload = Vec::with_capacity(14);
        payload.extend_from_slice(b"TEST123456"); // 10-byte serial
        payload.extend_from_slice(&[0x00u8, 0x1B]); // register 27
        payload.extend_from_slice(&[0x00u8, 0x01]); // value 1
        let response = crate::modbus::framer::encode_frame("TEST123456", 0x11, 0x06, &payload);

        let responses = vec![MockResponse::Raw(response)];

        let (_port, server, mut client) = setup_client_with_server(responses).await;

        let result = client.write_register(27, 1).await;
        assert!(result.is_ok(), "Write register failed: {:?}", result);

        server.await.unwrap();
    }

    // =======================================================================
    // probe_registers_at_slave tests
    // =======================================================================

    #[tokio::test]
    async fn probe_finds_meter_at_responding_slave() {
        // A meter at slave 0x03 responds with valid data.
        // probe_registers_at_slave should return the data with a single attempt.
        let meter_data: Vec<u16> = vec![
            2300, // V_phase_1 = 230.0V (>100V → valid)
            0, 0, 0, 0, 0, 0, 0,   // rest of IR 60-66
            100, // p_active_total (IR 68)
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]; // 30 values
        assert_eq!(meter_data.len(), 30);

        let responses = vec![MockResponse::ReadResponse {
            slave: 0x03,
            function: 0x04,
            base: 60,
            data: meter_data.clone(),
        }];

        let (_port, _server, mut client) = setup_client_with_server(responses).await;

        let result = client
            .probe_registers_at_slave(0x03, RegisterType::Input, 60, 30, Duration::from_secs(3))
            .await;

        assert!(
            result.is_ok(),
            "probe should succeed when meter responds: {:?}",
            result
        );
        assert_eq!(result.unwrap()[0], 2300);
    }

    #[tokio::test]
    async fn probe_returns_error_for_non_responding_slave() {
        // No mock response configured for slave 0x05.
        // The probe should time out after the short probe_timeout (no retries).
        // We use a very short timeout to keep the test fast.
        let _responses: Vec<MockResponse> = vec![];

        // We need a server that accepts the connection but never responds.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_handle = tokio::spawn(async move {
            let (mut _stream, _) = listener.accept().await.unwrap();
            // Read incoming frames but never respond — simulates a non-existent slave.
            let mut buf = [0u8; 1024];
            loop {
                match tokio::time::timeout(Duration::from_secs(2), _stream.read(&mut buf)).await {
                    Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                    Ok(Ok(_)) => {}
                }
            }
        });

        let mut client = ModbusClient::new("127.0.0.1", port, "TEST123456");
        client.set_timeout(Duration::from_secs(5)); // default (should not be used by probe)
        client.connect().await.unwrap();

        let start = std::time::Instant::now();
        let result = client
            .probe_registers_at_slave(
                0x05,
                RegisterType::Input,
                60,
                30,
                Duration::from_millis(200), // very short for test speed
            )
            .await;

        let elapsed = start.elapsed();

        assert!(
            result.is_err(),
            "probe should fail when slave doesn't respond"
        );
        // Single attempt: should be close to the probe_timeout, NOT 5× longer
        assert!(
            elapsed < Duration::from_secs(2),
            "probe should not retry — elapsed {elapsed:?} suggests retries occurred"
        );

        // Verify the original timeout was restored
        assert_eq!(client.timeout, Duration::from_secs(5));

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn probe_restores_original_slave_address() {
        // Verify that probe_registers_at_slave restores the original slave
        // address even when the probe fails (timeout).
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_handle = tokio::spawn(async move {
            let (mut _stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            loop {
                match tokio::time::timeout(Duration::from_secs(2), _stream.read(&mut buf)).await {
                    Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                    Ok(Ok(_)) => {}
                }
            }
        });

        let mut client = ModbusClient::new("127.0.0.1", port, "TEST123456");
        client.set_timeout(Duration::from_millis(100));
        client.connect().await.unwrap();

        assert_eq!(client.slave, 0x11); // default

        let _ = client
            .probe_registers_at_slave(0x05, RegisterType::Input, 60, 30, Duration::from_millis(50))
            .await;

        assert_eq!(
            client.slave, 0x11,
            "slave address must be restored after probe"
        );

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn probe_restores_original_timeout_on_success() {
        // Verify that probe_registers_at_slave restores the original timeout
        // after a successful probe.
        let meter_data: Vec<u16> = vec![2300; 30];
        let responses = vec![MockResponse::ReadResponse {
            slave: 0x03,
            function: 0x04,
            base: 60,
            data: meter_data,
        }];

        let (_port, _server, mut client) = setup_client_with_server(responses).await;
        // setup_client_with_server sets timeout to 500ms
        let expected_timeout = Duration::from_millis(500);
        assert_eq!(client.timeout, expected_timeout);

        let _ = client
            .probe_registers_at_slave(0x03, RegisterType::Input, 60, 30, Duration::from_secs(3))
            .await;

        assert_eq!(
            client.timeout, expected_timeout,
            "timeout must be restored after successful probe"
        );
    }

    #[tokio::test]
    async fn probe_does_not_retry_on_timeout() {
        // Confirms that probe_registers_at_slave makes exactly ONE attempt.
        // The mock server only provides one response slot; if the probe retried
        // it would send a second request that the server can't satisfy.
        // But more importantly: measure elapsed time to prove no retry delay.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_handle = tokio::spawn(async move {
            let (mut _stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            loop {
                match tokio::time::timeout(Duration::from_secs(2), _stream.read(&mut buf)).await {
                    Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                    Ok(Ok(_)) => {}
                }
            }
        });

        let mut client = ModbusClient::new("127.0.0.1", port, "TEST123456");
        client.set_timeout(Duration::from_secs(15));
        client.connect().await.unwrap();

        let probe_timeout = Duration::from_millis(200);
        let start = std::time::Instant::now();
        let _ = client
            .probe_registers_at_slave(0x07, RegisterType::Input, 60, 30, probe_timeout)
            .await;
        let elapsed = start.elapsed();

        // With retries, 5 attempts × (200ms + 500ms delay) = 3.5s.
        // Without retries: ~200ms.
        assert!(
            elapsed < Duration::from_secs(1),
            "single attempt should complete in ~200ms, got {elapsed:?} — retries likely occurred"
        );

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn probe_sequential_8_addresses_fast() {
        // Simulates the real meter-probe scenario: 8 non-responding addresses.
        // Measures total elapsed time to confirm the full scan is fast.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_handle = tokio::spawn(async move {
            let (mut _stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            loop {
                match tokio::time::timeout(Duration::from_secs(5), _stream.read(&mut buf)).await {
                    Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                    Ok(Ok(_)) => {}
                }
            }
        });

        let mut client = ModbusClient::new("127.0.0.1", port, "TEST123456");
        client.set_timeout(Duration::from_secs(15));
        client.connect().await.unwrap();

        let probe_timeout = Duration::from_millis(200);
        let addresses: &[u8] = &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];

        let start = std::time::Instant::now();
        let mut found = 0usize;
        for &addr in addresses {
            let result = client
                .probe_registers_at_slave(addr, RegisterType::Input, 60, 30, probe_timeout)
                .await;
            if result.is_ok() {
                found += 1;
            }
        }
        let elapsed = start.elapsed();

        assert_eq!(found, 0, "no meters should be detected on a silent server");
        // 8 × 200ms = 1.6s worst case. With old code: 8 × 5 × 15s = 600s.
        assert!(
            elapsed < Duration::from_secs(3),
            "8-address probe should take <3s, got {elapsed:?}"
        );

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn probe_detects_meter_among_8_addresses() {
        // Meter at address 0x03 responds; all others time out.
        // Uses a server that only responds to the correct slave.
        let meter_data: Vec<u16> = vec![
            2350, // V_phase_1 = 235.0V
            0, 0, 0, 0, 0, 0, 500, // IR 60-67
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]; // 30 values
        assert_eq!(meter_data.len(), 30);

        let responses = vec![MockResponse::ReadResponse {
            slave: 0x03,
            function: 0x04,
            base: 60,
            data: meter_data.clone(),
        }];

        let (_port, _server, mut client) = setup_client_with_server(responses).await;

        let addresses: &[u8] = &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let mut found_addrs: Vec<u8> = Vec::new();
        let mut found_data: Vec<Vec<u16>> = Vec::new();

        for &addr in addresses {
            if let Ok(data) = client
                .probe_registers_at_slave(
                    addr,
                    RegisterType::Input,
                    60,
                    30,
                    Duration::from_millis(200),
                )
                .await
            {
                found_addrs.push(addr);
                found_data.push(data);
            }
        }

        assert_eq!(found_addrs, vec![0x03]);
        assert_eq!(found_data[0][0], 2350);
    }

    #[tokio::test]
    async fn probe_restores_original_slave_on_success() {
        // Slave should be restored even when the probe succeeds.
        let meter_data: Vec<u16> = vec![2300; 30];
        let responses = vec![MockResponse::ReadResponse {
            slave: 0x05,
            function: 0x04,
            base: 60,
            data: meter_data,
        }];

        let (_port, _server, mut client) = setup_client_with_server(responses).await;

        assert_eq!(client.slave, 0x11);

        let _ = client
            .probe_registers_at_slave(0x05, RegisterType::Input, 60, 30, Duration::from_secs(3))
            .await;

        assert_eq!(
            client.slave, 0x11,
            "slave address must be restored after successful probe"
        );
    }

    // =======================================================================
    // Liveness probe tests
    // =======================================================================

    #[tokio::test]
    async fn liveness_probe_success() {
        // A responding dongle returns data for HR 0.
        let data: Vec<u16> = vec![0x0001]; // status register
        let responses = vec![MockResponse::ReadResponse {
            slave: 0x11,
            function: 0x03,
            base: 0,
            data,
        }];

        let (_port, _server, mut client) = setup_client_with_server(responses).await;

        let result = client.liveness_probe().await;
        assert!(result.is_ok(), "liveness probe should succeed: {result:?}");
    }

    #[tokio::test]
    async fn liveness_probe_treats_exception_as_alive() {
        // A Modbus exception (e.g. code 67 busy) still proves the dongle
        // is alive — it decoded and answered the request.
        let responses = vec![MockResponse::Exception {
            slave: 0x11,
            function: 0x03,
            code: 67,
        }];

        let (_port, _server, mut client) = setup_client_with_server(responses).await;

        let result = client.liveness_probe().await;
        assert!(
            result.is_ok(),
            "exception should be treated as alive: {result:?}"
        );
    }

    #[tokio::test]
    async fn liveness_probe_timeout_on_silent_server() {
        // A silent dongle (accepts TCP but never responds) should fail
        // the probe in one round-trip (~LIVENESS_TIMEOUT), not retry.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_handle = tokio::spawn(async move {
            let (mut _stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            loop {
                match tokio::time::timeout(Duration::from_secs(2), _stream.read(&mut buf)).await {
                    Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                    Ok(Ok(_)) => {}
                }
            }
        });

        let mut client = ModbusClient::new("127.0.0.1", port, "TEST123456");
        client.set_timeout(Duration::from_secs(15));
        client.connect().await.unwrap();

        let start = std::time::Instant::now();
        let result = client.liveness_probe().await;
        let elapsed = start.elapsed();

        assert!(
            result.is_err(),
            "liveness probe should fail on silent dongle"
        );
        // Should complete in ~LIVENESS_TIMEOUT (3 s), not 5× longer
        assert!(
            elapsed < Duration::from_secs(6),
            "liveness probe should not retry — elapsed {elapsed:?}"
        );

        server_handle.await.unwrap();
    }

    // =======================================================================
    // last_activity_age tests
    // =======================================================================

    #[tokio::test]
    async fn last_activity_age_returns_some_after_successful_read() {
        let data: Vec<u16> = (0..20).collect();
        let responses = vec![MockResponse::ReadResponse {
            slave: 0x11,
            function: 0x04,
            base: 0,
            data,
        }];

        let (_port, _server, mut client) = setup_client_with_server(responses).await;

        // Before any read, age should be None (no frame received yet)
        assert!(client.last_activity_age().is_none());

        // Perform a successful read — the consumer receives a frame
        let _ = client.read_registers(RegisterType::Input, 0, 20).await.unwrap();

        // After a successful read, age should be Some and very small
        let age = client.last_activity_age();
        assert!(
            age.is_some(),
            "last_activity_age should be Some after a successful read"
        );
        assert!(
            age.unwrap() < Duration::from_secs(1),
            "age should be <1s after a recent read, got {:?}",
            age
        );
    }

    // =======================================================================
    // read_all_with_extras minimal_telemetry tests
    // =======================================================================

    #[tokio::test]
    async fn read_all_with_extras_minimal_telemetry_skips_optional_blocks() {
        use crate::inverter::model::DeviceType;

        // Standard blocks only (no optional model-specific blocks).
        // The server only provides responses for the 4 standard blocks.
        // If minimal_telemetry=true, read_all_with_extras should NOT
        // attempt any optional blocks, so the server doesn't need to
        // provide them.
        let responses = vec![
            // input_0_59
            MockResponse::ReadResponse {
                slave: 0x11,
                function: 0x04,
                base: 0,
                data: (0..60).collect(),
            },
            // holding_0_59
            MockResponse::ReadResponse {
                slave: 0x11,
                function: 0x03,
                base: 0,
                data: (100..160).collect(),
            },
            // holding_60_119
            MockResponse::ReadResponse {
                slave: 0x11,
                function: 0x03,
                base: 60,
                data: (200..260).collect(),
            },
            // input_180_181: IR 180-183
            MockResponse::ReadResponse {
                slave: 0x11,
                function: 0x04,
                base: 180,
                data: (500..504).collect(),
            },
        ];

        let (_port, _server, mut client) = setup_client_with_server(responses).await;

        // With minimal_telemetry=true and a Gen3 device type (which has
        // extended slots as optional blocks), only standard blocks should
        // be returned.
        let blocks = client
            .read_all_with_extras(Some(&DeviceType::Gen3Hybrid), true)
            .await
            .unwrap();

        // Should have exactly 4 standard blocks, no optional ones
        assert_eq!(blocks.len(), 4);
        assert_eq!(blocks[0].block.name, "input_0_59");
        assert_eq!(blocks[1].block.name, "holding_0_59");
        assert_eq!(blocks[2].block.name, "holding_60_119");
        assert_eq!(blocks[3].block.name, "input_180_181");
    }

    #[tokio::test]
    async fn read_all_with_extras_minimal_telemetry_false_reads_optional_blocks() {
        use crate::inverter::model::DeviceType;

        // Standard blocks + optional extended slots (HR 240-299) for Gen3.
        let responses = vec![
            // input_0_59
            MockResponse::ReadResponse {
                slave: 0x11,
                function: 0x04,
                base: 0,
                data: (0..60).collect(),
            },
            // holding_0_59
            MockResponse::ReadResponse {
                slave: 0x11,
                function: 0x03,
                base: 0,
                data: (100..160).collect(),
            },
            // holding_60_119
            MockResponse::ReadResponse {
                slave: 0x11,
                function: 0x03,
                base: 60,
                data: (200..260).collect(),
            },
            // input_180_181: IR 180-183
            MockResponse::ReadResponse {
                slave: 0x11,
                function: 0x04,
                base: 180,
                data: (500..504).collect(),
            },
            // extended_slots (HR 240-299) — optional block for Gen3
            MockResponse::ReadResponse {
                slave: 0x11,
                function: 0x03,
                base: 240,
                data: (300..360).collect(),
            },
        ];

        let (_port, _server, mut client) = setup_client_with_server(responses).await;

        // With minimal_telemetry=false, optional blocks should be read.
        let blocks = client
            .read_all_with_extras(Some(&DeviceType::Gen3Hybrid), false)
            .await
            .unwrap();

        // Should have 5 blocks (4 standard + 1 optional)
        assert_eq!(blocks.len(), 5);
        assert!(blocks.iter().any(|b| b.block.name == "holding_240_299"));
    }

    // =======================================================================
    // Timeout fail-fast vs retry tests
    // =======================================================================

    #[tokio::test]
    async fn timeout_fails_fast_on_silent_server() {
        // A completely silent server (accepts TCP but never responds).
        // read_registers_raw should fail fast (1 attempt) rather than
        // retrying 5×. The test measures elapsed time to confirm.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_handle = tokio::spawn(async move {
            let (mut _stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            loop {
                match tokio::time::timeout(Duration::from_secs(3), _stream.read(&mut buf)).await {
                    Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                    Ok(Ok(_)) => {}
                }
            }
        });

        let mut client = ModbusClient::new("127.0.0.1", port, "TEST123456");
        client.set_timeout(Duration::from_millis(500));
        client.connect().await.unwrap();

        let start = std::time::Instant::now();
        let result = client
            .read_registers(RegisterType::Input, 0, 20)
            .await;
        let elapsed = start.elapsed();

        assert!(
            result.is_err(),
            "should fail on silent server"
        );
        // With fail-fast: ~500ms. With old 5× retry: ~2.5s + 4×500ms = ~4.5s.
        // Allow some margin for TCP setup.
        assert!(
            elapsed < Duration::from_secs(2),
            "should fail fast (~500ms), not retry 5× — elapsed {elapsed:?}"
        );

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn timeout_retries_on_recent_activity() {
        // Server sends a stale (wrong-slave) frame first, then the correct
        // response. The consumer receives the stale frame (updating
        // last_rx_instant), so the timeout should trigger a retry.
        let correct_data: Vec<u16> = (0..20).collect();

        let responses = vec![
            // Stale frame from wrong slave
            MockResponse::ReadResponse {
                slave: 0x35,
                function: 0x04,
                base: 200,
                data: (200..220).collect(),
            },
            // Correct response
            MockResponse::ReadResponse {
                slave: 0x11,
                function: 0x04,
                base: 0,
                data: correct_data,
            },
        ];

        let (_port, _server, mut client) = setup_client_with_server(responses).await;

        // The stale frame arrives first (consumer drops it, updates
        // last_rx_instant). The first send_and_await_response times out
        // because the correct response hasn't arrived yet. With the
        // activity-based retry, the client retries and gets the correct
        // response on the second attempt.
        let result = client
            .read_registers(RegisterType::Input, 0, 20)
            .await;

        assert!(
            result.is_ok(),
            "should succeed after retry on stale frame: {result:?}"
        );
        assert_eq!(result.unwrap()[0], 0);
    }
}
