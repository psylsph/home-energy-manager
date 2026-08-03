import { describe, it, expect } from 'vitest';
import { parseVersion, isUpdateAvailable, normaliseVersionKey } from '../../src/lib/updateCheck';

/**
 * Tests for the pure version-comparison helpers that drive the dismissible
 * "new version available" banner. The backend computes `update_available`
 * too, but the frontend recomputes locally so the per-version dismissal
 * ("did the user already hide *this* version?") works without a round-trip
 * and stays correct if a future payload omits the flag.
 */
describe('parseVersion', () => {
  it('parses a plain semver triple', () => {
    expect(parseVersion('0.70.2')).toEqual([0, 70, 2]);
    expect(parseVersion('1.2.3')).toEqual([1, 2, 3]);
  });

  it('strips a leading v or V', () => {
    expect(parseVersion('v0.70.2')).toEqual([0, 70, 2]);
    expect(parseVersion('V2.0.0')).toEqual([2, 0, 0]);
  });

  it('ignores a prerelease / build suffix', () => {
    expect(parseVersion('v0.71.0-rc.1')).toEqual([0, 71, 0]);
    expect(parseVersion('1.2.3+build.5')).toEqual([1, 2, 3]);
  });

  it('trims surrounding whitespace', () => {
    expect(parseVersion('  0.70.2  ')).toEqual([0, 70, 2]);
  });

  it('rejects malformed input', () => {
    expect(parseVersion('not-a-version')).toBeNull();
    expect(parseVersion('1')).toBeNull();
    expect(parseVersion('1.2')).toBeNull();
    expect(parseVersion('1.2.x')).toBeNull();
    expect(parseVersion('')).toBeNull();
  });
});

describe('isUpdateAvailable', () => {
  it('is true when latest is strictly newer', () => {
    expect(isUpdateAvailable('0.70.2', '0.70.3')).toBe(true);
    expect(isUpdateAvailable('0.70.99', '0.71.0')).toBe(true);
    expect(isUpdateAvailable('0.99.99', '1.0.0')).toBe(true);
  });

  it('is false when versions are equal', () => {
    expect(isUpdateAvailable('0.70.2', '0.70.2')).toBe(false);
  });

  it('is false when latest is older (e.g. running a local build ahead)', () => {
    expect(isUpdateAvailable('0.70.2', '0.70.1')).toBe(false);
  });

  it('accepts a leading v on either side', () => {
    expect(isUpdateAvailable('0.70.2', 'v0.70.3')).toBe(true);
    expect(isUpdateAvailable('v0.70.2', '0.70.3')).toBe(true);
  });

  it('is safe on parse failure (never spurious)', () => {
    expect(isUpdateAvailable('0.70.2', 'garbage')).toBe(false);
    expect(isUpdateAvailable('garbage', '0.70.3')).toBe(false);
    expect(isUpdateAvailable('', '')).toBe(false);
  });
});

describe('normaliseVersionKey', () => {
  it('collapses v-prefix and prerelease suffix to the same key', () => {
    expect(normaliseVersionKey('v0.70.3')).toBe('0.70.3');
    expect(normaliseVersionKey('0.70.3-rc.1')).toBe('0.70.3');
    expect(normaliseVersionKey('0.70.3')).toBe('0.70.3');
  });

  it('falls back to a stable lowercased key for unparseable input', () => {
    // Defensive: even a garbage value should produce a consistent key so a
    // dismissal still records *something* comparable.
    expect(normaliseVersionKey('Weird-Tag')).toBe('weird-tag');
  });
});
