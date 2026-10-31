import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
}));

function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

function toggleFor(label: string): HTMLElement {
  const labelEl = Array.from(document.body.querySelectorAll('span')).find(
    (s) => s.textContent?.trim() === label,
  );
  if (!labelEl) throw new Error(`label not found: ${label}`);
  const row = labelEl.parentElement?.parentElement;
  if (!row) throw new Error(`row not found for label: ${label}`);
  const toggle = row.querySelector('div.cursor-pointer');
  if (!toggle) throw new Error(`toggle not found for label: ${label}`);
  return toggle as HTMLElement;
}

describe('<PanelControlsSection/> — Energy Flow diagram node status words', () => {
  beforeEach(() => {
    silenceConsoleError();
    localStorage.removeItem('showFlowStatusWords');
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    localStorage.removeItem('showFlowStatusWords');
  });

  it('defaults node status words to ON so non-technical users see Generating / Importing / Charging by default', async () => {
    // The status words carry the direction signal that used to live in a
    // `+`/`-` prefix on the orbit node value (e.g. "-839W + Discharging"
    // used to read as a bug). Now that the orbit and BatteryPanel show
    // plain magnitudes, the words are the primary direction signal, so
    // they're on by default. The toggle remains in Settings so a user
    // who prefers the bare value can still turn them off.
    //
    // We force a fresh module load so the store re-reads localStorage and
    // picks up the (absent) key; otherwise the cached store from a prior
    // test would carry whatever state the previous test set it to.
    vi.resetModules();
    const { default: PanelControlsSection } = await import('../../src/components/settings/PanelControlsSection');
    const { useInverterStore: freshStore } = await import('../../src/store/useInverterStore');

    render(<PanelControlsSection flash={() => {}} initialHiddenPanels={[]} octopusConfigured={false} />);
    await screen.findByText('Show Node Status Words');

    expect(freshStore.getState().showFlowStatusWords).toBe(true);
    expect(localStorage.getItem('showFlowStatusWords')).toBeNull();
  });

  it('clicking the node-status toggle shows/hides node words, persisting each choice', async () => {
    // Fresh-load the store so it picks up the absent localStorage key and
    // initializes to the ON default, then exercise the toggle in both
    // directions.
    vi.resetModules();
    const { default: PanelControlsSection } = await import('../../src/components/settings/PanelControlsSection');
    const { useInverterStore: freshStore } = await import('../../src/store/useInverterStore');

    render(<PanelControlsSection flash={() => {}} initialHiddenPanels={[]} octopusConfigured={false} />);
    await screen.findByText('Show Node Status Words');

    // The toggle starts checked (default-on). First click turns it off.
    fireEvent.click(toggleFor('Show Node Status Words'));

    await waitFor(() => {
      expect(freshStore.getState().showFlowStatusWords).toBe(false);
      expect(localStorage.getItem('showFlowStatusWords')).toBe('false');
    });

    // Second click re-enables.
    fireEvent.click(toggleFor('Show Node Status Words'));

    await waitFor(() => {
      expect(freshStore.getState().showFlowStatusWords).toBe(true);
      expect(localStorage.getItem('showFlowStatusWords')).toBe('true');
    });
  });
});
