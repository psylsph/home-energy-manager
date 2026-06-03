/**
 * Mock GivEnergy Modbus TCP server with HTTP admin API.
 *
 * Implements just enough of the GivEnergy proprietary Modbus TCP protocol
 * to satisfy the poll loop: it responds to register-read requests with
 * realistic data and captures register-write requests for test assertions.
 *
 * The mock server runs in the Playwright global-setup process. Test workers
 * (separate processes) query captured writes via the HTTP admin API.
 *
 * Frame format:
 *   Bytes 0-1:    Transaction ID (0x5959)
 *   Bytes 2-3:    Protocol ID  (0x0001)
 *   Bytes 4-5:    Length (byte count of everything after byte 5)
 *   Byte  6:      Unit ID (0x01)
 *   Byte  7:      Function ID (0x02 = transparent)
 *   Bytes 8-17:   Serial (10 bytes, Latin-1, space-padded)
 *   Bytes 18-25:  Padding (big-endian u64 = 8)
 *   Byte  26:     Slave address
 *   Byte  27:     Inner function code (0x03/0x04/0x06)
 *   Bytes 28+:    Inner payload
 *   Last 2 bytes: CRC-16/Modbus (LE)
 */

import * as net from 'net';
import * as http from 'http';
import { crc16 } from './crc16.js';

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface RegisterWrite {
  address: number;
  value: number;
}

// ---------------------------------------------------------------------------
// Register storage
// ---------------------------------------------------------------------------

/** Input registers (read-only telemetry), 120 registers. */
const inputRegs = new Uint16Array(120);

/** Holding registers (read/write config), 360 registers. */
const holdingRegs = new Uint16Array(360);

/** All register writes received by the server, in order. */
const writes: RegisterWrite[] = [];

/** Reset all state. */
export function resetState(): void {
  inputRegs.fill(0);
  holdingRegs.fill(0);
  writes.length = 0;
  populateDefaults();
}

/** Drain all captured writes and clear the list. */
export function drainWrites(): RegisterWrite[] {
  const result = [...writes];
  writes.length = 0;
  return result;
}

/** Get all captured writes without clearing. */
export function peekWrites(): RegisterWrite[] {
  return [...writes];
}

/** Set an input register value. */
export function setInputReg(addr: number, value: number): void {
  if (addr < inputRegs.length) inputRegs[addr] = value & 0xFFFF;
}

/** Set a holding register value. */
export function setHoldingReg(addr: number, value: number): void {
  if (addr < holdingRegs.length) holdingRegs[addr] = value & 0xFFFF;
}

/** Get a holding register value. */
export function getHoldingReg(addr: number): number {
  return addr < holdingRegs.length ? holdingRegs[addr] : 0;
}

// ---------------------------------------------------------------------------
// Default register values (realistic Gen3 Hybrid snapshot)
// ---------------------------------------------------------------------------

