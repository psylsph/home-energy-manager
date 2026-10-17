import { describe, it, expect } from 'vitest';
import { exportStatusLabel, formatReward, formatSessionWindow, readSessionsResponse } from '../../src/lib/savingSessions';
import type { SavingSession } from '../../src/lib/types';

describe('formatSessionWindow', () => {
  it('shows UK summer time (BST)', () => {
    expect(formatSessionWindow('2026-10-19T17:00:00+00:00', '2026-10-19T18:00:00+00:00')).toBe(
      'Mon 19 Oct, 18:00–19:00',
    );
  });

  it('shows UK winter time (GMT) once the clocks have gone back', () => {
    expect(formatSessionWindow('2026-11-02T17:00:00+00:00', '2026-11-02T18:00:00+00:00')).toBe(
      'Mon 2 Nov, 17:00–18:00',
    );
  });

  it('uses the UK date, not the UTC one, across midnight', () => {
    // 23:30 UTC in summer is 00:30 the next day in the UK.
    expect(formatSessionWindow('2026-07-01T23:30:00+00:00', '2026-07-02T00:30:00+00:00')).toBe(
      'Thu 2 Jul, 00:30–01:30',
    );
  });

  it('does not throw on garbage', () => {
    expect(formatSessionWindow('nope', 'also nope')).toBe('Time Unknown');
  });
});

describe('formatReward', () => {
  const base = { id: 1, start: '', end: '', announced: false };
  it('names the reward when known', () => {
    expect(formatReward({ ...base, reward_octopoints_per_kwh: 73 })).toBe('73 OctoPoints per kWh');
  });
  it('is null when Octopus gave none', () => {
    expect(formatReward({ ...base, reward_octopoints_per_kwh: null })).toBeNull();
  });
});

describe('readSessionsResponse', () => {
  const good = {
    ok: true, mode: 'alert', configured: true, alerts_ready: true,
    checked_at: null, error: null, enrolled: true, sessions: [],
    export_slot: 1, export_floor_soc: 20, max_discharge_slots: 2,
  };
  it('accepts a well-formed response', () => {
    expect(readSessionsResponse(good)).not.toBeNull();
  });
  it('accepts export mode', () => {
    expect(readSessionsResponse({ ...good, mode: 'export' })).not.toBeNull();
  });

  it('rejects anything else', () => {
    expect(readSessionsResponse(null)).toBeNull();
    expect(readSessionsResponse('x')).toBeNull();
    expect(readSessionsResponse({ ok: true, data: {} })).toBeNull();
    expect(readSessionsResponse({ ...good, mode: 'turbo' })).toBeNull();
    expect(readSessionsResponse({ ...good, sessions: undefined })).toBeNull();
    expect(readSessionsResponse({ ...good, ok: false })).toBeNull();
  });
});

describe('exportStatusLabel', () => {
  const base: SavingSession = {
    id: 1, start: '', end: '', reward_octopoints_per_kwh: null, announced: true, skipped: false, export: null,
  };
  const withExport = (state: string, detail: string | null = null): SavingSession => ({
    ...base,
    skipped: state === 'skipped',
    export: { state, detail } as SavingSession['export'],
  });

  it('says nothing when there is no export status (alert mode)', () => {
    expect(exportStatusLabel(base)).toBeNull();
  });

  it('describes every state in plain words', () => {
    expect(exportStatusLabel(withExport('booked'))).toEqual({ text: 'Export Booked', tone: 'good' });
    expect(exportStatusLabel(withExport('running'))).toEqual({ text: 'Exporting Now', tone: 'good' });
    expect(exportStatusLabel(withExport('done'))).toEqual({ text: 'Exported', tone: 'neutral' });
    expect(exportStatusLabel(withExport('cancelled'))).toEqual({ text: 'Export Cancelled', tone: 'neutral' });
    expect(exportStatusLabel(withExport('skipped'))).toEqual({ text: 'Skipped', tone: 'neutral' });
  });

  it('puts the reason on a failure or a booking error', () => {
    expect(exportStatusLabel(withExport('failed', 'Missed: nothing applied'))).toEqual({
      text: 'Export Failed: Missed: nothing applied',
      tone: 'bad',
    });
    expect(exportStatusLabel(withExport('error', 'Slot 3 not on this inverter'))).toEqual({
      text: 'Not Booked: Slot 3 not on this inverter',
      tone: 'bad',
    });
    expect(exportStatusLabel(withExport('failed'))?.text).toBe('Export Failed');
  });

  it('treats a skipped session as skipped whatever else is recorded', () => {
    expect(exportStatusLabel({ ...withExport('cancelled'), skipped: true })).toEqual({
      text: 'Skipped',
      tone: 'neutral',
    });
  });
});
