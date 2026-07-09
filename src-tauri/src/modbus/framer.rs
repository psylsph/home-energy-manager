//! GivEnergy proprietary Modbus TCP frame encoder/decoder.
//!
//! Frame format:
//! ```text
//! Bytes 0-1:    Transaction ID      — always 0x5959
//! Bytes 2-3:    Protocol ID         — always 0x01
//! Bytes 4-5:    Length              — byte count of all following bytes
//! Byte  6:      Unit ID             — always 0x01
//! Byte  7:      Function ID         — 0x02 (transparent message)
//! Bytes 8-17:   Data adapter serial — 10 bytes, Latin-1
//! Bytes 18-25:  Padding             — big-endian u64 value 8 (0x0000000000000008)
//! Byte  26:     Slave address
//! Byte  27:     Inner function code (0x03=read holding, 0x04=read input, 0x06=write single register)
//! Bytes 28+:    Inner payload
//! Last 2 bytes: CRC-16/Modbus over bytes 26+
//! ```

use crc::{Crc, CRC_16_MODBUS};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Fixed transaction ID for GivEnergy frames.
const TRANSACTION_ID: u16 = 0x5959;

/// Fixed Modbus protocol identifier.
const PROTOCOL_ID: u16 = 0x0001;

/// Fixed unit identifier.
pub(crate) const UNIT_ID: u8 = 0x01;

/// Transparent-message function code used by the GivEnergy data adapter.
pub(crate) const FUNCTION_ID_TRANSPARENT: u8 = 0x02;

/// Heartbeat-request function code sent by the dongle every ~3 minutes.
/// The client must respond within 5 seconds; after 3 missed heartbeats
/// the dongle closes the TCP socket.
const FUNCTION_ID_HEARTBEAT: u8 = 0x01;

/// Length of the data-adapter serial field (Latin-1, space-padded).
///
/// Shared between the framer and the client so the inner-header offset of
/// read/write responses stays in sync with the frame layout.
pub const SERIAL_LEN: usize = 10;

/// Header size: transaction ID (2) + protocol ID (2) + length (2) + unit ID (1)
/// + function ID (1) + serial (10) + padding (8) = 26 bytes.
pub(crate) const HEADER_SIZE: usize = 2 + 2 + 2 + 1 + 1 + SERIAL_LEN + 8;

/// CRC-16/Modbus lookup table (compile-time generated).
const CRC_ALGO: Crc<u16> = Crc::<u16>::new(&CRC_16_MODBUS);

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur during frame decoding.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FramerError {
    #[error("frame too short: expected at least {min} bytes, got {actual}")]
    TooShort { min: usize, actual: usize },

    #[error("invalid transaction ID: expected 0x5959, got 0x{0:04X}")]
    InvalidTransactionId(u16),

    #[error("invalid protocol ID: expected 0x0001, got 0x{0:04X}")]
    InvalidProtocolId(u16),

    #[error("invalid unit ID: expected 0x01, got 0x{0:02X}")]
    InvalidUnitId(u8),

    #[error("invalid function ID: expected 0x02, got 0x{0:02X}")]
    InvalidFunctionId(u8),

    #[error("length field mismatch: header says {header}, actual bytes available {actual}")]
    LengthMismatch { header: u16, actual: usize },

    #[error("CRC mismatch: expected 0x{expected:04X}, calculated 0x{calculated:04X}")]
    CrcMismatch { expected: u16, calculated: u16 },

    #[error(
        "inner payload too short for CRC: need at least 3 bytes (slave + func + CRC), got {0}"
    )]
    InnerPayloadTooShort(usize),
}

// ---------------------------------------------------------------------------
// Decoded frame
// ---------------------------------------------------------------------------

/// A successfully decoded GivEnergy Modbus response frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    /// The Modbus slave address from the inner frame.
    pub slave: u8,
    /// The inner Modbus function code (e.g. 0x03 read-holding response).
    pub function: u8,
    /// The inner payload bytes (everything between function code and CRC).
    pub payload: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Register type
// ---------------------------------------------------------------------------

/// The type of Modbus register to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterType {
    /// Read holding registers (function code 0x03).
    Holding,
    /// Read input registers (function code 0x04).
    Input,
}

