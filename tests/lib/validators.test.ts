import { describe, it, expect } from 'vitest';
import { isValidIpv4Host } from '../../src/lib/validators';

/**
 * Tests for the IPv4 dotted-quad validator that powers the EV Charger
 * "Charger Address" field on the Settings page (issue #138).
 *
 * The validator must reject the typo "10.1.71" (only three octets) that
 * the issue reporter typed instead of "10.1.1.71", while still accepting
 * the obvious cases. Empty strings are intentionally rejected — the
 * SettingsPage checks `value === ''` first to mean "EVC disabled".
 */
describe('isValidIpv4Host', () => {
  describe('valid dotted-quads', () => {
    it.each([
      '0.0.0.0',
      '255.255.255.255',
      '10.1.1.71',
      '192.168.1.50',
      '127.0.0.1',
      '8.8.8.8',
    ])('accepts %s', (input) => {
      expect(isValidIpv4Host(input)).toBe(true);
    });
  });

  describe('the specific typo from issue #138', () => {
    it('rejects "10.1.71" (only three octets)', () => {
      expect(isValidIpv4Host('10.1.71')).toBe(false);
    });

    it('rejects "10.1.1.71" — wait, that IS valid', () => {
      // The intended value (a real IPv4) must be accepted.
      expect(isValidIpv4Host('10.1.1.71')).toBe(true);
    });
  });

  describe('rejects obvious garbage', () => {
    it.each([
      '',                 // empty (callers must check this themselves)
      '   ',              // whitespace only
      '10.1.71',          // 3 octets (issue #138)
      '10.1.1',           // 3 octets
      '10',               // single number
      '10.1.1.71.5',      // 5 octets
      '10.1.1.71 ',       // trailing whitespace
      ' 10.1.1.71',       // leading whitespace
      '10.1.1.256',       // octet out of range
      '10.1.1.-1',        // negative octet
      '10.1.1.01',        // leading zero
      '10.1.1.a',         // non-numeric
      '10,1,1,71',        // wrong separator
      'ev-charger.local', // DNS name (we only accept IPv4)
      '::1',              // IPv6
      'fe80::1',          // IPv6
    ])('rejects %s', (input) => {
      expect(isValidIpv4Host(input)).toBe(false);
    });
  });

  describe('range boundaries', () => {
    it('accepts 255.255.255.255 (max value)', () => {
      expect(isValidIpv4Host('255.255.255.255')).toBe(true);
    });

    it('rejects 256.0.0.0 (octet over 255)', () => {
      expect(isValidIpv4Host('256.0.0.0')).toBe(false);
    });

    it('rejects 1.2.3.999', () => {
      expect(isValidIpv4Host('1.2.3.999')).toBe(false);
    });
  });
});
