import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';
import type { CalibrationStatus } from '../../src/lib/calibration';

const apiGet = vi.fn();
const apiPost = vi.fn();
vi.mock('../../src/lib/api', () => ({
  apiGet: (...args: unknown[]) => apiGet(...args),
  apiPost: (...args: unknown[]) => apiPost(...args),
}));

import BatteryCalibrationCycle from '../../src/components/BatteryCalibrationCycle';

const T0 = Date.UTC(2026, 9, 30, 18, 0);

function status(overrides: Partial<CalibrationStatus> = {}): CalibrationStatus {
  return {
    ok: true,
    supported: true,
    running: false,
    calibration: null,
    last: null,
    default_hold_minutes: 120,
    min_hold_minutes: 30,
    max_hold_minutes: 480,
    floor_soc: 4,
    ...overrides,
  };
}

function running(phase: 'discharging' | 'charging' | 'holding' | 'restoring' = 'discharging'): CalibrationStatus {
  return status({
    running: true,
    calibration: {
      phase,
      started_at_ms: T0,
      phase_started_at_ms: T0,
      hold_minutes: 120,
      ...(phase === 'holding' ? { since_ms: T0 } : {}),
      ...(phase === 'restoring' ? { outcome: { result: 'cancelled' as const } } : {}),
    },
  });
}

function serve(...responses: CalibrationStatus[]) {
  let call = 0;
  apiGet.mockImplementation(async (path: string) => {
    if (path !== '/api/battery/calibration') throw new Error(`unexpected GET ${path}`);
    const data = responses[Math.min(call, responses.length - 1)];
    call += 1;
    return data;
  });
}

describe('BatteryCalibrationCycle', () => {
  beforeEach(() => {
    apiGet.mockReset();
    apiPost.mockReset();
    vi.spyOn(window, 'confirm').mockReturnValue(true);
  });
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it('offers a start with a two-hour hold and warns about exporting at full power', async () => {
    serve(status());
    render(<BatteryCalibrationCycle />);
    const hold = (await screen.findByLabelText('Hold at 100% for (Hours)')) as HTMLInputElement;
    expect(hold.value).toBe('2');
    expect(screen.getByRole('button', { name: 'Start Calibration' })).toBeTruthy();
    expect(screen.getByText(/^Exports at full power\. Start a few hours before your cheap rate\./)).toBeTruthy();
    // The cost detail sits behind the ⓘ (issue #423).
    expect(screen.getByRole('button', { name: 'More about the cost of a calibration cycle' })).toBeTruthy();
    expect(screen.getByText(/on a time-of-use tariff/i).getAttribute('role')).toBe('tooltip');
  });

  it('says when the inverter is not supported, with no start button', async () => {
    serve(status({ supported: false }));
    render(<BatteryCalibrationCycle />);
    expect(await screen.findByText(/isn't available for this inverter/i)).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Start Calibration' })).toBeNull();
  });

  it('starts with the chosen hold after confirming', async () => {
    serve(status(), running());
    apiPost.mockResolvedValue({ ok: true });
    render(<BatteryCalibrationCycle />);
    const hold = await screen.findByLabelText('Hold at 100% for (Hours)');
    fireEvent.change(hold, { target: { value: '3' } });
    fireEvent.click(screen.getByRole('button', { name: 'Start Calibration' }));
    await waitFor(() =>
      expect(apiPost).toHaveBeenCalledWith('/api/battery/calibration', { hold_minutes: 180 }),
    );
    expect(window.confirm).toHaveBeenCalled();
    expect(await screen.findByText('Exporting down to 4%')).toBeTruthy();
  });

  it('does nothing if the confirmation is declined', async () => {
    vi.mocked(window.confirm).mockReturnValue(false);
    serve(status());
    render(<BatteryCalibrationCycle />);
    fireEvent.click(await screen.findByRole('button', { name: 'Start Calibration' }));
    expect(apiPost).not.toHaveBeenCalled();
  });

  it('will not start with a hold out of range', async () => {
    serve(status());
    render(<BatteryCalibrationCycle />);
    const hold = await screen.findByLabelText('Hold at 100% for (Hours)');
    fireEvent.change(hold, { target: { value: '12' } });
    const button = screen.getByRole('button', { name: 'Start Calibration' }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    expect(screen.getByText(/between 0.5 and 8 hours/i)).toBeTruthy();
  });

  it('shows why a start was refused', async () => {
    serve(status());
    apiPost.mockRejectedValue(new Error('Stop Force Charge before starting a calibration'));
    render(<BatteryCalibrationCycle />);
    fireEvent.click(await screen.findByRole('button', { name: 'Start Calibration' }));
    expect(await screen.findByText('Stop Force Charge before starting a calibration')).toBeTruthy();
  });

  it('follows a running cycle and lets the user cancel it', async () => {
    serve(running('holding'), running('restoring'));
    apiPost.mockResolvedValue({ ok: true });
    render(<BatteryCalibrationCycle />);
    expect(await screen.findByText('Holding at 100% while the cells balance')).toBeTruthy();
    expect(screen.queryByLabelText('Hold at 100% for (Hours)')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel Calibration' }));
    await waitFor(() => expect(apiPost).toHaveBeenCalledWith('/api/battery/calibration/cancel'));
    expect(await screen.findByText('Cancelling: putting your settings back')).toBeTruthy();
    // Nothing left to cancel while the settings go back.
    expect(screen.queryByRole('button', { name: 'Cancel Calibration' })).toBeNull();
  });

  it('shows how the last cycle ended', async () => {
    serve(status({
      last: {
        started_at_ms: T0,
        finished_at_ms: T0 + 3_600_000,
        outcome: { result: 'failed', reason: 'The battery did not reach 4% within 24 hours' },
      },
    }));
    render(<BatteryCalibrationCycle />);
    expect(await screen.findByText(/The battery did not reach 4% within 24 hours/)).toBeTruthy();
  });

  it('refreshes while a cycle runs', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      serve(running('discharging'), running('charging'));
      render(<BatteryCalibrationCycle />);
      expect(await screen.findByText('Exporting down to 4%')).toBeTruthy();
      await vi.advanceTimersByTimeAsync(31_000);
      expect(await screen.findByText('Charging to 100%')).toBeTruthy();
    } finally {
      vi.useRealTimers();
    }
  });
});
