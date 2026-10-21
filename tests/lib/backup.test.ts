import { describe, it, expect } from 'vitest';
import {
  formatBackupTime,
  formatBytes,
  formatNextDue,
  isAbsolutePath,
  parseBackupForm,
} from '../../src/lib/backup';

describe('formatBytes', () => {
  it('scales through the units', () => {
    expect(formatBytes(0)).toBe('0 B');
    expect(formatBytes(1023)).toBe('1023 B');
    expect(formatBytes(1024)).toBe('1.0 KB');
    expect(formatBytes(1536)).toBe('1.5 KB');
    expect(formatBytes(150 * 1024)).toBe('150 KB');
    expect(formatBytes(142 * 1024 * 1024)).toBe('142 MB');
    expect(formatBytes(3.2 * 1024 ** 3)).toBe('3.2 GB');
  });

  it('shows a dash for missing or nonsense sizes', () => {
    expect(formatBytes(null)).toBe('—');
    expect(formatBytes(undefined)).toBe('—');
    expect(formatBytes(-1)).toBe('—');
    expect(formatBytes(Number.NaN)).toBe('—');
  });
});

describe('formatBackupTime', () => {
  it('formats unix seconds and dashes missing values', () => {
    expect(formatBackupTime(1_792_584_000)).toContain('2026');
    expect(formatBackupTime(null)).toBe('—');
    expect(formatBackupTime(Number.POSITIVE_INFINITY)).toBe('—');
  });
});

describe('formatNextDue', () => {
  it('reads "soon" once the due time is reached, else the time', () => {
    expect(formatNextDue(1000, 1000)).toBe('soon');
    expect(formatNextDue(900, 1000)).toBe('soon');
    expect(formatNextDue(1_792_584_000, 1_792_583_000)).toBe(formatBackupTime(1_792_584_000));
  });
});

describe('isAbsolutePath', () => {
  it('accepts Unix, Windows drive and UNC paths only', () => {
    expect(isAbsolutePath('/mnt/nas/hem')).toBe(true);
    expect(isAbsolutePath('C:\\Backups')).toBe(true);
    expect(isAbsolutePath('d:/backups')).toBe(true);
    expect(isAbsolutePath('\\\\nas\\share')).toBe(true);
    expect(isAbsolutePath('backups')).toBe(false);
    expect(isAbsolutePath('./backups')).toBe(false);
    expect(isAbsolutePath('C:backups')).toBe(false);
  });
});

describe('parseBackupForm', () => {
  const valid = { destination: ' /mnt/nas ', keep: '7', intervalHours: '24' };

  it('returns trimmed, parsed fields', () => {
    expect(parseBackupForm(valid)).toEqual({
      ok: true,
      value: { destination: '/mnt/nas', keep: 7, interval_hours: 24 },
    });
    expect(parseBackupForm({ ...valid, destination: '   ' })).toEqual({
      ok: true,
      value: { destination: '', keep: 7, interval_hours: 24 },
    });
  });

  it('rejects a relative folder', () => {
    const result = parseBackupForm({ ...valid, destination: 'backups' });
    expect(result.ok).toBe(false);
    expect(!result.ok && result.error).toContain('full path');
  });

  it.each(['0', '169', '1.5', '', 'abc', '-1'])('rejects an interval of %j', (intervalHours) => {
    const result = parseBackupForm({ ...valid, intervalHours });
    expect(result.ok).toBe(false);
    expect(!result.ok && result.error).toContain('hours');
  });

  it.each(['0', '366', '2.5', '', 'x'])('rejects keeping %j backups', (keep) => {
    const result = parseBackupForm({ ...valid, keep });
    expect(result.ok).toBe(false);
    expect(!result.ok && result.error).toContain('backups');
  });

  it('accepts the limits', () => {
    expect(parseBackupForm({ ...valid, keep: '365', intervalHours: '168' }).ok).toBe(true);
    expect(parseBackupForm({ ...valid, keep: '1', intervalHours: '1' }).ok).toBe(true);
  });
});
