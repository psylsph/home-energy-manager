import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, cleanup, waitFor } from '@testing-library/react';
import { cloneElement, isValidElement } from 'react';
import type { ReactElement } from 'react';

// ---------------------------------------------------------------------------
// jsdom has no layout, so recharts' ResponsiveContainer renders nothing and the
// axis ticks never reach the DOM. Give it a fixed size so the Y-axis labels can
// be read.
// ---------------------------------------------------------------------------

globalThis.ResizeObserver = class {
  observe() {}
  unobserve() {}
  disconnect() {}
};

vi.mock('recharts', async () => {
  const actual = await vi.importActual<typeof import('recharts')>('recharts');
  return {
    ...actual,
    ResponsiveContainer: ({ children }: { children: ReactElement }) =>
      isValidElement(children)
        ? cloneElement(children as ReactElement<{ width: number; height: number }>, { width: 600, height: 180 })
        : null,
  };
});

const fetchHistoryMock = vi.fn();

vi.mock('../../src/lib/api', () => ({
  fetchHistory: (...args: unknown[]) => fetchHistoryMock(...args),
}));

import SolarPowerChart from '../../src/components/SolarPowerChart';
import { useInverterStore } from '../../src/store/useInverterStore';

describe('<SolarPowerChart/> Y axis', () => {
  beforeEach(() => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    fetchHistoryMock.mockReset();
    useInverterStore.setState({
      panelGraphsScale: '24h',
      gridLineWeight: 'normal',
      panelGraphsYLock: false,
      panelGraphsYLockMax: 0,
      snapshot: null,
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  it('labels every Y-axis tick with its unit', async () => {
    const now = Date.now();
    fetchHistoryMock.mockResolvedValue({
      pv1_power: [
        { t: now - 3_600_000, v: 400 },
        { t: now - 1_800_000, v: 2400 },
        { t: now, v: 1200 },
      ],
      pv2_power: [],
    });
    const { container } = render(<SolarPowerChart />);
    await waitFor(() => {
      expect(container.querySelectorAll('.recharts-yAxis .recharts-cartesian-axis-tick').length).toBeGreaterThan(0);
    });
    const ticks = Array.from(
      container.querySelectorAll('.recharts-yAxis .recharts-cartesian-axis-tick-value'),
    ).map((el) => el.textContent ?? '');
    expect(ticks.length).toBeGreaterThan(0);
    for (const tick of ticks) {
      expect(tick).toMatch(/\d(\.\d)?\s?k?W$/);
    }
  });
});
