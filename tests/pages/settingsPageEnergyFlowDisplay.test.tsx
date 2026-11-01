import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, within } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
}));

import PanelControlsSection from '../../src/components/settings/PanelControlsSection';
import { useInverterStore } from '../../src/store/useInverterStore';

/** Settings → Panel Controls → Energy Flow Display (issue #422). */
describe('<PanelControlsSection/> — Energy Flow Display', () => {
  beforeEach(() => {
    localStorage.removeItem('energyFlowDisplay');
    useInverterStore.setState({ energyFlowDisplay: 'orbit' });
  });

  afterEach(() => {
    cleanup();
    localStorage.removeItem('energyFlowDisplay');
    useInverterStore.setState({ energyFlowDisplay: 'orbit' });
  });

  const renderSection = () =>
    render(<PanelControlsSection flash={() => {}} initialHiddenPanels={[]} octopusConfigured={false} />);
  const group = () => screen.getByRole('group', { name: 'Energy Flow Display' });

  it('offers Orbit and House with Orbit selected by default', () => {
    renderSection();
    const orbit = within(group()).getByRole('button', { name: 'Orbit' });
    const house = within(group()).getByRole('button', { name: 'House' });
    expect(orbit.getAttribute('aria-pressed')).toBe('true');
    expect(house.getAttribute('aria-pressed')).toBe('false');
  });

  it('saves the choice and applies it straight away', () => {
    renderSection();
    fireEvent.click(within(group()).getByRole('button', { name: 'House' }));
    expect(useInverterStore.getState().energyFlowDisplay).toBe('house');
    expect(localStorage.getItem('energyFlowDisplay')).toBe('house');
    expect(within(group()).getByRole('button', { name: 'House' }).getAttribute('aria-pressed')).toBe('true');

    fireEvent.click(within(group()).getByRole('button', { name: 'Orbit' }));
    expect(useInverterStore.getState().energyFlowDisplay).toBe('orbit');
  });

  it('shows the node status words toggle only for the orbit', () => {
    renderSection();
    expect(screen.queryByText('Show Node Status Words')).not.toBeNull();
    fireEvent.click(within(group()).getByRole('button', { name: 'House' }));
    expect(screen.queryByText('Show Node Status Words')).toBeNull();
    // The noise threshold applies to both views and stays.
    expect(screen.getByText(/Flows below this wattage show as zero/)).toBeDefined();
  });

  it('keeps the ⓘ out of the group name and gives a one-line hint', () => {
    renderSection();
    expect(group()).toBeDefined();
    expect(screen.getByText('How the Status page draws your energy flows.')).toBeDefined();
    expect(screen.getByRole('button', { name: 'More about Energy Flow Display' })).toBeDefined();
  });
});

/** Settings → Panel Controls → Show Today Statistics (issue #422, round 2). */
describe('<PanelControlsSection/> — Show Today Statistics', () => {
  beforeEach(() => {
    localStorage.removeItem('showTodayStats');
    localStorage.removeItem('energyFlowDisplay');
    useInverterStore.setState({ showTodayStats: true, energyFlowDisplay: 'orbit' });
  });

  afterEach(() => {
    cleanup();
    localStorage.removeItem('showTodayStats');
    localStorage.removeItem('energyFlowDisplay');
    useInverterStore.setState({ showTodayStats: true, energyFlowDisplay: 'orbit' });
  });

  const renderSection = () =>
    render(<PanelControlsSection flash={() => {}} initialHiddenPanels={[]} octopusConfigured={false} />);

  it('is on for the orbit by default and saves a change', () => {
    renderSection();
    const toggle = screen.getByRole('switch', { name: 'Show Today Statistics' });
    expect(toggle.getAttribute('aria-checked')).toBe('true');
    fireEvent.click(toggle);
    expect(useInverterStore.getState().showTodayStats).toBe(false);
    expect(localStorage.getItem('showTodayStats')).toBe('false');
    expect(screen.getByRole('switch', { name: 'Show Today Statistics' }).getAttribute('aria-checked')).toBe('false');
  });

  it('starts off when the house view is chosen, and the hint says so', () => {
    // The store applies the house default on the switch; nothing is saved.
    useInverterStore.getState().setEnergyFlowDisplay('house');
    renderSection();
    expect(screen.getByRole('switch', { name: 'Show Today Statistics' }).getAttribute('aria-checked')).toBe('false');
    expect(screen.getByText(/Hidden by default with the house view/)).toBeDefined();
  });

  it('keeps a saved show on the house view', () => {
    localStorage.setItem('showTodayStats', 'true');
    useInverterStore.setState({ showTodayStats: true });
    useInverterStore.getState().setEnergyFlowDisplay('house');
    renderSection();
    expect(screen.getByRole('switch', { name: 'Show Today Statistics' }).getAttribute('aria-checked')).toBe('true');
  });
});
