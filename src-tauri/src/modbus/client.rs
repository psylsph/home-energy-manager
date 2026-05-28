//! Modbus TCP client.
//!
//! Manages the TCP connection to the inverter, sending requests
//! and reading responses with configurable timeouts and retries.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::framer::{self, DecodedFrame, RegisterType};
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
    serial: String,
    /// Modbus slave address (typically 0x32).
    slave: u8,
    /// Underlying TCP stream, `None` when disconnected.
    stream: Option<TcpStream>,
    /// Timeout for individual read/write operations.
    timeout: Duration,
}

impl ModbusClient {
    /// Create a new client that will connect to `host:port` using the given
    /// adapter `serial` number.
    pub fn new(host: &str, port: u16, serial: &str) -> Self {
        Self {
            host: host.to_string(),
            port,
            serial: serial.to_string(),
            slave: 0x32, // default GivEnergy slave address
            stream: None,
            timeout: Duration::from_secs(5),
        }
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

    /// Send a raw frame and read back the response frame.
    ///
    /// The response is read in a streaming fashion: we first read enough bytes
    /// to determine the length from the header, then read the remaining payload.
    async fn send_and_receive(&mut self, frame: &[u8]) -> Result<DecodedFrame, ClientError> {
        let stream = self.stream.as_mut().ok_or(ClientError::NotConnected)?;

        // --- Send ---
        tokio::time::timeout(self.timeout, stream.write_all(frame))
            .await
            .map_err(|_| ClientError::Timeout)?
            .map_err(|e| ClientError::SendFailed(e.to_string()))?;

        // --- Receive ---
        // We need at least 6 bytes to read the length field (bytes 4-5).
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

        // Decode using the framer
        let decoded =
            framer::decode_frame(&full).map_err(|e| ClientError::FrameError(e.to_string()))?;

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

    /// Internal: read a single chunk of registers (no splitting).
    ///
    /// Tolerates the dongle returning fewer registers than requested — common
    /// on GivEnergy WiFi/Ethernet dongles with limited frame buffers.
    async fn read_registers_raw(
        &mut self,
        register_type: RegisterType,
        start: u16,
        count: u16,
    ) -> Result<Vec<u16>, ClientError> {
        // Build the request frame
        let request =
            framer::build_read_request(&self.serial, self.slave, register_type, start, count);

        // Send and receive
        let decoded = self.send_and_receive(&request).await?;

        // Verify the function code matches
        let expected_fc = register_type.function_code();
        if decoded.function != expected_fc {
            return Err(ClientError::InvalidResponse(format!(
                "function code mismatch: expected 0x{:02X}, got 0x{:02X}",
                expected_fc, decoded.function
            )));
        }

        // Parse the transparent response inner payload.
        // GivEnergy response format (after outer header + CRC stripped by framer):
        //   inverter_serial_number (10 bytes)
        //   base_register          (2 bytes, u16 BE)
        //   register_count         (2 bytes, u16 BE)
        //   register_values        (register_count * 2 bytes)
        //   check                  (2 bytes)
        //
        // The framer strips: outer MBAP header(6) + data_adapter_serial(10) +
        // padding(8) + CRC(2), leaving us with:
        //   device_address(1) + transparent_function(1) + [above inner payload]
        //
        // decoded.payload = everything after device_address + transparent_function.
        let payload = &decoded.payload;
        const INVERTER_SERIAL_LEN: usize = 10;
        const MIN_INNER_LEN: usize = INVERTER_SERIAL_LEN + 4; // serial + base_reg + reg_count

        if payload.len() < MIN_INNER_LEN {
            return Err(ClientError::InvalidResponse(format!(
                "response payload too short: need at least {} bytes for inner header, got {}",
                MIN_INNER_LEN,
                payload.len()
            )));
        }

        // Skip inverter serial number (10 bytes)
        let inner = &payload[INVERTER_SERIAL_LEN..];

        // Read base_register and register_count
        let _resp_base_register = u16::from_be_bytes([inner[0], inner[1]]);
        let resp_register_count = u16::from_be_bytes([inner[2], inner[3]]) as usize;

        // Register values follow
        let reg_data = &inner[4..];
        let max_values = count as usize;
        let actual_count = resp_register_count.min(max_values).min(reg_data.len() / 2);

        let mut values = Vec::with_capacity(actual_count);
        for chunk in reg_data.chunks_exact(2).take(actual_count) {
            values.push(u16::from_be_bytes([chunk[0], chunk[1]]));
        }

        Ok(values)
    }

    /// Write multiple holding registers (function code 0x10).
    ///
    /// Sends a Modbus write-multiple-registers request and verifies the
    /// acknowledgment from the device.
    pub async fn write_registers(&mut self, start: u16, values: &[u16]) -> Result<(), ClientError> {
        // Build the write-multiple-registers inner payload:
        //   start address (2 bytes) + quantity (2 bytes) + byte count (1) + values
        let quantity = values.len() as u16;
        let byte_count = (values.len() * 2) as u8;

        let mut payload = Vec::with_capacity(5 + values.len() * 2);
        payload.extend_from_slice(&start.to_be_bytes());
        payload.extend_from_slice(&quantity.to_be_bytes());
        payload.push(byte_count);
        for &val in values {
            payload.extend_from_slice(&val.to_be_bytes());
        }

        // Encode the full frame with function code 0x10
        let request = framer::encode_frame(&self.serial, self.slave, 0x10, &payload);

        // Send and receive
        let decoded = self.send_and_receive(&request).await?;

        // Verify function code
        if decoded.function != 0x10 {
            return Err(ClientError::InvalidResponse(format!(
                "function code mismatch: expected 0x10, got 0x{:02X}",
                decoded.function
            )));
        }

        // For a write response, payload format is:
        //   inverter_serial (10 bytes) + start address (2) + quantity (2)
        if decoded.payload.len() < 14 {
            return Err(ClientError::InvalidResponse(format!(
                "write response payload too short: need at least 14 bytes, got {}",
                decoded.payload.len()
            )));
        }

        // Skip inverter serial number (10 bytes)
        let inner = &decoded.payload[10..];
        let resp_start = u16::from_be_bytes([inner[0], inner[1]]);
        let resp_qty = u16::from_be_bytes([inner[2], inner[3]]);

        if resp_start != start || resp_qty != quantity {
            return Err(ClientError::InvalidResponse(format!(
                "write acknowledgment mismatch: start {} vs {}, quantity {} vs {}",
                resp_start, start, resp_qty, quantity
            )));
        }

        Ok(())
    }

    /// Read a set of poll blocks, returning raw data per block.
    ///
    /// Iterates over the provided blocks and issues a read request for each
    /// one. If any block fails the entire operation fails.
    pub async fn read_blocks(&mut self, blocks: &'static [RegisterBlock]) -> Result<Vec<BlockRead>, ClientError> {
        let mut results = Vec::with_capacity(blocks.len());

        for block in blocks {
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
