import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
}));

import PanelControlsSection from '../../src/components/settings/PanelControlsSection';
import { apiPost } from '../../src/lib/api';
import { useInverterStore } from '../../src/store/useInverterStore';

/**
 * Settings → Panel Controls → Show Node Status Words: the Generating /
 * Importing / Charging words under the orbit's nodes. On by default, and
 * saved in settings.json since issue #498, so clearing site data or opening
 * HEM in another browser no longer turns them off.
 */
describe('<PanelControlsSection/> — Show Node Status Words', () => {
  const flash = vi.fn();

  beforeEach(() => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(apiPost).mockReset();
    vi.mocked(apiPost).mockResolvedValue({ ok: true, data: {} });
    flash.mockReset();
    localStorage.clear();
    useInverterStore.setState({ showFlowStatusWords: true, energyFlowDisplay: 'orbit' });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    localStorage.clear();
    useInverterStore.setState({ showFlowStatusWords: true, energyFlowDisplay: 'orbit' });
  });

  const renderSection = () =>
    render(<PanelControlsSection flash={flash} initialHiddenPanels={[]} octopusConfigured={false} />);
  const toggle = () => screen.queryByRole('switch', { name: 'Show Node Status Words' });

  it('starts from the store, on by default', () => {
    renderSection();
    expect(toggle()!.getAttribute('aria-checked')).toBe('true');
    cleanup();
    useInverterStore.setState({ showFlowStatusWords: false });
    renderSection();
    expect(toggle()!.getAttribute('aria-checked')).toBe('false');
  });

  it('saves to settings.json in both directions, not to localStorage', async () => {
    renderSection();
    fireEvent.click(toggle()!);
    await waitFor(() =>
      expect(apiPost).toHaveBeenCalledWith('/api/settings', { status_show_flow_status_words: false }),
    );
    await waitFor(() => expect(useInverterStore.getState().showFlowStatusWords).toBe(false));
    await waitFor(() => expect(flash).toHaveBeenLastCalledWith('Node status words hidden', true));
    fireEvent.click(toggle()!);
    await waitFor(() =>
      expect(apiPost).toHaveBeenLastCalledWith('/api/settings', { status_show_flow_status_words: true }),
    );
    await waitFor(() => expect(useInverterStore.getState().showFlowStatusWords).toBe(true));
    expect(localStorage.getItem('showFlowStatusWords')).toBeNull();
  });

  it('moves back and says so when the save fails', async () => {
    vi.mocked(apiPost).mockRejectedValueOnce(new Error('disk full'));
    renderSection();
    fireEvent.click(toggle()!);
    await waitFor(() => expect(flash).toHaveBeenCalledWith('Failed to save: disk full', false));
    expect(toggle()!.getAttribute('aria-checked')).toBe('true');
    expect(useInverterStore.getState().showFlowStatusWords).toBe(true);
  });

  it('marks the change as the user\'s, so a settings load in flight leaves it', async () => {
    renderSection();
    const before = useInverterStore.getState().statusToggleRevision.showFlowStatusWords;
    fireEvent.click(toggle()!);
    await waitFor(() => expect(useInverterStore.getState().showFlowStatusWords).toBe(false));
    expect(useInverterStore.getState().statusToggleRevision.showFlowStatusWords).toBeGreaterThan(before);
  });

  it('is offered only with the orbit, whose nodes carry the words', () => {
    useInverterStore.setState({ energyFlowDisplay: 'house' });
    renderSection();
    expect(toggle()).toBeNull();
  });
});
