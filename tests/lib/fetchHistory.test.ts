import { describe, it, expect, beforeAll, afterAll, vi } from 'vitest';

// Regression tests for issue #134: the History "Today" chart started at 01:00
// and ended at 00:59 when the server's timezone differed from the user's
// (e.g. a UTC container on unRAID while the user is on BST).
//
// Background:
//   The "today" calendar window was computed server-side with chrono::Local,
//   so a server on UTC bounded the day at 00:00 UTC = 01:00 BST — one hour
//   off from both the user's wall clock and the inverter's local-midnight
//   today_*_kwh counter reset. PR #136 already aligned the *chart x-axis
//   domain* to the browser's local midnight but left the data query trusting
//   the server clock, so the axis and the data disagreed by exactly the
//   server/user TZ offset.
//
//   The fix sends an explicit start_ms/end_ms window for calendar-aligned
//   ranges ("today", "month") from the browser. The server honours that
//   window verbatim (api.rs `explicit_window`), so the server's timezone
//   becomes irrelevant to the query.
//
//   These tests pin the contract by capturing the URL fetchHistory builds.
//   They are TZ-agnostic in their core assertions (span widths) so they pass
//   on any CI zone, and add a pinned-TZ anchor that proves the window is
//   browser-local midnight — the property that broke under a UTC server.

const ORIGINAL_TZ = process.env.TZ;
const PINNED_TZ = 'Europe/London';

beforeAll(() => {
  process.env.TZ = PINNED_TZ;
});

afterAll(() => {
  if (ORIGINAL_TZ === undefined) {
    delete process.env.TZ;
  } else {
    process.env.TZ = ORIGINAL_TZ;
  }
});

/** Pull the query params fetchHistory would send for the given range. */
async function queryParamsFor(
  range: string,
  offset = 0,
  rolling = false,
): URLSearchParams {
  const fetchMock = vi.fn().mockResolvedValue(
    new Response(JSON.stringify({ ok: true, data: {} }), { status: 200 }),
  );
  vi.stubGlobal('fetch', fetchMock);

  try {
    const { fetchHistory } = await import('../../src/lib/api');
    await fetchHistory(range, ['solar_power'], offset, rolling);
  } finally {
    vi.unstubAllGlobals();
  }

  expect(fetchMock).toHaveBeenCalledTimes(1);
  const url = fetchMock.mock.calls[0][0] as string;
  return new URL(url, 'http://localhost:7337').searchParams;
}

describe('fetchHistory calendar-window wiring (issue #134)', () => {
  it('sends start_ms and end_ms for the "today" range', async () => {
    const p = await queryParamsFor('today');
    expect(p.has('start_ms')).toBe(true);
    expect(p.has('end_ms')).toBe(true);
    const start = Number(p.get('start_ms'));
    const end = Number(p.get('end_ms'));
    expect(end - start).toBe(86_400_000); // exactly 24h
  });

  it('bounds "today" at browser-local midnight, not server/UTC midnight', async () => {
    // Under the pinned Europe/London zone this is the property that broke:
    // a summer date's local midnight is 23:00 UTC (BST = UTC+1). If the
    // window were server-UTC it would start at 00:00 UTC instead.
    const p = await queryParamsFor('today');
    const start = Number(p.get('start_ms'));
    const startLocal = new Date(start);
    expect(startLocal.getHours()).toBe(0);
    expect(startLocal.getMinutes()).toBe(0);
    expect(startLocal.getSeconds()).toBe(0);
  });

  it('offset=1 steps "today" back by exactly one calendar day', async () => {
    const today = await queryParamsFor('today', 0);
    const yesterday = await queryParamsFor('today', 1);
    expect(Number(yesterday.get('start_ms'))).toBe(
      Number(today.get('start_ms')) - 86_400_000,
    );
    expect(Number(yesterday.get('end_ms'))).toBe(
      Number(today.get('end_ms')) - 86_400_000,
    );
  });

  it('sends start_ms and end_ms for the "month" range', async () => {
    const p = await queryParamsFor('month');
    expect(p.has('start_ms')).toBe(true);
    expect(p.has('end_ms')).toBe(true);
  });

  it('does NOT send start_ms/end_ms for rolling ranges (24h)', async () => {
    // Rolling ranges anchor at "now" and must keep using the server-side
    // rolling window; only calendar ranges are browser-bounded.
    const p = await queryParamsFor('24h', 0, true);
    expect(p.has('start_ms')).toBe(false);
    expect(p.has('end_ms')).toBe(false);
    expect(p.get('rolling')).toBe('true');
  });

  it('does NOT send start_ms/end_ms for multi-day rolling ranges (7d)', async () => {
    const p = await queryParamsFor('7d', 0, true);
    expect(p.has('start_ms')).toBe(false);
  });
});
