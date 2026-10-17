import { describe, it, expect } from 'vitest';
import { liveReadout, describeLiveReadout, LIVE_READBACK_STALE_AFTER_SECONDS } from '../../src/lib/liveReadback';
import { SNAPSHOT_STALE_AFTER_MS } from '../../src/hooks/useWebSocket';

// The read-only "Inverter Charge Rate" line beside a charge setting. Its age
// is measured from when this browser received the snapshot, not from the
// host's timestamp, so a host clock that is out of step with the viewer's
// cannot make a fresh reading look stale (or a stale one look fresh). The
// clock is injected so these tests never depend on the real time.

const NOW_MS = 1_800_000_000_000; // fixed instant

describe('liveReadout', () => {
  it('shows the live label when the snapshot arrived moments ago', () => {
    const readout = liveReadout({
      connected: true,
      percent: 62,
      watts: 1612,
      receivedAtMs: NOW_MS - 5_000,
      nowMs: NOW_MS,
    });
    expect(readout).toEqual({ kind: 'live', label: '62% (1.6 kW)' });
  });

  it('treats a snapshot exactly at the stale threshold as live', () => {
    const readout = liveReadout({
      connected: true,
      percent: 62,
      watts: 1612,
      receivedAtMs: NOW_MS - LIVE_READBACK_STALE_AFTER_SECONDS * 1000,
      nowMs: NOW_MS,
    });
    expect(readout.kind).toBe('live');
  });

  it('judges staleness on the whole seconds it displays, so 60.9 s reads as live at 60 s', () => {
    const readout = liveReadout({
      connected: true,
      percent: 62,
      watts: 1612,
      receivedAtMs: NOW_MS - 60_900,
      nowMs: NOW_MS,
      staleAfterSeconds: 60,
    });
    expect(readout.kind).toBe('live');
  });

  it('calls a snapshot stale once its displayed age passes the threshold', () => {
    const readout = liveReadout({
      connected: true,
      percent: 62,
      watts: 1612,
      receivedAtMs: NOW_MS - 61_000,
      nowMs: NOW_MS,
      staleAfterSeconds: 60,
    });
    expect(readout).toEqual({ kind: 'stale', label: '62% (1.6 kW)', ageSeconds: 61 });
  });

  it('counts whole elapsed seconds, so a part second is never shown as the next one', () => {
    // 119.6 s has not reached two minutes; rounding would make it "2 min ago".
    const readout = liveReadout({
      connected: true,
      percent: 62,
      watts: null,
      receivedAtMs: NOW_MS - 119_600,
      nowMs: NOW_MS,
      staleAfterSeconds: 60,
    });
    expect(readout).toEqual({ kind: 'stale', label: '62%', ageSeconds: 119 });
    expect(describeLiveReadout(readout)).toBe('Inverter Charge Rate: 62% · as last read 119s ago');
  });

  it('reports stale with its age once it is older than the threshold', () => {
    const readout = liveReadout({
      connected: true,
      percent: 62,
      watts: 1612,
      receivedAtMs: NOW_MS - 300_000,
      nowMs: NOW_MS,
    });
    expect(readout).toEqual({ kind: 'stale', label: '62% (1.6 kW)', ageSeconds: 300 });
  });

  it('honours an injected stale threshold', () => {
    const readout = liveReadout({
      connected: true,
      percent: 40,
      watts: null,
      receivedAtMs: NOW_MS - 11_000,
      nowMs: NOW_MS,
      staleAfterSeconds: 10,
    });
    expect(readout).toEqual({ kind: 'stale', label: '40%', ageSeconds: 11 });
  });

  it('ignores how far the host clock is off, because only receipt time is used', () => {
    // The snapshot's own timestamp is ten minutes old, as it would be if the
    // host clock ran slow, but it arrived a second ago on this machine.
    const readout = liveReadout({
      connected: true,
      percent: 62,
      watts: 1612,
      receivedAtMs: NOW_MS - 1_000,
      nowMs: NOW_MS,
    });
    expect(readout.kind).toBe('live');
  });

  it('treats a receipt time ahead of the clock as live rather than negative age', () => {
    const readout = liveReadout({
      connected: true,
      percent: 62,
      watts: 1612,
      receivedAtMs: NOW_MS + 3_000,
      nowMs: NOW_MS,
    });
    expect(readout.kind).toBe('live');
  });

  it('is unknown when the register has no percentage', () => {
    expect(liveReadout({
      connected: true,
      percent: undefined,
      watts: null,
      receivedAtMs: NOW_MS,
      nowMs: NOW_MS,
    })).toEqual({ kind: 'unknown' });
    expect(liveReadout({
      connected: true,
      percent: Number.NaN,
      watts: null,
      receivedAtMs: NOW_MS,
      nowMs: NOW_MS,
    })).toEqual({ kind: 'unknown' });
  });

  it('is unknown when no snapshot has been received on this machine', () => {
    expect(liveReadout({
      connected: true,
      percent: 62,
      watts: 1612,
      receivedAtMs: null,
      nowMs: NOW_MS,
    })).toEqual({ kind: 'unknown' });
    expect(liveReadout({
      connected: true,
      percent: 62,
      watts: 1612,
      receivedAtMs: undefined,
      nowMs: NOW_MS,
    })).toEqual({ kind: 'unknown' });
  });

  it('is unknown while the inverter is not connected, even with a fresh receipt time', () => {
    // A snapshot replayed to a reloaded page while the inverter is offline is
    // not a reading from now, so it cannot be shown as current or aged.
    expect(liveReadout({
      connected: false,
      percent: 40,
      watts: 1040,
      receivedAtMs: NOW_MS,
      nowMs: NOW_MS,
    })).toEqual({ kind: 'unknown' });
  });

  it('keeps a reading live through a whole default poll cycle and its reads', () => {
    // The default poll loop sleeps 60 s after its reads. A 60 s threshold
    // called every healthy reading stale just before the next one landed.
    expect(LIVE_READBACK_STALE_AFTER_SECONDS).toBe(90);
    const readout = liveReadout({
      connected: true,
      percent: 62,
      watts: 1612,
      receivedAtMs: NOW_MS - 85_000,
      nowMs: NOW_MS,
    });
    expect(readout.kind).toBe('live');
  });

  it('turns stale before the connection watchdog clears the reading', () => {
    // Past the watchdog the snapshot is cleared and the readout says
    // "unknown", so a threshold at or above it would never show its age.
    expect(LIVE_READBACK_STALE_AFTER_SECONDS * 1000).toBeLessThan(SNAPSHOT_STALE_AFTER_MS);
  });
});

