//! Modbus TCP client.
//!
//! Manages the TCP connection to the inverter, sending requests
//! and reading responses with configurable timeouts and retries.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::framer::{self, crc16_modbus, DecodedFrame, RegisterType};
use super::registers::{RegisterBlock, STANDARD_POLL_BLOCKS};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum response frame size we are willing to accept (bytes).
const MAX_RESPONSE_SIZE: usize = 4096;

/// Format raw bytes as a compact hex string for diagnostic logging.
/// Shows up to 64 bytes; truncates with "…" if longer.
fn dump_hex(data: &[u8]) -> String {
    use std::fmt::Write;
    let limit = data.len().min(64);
    let mut s = String::with_capacity(limit * 3);
    for (i, b) in data.iter().enumerate().take(limit) {
        if i > 0 {
            s.push(' ');
        }
        write!(s, "{b:02X}").ok();
    }
    if data.len() > 64 {
        s.push_str(" …");
    }
    s
}

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

    blocks.extend(device_type.extra_poll_blocks().iter());
    blocks
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

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
    /// Underlying TCP stream, `None` when disconnected.
    stream: Option<TcpStream>,
    /// Timeout for individual read/write operations.
    timeout: Duration,
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
            stream: None,
            timeout: Duration::from_secs(15),
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

    /// Connect to the inverter. Returns `Err(ClientError::AlreadyConnected)` if
    /// a connection is already open.
    pub async fn connect(&mut self) -> Result<(), ClientError> {
        if self.stream.is_some() {
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
        let ka_conf = socket2::TcpKeepalive::new()
            .with_time(std::time::Duration::from_secs(10))
            .with_interval(std::time::Duration::from_secs(5));
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

        self.stream = Some(stream);
        Ok(())
    }

    /// Drain any buffered data from the TCP receive buffer.
    ///
    /// The GivEnergy dongle caches responses from the previous session and
    /// flushes them when a new TCP connection is established. If we don't
    /// drain these stale frames, they corrupt the request-response pairing
    /// for our first real poll.
    pub async fn drain(&mut self) {
        let stream = match self.stream.as_mut() {
            Some(s) => s,
            None => return,
        };

        // Set a very short read timeout so we don't block
        let _ = stream.set_nodelay(true);
        let mut buf = [0u8; 512];
        let mut drained = 0usize;

        loop {
            match tokio::time::timeout(Duration::from_millis(100), stream.read(&mut buf)).await {
                Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break, // EOF, read error, or timeout — buffer is clear
                Ok(Ok(n)) => drained += n,
            }
        }

        if drained > 0 {
            tracing::debug!("Drained {drained} bytes of stale data from dongle");
        }
    }

    /// Disconnect gracefully.
    pub async fn disconnect(&mut self) {
        if let Some(mut stream) = self.stream.take() {
            let _ = stream.shutdown().await;
        }
    }

    /// Check if the client is currently connected.
    pub fn is_connected(&self) -> bool {
        self.stream.is_some()
    }

    // -----------------------------------------------------------------------
    // Core I/O helpers
    // -----------------------------------------------------------------------

    /// Send a raw frame to the dongle without waiting for a response.
    async fn send_raw(&mut self, frame: &[u8]) -> Result<(), ClientError> {
        let stream = self.stream.as_mut().ok_or(ClientError::NotConnected)?;
        tokio::time::timeout(self.timeout, stream.write_all(frame))
            .await
            .map_err(|_| ClientError::Timeout)?
            .map_err(|e| ClientError::SendFailed(e.to_string()))?;
        Ok(())
    }

    /// Read and decode one response frame from the dongle.
    ///
    /// Read and decode one response frame from the dongle.
    ///
    /// Reads the MBAP header to determine length, then reads the rest.
    /// Does NOT check for Modbus exceptions — callers handle those.
    ///
    /// Heartbeat requests (function ID 0x01) are handled transparently:
    /// the response is echoed back and the method loops to read the next
    /// frame until a non-heartbeat frame arrives.
    async fn receive_frame(&mut self) -> Result<DecodedFrame, ClientError> {
        loop {
            let stream = self.stream.as_mut().ok_or(ClientError::NotConnected)?;

            // Read the 6-byte MBAP header
            let mut header_buf = [0u8; 6];
            let header_result =
                tokio::time::timeout(self.timeout, stream.read_exact(&mut header_buf)).await;
            match &header_result {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    let kind = e.kind();
                    tracing::warn!(
                        error = %e,
                        ?kind,
                        "TCP read error while reading MBAP header"
                    );
                    return Err(ClientError::ReceiveFailed(format!(
                        "reading MBAP header: {e} (kind={kind:?})"
                    )));
                }
                Err(_) => {
                    tracing::warn!(
                        timeout_secs = self.timeout.as_secs(),
                        "Timeout waiting for MBAP header — dongle not responding"
                    );
                    return Err(ClientError::Timeout);
                }
            }

            // Parse length field (big-endian u16 at bytes 4-5).
            let length = u16::from_be_bytes([header_buf[4], header_buf[5]]) as usize;

            // Sanity-check the MBAP header before committing to a large read.
            // The GivEnergy protocol uses transaction ID 0x5959 and protocol ID 0x0001.
            // A corrupted frame from stale TCP buffers may have garbage here.
            let txn_id = u16::from_be_bytes([header_buf[0], header_buf[1]]);
            let proto_id = u16::from_be_bytes([header_buf[2], header_buf[3]]);
            if txn_id != 0x5959 || proto_id != 0x0001 {
                tracing::warn!(
                    "Suspicious MBAP header: txn=0x{txn_id:04X} proto=0x{proto_id:04X} len={length} — likely stale/corrupted frame"
                );
            }

            if length > MAX_RESPONSE_SIZE {
                tracing::error!(
                    length,
                    max = MAX_RESPONSE_SIZE,
                    "MBAP length field exceeds maximum — possible frame corruption or MTU issue"
                );
                return Err(ClientError::InvalidResponse(format!(
                    "response length {length} exceeds maximum {MAX_RESPONSE_SIZE}"
                )));
            }

            if length < 4 {
                // Need at least unit_id(1) + func(1) + inner_pdu(0+) + CRC(2)
                tracing::warn!(
                    length,
                    "MBAP length field unusually small — possible frame corruption"
                );
            }

            // Read the remaining `length` bytes (everything after the 6-byte MBAP header).
            let mut rest = vec![0u8; length];
            let body_result =
                tokio::time::timeout(self.timeout, stream.read_exact(&mut rest)).await;
            match &body_result {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    let kind = e.kind();
                    tracing::warn!(
                        error = %e,
                        ?kind,
                        length,
                        "TCP read error while reading frame body (MBAP len={length})"
                    );
                    return Err(ClientError::ReceiveFailed(format!(
                        "reading frame body (len={length}): {e} (kind={kind:?})"
                    )));
                }
                Err(_) => {
                    tracing::warn!(
                        timeout_secs = self.timeout.as_secs(),
                        length,
                        "Timeout reading frame body (MBAP len={length}) — partial frame from dongle"
                    );
                    return Err(ClientError::Timeout);
                }
            }

            // Reassemble the complete frame
            let mut full = Vec::with_capacity(6 + length);
            full.extend_from_slice(&header_buf);
            full.extend_from_slice(&rest);

            // Handle heartbeat requests from the dongle.
            // The dongle sends a heartbeat (function ID 0x01) every ~3 minutes.
            // We must respond within 5 seconds; after 3 missed heartbeats the
            // dongle closes the TCP connection. Silently respond and loop to
            // read the actual response frame.
            if super::framer::is_heartbeat_request(&full) {
                let response = super::framer::build_heartbeat_response(&full);
                tracing::debug!(
                    "Heartbeat request received ({} bytes), sending response",
                    full.len()
                );
                // Best-effort send — if the write fails the connection is already
                // dead and the next read will surface it.
                if let Some(s) = self.stream.as_mut() {
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        s.write_all(&response),
                    )
                    .await;
                }
                continue; // loop to read next frame
            }

            // Not a heartbeat — proceed with normal decode.

            // Auto-discover dongle serial from response header (bytes 8-17).
            if self.serial.is_empty() && full.len() >= 18 {
                let discovered = std::str::from_utf8(&full[8..18])
                    .unwrap_or("")
                    .trim_end()
                    .to_string();
                if !discovered.is_empty() {
                    let serial_hex: String = full[8..18]
                        .iter()
                        .map(|b| format!("{b:02X}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    tracing::info!(
                        serial = %discovered,
                        serial_raw = %serial_hex,
                        frame_len = full.len(),
                        "Auto-discovered dongle serial (pending decode validation)"
                    );
                    // Validate: serial should be printable ASCII, not all spaces
                    if discovered.trim().is_empty()
                        || discovered
                            .chars()
                            .any(|c| !c.is_ascii_graphic() && c != ' ')
                    {
                        tracing::warn!(
                            serial_raw = %serial_hex,
                            "Auto-discovered serial looks suspicious (non-printable chars)"
                        );
                    }

                    // Decode the frame to verify it's complete.
                    // We ONLY accept the discovered serial if the full frame
                    // decodes successfully. A truncated frame (like 19 bytes
                    // where the inner PDU is missing) means the serial is
                    // suspect — using it for subsequent requests causes the
                    // dongle to stop responding on some firmware versions.
                    let hex_bytes = dump_hex(&full);
                    match framer::decode_frame(&full) {
                        Ok(decoded) => {
                            tracing::info!(
                                serial = %discovered,
                                "Auto-discovered serial confirmed — frame valid"
                            );
                            self.serial = discovered;
                            self.serial_discovered = true;
                            return Ok(decoded);
                        }
                        Err(e) => {
                            tracing::warn!(
                                frame_len = full.len(),
                                error = %e,
                                raw_hex = %hex_bytes,
                                "Auto-discovered serial REJECTED — frame truncated, keeping empty serial"
                            );
                            self.serial_suspect = true;
                            // Keep serial empty for subsequent requests
                            return Err(ClientError::FrameError(format!(
                                "auto-discover frame truncated: {e}"
                            )));
                        }
                    }
                }
            }

            // Normal path: no auto-discovery needed or failed — just decode
            let hex_bytes = dump_hex(&full);
            return framer::decode_frame(&full).map_err(|e| {
                tracing::warn!(
                    frame_len = full.len(),
                    error = %e,
                    raw_hex = %hex_bytes,
                    "Frame decode failed"
                );
                ClientError::FrameError(e.to_string())
            });
        }
    }

    /// Drain any complete response frames buffered in the TCP socket.
    ///
    /// Called before write batches to flush stale read responses left over
    /// from the previous poll cycle. Uses short timeouts so it returns
    /// quickly once the buffer is empty.
    pub async fn drain_stale_frames(&mut self) {
        let stream = match self.stream.as_mut() {
            Some(s) => s,
            None => return,
        };

        let mut drained = 0usize;
        loop {
            let mut header = [0u8; 6];
            match tokio::time::timeout(Duration::from_millis(200), stream.read_exact(&mut header))
                .await
            {
                Ok(Ok(_)) => {
                    let length = u16::from_be_bytes([header[4], header[5]]) as usize;
                    if length > 0 && length < MAX_RESPONSE_SIZE {
                        let mut rest = vec![0u8; length];
                        let _ = tokio::time::timeout(
                            Duration::from_millis(200),
                            stream.read_exact(&mut rest),
                        )
                        .await;
                    }
                    drained += 1;
                }
                _ => break, // timeout or error — buffer is clear
            }
        }

        if drained > 0 {
            tracing::info!("Drained {drained} stale frame(s)");
        }
    }

    /// Send a raw frame and read back one response frame.
    ///
    /// Convenience wrapper that sends a request and reads one response.
    /// Checks for Modbus exception responses and returns `Err` for them.
    async fn send_and_receive(&mut self, frame: &[u8]) -> Result<DecodedFrame, ClientError> {
        let t0 = std::time::Instant::now();
        self.send_raw(frame).await?;
        let send_elapsed = t0.elapsed();

        // Log the raw request frame at debug level. This is essential for
        // diagnosing dongles that accept TCP but never respond — we need to
        // see the serial, slave address, and register range we sent.
        let hex_preview: String = frame
            .iter()
            .take(30)
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ");
        tracing::debug!(
            req_len = frame.len(),
            send_ms = send_elapsed.as_millis() as u64,
            "Sent request, awaiting response: [{}]",
            hex_preview,
        );

        let decoded = self.receive_frame().await?;
        let total_elapsed = t0.elapsed();

        // Log response timing at debug level for diagnostics.
        tracing::debug!(
            resp_slave = decoded.slave,
            resp_func = decoded.function,
            resp_payload_len = decoded.payload.len(),
            total_ms = total_elapsed.as_millis() as u64,
            "Response received"
        );

        // Check for Modbus exception response (function code with high bit set)
        if decoded.function >= 0x80 {
            let exception_code = decoded.payload.first().copied().unwrap_or(0);
            return Err(ClientError::InvalidResponse(format!(
                "Modbus exception: function 0x{:02X}, code {}",
                decoded.function, exception_code
            )));
        }

        Ok(decoded)
    }

    // -----------------------------------------------------------------------
    // Register operations
    // -----------------------------------------------------------------------

    /// Maximum number of registers to request in a single Modbus read.
    /// The GivEnergy WiFi/Ethernet dongle has a limited frame buffer and will
    /// return fewer registers than requested if this is exceeded.
    const MAX_REGISTERS_PER_READ: u16 = 20;

    /// Inter-request delay to avoid overwhelming the GivEnergy dongle.
    /// The dongle has a very slow processor and limited frame buffer.
    /// The givenergy-modbus reference library uses 250ms; we use 150ms
    /// as a compromise between reliability and poll speed.
    const INTER_REQUEST_DELAY: Duration = Duration::from_millis(150);

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
                tokio::time::sleep(Self::INTER_REQUEST_DELAY).await;
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
        let reg_type_name = match register_type {
            RegisterType::Input => "IR",
            RegisterType::Holding => "HR",
        };

        for attempt in 0..=Self::MAX_STALE_RETRIES {
            // Build the request frame
            let request =
                framer::build_read_request(&self.serial, self.slave, register_type, start, count);

            // Log the first attempt at info level so it's visible in the
            // developer console even at default log level.
            if attempt == 0 {
                tracing::debug!(
                    "Reading {reg_type_name} {start}..{} ({} regs) from slave 0x{:02X}, serial={:?}",
                    start + count - 1,
                    count,
                    self.slave,
                    if self.serial.is_empty() { "<auto-discover>" } else { &self.serial },
                );
            }

            // Send and receive
            let decoded = self.send_and_receive(&request).await?;

            // Check for stale response (slave/function/base register from a previous request).
            if decoded.slave != self.slave {
                if attempt < Self::MAX_STALE_RETRIES {
                    tracing::debug!(
                        "Stale response (got slave 0x{:02X}, expected 0x{:02X}) — retrying ({}/{})",
                        decoded.slave,
                        self.slave,
                        attempt + 1,
                        Self::MAX_STALE_RETRIES,
                    );
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                return Err(ClientError::InvalidResponse(format!(
                    "slave mismatch: expected 0x{:02X}, got 0x{:02X}",
                    self.slave, decoded.slave
                )));
            }

            if decoded.function != expected_fc {
                if attempt < Self::MAX_STALE_RETRIES {
                    tracing::debug!(
                        "Stale response (got 0x{:02X}, expected 0x{:02X}) — retrying ({}/{})",
                        decoded.function,
                        expected_fc,
                        attempt + 1,
                        Self::MAX_STALE_RETRIES,
                    );
                    // Brief pause before retry to let the dongle catch up
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                return Err(ClientError::InvalidResponse(format!(
                    "function code mismatch: expected 0x{:02X}, got 0x{:02X}",
                    expected_fc, decoded.function
                )));
            }

            let (resp_base_register, resp_register_count) =
                Self::response_register_metadata(&decoded)?;
            if resp_base_register != start || resp_register_count > count {
                if attempt < Self::MAX_STALE_RETRIES {
                    tracing::debug!(
                        "Stale response (got {reg_type_name} {}..{} count {}, expected {}..{} count {}) — retrying ({}/{})",
                        resp_base_register,
                        resp_base_register.saturating_add(resp_register_count.saturating_sub(1)),
                        resp_register_count,
                        start,
                        start + count - 1,
                        count,
                        attempt + 1,
                        Self::MAX_STALE_RETRIES,
                    );
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                return Err(ClientError::InvalidResponse(format!(
                    "register range mismatch: expected {}..{} count {}, got {}..{} count {}",
                    start,
                    start + count - 1,
                    count,
                    resp_base_register,
                    resp_base_register.saturating_add(resp_register_count.saturating_sub(1)),
                    resp_register_count,
                )));
            }

            // --- Valid response, parse register data ---
            return Self::parse_register_response(&decoded, count);
        }

        // Unreachable, but keeps the compiler happy
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
    /// The CRC/check field is CrcModbus(function_code + register + value),
    /// which differs from the standard Modbus CRC over the full PDU.
    /// However, the dongle appears to ignore the CRC on incoming requests.
    ///
    /// Handles stale read responses and dongle-busy exceptions (code 67)
    /// with automatic retries.
    pub async fn write_register(&mut self, register: u16, value: u16) -> Result<(), ClientError> {
        let inner_function: u8 = 6; // Write Single Holding Register
        let device_address: u8 = 0x11; // Inverter setup address for writes

        // Build inner payload: register(2) + value(2)
        let mut payload = Vec::with_capacity(4);
        payload.extend_from_slice(&register.to_be_bytes());
        payload.extend_from_slice(&value.to_be_bytes());

        // Calculate check/CRC per givenergy-modbus convention:
        // CrcModbus(function_code + register + value)
        let mut check_data = Vec::with_capacity(5);
        check_data.push(inner_function);
        check_data.extend_from_slice(&register.to_be_bytes());
        check_data.extend_from_slice(&value.to_be_bytes());
        let check = crc16_modbus(&check_data);
        payload.extend_from_slice(&check.to_le_bytes());

        // Encode the full frame
        let request = framer::encode_frame(&self.serial, device_address, inner_function, &payload);

        // GivEnergy dongles return exception code 67 (busy) frequently.
        // We retry a few times with moderate delays, but fail fast rather
        // than blocking the entire poll loop.  Some registers (e.g. HR 32)
        // appear to be persistently unwritable on certain inverter models —
        // no amount of retrying will help.
        let max_attempts: u8 = 6;
        let mut need_resend = true;

        for attempt in 0..max_attempts {
            if need_resend {
                self.send_raw(&request).await?;
                need_resend = false;
            }

            let decoded = match self.receive_frame().await {
                Ok(d) => d,
                Err(e) => {
                    if attempt + 1 < max_attempts {
                        tracing::debug!("Write read error at {register}: {e}");
                        need_resend = true;
                        continue;
                    }
                    return Err(e);
                }
            };

            // Exception response
            if decoded.function >= 0x80 {
                let code = decoded.payload.first().copied().unwrap_or(0);
                if code == 67 && attempt + 1 < max_attempts {
                    tracing::debug!(
                        "Write at {register} got exception 67 (busy), retrying ({}/{})",
                        attempt + 1,
                        max_attempts
                    );
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    need_resend = true;
                    continue;
                }
                if code == 67 {
                    // Exception 67 on final attempt: known limitation on some
                    // registers (e.g. HR 32 on AC Coupled). Treat as soft failure
                    // — the inverter may have still processed the write, and
                    // the master enable flag (HR 96/59) will handle the intent.
                    tracing::warn!(
                        "Write at {register} got exception 67 after {max_attempts} retries — treating as acknowledged"
                    );
                    return Ok(());
                }
                return Err(ClientError::InvalidResponse(format!(
                    "Modbus exception: function 0x{:02X}, code {}",
                    decoded.function, code
                )));
            }

            // Wrong function code — stale read response, drain it
            if decoded.function != inner_function {
                if attempt + 1 < max_attempts {
                    tracing::debug!(
                        "Write at {register} got stale response (func 0x{:02X}), resending ({}/{})",
                        decoded.function,
                        attempt + 1,
                        max_attempts
                    );
                    need_resend = true;
                    continue;
                }
                return Err(ClientError::InvalidResponse(format!(
                    "function code mismatch: expected 0x{:02X}, got 0x{:02X}",
                    inner_function, decoded.function
                )));
            }

            // Correct function code (6) — verify it's our ack
            // Response payload: inverter_serial(10) + register(2) + value(2) + check(2)
            // After decode_frame strips slave+func+CRC, we have: inverter_serial(10) + register(2) + value(2)
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
                // Stale write ack from a previous write
                if attempt + 1 < max_attempts {
                    tracing::debug!(
                        "Write at {register} got stale ack (reg {resp_register}), draining"
                    );
                    continue;
                }
                return Err(ClientError::InvalidResponse(format!(
                    "write acknowledgment mismatch: register {} vs {}",
                    resp_register, register
                )));
            }

            // Success
            tracing::debug!("Write ack: register {register} = {value} (0x{value:04X})");
            return Ok(());
        }

        Err(ClientError::InvalidResponse(
            "exhausted write retries".to_string(),
        ))
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
                tokio::time::sleep(Self::INTER_REQUEST_DELAY).await;
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
                    tracing::warn!(
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
    pub async fn read_all_with_extras(
        &mut self,
        device_type: Option<&crate::inverter::model::DeviceType>,
    ) -> Result<Vec<BlockRead>, ClientError> {
        let mut results = self.read_blocks(STANDARD_POLL_BLOCKS).await?;

        if let Some(dt) = device_type {
            for block in model_specific_blocks_in_poll_order(dt) {
                // Pause between blocks to let the dongle catch up.
                tokio::time::sleep(Self::INTER_REQUEST_DELAY).await;

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

        Ok(results)
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
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
        assert_eq!(STANDARD_POLL_BLOCKS.len(), 3);
        assert_eq!(STANDARD_POLL_BLOCKS[0].name, "input_0_59");
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
    fn ac_coupled_model_specific_poll_order_still_reads_ac_config() {
        use crate::inverter::model::DeviceType;

        let blocks = model_specific_blocks_in_poll_order(&DeviceType::ACCoupled);
        let names: Vec<&str> = blocks.iter().map(|b| b.name).collect();

        assert_eq!(names, vec!["holding_300_359"]);
    }
}
