/**
 * CRC-16/Modbus lookup table implementation.
 *
 * Polynomial: 0x8005 (reflected = 0xA001)
 * Initial value: 0xFFFF
 */

const TABLE = new Uint16Array(256);

// Build lookup table
for (let i = 0; i < 256; i++) {
  let crc = i;
  for (let j = 0; j < 8; j++) {
    if (crc & 1) {
      crc = (crc >> 1) ^ 0xA001;
    } else {
      crc >>= 1;
    }
  }
  TABLE[i] = crc;
}

/**
 * Compute CRC-16/Modbus over the given data.
 */
export function crc16(data: Buffer): number {
  let crc = 0xFFFF;
  for (let i = 0; i < data.length; i++) {
    crc = (crc >> 8) ^ TABLE[(crc ^ data[i]) & 0xFF];
  }
  return crc;
}
