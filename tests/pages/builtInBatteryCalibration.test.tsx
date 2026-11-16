import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, act } from '@testing-library/react';
import { makeSnapshot } from '../fixtures/snapshot';
import type { InverterSnapshot } from '../../src/lib/types';

const apiPost = vi.fn();
vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(),
  apiPost: (...args: unknown[]) => apiPost(...args),
}));

import BuiltInBatteryCalibration from '../../src/components/BuiltInBatteryCalibration';
import { useInverterStore } from '../../src/store/useInverterStore';

function show(overrides: Partial<InverterSnapshot> = {}) {
  useInverterStore.setState({
    snapshot: makeSnapshot({ supports_battery_calibration: true, battery_calibration_stage: 0, ...overrides }),
  });
}

describe('BuiltInBatteryCalibration (issue #477)', () => {
  beforeEach(() => {
    apiPost.mockReset();
    vi.spyOn(window, 'confirm').mockReturnValue(true);
  });
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    useInverterStore.setState({ snapshot: null });
  });

  it('is offered outside developer mode when the inverter supports it', () => {
    useInverterStore.setState({ developerMode: false });
    show();
    render(<BuiltInBatteryCalibration />);
    expect(screen.getByRole('heading', { name: 'Battery Calibration' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Start Calibration' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Charge Only' })).toBeTruthy();
  });

  it('is hidden when the inverter does not support it (three-phase, HV, no battery)', () => {
    show({ supports_battery_calibration: false });
    const { container } = render(<BuiltInBatteryCalibration />);
    expect(container.textContent).toBe('');
  });

  it('no longer claims the run cannot be cancelled or mentions BMS firmware', () => {
    show();
    const { container } = render(<BuiltInBatteryCalibration />);
    expect(container.textContent).not.toMatch(/cannot be cancelled|BMS firmware|3000/i);
  });

  it('starts with control 1 after confirming', async () => {
    apiPost.mockResolvedValue({ ok: true });
    show();
    render(<BuiltInBatteryCalibration />);
    fireEvent.click(screen.getByRole('button', { name: 'Start Calibration' }));
    await waitFor(() => expect(apiPost).toHaveBeenCalledWith('/api/control/calibration', { stage: 1 }));
    expect(window.confirm).toHaveBeenCalledTimes(1);
    expect(await screen.findByText('✓ Sent')).toBeTruthy();
  });

  it('charge only sends control 3', async () => {
    apiPost.mockResolvedValue({ ok: true });
    show();
    render(<BuiltInBatteryCalibration />);
    fireEvent.click(screen.getByRole('button', { name: 'Charge Only' }));
    await waitFor(() => expect(apiPost).toHaveBeenCalledWith('/api/control/calibration', { stage: 3 }));
  });

  it('does nothing when the confirmation is declined', () => {
    vi.mocked(window.confirm).mockReturnValue(false);
    show();
    render(<BuiltInBatteryCalibration />);
    fireEvent.click(screen.getByRole('button', { name: 'Start Calibration' }));
    expect(apiPost).not.toHaveBeenCalled();
  });

  it('stop is only available while a run is under way and sends control 0', async () => {
    apiPost.mockResolvedValue({ ok: true });
    show({ battery_calibration_stage: 0 });
    const idle = render(<BuiltInBatteryCalibration />);
    expect((screen.getByRole('button', { name: 'Stop Calibration' }) as HTMLButtonElement).disabled).toBe(true);
    idle.unmount();

    show({ battery_calibration_stage: 3 });
    render(<BuiltInBatteryCalibration />);
    expect(screen.getByText('Charging…')).toBeTruthy();
    expect((screen.getByRole('button', { name: 'Start Calibration' }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole('button', { name: 'Charge Only' }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: 'Stop Calibration' }));
    await waitFor(() => expect(apiPost).toHaveBeenCalledWith('/api/control/calibration', { stage: 0 }));
  });

  it('lets Stop clear a finished or unrecognised stage', () => {
    for (const stage of [7, 9]) {
      show({ battery_calibration_stage: stage });
      const { unmount } = render(<BuiltInBatteryCalibration />);
      expect((screen.getByRole('button', { name: 'Stop Calibration' }) as HTMLButtonElement).disabled).toBe(false);
      unmount();
    }
  });

  it('locks every button until the inverter reading moves, so a second command cannot follow', async () => {
    apiPost.mockResolvedValue({ ok: true });
    show({ battery_calibration_stage: 0 });
    render(<BuiltInBatteryCalibration />);
    fireEvent.click(screen.getByRole('button', { name: 'Start Calibration' }));
    await waitFor(() => expect(apiPost).toHaveBeenCalledTimes(1));
    await screen.findByText('✓ Sent');
    for (const name of [/Sent|Start Calibration/, 'Charge Only', 'Stop Calibration']) {
      expect((screen.getByRole('button', { name }) as HTMLButtonElement).disabled).toBe(true);
    }
    // The next poll shows the routine has begun: Stop opens up.
    act(() => show({ battery_calibration_stage: 1 }));
    expect((screen.getByRole('button', { name: 'Stop Calibration' }) as HTMLButtonElement).disabled).toBe(false);
  });

  it('unlocks straight away when the command fails', async () => {
    apiPost.mockRejectedValue(new Error('refused'));
    show();
    render(<BuiltInBatteryCalibration />);
    fireEvent.click(screen.getByRole('button', { name: 'Charge Only' }));
    await screen.findByText('refused');
    expect((screen.getByRole('button', { name: '✗ Error' }) as HTMLButtonElement).disabled).toBe(false);
    expect((screen.getByRole('button', { name: 'Start Calibration' }) as HTMLButtonElement).disabled).toBe(false);
  });

  it('shows an error when the request fails', async () => {
    apiPost.mockRejectedValue(new Error('refused'));
    show();
    render(<BuiltInBatteryCalibration />);
    fireEvent.click(screen.getByRole('button', { name: 'Start Calibration' }));
    expect(await screen.findByText('✗ Error')).toBeTruthy();
    expect(screen.getByText('refused')).toBeTruthy();
  });
});