function populateDefaults(): void {
  // Device type: Gen3 Hybrid (0x2001)
  holdingRegs[0] = 0x2001;

  // Serial number: "SA12345678" in HR 13-17 (5 registers = 10 chars)
  const serial = Buffer.from('SA12345678');
  for (let i = 0; i < 5; i++) {
    holdingRegs[13 + i] = (serial[i * 2] << 8) | serial[i * 2 + 1];
  }

  // ARM firmware: version 352 → century 3 → Gen3 confirmed
  holdingRegs[21] = 352;

  // Battery power mode: 1 = self-consumption (eco)
  holdingRegs[27] = 1;

  // Enable discharge: false (eco mode)
  holdingRegs[59] = 0;

  // Battery SOC reserve: 4%
  holdingRegs[110] = 4;

  // Charge rate: 100%
  holdingRegs[111] = 100;

  // Discharge rate: 100%
  holdingRegs[112] = 100;

  // Active power rate: 100%
  holdingRegs[50] = 100;

  // Charge target SOC: 100
  holdingRegs[116] = 100;

  // Enable charge: false
  holdingRegs[96] = 0;

  // Enable charge target: false
  holdingRegs[20] = 0;

  // Charge slot 1: disabled (0,0)
  holdingRegs[94] = 0;
  holdingRegs[95] = 0;

  // Charge slot 2: disabled (0,0)
  holdingRegs[31] = 0;
  holdingRegs[32] = 0;

  // Discharge slot 1: disabled (0,0)
  holdingRegs[56] = 0;
  holdingRegs[57] = 0;

  // Discharge slot 2: disabled (0,0)
  holdingRegs[44] = 0;
  holdingRegs[45] = 0;

  // ---- Input registers (telemetry) ----

  // Status: 1 = normal
  inputRegs[0] = 1;

  // PV1 voltage: 350.0V → 3500 (0.1V units)
  inputRegs[1] = 3500;

  // PV2 voltage: 320.0V → 3200
  inputRegs[2] = 3200;

  // Grid voltage: 241.5V → 2415 (0.1V units)
  inputRegs[5] = 2415;

  // PV1 current: 3.5A → 35 (0.1A units)
  inputRegs[8] = 35;

  // PV2 current: 2.8A → 28
  inputRegs[9] = 28;

  // Grid frequency: 50.01Hz → 5001 (0.01Hz units)
  inputRegs[13] = 5001;

  // PV1 energy today: 12.5kWh → 125 (0.1kWh units)
  inputRegs[17] = 125;

  // PV1 power: 1225W
  inputRegs[18] = 1225;

  // PV2 energy today: 9.0kWh → 90
  inputRegs[19] = 90;

  // PV2 power: 896W
  inputRegs[20] = 896;

  // Today export energy: 2.3kWh → 23
  inputRegs[25] = 23;

  // Today import energy: 5.1kWh → 51
  inputRegs[26] = 51;

  // Grid power: -200W (importing) — signed, stored as two's complement
  inputRegs[30] = (-200) & 0xFFFF;

  // Today consumption: 15.2kWh → 152
  inputRegs[35] = 152;

  // Today charge energy: 8.0kWh → 80
  inputRegs[36] = 80;

  // Today discharge energy: 6.5kWh → 65
  inputRegs[37] = 65;

  // Inverter temperature: 42.5°C → 425 (0.1°C units)
  inputRegs[41] = 425;

  // Battery voltage: 51.20V → 5120 (0.01V units)
  inputRegs[50] = 5120;

  // Battery current: 5.00A → 500 (0.01A units, charging)
  inputRegs[51] = 500;

  // Battery power: 256W (charging, positive)
  inputRegs[52] = 256;

  // Battery temperature: 28.5°C → 285 (0.1°C units)
  inputRegs[56] = 285;

  // Battery SOC: 75%
  inputRegs[59] = 75;
}

// ---------------------------------------------------------------------------
// Frame encoding / decoding helpers
// ---------------------------------------------------------------------------

const TRANSACTION_ID = 0x5959;
const PROTOCOL_ID = 0x0001;
const UNIT_ID = 0x01;
const FUNCTION_TRANSPARENT = 0x02;
const SERIAL_LEN = 10;
const HEADER_SIZE = 2 + 2 + 2 + 1 + 1 + SERIAL_LEN + 8; // = 26

function encodeSerial(serial: string): Buffer {
  const buf = Buffer.alloc(SERIAL_LEN, 0x20); // space-padded
  Buffer.from(serial, 'latin1').copy(buf);
  return buf;
}

function buildFrame(serial: string, slave: number, func: number, payload: Buffer): Buffer {
  const serialBuf = encodeSerial(serial);

  // Inner PDU: slave + func + payload + CRC
  const innerPreCrc = Buffer.alloc(2 + payload.length);
  innerPreCrc[0] = slave;
  innerPreCrc[1] = func;
  payload.copy(innerPreCrc, 2);
  const crc = crc16(innerPreCrc);
  const inner = Buffer.concat([innerPreCrc, Buffer.from([crc & 0xFF, (crc >> 8) & 0xFF])]);

  const length = 1 + 1 + SERIAL_LEN + 8 + inner.length;

  const frame = Buffer.alloc(6 + length);
  frame.writeUInt16BE(TRANSACTION_ID, 0);
  frame.writeUInt16BE(PROTOCOL_ID, 2);
  frame.writeUInt16BE(length, 4);
  frame[6] = UNIT_ID;
  frame[7] = FUNCTION_TRANSPARENT;
  serialBuf.copy(frame, 8);
  frame.writeBigUInt64BE(8n, 18);
  inner.copy(frame, HEADER_SIZE);

  return frame;
}

