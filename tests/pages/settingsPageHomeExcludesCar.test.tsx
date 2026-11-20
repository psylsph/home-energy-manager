import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
}));

import PanelControlsSection from '../../src/components/settings/PanelControlsSection';
import { apiPost } from '../../src/lib/api';
import { useInverterStore } from '../../src/store/useInverterStore';

/**
 * Settings → Panel Controls → Home Excludes Car Charging (issue #457): show
 * the Status diagrams' Home reading without the car's charging, off by
 * default. Saved in settings.json since issue #498.
 */
describe('<PanelControlsSection/> — Home Excludes Car Charging', () => {
  const flash = vi.fn();

  beforeEach(() => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(apiPost).mockReset();
    vi.mocked(apiPost).mockResolvedValue({ ok: true, data: {} });
    flash.mockReset();
    localStorage.clear();
    useInverterStore.setState({ homeExcludesCar: false });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    localStorage.clear();
    useInverterStore.setState({ homeExcludesCar: false });
  });

  const renderSection = () =>
    render(<PanelControlsSection flash={flash} initialHiddenPanels={[]} octopusConfigured={false} />);
  const toggle = () => screen.getByRole('switch', { name: 'Home Excludes Car Charging' });

  it('starts from the store, off by default', () => {
    renderSection();
    expect(toggle().getAttribute('aria-checked')).toBe('false');
    cleanup();
    useInverterStore.setState({ homeExcludesCar: true });
    renderSection();
    expect(toggle().getAttribute('aria-checked')).toBe('true');
  });

  it('saves to settings.json in both directions, not to localStorage', async () => {
    renderSection();
    fireEvent.click(toggle());
    await waitFor(() =>
      expect(apiPost).toHaveBeenCalledWith('/api/settings', { status_home_excludes_car: true }),
    );
    await waitFor(() => expect(useInverterStore.getState().homeExcludesCar).toBe(true));
    await waitFor(() => expect(flash).toHaveBeenLastCalledWith('Home shown without car charging', true));
    fireEvent.click(toggle());
    await waitFor(() =>
      expect(apiPost).toHaveBeenLastCalledWith('/api/settings', { status_home_excludes_car: false }),
    );
    await waitFor(() => expect(useInverterStore.getState().homeExcludesCar).toBe(false));
    expect(localStorage.getItem('homeExcludesCar')).toBeNull();
  });

  it('moves back and says so when the save fails', async () => {
    vi.mocked(apiPost).mockRejectedValueOnce(new Error('disk full'));
    renderSection();
    fireEvent.click(toggle());
    await waitFor(() => expect(flash).toHaveBeenCalledWith('Failed to save: disk full', false));
    expect(toggle().getAttribute('aria-checked')).toBe('false');
    expect(useInverterStore.getState().homeExcludesCar).toBe(false);
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
