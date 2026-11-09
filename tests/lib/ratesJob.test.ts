import { describe, it, expect } from 'vitest';
import {
  isRepriceDateValid,
  progressText,
  resultText,
  todayIso,
  type RatesJobRecord,
  type RatesJobStatus,
} from '../../src/lib/ratesJob';

const record = (over: Partial<RatesJobRecord> = {}): RatesJobRecord => ({
  kind: 'reprice',
  at: 1_800_000_000,
  ok: true,
  rows: 1234,
  error: null,
  ...over,
});

const status = (over: Partial<RatesJobStatus> = {}): RatesJobStatus => ({
  running: true,
  kind: 'reprice',
  done: 0,
  total: 0,
  last: null,
  ...over,
});

describe('todayIso', () => {
  it('formats the local date with zero padding', () => {
    expect(todayIso(new Date(2026, 0, 5, 23, 59))).toBe('2026-01-05');
    expect(todayIso(new Date(2026, 10, 9, 0, 1))).toBe('2026-11-09');
  });
});

describe('isRepriceDateValid', () => {
  const today = '2026-11-09';
  it('accepts today and any earlier real date', () => {
    expect(isRepriceDateValid('2026-11-09', today)).toBe(true);
    expect(isRepriceDateValid('2025-02-28', today)).toBe(true);
  });
  it('rejects a date after today', () => {
    expect(isRepriceDateValid('2026-11-10', today)).toBe(false);
  });
  it('rejects blank, malformed and impossible dates', () => {
    for (const bad of ['', 'yesterday', '2026-1-5', '2026-13-01', '2026-02-30', '2025-02-29']) {
      expect(isRepriceDateValid(bad, today), bad).toBe(false);
    }
  });
  it('accepts a leap day', () => {
    expect(isRepriceDateValid('2024-02-29', today)).toBe(true);
  });
});

describe('progressText', () => {
  it('names the job and shows how far it has got', () => {
    expect(progressText(status({ done: 5000, total: 12000 }))).toBe('Re-pricing… 5,000 of 12,000 rows');
    expect(progressText(status({ kind: 'backfill', done: 1, total: 1 }))).toBe('Filling rates… 1 of 1 row');
  });
  it('shows no counts before the total is known', () => {
    expect(progressText(status())).toBe('Re-pricing…');
  });
});

describe('resultText', () => {
  it('summarises a finished re-price and backfill', () => {
    expect(resultText(record())).toBe('Re-priced 1,234 rows');
    expect(resultText(record({ rows: 1 }))).toBe('Re-priced 1 row');
    expect(resultText(record({ kind: 'backfill' }))).toBe('Checked 1,234 rows for missing rates');
  });
  it('says why a job failed', () => {
    expect(resultText(record({ ok: false, error: 'disk full' }))).toBe('Re-pricing failed: disk full');
    expect(resultText(record({ kind: 'backfill', ok: false, error: null }))).toBe(
      'Filling rates failed: unknown error',
    );
  });
});