/**
 * Build a read-response frame for input/holding registers.
 */
function buildReadResponse(
  serial: string,
  slave: number,
  func: number,
  baseRegister: number,
  regCount: number,
  regs: Uint16Array,
): Buffer {
  const dataLen = regCount * 2;
  const payloadLen = 10 + 4 + dataLen;
  const payload = Buffer.alloc(payloadLen);

  // Inverter serial (10 bytes)
  const invSerial = encodeSerial('SA12345678');
  invSerial.copy(payload, 0);

  // Base register
  payload.writeUInt16BE(baseRegister, 10);
  // Register count
  payload.writeUInt16BE(regCount, 12);

  // Register values (big-endian u16)
  for (let i = 0; i < regCount; i++) {
    const addr = baseRegister + i;
    const val = addr < regs.length ? regs[addr] : 0;
    payload.writeUInt16BE(val, 14 + i * 2);
  }

  return buildFrame(serial, slave, func, payload);
}

/**
 * Build a write-response (FC6 ack) frame.
 */
function buildWriteResponse(
  serial: string,
  slave: number,
  register: number,
  value: number,
): Buffer {
  const payload = Buffer.alloc(16);

  // Inverter serial (10 bytes)
  const invSerial = encodeSerial('SA12345678');
  invSerial.copy(payload, 0);

  // Echo back register and value
  payload.writeUInt16BE(register, 10);
  payload.writeUInt16BE(value, 12);

  // Check: CRC-16/Modbus(FC6 + register + value)
  const checkData = Buffer.alloc(5);
  checkData[0] = 0x06;
  checkData.writeUInt16BE(register, 1);
  checkData.writeUInt16BE(value, 3);
  const check = crc16(checkData);
  payload.writeUInt16LE(check, 14);

  return buildFrame(serial, slave, 0x06, payload);
}

// ---------------------------------------------------------------------------
// Client connection handler (Modbus TCP)
// ---------------------------------------------------------------------------

function handleClient(sock: net.Socket): void {
  let buffer = Buffer.alloc(0);

  sock.on('data', (data: Buffer) => {
    buffer = Buffer.concat([buffer, data]);

    // Process complete frames
    while (buffer.length >= 6) {
      const txnId = buffer.readUInt16BE(0);
      const protoId = buffer.readUInt16BE(2);
      const length = buffer.readUInt16BE(4);
      const totalFrameLen = 6 + length;

      if (buffer.length < totalFrameLen) break; // incomplete frame

      const frame = buffer.subarray(0, totalFrameLen);
      buffer = buffer.subarray(totalFrameLen);

      // Validate header
      if (txnId !== TRANSACTION_ID || protoId !== PROTOCOL_ID) {
        continue;
      }

      // Parse inner PDU (from byte 26)
      if (frame.length < HEADER_SIZE + 4) continue;

      const innerPdu = frame.subarray(HEADER_SIZE);
      const slave = innerPdu[0];
      const innerFunc = innerPdu[1];
      const innerPayload = innerPdu.subarray(2, innerPdu.length - 2); // strip CRC

      if (innerFunc === 0x03 || innerFunc === 0x04) {
        // Read holding/input registers
        if (innerPayload.length < 4) continue;
        const startReg = innerPayload.readUInt16BE(0);
        const regCount = innerPayload.readUInt16BE(2);

        const regs = innerFunc === 0x03 ? holdingRegs : inputRegs;
        const response = buildReadResponse('SA12345678', slave, innerFunc, startReg, regCount, regs);
        sock.write(response);
      } else if (innerFunc === 0x06) {
        // Write single holding register
        if (innerPayload.length < 4) continue;

        const register = innerPayload.readUInt16BE(0);
        const value = innerPayload.readUInt16BE(2);

        // Apply the write to our holding register storage
        if (register < holdingRegs.length) {
          holdingRegs[register] = value & 0xFFFF;
        }

        writes.push({ address: register, value });

        // Send ack — use device address 0x11 for write responses
        const response = buildWriteResponse('SA12345678', 0x11, register, value);
        sock.write(response);
      }
    }
  });

  sock.on('error', () => { /* ignore */ });
}

