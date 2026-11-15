import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, act } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
}));

import PanelControlsSection from '../../src/components/settings/PanelControlsSection';
import { apiPost } from '../../src/lib/api';
import { useInverterStore } from '../../src/store/useInverterStore';

/**
 * Settings → Panel Controls → Show Car Charging Slots (issue #474): the
 * car's planned Octopus charges in the house view's Next Charge card. Saved
 * in settings.json, because the backend fetches the list for it.
 */
describe('<PanelControlsSection/> — Show Car Charging Slots', () => {
  beforeEach(() => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(apiPost).mockReset();
    vi.mocked(apiPost).mockResolvedValue({ ok: true, data: {} });
    useInverterStore.setState({ showCarCharging: false, energyFlowDisplay: 'house' });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    useInverterStore.setState({ showCarCharging: false, energyFlowDisplay: 'orbit' });
  });

  const renderSection = (octopusConfigured: boolean) =>
    render(
      <PanelControlsSection flash={() => {}} initialHiddenPanels={[]} octopusConfigured={octopusConfigured} />,
    );
  const toggle = () => screen.queryByRole('switch', { name: 'Show Car Charging Slots' });

  it('is only offered once an Octopus account is saved', () => {
    renderSection(false);
    expect(toggle()).toBeNull();
    expect(screen.queryByText('Show Car Charging Slots')).toBeNull();
  });

  it('stays offered with the orbit view, so a switch that makes HEM ask Octopus can always be turned off', () => {
    useInverterStore.setState({ energyFlowDisplay: 'orbit', showCarCharging: true });
    renderSection(true);
    expect(toggle()!.getAttribute('aria-checked')).toBe('true');
  });

  it('starts from the saved value, off by default', () => {
    renderSection(true);
    expect(toggle()!.getAttribute('aria-checked')).toBe('false');
    cleanup();
    useInverterStore.setState({ showCarCharging: true });
    renderSection(true);
    expect(toggle()!.getAttribute('aria-checked')).toBe('true');
  });

  it('shows what was saved after the Display view is switched and back', async () => {
    renderSection(true);
    fireEvent.click(toggle()!);
    await waitFor(() => expect(useInverterStore.getState().showCarCharging).toBe(true));
    act(() => useInverterStore.setState({ energyFlowDisplay: 'orbit' }));
    act(() => useInverterStore.setState({ energyFlowDisplay: 'house' }));
    expect(toggle()!.getAttribute('aria-checked')).toBe('true');
  });

  it('saves to the backend and turns the card line on and off', async () => {
    renderSection(true);
    fireEvent.click(toggle()!);
    await waitFor(() =>
      expect(apiPost).toHaveBeenCalledWith('/api/settings', { status_show_car_charging: true }),
    );
    await waitFor(() => expect(useInverterStore.getState().showCarCharging).toBe(true));
    fireEvent.click(toggle()!);
    await waitFor(() =>
      expect(apiPost).toHaveBeenLastCalledWith('/api/settings', { status_show_car_charging: false }),
    );
    await waitFor(() => expect(useInverterStore.getState().showCarCharging).toBe(false));
  });

  it('takes no second click while its save is in flight (issue #476)', async () => {
    let finish: (value: unknown) => void = () => {};
    vi.mocked(apiPost).mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    renderSection(true);
    fireEvent.click(toggle()!);
    await waitFor(() => expect(toggle()!.getAttribute('aria-disabled')).toBe('true'));
    fireEvent.click(toggle()!);
    expect(apiPost).toHaveBeenCalledTimes(1);
    await act(async () => finish({ ok: true, data: {} }));
    expect(toggle()!.getAttribute('aria-disabled')).not.toBe('true');
    expect(useInverterStore.getState().showCarCharging).toBe(true);
  });

  it('moves back, leaving the card as it was, when the save fails', async () => {
    vi.mocked(apiPost).mockRejectedValueOnce(new Error('disk full'));
    renderSection(true);
    fireEvent.click(toggle()!);
    await waitFor(() => expect(toggle()!.getAttribute('aria-checked')).toBe('false'));
    expect(useInverterStore.getState().showCarCharging).toBe(false);
  });
});
