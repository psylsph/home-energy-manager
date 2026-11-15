import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, act } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
}));

import PanelControlsSection from '../../src/components/settings/PanelControlsSection';
import { apiPost } from '../../src/lib/api';
import { useInverterStore } from '../../src/store/useInverterStore';

/**
 * Settings → Panel Controls → Show Battery Runtime (issue #472): when the
 * battery reaches its reserve, in the house view's Battery Level card. Off
 * by default, saved in settings.json so every browser and device shares it
 * (it used to be per-browser localStorage, which looked broken elsewhere).
 */
describe('<PanelControlsSection/> — Show Battery Runtime', () => {
  const flash = vi.fn();

  beforeEach(() => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(apiPost).mockReset();
    vi.mocked(apiPost).mockResolvedValue({ ok: true, data: {} });
    flash.mockReset();
    localStorage.removeItem('showBatteryRuntime');
    useInverterStore.setState({ showBatteryRuntime: false, energyFlowDisplay: 'house' });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    localStorage.removeItem('showBatteryRuntime');
    useInverterStore.setState({ showBatteryRuntime: false, energyFlowDisplay: 'orbit' });
  });

  const renderSection = () =>
    render(<PanelControlsSection flash={flash} initialHiddenPanels={[]} octopusConfigured={false} />);
  const toggle = () => screen.queryByRole('switch', { name: 'Show Battery Runtime' });

  it('starts from the saved value, off by default', () => {
    renderSection();
    expect(toggle()!.getAttribute('aria-checked')).toBe('false');
    cleanup();
    useInverterStore.setState({ showBatteryRuntime: true });
    renderSection();
    expect(toggle()!.getAttribute('aria-checked')).toBe('true');
  });

  it('saves to settings.json in both directions, not to localStorage', async () => {
    renderSection();
    fireEvent.click(toggle()!);
    await waitFor(() =>
      expect(apiPost).toHaveBeenCalledWith('/api/settings', { status_show_battery_runtime: true }),
    );
    await waitFor(() => expect(useInverterStore.getState().showBatteryRuntime).toBe(true));
    await waitFor(() => expect(flash).toHaveBeenLastCalledWith('Battery runtime shown', true));
    fireEvent.click(toggle()!);
    await waitFor(() =>
      expect(apiPost).toHaveBeenLastCalledWith('/api/settings', { status_show_battery_runtime: false }),
    );
    await waitFor(() => expect(useInverterStore.getState().showBatteryRuntime).toBe(false));
    expect(localStorage.getItem('showBatteryRuntime')).toBeNull();
  });

  it('moves back and says so when the save fails', async () => {
    vi.mocked(apiPost).mockRejectedValueOnce(new Error('disk full'));
    renderSection();
    fireEvent.click(toggle()!);
    await waitFor(() => expect(flash).toHaveBeenCalledWith('Failed to save: disk full', false));
    expect(toggle()!.getAttribute('aria-checked')).toBe('false');
    expect(useInverterStore.getState().showBatteryRuntime).toBe(false);
  });

  // Issue #476: two quick clicks sent two saves that could land in either
  // order, leaving settings.json and the switch disagreeing.
  it('takes no second click while its save is in flight', async () => {
    let finish: (value: unknown) => void = () => {};
    vi.mocked(apiPost).mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    renderSection();
    fireEvent.click(toggle()!);
    await waitFor(() => expect(toggle()!.getAttribute('aria-disabled')).toBe('true'));
    fireEvent.click(toggle()!);
    expect(apiPost).toHaveBeenCalledTimes(1);
    expect(useInverterStore.getState().showBatteryRuntime).toBe(true);
    await act(async () => finish({ ok: true, data: {} }));
    expect(toggle()!.getAttribute('aria-disabled')).not.toBe('true');
    fireEvent.click(toggle()!);
    await waitFor(() =>
      expect(apiPost).toHaveBeenLastCalledWith('/api/settings', { status_show_battery_runtime: false }),
    );
  });

  it('marks the change as the user\'s, so a settings load in flight leaves it', async () => {
    renderSection();
    const before = useInverterStore.getState().statusToggleRevision.showBatteryRuntime;
    fireEvent.click(toggle()!);
    await waitFor(() => expect(useInverterStore.getState().showBatteryRuntime).toBe(true));
    expect(useInverterStore.getState().statusToggleRevision.showBatteryRuntime).toBeGreaterThan(before);
  });

  it('is only offered with the house view, which has the Battery Level card', () => {
    useInverterStore.setState({ energyFlowDisplay: 'orbit' });
    renderSection();
    expect(toggle()).toBeNull();
  });
});