impl RegisterType {
    /// Returns the Modbus function code for this register type.
    pub const fn function_code(self) -> u8 {
        match self {
            RegisterType::Holding => 0x03,
            RegisterType::Input => 0x04,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Calculate CRC-16/Modbus over the given data.
///
/// Uses the standard Modbus polynomial `0x8005` with initial value `0xFFFF`.
pub fn crc16_modbus(data: &[u8]) -> u16 {
    CRC_ALGO.checksum(data)
}

/// Encode the serial number string into exactly 10 bytes (Latin-1, space-padded).
///
/// Panics if the serial string is longer than 10 bytes.
pub(crate) fn encode_serial(serial: &str) -> [u8; SERIAL_LEN] {
    assert!(
        serial.len() <= SERIAL_LEN,
        "serial number must be at most {SERIAL_LEN} bytes, got {}",
        serial.len()
    );
    let mut buf = [b' '; SERIAL_LEN];
    let bytes = serial.as_bytes();
    buf[..bytes.len()].copy_from_slice(bytes);
    buf
}

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

/// Check whether a raw frame is a heartbeat request from the dongle.
///
/// Heartbeat frames have the same 0x59590001 MBAP header but function ID
/// 0x01 at byte 7 instead of 0x02. They are minimal — just the header.
pub fn is_heartbeat_request(data: &[u8]) -> bool {
    data.len() >= 8
        && data[0] == 0x59
        && data[1] == 0x59
        && data[2] == 0x00
        && data[3] == 0x01
        && data[7] == FUNCTION_ID_HEARTBEAT
        && u16::from_be_bytes([data[4], data[5]]) as usize == data.len() - 6
}

/// Build a heartbeat response frame by echoing the request back to the dongle.
///
/// The response must be sent within 5 seconds. After 3 missed heartbeats the
/// dongle closes the TCP connection.
pub fn build_heartbeat_response(request: &[u8]) -> Vec<u8> {
    request.to_vec()
}

/// Build a complete GivEnergy Modbus TCP frame.
///
/// The frame consists of a GivEnergy header wrapping an inner Modbus request
/// (slave address + function code + payload) with a CRC-16/Modbus trailer.
///
/// # Arguments
/// * `serial` — data adapter serial number (up to 10 Latin-1 characters)
/// * `slave` — Modbus slave address
/// * `function` — inner Modbus function code (e.g. 0x03, 0x04, 0x10)
/// * `payload` — inner payload bytes (register address, count, values, etc.)
///
/// # Returns
/// The fully assembled frame as a `Vec<u8>`.
pub fn encode_frame(serial: &str, slave: u8, function: u8, payload: &[u8]) -> Vec<u8> {
    let serial_bytes = encode_serial(serial);

    // Build inner PDU first: slave + function + payload
    let mut inner = Vec::with_capacity(2 + payload.len());
    inner.push(slave);
    inner.push(function);
    inner.extend_from_slice(payload);

    // Append CRC over the inner PDU (slave + function + payload)
    let crc = crc16_modbus(&inner);
    inner.extend_from_slice(&crc.to_le_bytes());

    // Calculate the length field: unit_id(1) + function_id(1) + serial(10) + padding(8) + inner_pdu
    let length = (1 + 1 + SERIAL_LEN + 8 + inner.len()) as u16;

    // Assemble the full frame
    let mut frame = Vec::with_capacity(HEADER_SIZE + inner.len());
    frame.extend_from_slice(&TRANSACTION_ID.to_be_bytes()); // bytes 0-1
    frame.extend_from_slice(&PROTOCOL_ID.to_be_bytes()); // bytes 2-3
    frame.extend_from_slice(&length.to_be_bytes()); // bytes 4-5
    frame.push(UNIT_ID); // byte  6
    frame.push(FUNCTION_ID_TRANSPARENT); // byte  7
    frame.extend_from_slice(&serial_bytes); // bytes 8-17
    frame.extend_from_slice(&8u64.to_be_bytes()); // bytes 18-25 (big-endian padding)
    frame.extend_from_slice(&inner); // bytes 26+

    frame
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

/// Decode a GivEnergy Modbus TCP response frame.
///
/// Validates the header fields and verifies the CRC-16/Modbus checksum
/// over the inner payload.
///
/// # Arguments
/// * `data` — raw bytes received from the data adapter
///
/// # Returns
/// A `DecodedFrame` on success, or a `FramerError` describing what went wrong.
pub fn decode_frame(data: &[u8]) -> Result<DecodedFrame, FramerError> {
    // We need at least the fixed header (26 bytes) + slave(1) + func(1) + CRC(2) = 30 bytes.
    let min_bytes = HEADER_SIZE + 4;
    if data.len() < min_bytes {
        return Err(FramerError::TooShort {
            min: min_bytes,
            actual: data.len(),
        });
    }

    // --- Validate fixed header fields ---
    let transaction_id = u16::from_be_bytes([data[0], data[1]]);
    if transaction_id != TRANSACTION_ID {
        return Err(FramerError::InvalidTransactionId(transaction_id));
    }

    let protocol_id = u16::from_be_bytes([data[2], data[3]]);
    if protocol_id != PROTOCOL_ID {
        return Err(FramerError::InvalidProtocolId(protocol_id));
    }

    let unit_id = data[6];
    // Reference framer accepts both 0x00 (Android/iOS app) and 0x01.
    if unit_id != 0x00 && unit_id != UNIT_ID {
        return Err(FramerError::InvalidUnitId(unit_id));
    }

    let function_id = data[7];
    if function_id != FUNCTION_ID_TRANSPARENT {
        return Err(FramerError::InvalidFunctionId(function_id));
    }

    // --- Validate length field ---
    let length = u16::from_be_bytes([data[4], data[5]]) as usize;
    // Length field counts bytes *after* itself (i.e. from byte 6 onwards).
    let bytes_after_length = data.len() - 6;
    if length != bytes_after_length {
        return Err(FramerError::LengthMismatch {
            header: length as u16,
            actual: bytes_after_length,
        });
    }

    // --- Extract inner PDU (from byte 26 to end) ---
    let inner_pdu = &data[HEADER_SIZE..];
    if inner_pdu.len() < 4 {
        // Need at least: slave(1) + func(1) + CRC(2)
        return Err(FramerError::InnerPayloadTooShort(inner_pdu.len()));
    }

    // Split off the trailing CRC (last 2 bytes)
    let crc_offset = inner_pdu.len() - 2;
    let crc_bytes = &inner_pdu[crc_offset..];
    let received_crc = u16::from_le_bytes([crc_bytes[0], crc_bytes[1]]);

    // The reference library (givenergy-modbus) notes: "it is unclear how a
    // response CRC is calculated or should be verified" and does not
    // validate response CRCs.  We log mismatches but don't reject frames,
    // since the CRC algorithm for responses differs from the standard
    // Modbus CRC-16 we compute here.
    let calculated_crc = crc16_modbus(&inner_pdu[..crc_offset]);
    if received_crc != calculated_crc {
        tracing::debug!(
            "Response CRC mismatch: received 0x{:04X}, calculated 0x{:04X}",
            received_crc,
            calculated_crc
        );
    }

    // --- Build decoded frame ---
    let slave = inner_pdu[0];
    let function = inner_pdu[1];
    let payload = inner_pdu[2..crc_offset].to_vec();

    Ok(DecodedFrame {
        slave,
        function,
        payload,
    })
}

// ---------------------------------------------------------------------------
// High-level request builder
// ---------------------------------------------------------------------------

/// Build a Modbus read-register request wrapped in a GivEnergy frame.
///
/// # Arguments
/// * `serial` — data adapter serial number (up to 10 Latin-1 characters)
/// * `slave` — Modbus slave address
/// * `register_type` — `Input` (function 0x04) or `Holding` (function 0x03)
/// * `start` — starting register address (0-based)
/// * `count` — number of registers to read
///
/// # Returns
/// A fully encoded GivEnergy frame ready to send to the data adapter.
pub fn build_read_request(
    serial: &str,
    slave: u8,
    register_type: RegisterType,
    start: u16,
    count: u16,
) -> Vec<u8> {
    // Modbus read request payload: start address (2 bytes) + count (2 bytes)
    let mut payload = Vec::with_capacity(4);
    payload.extend_from_slice(&start.to_be_bytes());
    payload.extend_from_slice(&count.to_be_bytes());

    encode_frame(serial, slave, register_type.function_code(), &payload)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // CRC tests
    // -----------------------------------------------------------------------

    #[test]
    fn crc_known_value() {
        // Standard Modbus CRC test vector: "123456789" → 0x4B37
        let crc = crc16_modbus(b"123456789");
        assert_eq!(crc, 0x4B37, "CRC-16/Modbus of '123456789' should be 0x4B37");
    }

    #[test]
    fn crc_empty_input() {
        let crc = crc16_modbus(b"");
        assert_eq!(
            crc, 0xFFFF,
            "CRC-16/Modbus of empty input should be initial value 0xFFFF"
        );
    }

    #[test]
    fn crc_single_byte() {
        // CRC of a single zero byte
        let crc = crc16_modbus(&[0x00]);
        assert_eq!(crc, 0x40BF, "CRC-16/Modbus of [0x00] should be 0x40BF");
    }

    // -----------------------------------------------------------------------
    // Serial encoding
    // -----------------------------------------------------------------------

    #[test]
    fn serial_encoding_short_serial() {
        let encoded = encode_serial("SA1234");
        assert_eq!(&encoded[..6], b"SA1234");
        assert_eq!(&encoded[6..], b"    "); // space-padded
    }

    #[test]
    fn serial_encoding_exact_length() {
        let encoded = encode_serial("SA12345678");
        assert_eq!(&encoded, b"SA12345678");
    }

    #[test]
    #[should_panic(expected = "serial number must be at most 10 bytes")]
    fn serial_encoding_too_long_panics() {
        let _ = encode_serial("SERIAL_TOO_LONG");
    }

    // -----------------------------------------------------------------------
    // Encode / decode roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn encode_decode_roundtrip() {
        let serial = "SA12345678";
        let slave = 0x32;
        let function = 0x03;
        let payload = vec![0x00, 0x00, 0x00, 0x0A]; // read 10 registers from address 0

        let frame = encode_frame(serial, slave, function, &payload);
        let decoded = decode_frame(&frame).expect("frame should decode successfully");

        assert_eq!(decoded.slave, slave);
        assert_eq!(decoded.function, function);
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn encode_decode_empty_payload() {
        let serial = "SN0001";
        let slave = 0x01;
        let function = 0x04;

        let frame = encode_frame(serial, slave, function, &[]);
        let decoded = decode_frame(&frame).expect("frame should decode successfully");

        assert_eq!(decoded.slave, slave);
        assert_eq!(decoded.function, function);
        assert!(decoded.payload.is_empty());
    }

    // -----------------------------------------------------------------------
    // Padding validation (big-endian)
    // -----------------------------------------------------------------------

    #[test]
    fn padding_is_big_endian() {
        let frame = encode_frame("SA1234", 0x01, 0x03, &[]);

        // Padding occupies bytes 18..26
        let padding = &frame[18..26];
        let padding_value =
            u64::from_be_bytes(padding.try_into().expect("padding should be 8 bytes"));
        assert_eq!(padding_value, 8, "padding must be big-endian u64 value 8");

        // Verify the actual bytes are 0x00 0x00 0x00 0x00 0x00 0x00 0x00 0x08
        assert_eq!(
            padding,
            &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08],
            "padding bytes must be big-endian representation of 8"
        );
    }

    // -----------------------------------------------------------------------
    // Header field validation
    // -----------------------------------------------------------------------

    #[test]
    fn frame_has_correct_header_fields() {
        let frame = encode_frame("SA1234", 0x01, 0x03, &[0x00, 0x01]);

        // Transaction ID
        assert_eq!(&frame[0..2], &[0x59, 0x59]);
        // Protocol ID
        assert_eq!(&frame[2..4], &[0x00, 0x01]);
        // Unit ID
        assert_eq!(frame[6], 0x01);
        // Function ID
        assert_eq!(frame[7], 0x02);
        // Serial
        assert_eq!(&frame[8..18], b"SA1234    ");
    }

    // -----------------------------------------------------------------------
    // Length field validation
    // -----------------------------------------------------------------------

    #[test]
    fn length_field_is_correct() {
        let payload = vec![0xAA, 0xBB, 0xCC];
        let frame = encode_frame("SA1234", 0x01, 0x03, &payload);

        let length = u16::from_be_bytes([frame[4], frame[5]]) as usize;
        // Length counts bytes from byte 6 onwards
        let expected_length = frame.len() - 6;
        assert_eq!(length, expected_length);
    }

    // -----------------------------------------------------------------------
    // Decode error cases
    // -----------------------------------------------------------------------

    #[test]
    fn decode_too_short() {
        let result = decode_frame(&[0x59, 0x59]);
        assert!(matches!(result, Err(FramerError::TooShort { .. })));
    }

    #[test]
    fn decode_invalid_transaction_id() {
        let mut frame = encode_frame("SA1234", 0x01, 0x03, &[0x00]);
        // Corrupt the transaction ID
        frame[0] = 0x00;
        frame[1] = 0x00;

        let result = decode_frame(&frame);
        assert!(matches!(
            result,
            Err(FramerError::InvalidTransactionId(0x0000))
        ));
    }

    #[test]
    fn decode_invalid_protocol_id() {
        let mut frame = encode_frame("SA1234", 0x01, 0x03, &[0x00]);
        // Corrupt the protocol ID
        frame[2] = 0x00;
        frame[3] = 0x02;

        let result = decode_frame(&frame);
        assert!(matches!(
            result,
            Err(FramerError::InvalidProtocolId(0x0002))
        ));
    }

    #[test]
    fn decode_invalid_unit_id() {
        let mut frame = encode_frame("SA1234", 0x01, 0x03, &[0x00]);
        // Corrupt the unit ID
        frame[6] = 0xFF;

        let result = decode_frame(&frame);
        assert!(matches!(result, Err(FramerError::InvalidUnitId(0xFF))));
    }

    #[test]
    fn decode_invalid_function_id() {
        let mut frame = encode_frame("SA1234", 0x01, 0x03, &[0x00]);
        // Corrupt the transparent function ID
        frame[7] = 0x03;

        let result = decode_frame(&frame);
        assert!(matches!(result, Err(FramerError::InvalidFunctionId(0x03))));
    }

    #[test]
    fn decode_crc_mismatch_now_lenient() {
        let mut frame = encode_frame("SA1234", 0x01, 0x03, &[0x00, 0x01]);
        // Corrupt the last byte (part of CRC)
        let last = frame.len() - 1;
        frame[last] ^= 0xFF;

        // CRC mismatches are now lenient (logged but not rejected)
        // because the response CRC algorithm is unknown per the reference library.
        let result = decode_frame(&frame);
        assert!(result.is_ok());
    }

    #[test]
    fn decode_length_mismatch() {
        let mut frame = encode_frame("SA1234", 0x01, 0x03, &[0x00, 0x01]);
        // Append an extra byte so length field no longer matches
        frame.push(0x00);

        let result = decode_frame(&frame);
        assert!(matches!(result, Err(FramerError::LengthMismatch { .. })));
    }

    // -----------------------------------------------------------------------
    // Register type
    // -----------------------------------------------------------------------

    #[test]
    fn register_type_function_codes() {
        assert_eq!(RegisterType::Holding.function_code(), 0x03);
        assert_eq!(RegisterType::Input.function_code(), 0x04);
    }

    // -----------------------------------------------------------------------
    // build_read_request
    // -----------------------------------------------------------------------

    #[test]
    fn build_read_holding_request() {
        let frame = build_read_request("SA1234", 0x32, RegisterType::Holding, 0x00, 10);
        let decoded = decode_frame(&frame).expect("should decode");

        assert_eq!(decoded.slave, 0x32);
        assert_eq!(decoded.function, 0x03);
        // Payload should be start address (0x0000) + count (0x000A)
        assert_eq!(decoded.payload, vec![0x00, 0x00, 0x00, 0x0A]);
    }

    #[test]
    fn build_read_input_request() {
        let frame = build_read_request("SN9999", 0x01, RegisterType::Input, 100, 5);
        let decoded = decode_frame(&frame).expect("should decode");

        assert_eq!(decoded.slave, 0x01);
        assert_eq!(decoded.function, 0x04);
        // start=100 → 0x0064, count=5 → 0x0005
        assert_eq!(decoded.payload, vec![0x00, 0x64, 0x00, 0x05]);
    }

    // -----------------------------------------------------------------------
    // CRC at end of inner PDU
    // -----------------------------------------------------------------------

    #[test]
    fn inner_crc_is_little_endian() {
        let frame = encode_frame("SA1234", 0x01, 0x03, &[0x00]);

        // The inner PDU starts at byte 26 (HEADER_SIZE)
        let inner_pdu = &frame[HEADER_SIZE..];
        let len = inner_pdu.len();
        // Last 2 bytes are the CRC in little-endian
        let crc_bytes = &inner_pdu[len - 2..];
        let crc = u16::from_le_bytes([crc_bytes[0], crc_bytes[1]]);

        // Manually calculate expected CRC
        let expected = crc16_modbus(&inner_pdu[..len - 2]);
        assert_eq!(crc, expected);
    }

    // -----------------------------------------------------------------------
    // Response frame decode (simulated)
    // -----------------------------------------------------------------------

    #[test]
    fn decode_simulated_read_response() {
        // Simulate a response to a read-holding-registers request:
        // slave=0x32, function=0x03, byte_count=4, data=0x1234 0x5678
        let mut inner = vec![0x32, 0x03, 0x04, 0x12, 0x34, 0x56, 0x78];
        let crc = crc16_modbus(&inner);
        inner.extend_from_slice(&crc.to_le_bytes());

        // Build a GivEnergy header
        let length = (1 + 1 + SERIAL_LEN + 8 + inner.len()) as u16;
        let mut frame = Vec::with_capacity(HEADER_SIZE + inner.len());
        frame.extend_from_slice(&TRANSACTION_ID.to_be_bytes());
        frame.extend_from_slice(&PROTOCOL_ID.to_be_bytes());
        frame.extend_from_slice(&length.to_be_bytes());
        frame.push(UNIT_ID);
        frame.push(FUNCTION_ID_TRANSPARENT);
        frame.extend_from_slice(b"SA1234    ");
        frame.extend_from_slice(&8u64.to_be_bytes());
        frame.extend_from_slice(&inner);

        let decoded = decode_frame(&frame).expect("should decode response");
        assert_eq!(decoded.slave, 0x32);
        assert_eq!(decoded.function, 0x03);
        assert_eq!(decoded.payload, vec![0x04, 0x12, 0x34, 0x56, 0x78]);
    }

    // -----------------------------------------------------------------------
    // Heartbeat detection and response
    // -----------------------------------------------------------------------

    #[test]
    fn heartbeat_is_detected() {
        // Build a minimal heartbeat frame: MBAP header with fid=0x01
        let heartbeat = vec![
            0x59, 0x59, // transaction ID
            0x00, 0x01, // protocol ID
            0x00, 0x02, // length = 2 (uid + fid only)
            0x01, // unit ID
            0x01, // function ID (heartbeat)
        ];
        assert!(is_heartbeat_request(&heartbeat));
    }

    #[test]
    fn heartbeat_with_garbage_length_is_rejected() {
        // Same MBAP header as a real heartbeat, but the length field doesn't
        // match the actual byte count — must not be echoed back.
        let heartbeat = vec![
            0x59, 0x59, // transaction ID
            0x00, 0x01, // protocol ID
            0xFF, 0xFF, // length = 65535 (garbage — should be 2)
            0x01, // unit ID
            0x01, // function ID (heartbeat)
        ];
        assert!(!is_heartbeat_request(&heartbeat));
    }

    #[test]
    fn heartbeat_response_echoes_request() {
        let request = vec![0x59, 0x59, 0x00, 0x01, 0x00, 0x02, 0x01, 0x01];
        let response = build_heartbeat_response(&request);
        assert_eq!(response, request);
    }

    #[test]
    fn normal_frame_is_not_heartbeat() {
        let frame = encode_frame("SA1234", 0x01, 0x03, &[0x00, 0x01]);
        assert!(!is_heartbeat_request(&frame));
    }
}
