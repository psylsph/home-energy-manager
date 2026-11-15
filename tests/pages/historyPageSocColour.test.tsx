import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, cleanup, waitFor } from '@testing-library/react';

// Issue #476: the SOC line's indigo was hard to read on the dark background.
// It now uses the Home tab's Car Charging violet, at the standard 2px width.

const fetchHistoryMock = vi.fn(async (...args: unknown[]) => {
  const [, fields] = args as [string, string[]];
  const result: Record<string, { t: number; v: number }[]> = {};
  for (const f of fields) result[f] = [{ t: 1_700_000_000_000, v: 40 }, { t: 1_700_000_300_000, v: 45 }];
  return result;
});

vi.mock('../../src/lib/api', () => ({
  apiGet: async () => ({ ok: true, data: {} }),
  fetchHistory: (...args: unknown[]) => fetchHistoryMock(...args),
  fetchHistorySummary: async () => null,
  getApiBase: () => 'http://localhost:7337',
  getServerPort: () => 7337,
  isTauri: false,
}));

type AreaProps = { dataKey: string; stroke: string; strokeWidth: number };
const areas: AreaProps[] = [];

vi.mock('recharts', () => ({
  ResponsiveContainer: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  AreaChart: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  Area: (props: AreaProps) => {
    areas.push(props);
    return null;
  },
  CartesianGrid: () => null,
  ReferenceLine: () => null,
  Tooltip: () => null,
  XAxis: () => null,
  YAxis: () => null,
}));

globalThis.ResizeObserver = class {
  observe() {}
  unobserve() {}
  disconnect() {}
};

import HistoryPage from '../../src/pages/HistoryPage';
import { useInverterStore } from '../../src/store/useInverterStore';

describe('<HistoryPage/> — SOC line colour (issue #476)', () => {
  beforeEach(() => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    areas.length = 0;
    useInverterStore.setState({ snapshot: null, chartRange: '24h', gridLineWeight: 'standard' });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it('draws SOC in the Car Charging violet at the standard line width', async () => {
    render(<HistoryPage />);
    await waitFor(() => expect(areas.some((a) => a.dataKey === 'soc')).toBe(true));
    const soc = areas.filter((a) => a.dataKey === 'soc').at(-1)!;
    expect(soc.stroke).toBe('#A78BFA');
    expect(soc.strokeWidth).toBe(2);
  });
});
