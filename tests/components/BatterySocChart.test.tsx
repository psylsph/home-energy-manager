import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// BatterySocChart replicates the History → Battery "SOC %" chart on the
// Battery tab. It fetches `soc` history, applies the spike filter, and
// renders one of four states: loading spinner, error, empty, or the chart.
// We mock fetchHistory to drive each state and assert the rendered text.
// ---------------------------------------------------------------------------

// recharts' ResponsiveContainer needs ResizeObserver under jsdom.
globalThis.ResizeObserver = class {
  observe() {}
  unobserve() {}
  disconnect() {}
};

const fetchHistoryMock = vi.fn();

vi.mock('../../src/lib/api', () => ({
  fetchHistory: (...args: unknown[]) => fetchHistoryMock(...args),
}));

import BatterySocChart from '../../src/components/BatterySocChart';
import { useInverterStore } from '../../src/store/useInverterStore';

function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

describe('<BatterySocChart/>', () => {
  beforeEach(() => {
    silenceConsoleError();
    fetchHistoryMock.mockReset();
    useInverterStore.setState({
      panelGraphsScale: '24h',
      gridLineWeight: 'normal',
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  it('shows the loading spinner before the first fetch resolves', () => {
    fetchHistoryMock.mockImplementation(() => new Promise(() => {}));
    const { container } = render(<BatterySocChart />);
    expect(container.querySelector('.animate-spin')).not.toBeNull();
  });

  it('renders the "Last 24h" title for the rolling 24h scale', async () => {
    fetchHistoryMock.mockResolvedValue({ soc: [] });
    render(<BatterySocChart />);
    expect(await screen.findByText('SOC — Last 24h')).toBeDefined();
  });

  it('renders the "SOC Today" title for the today scale', async () => {
    useInverterStore.setState({ panelGraphsScale: 'today' });
    fetchHistoryMock.mockResolvedValue({ soc: [] });
    render(<BatterySocChart />);
    expect(await screen.findByText('SOC Today')).toBeDefined();
  });

  it('shows the empty-state message when no soc points are returned', async () => {
    fetchHistoryMock.mockResolvedValue({ soc: [] });
    const { container } = render(<BatterySocChart />);
    await waitFor(() => {
      expect(container.textContent).toContain('No SOC history yet today');
    });
  });

  it('shows the empty-state subtitle', async () => {
    fetchHistoryMock.mockResolvedValue({ soc: [] });
    const { container } = render(<BatterySocChart />);
    await waitFor(() => {
      expect(container.textContent).toContain('History is recorded while the app is running');
    });
  });

  it('shows the error message when the fetch rejects', async () => {
    fetchHistoryMock.mockRejectedValue(new Error('boom'));
    const { container } = render(<BatterySocChart />);
    await waitFor(() => {
      expect(container.textContent).toContain('Failed to load SOC history');
    });
  });

  it('leaves the loading spinner once soc data resolves', async () => {
    fetchHistoryMock.mockResolvedValue({
      soc: [
        { t: 1000, v: 50 },
        { t: 2000, v: 60 },
      ],
    });
    const { container } = render(<BatterySocChart />);
    // Once data resolves, the spinner / empty / error states are all gone —
    // the component has moved into the chart-render branch.
    await waitFor(() => {
      expect(container.querySelector('.animate-spin')).toBeNull();
      expect(container.textContent).not.toContain('No SOC history yet today');
      expect(container.textContent).not.toContain('Failed to load SOC history');
    });
  });

  it('requests the soc field from fetchHistory', async () => {
    fetchHistoryMock.mockResolvedValue({ soc: [] });
    render(<BatterySocChart />);
    await waitFor(() => {
      expect(fetchHistoryMock).toHaveBeenCalled();
    });
    const call = fetchHistoryMock.mock.calls[0] as unknown[];
    // args: (range, fields, offset, rolling)
    expect(call[1]).toEqual(['soc']);
  });

  it('passes the rolling flag based on the scale', async () => {
    useInverterStore.setState({ panelGraphsScale: '24h' });
    fetchHistoryMock.mockResolvedValue({ soc: [] });
    render(<BatterySocChart />);
    await waitFor(() => {
      expect(fetchHistoryMock).toHaveBeenCalled();
    });
    const call = fetchHistoryMock.mock.calls[0] as unknown[];
    // 24h is a rolling range → rolling=true.
    expect(call[3]).toBe(true);
  });
});
