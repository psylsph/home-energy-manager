import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
}));

import PanelControlsSection from '../../src/components/settings/PanelControlsSection';
import { useInverterStore } from '../../src/store/useInverterStore';

/**
 * Settings → Panel Controls → Home Excludes Car Charging (issue #457): show
 * the Status diagrams' Home reading without the car's charging, off by
 * default so nothing changes until it is turned on.
 */
describe('<PanelControlsSection/> — Home Excludes Car Charging', () => {
  beforeEach(() => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    localStorage.removeItem('homeExcludesCar');
    useInverterStore.setState({ homeExcludesCar: false });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    localStorage.removeItem('homeExcludesCar');
    useInverterStore.setState({ homeExcludesCar: false });
  });

  const renderSection = () =>
    render(<PanelControlsSection flash={() => {}} initialHiddenPanels={[]} octopusConfigured={false} />);
  const toggle = () => screen.getByRole('switch', { name: 'Home Excludes Car Charging' });

  it('is off by default with nothing saved', () => {
    renderSection();
    expect(toggle().getAttribute('aria-checked')).toBe('false');
    expect(localStorage.getItem('homeExcludesCar')).toBeNull();
    expect(useInverterStore.getState().homeExcludesCar).toBe(false);
  });

  it('saves the choice to localStorage and the store in both directions', () => {
    renderSection();
    fireEvent.click(toggle());
    expect(useInverterStore.getState().homeExcludesCar).toBe(true);
    expect(localStorage.getItem('homeExcludesCar')).toBe('true');
    expect(toggle().getAttribute('aria-checked')).toBe('true');

    fireEvent.click(toggle());
    expect(useInverterStore.getState().homeExcludesCar).toBe(false);
    expect(localStorage.getItem('homeExcludesCar')).toBe('false');
    expect(toggle().getAttribute('aria-checked')).toBe('false');
  });

  it('shows a one-line hint and keeps the full explanation behind the ⓘ', () => {
    renderSection();
    expect(screen.getByText("Show the Status Home load without the car's charging.")).toBeDefined();
    const tip = screen.getByRole('button', { name: 'More about Home Excludes Car Charging' });
    // The ⓘ is not part of the row's labelling element.
    expect(tip.closest('label')).toBeNull();
    fireEvent.click(tip);
    const tipEl = screen.getByRole('tooltip');
    expect(tipEl.textContent).toContain('never below zero');
    expect(tipEl.textContent).toContain('Home (Excl. Car)');
  });
});

/** The saved choice loads with the store: a saved true stays on, anything else stays off. */
describe('<PanelControlsSection/> — Home Excludes Car Charging saved state', () => {
  function silenceConsoleError() {
    return vi.spyOn(console, 'error').mockImplementation(() => {});
  }

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    localStorage.removeItem('homeExcludesCar');
  });

  it('loads a saved true as on', async () => {
    silenceConsoleError();
    localStorage.setItem('homeExcludesCar', 'true');
    vi.resetModules();
    const { default: FreshSection } = await import('../../src/components/settings/PanelControlsSection');
    const { useInverterStore: freshStore } = await import('../../src/store/useInverterStore');
    render(<FreshSection flash={() => {}} initialHiddenPanels={[]} octopusConfigured={false} />);
    expect(freshStore.getState().homeExcludesCar).toBe(true);
    expect(screen.getByRole('switch', { name: 'Home Excludes Car Charging' }).getAttribute('aria-checked')).toBe('true');
  });

  it('loads anything but a literal true as off (garbage-safe, issue #457)', async () => {
    silenceConsoleError();
    localStorage.setItem('homeExcludesCar', 'yes');
    vi.resetModules();
    const { default: FreshSection } = await import('../../src/components/settings/PanelControlsSection');
    const { useInverterStore: freshStore } = await import('../../src/store/useInverterStore');
    render(<FreshSection flash={() => {}} initialHiddenPanels={[]} octopusConfigured={false} />);
    expect(freshStore.getState().homeExcludesCar).toBe(false);
    expect(screen.getByRole('switch', { name: 'Home Excludes Car Charging' }).getAttribute('aria-checked')).toBe('false');
  });
});
