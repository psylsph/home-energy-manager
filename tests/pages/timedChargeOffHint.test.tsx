import { describe, it, expect, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import { MemoryRouter } from 'react-router';
import TimedChargeOffHint from '../../src/components/TimedChargeOffHint';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot } from '../../src/lib/types';

const SLOT = { enabled: true, start_hour: 0, start_minute: 30, end_hour: 5, end_minute: 30, target_soc: 100 };

function setSnapshot(over: Partial<InverterSnapshot>) {
  useInverterStore.setState({
    connectionState: 'connected',
    snapshot: {
      device_type_code: '2001',
      charging_mode: 'standard',
      enable_charge: false,
      charge_slots: [SLOT],
      forecast_plan_auto_refresh: true,
      ...over,
    } as InverterSnapshot,
  });
}

afterEach(() => {
  cleanup();
  useInverterStore.setState({ snapshot: null, connectionState: 'disconnected' });
});

const renderHint = () => render(<MemoryRouter><TimedChargeOffHint /></MemoryRouter>);

describe('TimedChargeOffHint (issue #448)', () => {
  it('says the planned charge will not run and links to Control', () => {
    setSnapshot({});
    renderHint();
    expect(screen.getByTestId('timed-charge-off-hint').textContent).toMatch(/Timed Charge is off/);
    expect(screen.getByRole('link', { name: 'Control page' }).getAttribute('href')).toBe('/control');
  });

  it('is hidden while Timed Charge is armed', () => {
    setSnapshot({ enable_charge: true });
    renderHint();
    expect(screen.queryByTestId('timed-charge-off-hint')).toBeNull();
  });

  it('is hidden when automatic planning is off', () => {
    setSnapshot({ forecast_plan_auto_refresh: false, forecast_plan_auto_apply_enabled: false });
    renderHint();
    expect(screen.queryByTestId('timed-charge-off-hint')).toBeNull();
  });

  it('is hidden once the plan has cleared the slot', () => {
    setSnapshot({ charge_slots: [{ ...SLOT, enabled: false }] });
    renderHint();
    expect(screen.queryByTestId('timed-charge-off-hint')).toBeNull();
  });

  it('is hidden while the connection is down and the snapshot may be stale', () => {
    setSnapshot({});
    useInverterStore.setState({ connectionState: 'reconnecting' });
    renderHint();
    expect(screen.queryByTestId('timed-charge-off-hint')).toBeNull();
  });

  it('is hidden without a snapshot', () => {
    renderHint();
    expect(screen.queryByTestId('timed-charge-off-hint')).toBeNull();
  });
});
