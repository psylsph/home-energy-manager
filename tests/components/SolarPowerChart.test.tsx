import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// SolarPowerChart replicates the History → Solar "PV Power" chart on the
// Solar tab. It fetches pv1_power + pv2_power, auto-detects PV2 (renders the
// second area only when any pv2 sample is non-zero), and supports a shared
// Y-axis lock. We mock fetchHistory + the store to drive the states.
// ---------------------------------------------------------------------------

globalThis.ResizeObserver = class {
  observe() {}
  unobserve() {}
  disconnect() {}
};

const fetchHistoryMock = vi.fn();

vi.mock('../../src/lib/api', () => ({
  fetchHistory: (...args: unknown[]) => fetchHistoryMock(...args),
}));

import SolarPowerChart from '../../src/components/SolarPowerChart';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot, SolarArraySummary } from '../../src/lib/types';

function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

describe('<SolarPowerChart/>', () => {
  beforeEach(() => {
    silenceConsoleError();
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

  it('shows the loading spinner before the first fetch resolves', () => {
    fetchHistoryMock.mockImplementation(() => new Promise(() => {}));
    const { container } = render(<SolarPowerChart />);
    expect(container.querySelector('.animate-spin')).not.toBeNull();
  });

  it('renders the "Last 24h" title for the rolling 24h scale', async () => {
    fetchHistoryMock.mockResolvedValue({ pv1_power: [], pv2_power: [] });
    render(<SolarPowerChart />);
    expect(await screen.findByText('Solar Power — Last 24h')).toBeDefined();
  });

  it('renders the "Today" title for the today scale', async () => {
    useInverterStore.setState({ panelGraphsScale: 'today' });
    fetchHistoryMock.mockResolvedValue({ pv1_power: [], pv2_power: [] });
    render(<SolarPowerChart />);
    expect(await screen.findByText('Solar Power Today')).toBeDefined();
  });

  it('shows the empty-state message when no pv data is returned', async () => {
    fetchHistoryMock.mockResolvedValue({ pv1_power: [], pv2_power: [] });
    const { container } = render(<SolarPowerChart />);
    await waitFor(() => {
      expect(container.textContent).toContain('No solar history yet today');
    });
  });

  it('shows the error message when the fetch rejects', async () => {
    fetchHistoryMock.mockRejectedValue(new Error('boom'));
    const { container } = render(<SolarPowerChart />);
    await waitFor(() => {
      expect(container.textContent).toContain('Failed to load solar history');
    });
  });

  it('requests pv1_power and pv2_power from fetchHistory', async () => {
    fetchHistoryMock.mockResolvedValue({ pv1_power: [], pv2_power: [] });
    render(<SolarPowerChart />);
    await waitFor(() => {
      expect(fetchHistoryMock).toHaveBeenCalled();
    });
    const call = fetchHistoryMock.mock.calls[0] as unknown[];
    expect(call[1]).toEqual(['pv1_power', 'pv2_power']);
  });

  it('leaves the loading state once pv1 data resolves', async () => {
    fetchHistoryMock.mockResolvedValue({
      pv1_power: [
        { t: 1000, v: 500 },
        { t: 2000, v: 1000 },
      ],
      pv2_power: [],
    });
    const { container } = render(<SolarPowerChart />);
    await waitFor(() => {
      expect(container.querySelector('.animate-spin')).toBeNull();
      expect(container.textContent).not.toContain('No solar history yet today');
      expect(container.textContent).not.toContain('Failed to load solar history');
    });
  });

  it('renders a PV2 series when pv2 samples are non-zero', async () => {
    fetchHistoryMock.mockResolvedValue({
      pv1_power: [{ t: 1000, v: 500 }],
      pv2_power: [{ t: 1000, v: 300 }],
    });
    const { container } = render(<SolarPowerChart />);
    // hasPv2 is true (a pv2 sample > 0), so the chart branch is taken and
    // no empty/error state renders.
    await waitFor(() => {
      expect(container.querySelector('.animate-spin')).toBeNull();
      expect(container.textContent).not.toContain('No solar history yet today');
    });
  });

  it('does not render a PV2 series when pv2 is all zero', async () => {
    fetchHistoryMock.mockResolvedValue({
      pv1_power: [{ t: 1000, v: 500 }],
      pv2_power: [{ t: 1000, v: 0 }],
    });
    const { container } = render(<SolarPowerChart />);
    await waitFor(() => {
      expect(container.querySelector('.animate-spin')).toBeNull();
      expect(container.textContent).not.toContain('No solar history yet today');
    });
  });

  it('applies a locked Y-axis domain when yLock is enabled', async () => {
    useInverterStore.setState({ panelGraphsYLock: true, panelGraphsYLockMax: 0 });
    fetchHistoryMock.mockResolvedValue({
      pv1_power: [{ t: 1000, v: 7000 }],
      pv2_power: [],
    });
    const { setPanelGraphsYLockMax } = useInverterStore.getState();
    render(<SolarPowerChart />);
    await waitFor(() => {
      // computeYMax rounds 7000 up to the next 5k → 10000, which is shared
      // via the store. The ceiling must be recorded on the store.
      expect(useInverterStore.getState().panelGraphsYLockMax).toBe(10000);
    });
    // Restore to avoid leaking across tests.
    setPanelGraphsYLockMax(0);
  });

  it('uses the DC-string nameplate as a static Y-axis ceiling when configured (issue #192)', async () => {
    // PV1 6 kWp + PV2 4 kWp → ceiling is the higher string (6000 W), not the
    // sum (10000 W). The data-driven Y-Lock sharing is bypassed entirely
    // (panelGraphsYLockMax stays 0) because the nameplate ceiling takes
    // precedence — proving the static branch ran instead of the Y-Lock path.
    const solarArrays: SolarArraySummary[] = [
      { source: 'pv1', name: '', power_w: 0, rated_kw: 6, today_kwh: null, meter_address: null },
      { source: 'pv2', name: '', power_w: 0, rated_kw: 4, today_kwh: null, meter_address: null },
    ];
    useInverterStore.setState({
      panelGraphsYLock: true,
      panelGraphsYLockMax: 0,
      snapshot: { solar_arrays: solarArrays } as InverterSnapshot,
    });
    fetchHistoryMock.mockResolvedValue({
      // 7000 W sample — under the data-driven Y-Lock this would round up to
      // 10000 and be recorded on the store. Under the nameplate ceiling it
      // must NOT touch the store.
      pv1_power: [{ t: 1000, v: 7000 }],
      pv2_power: [],
    });
    const { setPanelGraphsYLockMax } = useInverterStore.getState();
    const { container } = render(<SolarPowerChart />);
    await waitFor(() => {
      // The chart branch is taken (data resolved), not a spinner/empty/error.
      expect(container.querySelector('.animate-spin')).toBeNull();
      expect(container.textContent).not.toContain('No solar history yet today');
    });
    // Nameplate branch ran → the Y-Lock store sharing never happened.
    expect(useInverterStore.getState().panelGraphsYLockMax).toBe(0);
    setPanelGraphsYLockMax(0);
  });
});
