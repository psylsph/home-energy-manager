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
    /// Modbus slave address (typically 0x32).
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
            slave: 0x32, // default GivEnergy slave address
            stream: None,
            timeout: Duration::from_secs(5),
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

    /// Update the serial (e.g. after auto-discovery).
    pub fn set_serial(&mut self, serial: String) {
        self.serial = serial;
    }

    /// Set the Modbus slave address (default is `0x32`).
    pub fn set_slave(&mut self, slave: u8) {
        self.slave = slave;
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
        let stream = tokio::time::timeout(self.timeout, TcpStream::connect(&addr))
            .await
            .map_err(|_| ClientError::Timeout)?
            .map_err(|e| ClientError::ConnectionFailed(e.to_string()))?;

        // Set the read/write timeout on the stream
        stream
            .set_nodelay(true)
            .map_err(|e| ClientError::ConnectionFailed(e.to_string()))?;

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
    /// Reads the MBAP header to determine length, then reads the rest.
    /// Does NOT check for Modbus exceptions — callers handle those.
    async fn receive_frame(&mut self) -> Result<DecodedFrame, ClientError> {
        let stream = self.stream.as_mut().ok_or(ClientError::NotConnected)?;

        // Read the 6-byte MBAP header
        let mut header_buf = [0u8; 6];
        tokio::time::timeout(self.timeout, stream.read_exact(&mut header_buf))
            .await
            .map_err(|_| ClientError::Timeout)?
            .map_err(|e| ClientError::ReceiveFailed(e.to_string()))?;

        // Parse length field (big-endian u16 at bytes 4-5).
        let length = u16::from_be_bytes([header_buf[4], header_buf[5]]) as usize;

        if length > MAX_RESPONSE_SIZE {
            return Err(ClientError::InvalidResponse(format!(
                "response length {} exceeds maximum {}",
                length, MAX_RESPONSE_SIZE
            )));
        }

        // Read the remaining `length` bytes (everything after the 6-byte MBAP header).
        let mut rest = vec![0u8; length];
        tokio::time::timeout(self.timeout, stream.read_exact(&mut rest))
            .await
            .map_err(|_| ClientError::Timeout)?
            .map_err(|e| ClientError::ReceiveFailed(e.to_string()))?;

        // Reassemble the complete frame
        let mut full = Vec::with_capacity(6 + length);
        full.extend_from_slice(&header_buf);
        full.extend_from_slice(&rest);

        // Auto-discover dongle serial from response header (bytes 8-17).
        if self.serial.is_empty() && full.len() >= 18 {
            let discovered = std::str::from_utf8(&full[8..18])
                .unwrap_or("")
                .trim_end()
                .to_string();
            if !discovered.is_empty() {
                tracing::info!(serial = %discovered, "Auto-discovered dongle serial");
                self.serial = discovered;
                self.serial_discovered = true;
            }
        }

        // Decode using the framer
        framer::decode_frame(&full).map_err(|e| ClientError::FrameError(e.to_string()))
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
        self.send_raw(frame).await?;
        let decoded = self.receive_frame().await?;

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
                for _ in 0..(chunk_size - returned) {
                    all_values.push(0);
                }
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
    /// slave addresses (0x33–0x37) without affecting the default slave (0x32)
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
            // Build the request frame
            let request =
                framer::build_read_request(&self.serial, self.slave, register_type, start, count);

            // Send and receive
            let decoded = self.send_and_receive(&request).await?;

            // Check for stale response (function code from a previous request)
            if decoded.function != expected_fc {
                if attempt < Self::MAX_STALE_RETRIES {
                    tracing::debug!(
                        "Stale response (got 0x{:02X}, expected 0x{:02X}) — retrying ({}/{})",
                        decoded.function, expected_fc,
                        attempt + 1, Self::MAX_STALE_RETRIES,
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

            // --- Valid response, parse register data ---
            return Self::parse_register_response(&decoded, count);
        }

        // Unreachable, but keeps the compiler happy
        Err(ClientError::InvalidResponse("exhausted retries".to_string()))
    }

    /// Parse register values from a decoded read response frame.
    fn parse_register_response(
        decoded: &DecodedFrame,
        count: u16,
    ) -> Result<Vec<u16>, ClientError> {

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
        let _resp_base_register = u16::from_be_bytes([inner[0], inner[1]]);
        let resp_register_count = u16::from_be_bytes([inner[2], inner[3]]) as usize;

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
        let request = framer::encode_frame(
            &self.serial,
            device_address,
            inner_function,
            &payload,
        );

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
                        attempt + 1, max_attempts
                    );
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    need_resend = true;
                    continue;
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
                        decoded.function, attempt + 1, max_attempts
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

        Err(ClientError::InvalidResponse("exhausted write retries".to_string()))
    }

    /// Read a set of poll blocks, returning raw data per block.
    ///
    /// Iterates over the provided blocks and issues a read request for each
    /// one. If any block fails the entire operation fails.
    pub async fn read_blocks(&mut self, blocks: &'static [RegisterBlock]) -> Result<Vec<BlockRead>, ClientError> {
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

            let data = self
                .read_registers(reg_type, block.start, block.count)
                .await?;
            results.push(BlockRead { block, data });
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
        assert_eq!(client.slave, 0x32);
        client.set_slave(0x01);
        assert_eq!(client.slave, 0x01);
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
}
