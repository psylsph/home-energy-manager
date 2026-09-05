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
import { attachErrorHandler } from './process-errors.js';

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

/** Input registers (read-only telemetry), including the IR 180-183 block. */
const inputRegs = new Uint16Array(184);

/** Holding registers (read/write config), 2100 registers (covers HR 2040-2075 for Gateway). */
const holdingRegs = new Uint16Array(2100);

/** All register writes received by the server, in order. */
const writes: RegisterWrite[] = [];

/** Protocol violations observed by the mock, surfaced through the admin API. */
const protocolErrors: string[] = [];

/**
 * When enabled, emulates Gen3 Hybrid firmware that re-asserts HR59=1
 * (enable discharge) whenever a discharge slot register remains non-zero —
 * the behaviour issue #289's clear/restore fallback defends against.
 *
 * Timing matters: real firmware re-asserts on its own cycle, so the client's
 * HR59=0 write IS observed by the next read before the re-assert lands. The
 * mock therefore defers the re-assert until after the next register read has
 * been served — an atomic re-assert inside the write handler would make the
 * written 0 unobservable, and the backend's re-arm detector (which anchors
 * on a confirming-off readback) could never classify.
 */
let rearmHr59Enabled = false;

/** A re-assert is scheduled and fires after the next register read. */
let hr59ReassertPending = false;

/**
 * How many upcoming FC06 writes must be rejected with a Modbus exception
 * (code 4, server device failure) instead of applied. Lets specs emulate an
 * inverter that refuses register writes — e.g. a Stop whose disarm batch is
 * rejected mid-flight, leaving the exit pending across a backend restart.
 */
let rejectWritesRemaining = 0;

/** Discharge slot time registers for the standard (single-phase) layout. */
const DISCHARGE_SLOT_REGS = [56, 57, 44, 45];

function hasPopulatedDischargeSlot(): boolean {
  return DISCHARGE_SLOT_REGS.some((addr) => holdingRegs[addr] !== 0);
}

/** Enable/disable the HR59 re-arm firmware emulation. */
export function setRearmHr59(enabled: boolean): void {
  rearmHr59Enabled = enabled;
  if (!enabled) hr59ReassertPending = false;
}

/** Arm `count` upcoming FC06 write rejections (0 clears the emulation). */
export function setRejectWrites(count: number): void {
  rejectWritesRemaining = Math.max(0, count);
}

