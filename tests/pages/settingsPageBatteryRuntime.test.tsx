import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
}));

import PanelControlsSection from '../../src/components/settings/PanelControlsSection';
import { useInverterStore } from '../../src/store/useInverterStore';

/**
 * Settings → Panel Controls → Show Battery Runtime (issue #472): when the
 * battery reaches its reserve, in the house view's Battery Level card. Off
 * by default, kept in localStorage.
 */
describe('<PanelControlsSection/> — Show Battery Runtime', () => {
  beforeEach(() => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
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
    render(<PanelControlsSection flash={() => {}} initialHiddenPanels={[]} octopusConfigured={false} />);
  const toggle = () => screen.queryByRole('switch', { name: 'Show Battery Runtime' });

  it('is off by default', () => {
    renderSection();
    expect(toggle()!.getAttribute('aria-checked')).toBe('false');
    expect(localStorage.getItem('showBatteryRuntime')).toBeNull();
  });

  it('saves the choice to localStorage and the store in both directions', () => {
    renderSection();
    fireEvent.click(toggle()!);
    expect(useInverterStore.getState().showBatteryRuntime).toBe(true);
    expect(localStorage.getItem('showBatteryRuntime')).toBe('true');
    fireEvent.click(toggle()!);
    expect(useInverterStore.getState().showBatteryRuntime).toBe(false);
    expect(localStorage.getItem('showBatteryRuntime')).toBe('false');
  });

  it('is only offered with the house view, which has the Battery Level card', () => {
    useInverterStore.setState({ energyFlowDisplay: 'orbit' });
    renderSection();
    expect(toggle()).toBeNull();
  });
});