describe('describeLiveReadout', () => {
  it('prefixes a live value with what it is', () => {
    expect(describeLiveReadout({ kind: 'live', label: '62% (1.6 kW)' }))
      .toBe('Inverter Charge Rate: 62% (1.6 kW)');
  });

  it('says unknown when the value cannot be read', () => {
    expect(describeLiveReadout({ kind: 'unknown' })).toBe('Inverter Charge Rate: Unknown');
  });

  it('gives a stale value its age in seconds under two minutes', () => {
    expect(describeLiveReadout({ kind: 'stale', label: '62%', ageSeconds: 42 }))
      .toBe('Inverter Charge Rate: 62% · as last read 42s ago');
    // Just past the 90 s threshold, where younger readings show as live: a
    // minute count would read as younger than readings shown as current.
    expect(describeLiveReadout({ kind: 'stale', label: '62%', ageSeconds: 95 }))
      .toBe('Inverter Charge Rate: 62% · as last read 95s ago');
  });

  it('counts whole elapsed minutes from two minutes, never rounding a part minute up', () => {
    expect(describeLiveReadout({ kind: 'stale', label: '62%', ageSeconds: 120 }))
      .toBe('Inverter Charge Rate: 62% · as last read 2 min ago');
    expect(describeLiveReadout({ kind: 'stale', label: '62%', ageSeconds: 179 }))
      .toBe('Inverter Charge Rate: 62% · as last read 2 min ago');
  });

  it('gives an older stale value its age in whole minutes', () => {
    expect(describeLiveReadout({ kind: 'stale', label: '62% (1.6 kW)', ageSeconds: 300 }))
      .toBe('Inverter Charge Rate: 62% (1.6 kW) · as last read 5 min ago');
  });
});