/** Reset all state. */
export function resetState(): void {
  inputRegs.fill(0);
  holdingRegs.fill(0);
  writes.length = 0;
  protocolErrors.length = 0;
  rearmHr59Enabled = false;
  hr59ReassertPending = false;
  rejectWritesRemaining = 0;
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

/** Return protocol errors recorded since the last reset. */
export function getProtocolErrors(): string[] {
  return [...protocolErrors];
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
const MIN_FRAME_SIZE = HEADER_SIZE + 4;
const MAX_FRAME_BODY = 512;
const MAX_REGISTERS_PER_REQUEST = 125;
const VALID_SLAVE_IDS = new Set([
  0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
  0x11, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37,
]);

export interface ValidatedRequest {
  slave: number;
  functionCode: number;
  payload: Buffer;
  startRegister?: number;
  registerCount?: number;
  register?: number;
  value?: number;
}

function encodeSerial(serial: string): Buffer {
  const buf = Buffer.alloc(SERIAL_LEN, 0x20); // space-padded
  Buffer.from(serial, 'latin1').copy(buf);
  return buf;
}

export function buildFrame(serial: string, slave: number, func: number, payload: Buffer): Buffer {
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

/**
 * Validate and decode an incoming request before it can touch mock state.
 * Keeping this strict makes an E2E test fail on malformed traffic instead of
 * hiding a bad request behind zero-filled register data.
 */
export function validateRequestFrame(frame: Buffer): ValidatedRequest {
  if (frame.length < 6) {
    throw new Error(`frame too short for length header: ${frame.length} bytes`);
  }

  const declaredLength = frame.readUInt16BE(4);
  if (declaredLength !== frame.length - 6) {
    throw new Error(
      `frame length mismatch: header=${declaredLength}, actual=${frame.length - 6}`,
    );
  }
  if (declaredLength > MAX_FRAME_BODY) {
    throw new Error(`frame body exceeds mock limit: ${declaredLength} bytes`);
  }
  if (frame.length < MIN_FRAME_SIZE) {
    throw new Error(`frame too short: expected at least ${MIN_FRAME_SIZE} bytes, got ${frame.length}`);
  }

  if (frame.readUInt16BE(0) !== TRANSACTION_ID) {
    throw new Error('invalid transaction ID');
  }
  if (frame.readUInt16BE(2) !== PROTOCOL_ID) {
    throw new Error('invalid protocol ID');
  }
  if (frame[6] !== UNIT_ID) {
    throw new Error(`invalid unit ID: ${frame[6]}`);
  }
  if (frame[7] !== FUNCTION_TRANSPARENT) {
    throw new Error(`invalid transparent function ID: ${frame[7]}`);
  }

  const innerPdu = frame.subarray(HEADER_SIZE);
  const crcOffset = innerPdu.length - 2;
  const receivedCrc = innerPdu.readUInt16LE(crcOffset);
  const calculatedCrc = crc16(innerPdu.subarray(0, crcOffset));
  if (receivedCrc !== calculatedCrc) {
    throw new Error(
      `CRC mismatch: received=0x${receivedCrc.toString(16)}, calculated=0x${calculatedCrc.toString(16)}`,
    );
  }

  const slave = innerPdu[0];
  if (!VALID_SLAVE_IDS.has(slave)) {
    throw new Error(`invalid slave ID: 0x${slave.toString(16)}`);
  }
  const functionCode = innerPdu[1];
  const payload = innerPdu.subarray(2, crcOffset);

  if (functionCode === 0x03 || functionCode === 0x04) {
    if (payload.length !== 4) {
      throw new Error(`read request payload must be 4 bytes, got ${payload.length}`);
    }
    const startRegister = payload.readUInt16BE(0);
    const registerCount = payload.readUInt16BE(2);
    if (registerCount === 0 || registerCount > MAX_REGISTERS_PER_REQUEST) {
      throw new Error(`invalid register count: ${registerCount}`);
    }
    const regs = functionCode === 0x03 ? holdingRegs : inputRegs;
    if (startRegister + registerCount > regs.length) {
      throw new Error(
        `register range ${startRegister}..${startRegister + registerCount - 1} exceeds ${functionCode === 0x03 ? 'holding' : 'input'} register bank`,
      );
    }
    return { slave, functionCode, payload, startRegister, registerCount };
  }

  if (functionCode === 0x06) {
    if (payload.length !== 4) {
      throw new Error(`write request payload must be 4 bytes, got ${payload.length}`);
    }
    const register = payload.readUInt16BE(0);
    if (register >= holdingRegs.length) {
      throw new Error(`holding register ${register} is out of range`);
    }
    return { slave, functionCode, payload, register, value: payload.readUInt16BE(2) };
  }

  throw new Error(`unsupported inner function code: 0x${functionCode.toString(16)}`);
}

function recordProtocolError(sock: net.Socket, error: unknown): void {
  const message = error instanceof Error ? error.message : String(error);
  protocolErrors.push(message);
  console.error(`[mock-modbus] ${message}`);
  sock.destroy();
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
      const length = buffer.readUInt16BE(4);
      if (length > MAX_FRAME_BODY) {
        recordProtocolError(sock, new Error(`frame body exceeds mock limit: ${length} bytes`));
        return;
      }
      const totalFrameLen = 6 + length;

      if (buffer.length < totalFrameLen) break; // incomplete frame

      const frame = buffer.subarray(0, totalFrameLen);
      buffer = buffer.subarray(totalFrameLen);

      let request: ValidatedRequest;
      try {
        request = validateRequestFrame(frame);
      } catch (error) {
        recordProtocolError(sock, error);
        return;
      }

      // The validator owns these fields; retain the local names for the
      // response-building code below.
      const { slave, functionCode: innerFunc } = request;

      if (innerFunc === 0x03 || innerFunc === 0x04) {
        // Read holding/input registers
        const startReg = request.startRegister!;
        const regCount = request.registerCount!;

        const regs = innerFunc === 0x03 ? holdingRegs : inputRegs;
        const response = buildReadResponse('SA12345678', slave, innerFunc, startReg, regCount, regs);
        sock.write(response);

        // Fire a pending HR59 re-assert only after a HOLDING read that covers
        // register 59 has observed the written 0 (see the timing note above).
        // Any earlier read (e.g. the input-register block) must not consume
        // the pending re-assert, or the poll's holding read would already see
        // the re-asserted value and the written 0 would be unobservable.
        if (hr59ReassertPending && innerFunc === 0x03 && startReg <= 59 && 59 < startReg + regCount) {
          hr59ReassertPending = false;
          if (rearmHr59Enabled && hasPopulatedDischargeSlot()) {
            holdingRegs[59] = 1;
          }
        }
      } else if (innerFunc === 0x06) {
        // Write single holding register
        const register = request.register!;
        const value = request.value!;

        // Write-rejection emulation: respond with a Modbus exception
        // (FC 0x86, code 4 = server device failure) without applying the
        // write or recording it as captured — the register must still hold
        // its previous value, exactly like a real refusing device.
        if (rejectWritesRemaining > 0) {
          rejectWritesRemaining -= 1;
          const exception = buildFrame(
            'SA12345678',
            slave,
            0x06 | 0x80,
            Buffer.from([0x04]),
          );
          sock.write(exception);
          continue;
        }

        // Apply the write to our holding register storage
        if (register < holdingRegs.length) {
          holdingRegs[register] = value & 0xFFFF;
        }

        // Re-arm firmware emulation (issue #289): real Gen3 Hybrid firmware
        // re-asserts enable_discharge whenever discharge slots remain
        // programmed. The re-assert fires after the next read so the
        // written 0 is observable once (see the timing note above).
        if (
          rearmHr59Enabled
          && register === 59
          && value === 0
          && hasPopulatedDischargeSlot()
        ) {
          hr59ReassertPending = true;
        }

        writes.push({ address: register, value });

        // Send the ack from the addressed device.
        const response = buildWriteResponse('SA12345678', slave, register, value);
        sock.write(response);
      }
    }
  });

  attachErrorHandler(sock, 'mock Modbus client');
}