// ---------------------------------------------------------------------------
// HTTP admin API
// ---------------------------------------------------------------------------

const ADMIN_PORT = 18900;

/**
 * Start the HTTP admin API for test workers to query captured writes.
 * Endpoints:
 *   GET  /writes       — peek at captured writes (non-destructive)
 *   POST /writes/drain — drain all captured writes
 *   POST /reset        — reset all state
 *   POST /holding-reg  — set a holding register {address, value}
 *   POST /input-reg    — set an input register {address, value}
 */
export function startAdminApi(): http.Server {
  const server = http.createServer((req, res) => {
    res.setHeader('Content-Type', 'application/json');

    if (req.method === 'GET' && req.url === '/writes') {
      res.end(JSON.stringify({ ok: true, writes }));
    } else if (req.method === 'POST' && req.url === '/writes/drain') {
      const result = [...writes];
      writes.length = 0;
      res.end(JSON.stringify({ ok: true, writes: result }));
    } else if (req.method === 'POST' && req.url === '/reset') {
      resetState();
      res.end(JSON.stringify({ ok: true }));
    } else if (req.method === 'POST' && req.url === '/holding-reg') {
      let body = '';
      req.on('data', (chunk) => { body += chunk; });
      req.on('end', () => {
        try {
          const { address, value } = JSON.parse(body);
          setHoldingReg(address, value);
          res.end(JSON.stringify({ ok: true }));
        } catch {
          res.statusCode = 400;
          res.end(JSON.stringify({ ok: false, error: 'Invalid JSON' }));
        }
      });
    } else if (req.method === 'POST' && req.url === '/input-reg') {
      let body = '';
      req.on('data', (chunk) => { body += chunk; });
      req.on('end', () => {
        try {
          const { address, value } = JSON.parse(body);
          setInputReg(address, value);
          res.end(JSON.stringify({ ok: true }));
        } catch {
          res.statusCode = 400;
          res.end(JSON.stringify({ ok: false, error: 'Invalid JSON' }));
        }
      });
    } else {
      res.statusCode = 404;
      res.end(JSON.stringify({ ok: false, error: 'Not found' }));
    }
  });

  server.listen(ADMIN_PORT, '127.0.0.1', () => {
    console.log(`Mock Modbus admin API listening on 127.0.0.1:${ADMIN_PORT}`);
  });

  return server;
}

// ---------------------------------------------------------------------------
// Server lifecycle
// ---------------------------------------------------------------------------

let modbusServer: net.Server | null = null;
let adminServer: http.Server | null = null;

/**
 * Start the mock Modbus TCP server on the given port.
 * Also starts the admin HTTP API on port ADMIN_PORT.
 */
export async function startModbusServer(port: number = 18899): Promise<void> {
  if (modbusServer) throw new Error('Modbus server already running');

  resetState();
  adminServer = startAdminApi();

  return new Promise((resolve) => {
    modbusServer = net.createServer(handleClient);
    modbusServer.listen(port, '127.0.0.1', () => {
      console.log(`Mock Modbus server listening on 127.0.0.1:${port}`);
      resolve();
    });
  });
}

/**
 * Stop the mock Modbus TCP server and admin API.
 */
export async function stopModbusServer(): Promise<void> {
  if (adminServer) {
    await new Promise<void>((resolve) => { adminServer!.close(() => resolve()); });
    adminServer = null;
  }
  if (!modbusServer) return;
  return new Promise((resolve) => {
    modbusServer!.close(() => {
      modbusServer = null;
      resolve();
    });
  });
}
