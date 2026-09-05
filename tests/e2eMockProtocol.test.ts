// @vitest-environment node

import { describe, expect, test } from 'vitest';
import { buildFrame, validateRequestFrame } from '../e2e/mock-modbus.js';

function readRequest(slave = 0x11, start = 0, count = 60): Buffer {
  const payload = Buffer.alloc(4);
  payload.writeUInt16BE(start, 0);
  payload.writeUInt16BE(count, 2);
  return buildFrame('SA12345678', slave, 0x04, payload);
}

describe('E2E Modbus request validation', () => {
  test('accepts a valid input-register request', () => {
    expect(validateRequestFrame(readRequest())).toMatchObject({
      slave: 0x11,
      functionCode: 0x04,
      startRegister: 0,
      registerCount: 60,
    });
  });

  test('rejects a bad inner CRC', () => {
    const frame = readRequest();
    frame[frame.length - 1] ^= 0x01;

    expect(() => validateRequestFrame(frame)).toThrow(/CRC/i);
  });

  test('rejects the wrong outer unit ID', () => {
    const frame = readRequest();
    frame[6] = 0x02;

    expect(() => validateRequestFrame(frame)).toThrow(/unit ID/i);
  });

  test('rejects an unknown inner slave ID', () => {
    expect(() => validateRequestFrame(readRequest(0x40))).toThrow(/slave/i);
  });

  test.each([0x50, 0x70, 0x7f, 0x8f, 0xa0])('accepts an HV slave ID 0x%s', (slave) => {
    expect(validateRequestFrame(readRequest(slave))).toMatchObject({ slave });
  });

  test('rejects a short read payload', () => {
    expect(() => validateRequestFrame(
      buildFrame('SA12345678', 0x11, 0x04, Buffer.from([0, 0, 0])),
    )).toThrow(/payload/i);
  });

  test('rejects an out-of-range read', () => {
    expect(() => validateRequestFrame(readRequest(0x11, 183, 2))).toThrow(/range/i);
  });

  test('rejects an out-of-range holding-register write', () => {
    const payload = Buffer.alloc(4);
    payload.writeUInt16BE(2100, 0);
    payload.writeUInt16BE(1, 2);

    expect(() => validateRequestFrame(
      buildFrame('SA12345678', 0x11, 0x06, payload),
    )).toThrow(/range/i);
  });
});