// ---------------------------------------------------------------------------
// HTTP admin API
// ---------------------------------------------------------------------------

const ADMIN_PORT = 18900;

/**
 * Start the HTTP admin API for test workers to query captured writes.
 * Endpoints:
 *   GET  /writes       — peek at captured writes (non-destructive)
 *   GET  /protocol-errors — protocol violations since the last reset
 *   POST /writes/drain — drain all captured writes
 *   POST /reset        — reset all state
 *   POST /holding-reg  — set a holding register {address, value}
 *   POST /input-reg    — set an input register {address, value}
 *   POST /rearm-hr59   — toggle the HR59 re-arm firmware emulation {enabled}
 *   POST /reject-writes — reject the next {count} FC06 writes with an exception
 *   GET  /reject-writes — inspect the remaining rejection count
 */
export function startAdminApi(): http.Server {
  const server = http.createServer((req, res) => {
    res.setHeader('Content-Type', 'application/json');

    if (req.method === 'GET' && req.url === '/writes') {
      res.end(JSON.stringify({ ok: true, writes }));
    } else if (req.method === 'GET' && req.url === '/protocol-errors') {
      res.end(JSON.stringify({ ok: true, errors: getProtocolErrors() }));
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
    } else if (req.method === 'POST' && req.url === '/rearm-hr59') {
      let body = '';
      req.on('data', (chunk) => { body += chunk; });
      req.on('end', () => {
        try {
          const { enabled } = JSON.parse(body);
          setRearmHr59(Boolean(enabled));
          res.end(JSON.stringify({ ok: true, enabled: rearmHr59Enabled }));
        } catch {
          res.statusCode = 400;
          res.end(JSON.stringify({ ok: false, error: 'Invalid JSON' }));
        }
      });
    } else if (req.method === 'POST' && req.url === '/reject-writes') {
      let body = '';
      req.on('data', (chunk) => { body += chunk; });
      req.on('end', () => {
        try {
          const { count } = JSON.parse(body);
          setRejectWrites(Number(count) || 0);
          res.end(JSON.stringify({ ok: true, rejectWritesRemaining }));
        } catch {
          res.statusCode = 400;
          res.end(JSON.stringify({ ok: false, error: 'Invalid JSON' }));
        }
      });
    } else if (req.method === 'GET' && req.url === '/reject-writes') {
      res.end(JSON.stringify({ ok: true, rejectWritesRemaining }));
    } else {
      res.statusCode = 404;
      res.end(JSON.stringify({ ok: false, error: 'Not found' }));
    }
  });

  attachErrorHandler(server, 'mock Modbus admin API');
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

export interface ClosableServer {
  close(callback?: (error?: Error) => void): void;
  closeAllConnections?: () => void;
  closeIdleConnections?: () => void;
}

/** Close a Node server without waiting indefinitely for an open client. */
export async function closeServerWithDeadline(
  server: ClosableServer,
  label: string,
  timeoutMs = 1_000,
): Promise<void> {
  await new Promise<void>((resolve) => {
    let settled = false;
    const finish = (): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve();
    };
    const timer = setTimeout(() => {
      console.warn(`[${label}] server close exceeded ${timeoutMs}ms`);
      finish();
    }, timeoutMs);

    // These methods close active and idle sockets before close() waits for
    // the listener itself. They are available on the supported Node runtime;
    // optional chaining keeps the helper compatible with older Node types.
    server.closeAllConnections?.();
    server.closeIdleConnections?.();
    server.close(() => finish());
  });
}

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
    attachErrorHandler(modbusServer, 'mock Modbus TCP server');
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
    await closeServerWithDeadline(adminServer, 'mock admin server');
    adminServer = null;
  }
  if (!modbusServer) return;
  await closeServerWithDeadline(modbusServer, 'mock Modbus server');
  modbusServer = null;
}
