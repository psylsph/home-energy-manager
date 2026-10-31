import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, within } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Settings → Panel Controls "History Chart Averages" group (issue #345).
//
// Mirrors settingsPageGridLines.test.tsx: the side-effecting libs are stubbed
// so the page can mount, while the real Zustand store stays the subject under
// test so the toggle's persistence wiring is exercised end-to-end.
// ---------------------------------------------------------------------------

vi.mock('../../src/lib/api', () => ({
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
}));

import PanelControlsSection from '../../src/components/settings/PanelControlsSection';

const Section = () => (
  <PanelControlsSection flash={() => {}} initialHiddenPanels={[]} octopusConfigured={false} />
);
import { useInverterStore } from '../../src/store/useInverterStore';

function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

describe('<PanelControlsSection/> — History Chart Averages (issue #345)', () => {
  beforeEach(() => {
    silenceConsoleError();
    localStorage.removeItem('showHistoryAverages');
    useInverterStore.setState({ showHistoryAverages: false });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    localStorage.removeItem('showHistoryAverages');
  });

  it('renders the sub-section heading inside Panel Controls', async () => {
    render(<Section />);
    expect(
      await screen.findByRole('heading', { name: 'History Chart Averages', level: 3 }),
    ).toBeDefined();
  });

  it('defaults the toggle to off', async () => {
    render(<Section />);
    const toggle = await screen.findByRole('switch', {
      name: 'Show Average Lines on History Charts',
    });
    expect(toggle.getAttribute('aria-checked')).toBe('false');
  });

  it('clicking the toggle enables averages and persists to localStorage', async () => {
    render(<Section />);
    const toggle = await screen.findByRole('switch', {
      name: 'Show Average Lines on History Charts',
    });
    fireEvent.click(toggle);

    await waitFor(() => {
      expect(useInverterStore.getState().showHistoryAverages).toBe(true);
      expect(localStorage.getItem('showHistoryAverages')).toBe('true');
    });
    expect(toggle.getAttribute('aria-checked')).toBe('true');
  });

  it('sits between Panel Graphs and Energy Flow Diagram, with Chart Grid Lines folded under Advanced', async () => {
    render(<Section />);
    const section = (await screen.findByRole('heading', { name: 'Panel Controls', level: 2 })).closest('section')!;
    const groups = () =>
      within(section).getAllByRole('heading', { level: 3 }).map((h) => h.textContent ?? '');

    expect(groups()).toEqual(['Panel Visibility', 'Panel Graphs', 'History Chart Averages', 'Energy Flow Diagram']);

    fireEvent.click(within(section).getByRole('button', { name: 'Advanced' }));
    expect(groups()).toEqual([
      'Panel Visibility', 'Panel Graphs', 'History Chart Averages', 'Energy Flow Diagram', 'Chart Grid Lines',
    ]);
  });
});
