import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Settings → App — Refresh Interval and HTTP Port placement (issue #435).
//
// Both rows are always visible, without unfolding anything, and sit at the
// bottom of the App section, after the desktop-only toggles.
// ---------------------------------------------------------------------------

vi.mock('../../src/lib/api', () => ({
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
}));

import AppSection from '../../src/components/settings/AppSection';

const APP_DEFAULTS = {
  intervalSecs: 20,
  httpPort: 7337,
  autostartEnabled: false,
  minimiseToTray: false,
  startMinimised: false,
};

const Section = ({ initial = APP_DEFAULTS }: { initial?: typeof APP_DEFAULTS }) => (
  <AppSection flash={() => {}} initial={initial} />
);

const tauriWindow = window as unknown as { __TAURI_INTERNALS__?: unknown };

describe('<AppSection/> — Refresh Interval and HTTP Port placement (issue #435)', () => {
  afterEach(() => {
    cleanup();
    delete tauriWindow.__TAURI_INTERNALS__;
  });

  it('shows Refresh Interval without unfolding anything', () => {
    render(<Section />);
    expect(screen.getByText('Refresh Interval')).toBeDefined();
    // The whole interval step row is right there beside it.
    expect(screen.getByText('20s')).toBeDefined();
  });

  it('shows HTTP Port without unfolding anything', () => {
    render(<Section />);
    expect(screen.getByText('HTTP Port')).toBeDefined();
    const input = screen.getByDisplayValue('7337') as HTMLInputElement;
    expect(input.type).toBe('number');
  });

  it('has no Advanced expander in the App section', () => {
    render(<Section />);
    expect(screen.queryByRole('button', { name: 'Advanced' })).toBeNull();
  });

  it('puts both rows after the desktop toggles, at the bottom of the section', () => {
    tauriWindow.__TAURI_INTERNALS__ = {};
    const { container } = render(<Section />);
    const lastToggle = screen.getByText('Start Hidden in Tray');
    const interval = screen.getByText('Refresh Interval');
    const port = screen.getByText('HTTP Port');
    const follows = (a: Node, b: Node) =>
      (a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0;
    expect(follows(lastToggle, interval)).toBe(true);
    expect(follows(interval, port)).toBe(true);
    // Only the port's own Save button follows its input: it is the last row.
    const portInput = screen.getByDisplayValue('7337');
    const after = Array.from(container.querySelectorAll('label, input, button')).filter((el) =>
      follows(portInput, el),
    );
    expect(after.map((el) => el.textContent)).toEqual(['Save']);
  });
});
