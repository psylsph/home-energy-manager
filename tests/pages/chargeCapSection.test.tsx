import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, act } from '@testing-library/react';
import type { ChargeCapStatus } from '../../src/lib/chargeCap';
import { useInverterStore } from '../../src/store/useInverterStore';
import { makeSnapshot } from '../fixtures/snapshot';

const apiGet = vi.fn();
const apiPost = vi.fn();
vi.mock('../../src/lib/api', () => ({
  apiGet: (...args: unknown[]) => apiGet(...args),
  apiPost: (...args: unknown[]) => apiPost(...args),
}));

import ChargeCapSection from '../../src/components/ChargeCapSection';

function status(overrides: Partial<ChargeCapStatus> = {}): ChargeCapStatus {
  return {
    ok: true,
    enabled: false,
    cap_soc: 95,
    resume_soc: 92,
    state: 'off',
    supported: true,
    connected: true,
    holding: false,
    min_cap_soc: 50,
    max_cap_soc: 99,
    min_resume_soc: 40,
    min_gap: 2,
    ...overrides,
  };
}

const save = () => screen.getByRole('button', { name: 'Save' }) as HTMLButtonElement;

describe('ChargeCapSection', () => {
  beforeEach(() => {
    apiGet.mockReset();
    apiPost.mockReset();
    useInverterStore.setState({ snapshot: null });
  });
  afterEach(cleanup);

  it('is hidden on an inverter that cannot pause charging', async () => {
    apiGet.mockResolvedValue(status({ supported: false, state: 'unsupported' }));
    const { container } = render(<ChargeCapSection />);
    await waitFor(() => expect(apiGet).toHaveBeenCalledWith('/api/charge-cap'));
    await Promise.resolve();
    expect(container.textContent).toBe('');
  });

  it('stays visible while a pause the cap started is still being handed back', async () => {
    apiGet.mockResolvedValue(status({ supported: false, holding: true, state: 'releasing' }));
    render(<ChargeCapSection />);
    expect(await screen.findByRole('heading', { name: 'Charge Cap' })).toBeTruthy();
  });

  it('says when it could not load', async () => {
    apiGet.mockRejectedValue(new Error('offline'));
    render(<ChargeCapSection />);
    expect(await screen.findByText("Couldn't load the charge cap.")).toBeTruthy();
  });

  it('shows both warnings and the last full charge once switched on', async () => {
    const lastFull = Math.floor(Date.now() / 1000) - 12 * 86_400 - 60;
    apiGet.mockResolvedValue(status({ enabled: true, state: 'waiting', last_full_charge_at: lastFull, full_charge_days: 30 }));
    render(<ChargeCapSection />);
    expect(await screen.findByTestId('charge-cap-hem-warning')).toBeTruthy();
    expect(screen.getByTestId('charge-cap-hem-warning').textContent).toMatch(/Only works while HEM runs/);
    expect(screen.getByTestId('charge-cap-balance-warning').textContent).toBe(
      'Let the battery reach 100% every 30 days so the cells balance. Last full charge: 12 days ago.',
    );
    expect(screen.getByTestId('charge-cap-state').textContent).toBe('Charging normally until the battery reaches 95%.');
    // The full story sits behind the ⓘ (issue #423).
    expect(screen.getByRole('button', { name: 'More about Charge Cap' })).toBeTruthy();
  });

  it('asks again once the backend has read the inverter', async () => {
    apiGet
      .mockResolvedValueOnce(status({ supported: false, connected: false, state: 'unsupported' }))
      .mockResolvedValue(status());
    useInverterStore.setState({ snapshot: makeSnapshot({ timestamp: 1000 }) });
    render(<ChargeCapSection />);
    await waitFor(() => expect(apiGet).toHaveBeenCalledTimes(1));
    // Settle any re-render the first answer causes: still one ask per snapshot.
    await act(async () => {});
    expect(apiGet).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole('heading', { name: 'Charge Cap' })).toBeNull();
    act(() => useInverterStore.setState({ snapshot: makeSnapshot({ timestamp: 1010 }) }));
    expect(await screen.findByRole('heading', { name: 'Charge Cap', level: 3 })).toBeInTheDocument();
    expect(apiGet).toHaveBeenCalledTimes(2);
  });

  it('hides the levels and warnings while off', async () => {
    apiGet.mockResolvedValue(status());
    render(<ChargeCapSection />);
    await screen.findByRole('heading', { name: 'Charge Cap' });
    expect(screen.queryByLabelText('Stop Charging at (%)')).toBeNull();
    expect(screen.queryByTestId('charge-cap-hem-warning')).toBeNull();
    expect(save().disabled).toBe(true);
  });

  it('follows the live state from the snapshot', async () => {
    apiGet.mockResolvedValue(status({ enabled: true, state: 'waiting' }));
    useInverterStore.setState({ snapshot: makeSnapshot({ charge_cap_state: 'holding' }) });
    render(<ChargeCapSection />);
    expect((await screen.findByTestId('charge-cap-state')).textContent).toBe(
      'Charging paused at 95%. Charges again at 92%.',
    );
  });

  it('blocks a save with levels the backend would refuse', async () => {
    apiGet.mockResolvedValue(status({ enabled: true, state: 'waiting' }));
    render(<ChargeCapSection />);
    const resume = await screen.findByLabelText('Resume Charging at (%)');
    fireEvent.change(resume, { target: { value: '94' } });
    expect(screen.getByText('Resume charging at least 2% below the cap.')).toBeTruthy();
    expect(save().disabled).toBe(true);
    fireEvent.change(screen.getByLabelText('Stop Charging at (%)'), { target: { value: '100' } });
    expect(screen.getByText('Choose a cap between 50% and 99%.')).toBeTruthy();
  });

  it('saves the switch and both levels', async () => {
    apiGet.mockResolvedValue(status());
    apiPost.mockResolvedValue(status({ enabled: true, cap_soc: 90, resume_soc: 85, state: 'waiting' }));
    render(<ChargeCapSection />);
    fireEvent.click(await screen.findByRole('switch', { name: 'Enable Charge Cap' }));
    fireEvent.change(screen.getByLabelText('Stop Charging at (%)'), { target: { value: '90' } });
    fireEvent.change(screen.getByLabelText('Resume Charging at (%)'), { target: { value: '85' } });
    fireEvent.click(save());
    await screen.findByText('Saved');
    expect(apiPost).toHaveBeenCalledWith('/api/charge-cap', { enabled: true, cap_soc: 90, resume_soc: 85 });
    expect(screen.getByTestId('charge-cap-state').textContent).toBe('Charging normally until the battery reaches 90%.');
    expect(save().disabled).toBe(true);
  });

  it('switches off with the saved levels when the edited ones are hidden and invalid', async () => {
    apiGet.mockResolvedValue(status({ enabled: true, cap_soc: 90, resume_soc: 85, state: 'waiting' }));
    apiPost.mockResolvedValue(status({ enabled: false, cap_soc: 90, resume_soc: 85 }));
    render(<ChargeCapSection />);
    fireEvent.change(await screen.findByLabelText('Stop Charging at (%)'), { target: { value: '60' } });
    fireEvent.click(screen.getByRole('switch', { name: 'Enable Charge Cap' }));
    fireEvent.click(save());
    await screen.findByText('Saved');
    expect(apiPost).toHaveBeenCalledWith('/api/charge-cap', { enabled: false, cap_soc: 90, resume_soc: 85 });
  });

  it('falls back to the fetched state when the snapshot has none', async () => {
    apiGet.mockResolvedValue(status({ enabled: true, cap_soc: 90, resume_soc: 85, state: 'waiting' }));
    useInverterStore.setState({ snapshot: makeSnapshot({ charge_cap_state: '' }) });
    render(<ChargeCapSection />);
    expect((await screen.findByTestId('charge-cap-state')).textContent).toBe('Charging normally until the battery reaches 90%.');
  });

  it('shows the error when a save fails and keeps the edits', async () => {
    apiGet.mockResolvedValue(status());
    apiPost.mockRejectedValue(new Error("This inverter can't pause charging, so the charge cap isn't available"));
    render(<ChargeCapSection />);
    fireEvent.click(await screen.findByRole('switch', { name: 'Enable Charge Cap' }));
    fireEvent.click(save());
    expect(await screen.findByText(/can't pause charging/)).toBeTruthy();
    expect(screen.getByRole('switch', { name: 'Enable Charge Cap' }).getAttribute('aria-checked')).toBe('true');
  });
});
