import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, cleanup, waitFor } from '@testing-library/react';

// Issue #476: the Battery page's SOC chart matches History and Power, drawn
// in the shared SOC colour at the standard 2px width.

const fetchHistoryMock = vi.fn();
vi.mock('../../src/lib/api', () => ({
  fetchHistory: (...args: unknown[]) => fetchHistoryMock(...args),
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
  Tooltip: () => null,
  XAxis: () => null,
  YAxis: () => null,
}));

import BatterySocChart from '../../src/components/BatterySocChart';
import { SOC_COLOR } from '../../src/lib/chartSeries';
import { useInverterStore } from '../../src/store/useInverterStore';

describe('<BatterySocChart/> — SOC colour (issue #476)', () => {
  beforeEach(() => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    areas.length = 0;
    fetchHistoryMock.mockResolvedValue({ soc: [{ t: 1_700_000_000_000, v: 40 }, { t: 1_700_000_300_000, v: 45 }] });
    useInverterStore.setState({ panelGraphsScale: '24h', gridLineWeight: 'normal' });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  it('is the readable violet the History and Power pages use', () => {
    expect(SOC_COLOR).toBe('#A78BFA');
  });

  it('draws the SOC line in it at the standard width', async () => {
    render(<BatterySocChart />);
    await waitFor(() => expect(areas.length).toBeGreaterThan(0));
    const soc = areas.at(-1)!;
    expect(soc.stroke).toBe(SOC_COLOR);
    expect(soc.strokeWidth).toBe(2);
  });
});
