import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Settings → App — "Minimise to Tray" / "Start Hidden in Tray" toggles (#217).
//
// Both toggles render only when the app is running inside the Tauri shell (`'__TAURI_INTERNALS__' in
// window`). jsdom doesn't provide that marker, so each test injects it before
// render and removes it after. Persisting is a plain `apiPost('/api/settings')`
// (no Tauri command), so the assertion is on the mocked apiPost payload.
// ---------------------------------------------------------------------------

vi.mock('../../src/lib/api', () => ({
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
}));

import AppSection from '../../src/components/settings/AppSection';
import { UpdatesSection } from '../../src/components/settings/AboutSections';
import { apiPost } from '../../src/lib/api';

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

/** Silence noisy React act() warnings from async setState in useEffects. */
function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

/**
 * Locate the clickable Toggle widget for a given row label. The Toggle is a
 * bare `<div>` with no role/label (the accessible label is the sibling span),
 * so we anchor on the label text, walk up to the row container, then down to
 * the `cursor-pointer` toggle div.
 */
function toggleFor(labelText: string): HTMLElement {
  const label = screen.getByText(labelText);
  const row = label.closest('div.flex.items-center.justify-between');
  if (!row) throw new Error(`row for "${labelText}" not found`);
  const toggle = row.querySelector('.cursor-pointer') as HTMLElement | null;
  if (!toggle) throw new Error(`toggle widget for "${labelText}" not found`);
  return toggle;
}

describe('<AppSection/> — Minimise to Tray toggles (issue #217)', () => {
  beforeEach(() => {
    silenceConsoleError();
    // Pretend we're inside the Tauri shell so the desktop-only toggles render.
    (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    delete (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  it('top-aligns the Check for New Releases switch beside wrapped help text', async () => {
    render(<UpdatesSection flash={() => {}} initialCheckForUpdates />);
    const label = await screen.findByText('Check for New Releases');
    const row = label.parentElement?.parentElement;
    expect(row?.className).toContain('items-start');
    expect(row?.className).toContain('gap-3');
    expect(row?.querySelector('.cursor-pointer')?.className).toContain('shrink-0');
  });

  it('renders both tray toggles when running inside the Tauri shell', async () => {
    render(<Section />);
    expect(await screen.findByText('Minimise to Tray')).toBeDefined();
    expect(await screen.findByText('Start Hidden in Tray')).toBeDefined();
  });

  it('hides both tray toggles in headless mode (no Tauri shell)', async () => {
    delete (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
    render(<Section />);
    await screen.findByRole('heading', { name: 'App' });
    await waitFor(() => {
      expect(screen.queryByText('Start on Login')).toBeNull();
    });
    expect(screen.queryByText('Minimise to Tray')).toBeNull();
    expect(screen.queryByText('Start Hidden in Tray')).toBeNull();
  });

  it('clicking Minimise to Tray persists minimise_to_tray=true', async () => {
    render(<Section />);
    await screen.findByText('Minimise to Tray');
    fireEvent.click(toggleFor('Minimise to Tray'));
    await waitFor(() => {
      expect(apiPost).toHaveBeenCalledWith('/api/settings', { minimise_to_tray: true });
    });
  });

  it('clicking Start Hidden in Tray persists start_minimised=true', async () => {
    render(<Section />);
    await screen.findByText('Start Hidden in Tray');
    fireEvent.click(toggleFor('Start Hidden in Tray'));
    await waitFor(() => {
      expect(apiPost).toHaveBeenCalledWith('/api/settings', { start_minimised: true });
    });
  });

  it('reverts the toggle if the persist call fails', async () => {
    // The first apiPost in this render is the optimistic save; make it reject
    // so the handler rolls the local state back to its previous (false) value.
    vi.mocked(apiPost).mockRejectedValueOnce(new Error('network down'));
    render(<Section />);
    await screen.findByText('Minimise to Tray');
    const toggle = toggleFor('Minimise to Tray');
    fireEvent.click(toggle);
    // After the failed save the toggle must report the unchecked visual
    // state again (background reverts to the elevated/inactive colour).
    await waitFor(() => {
      const bg = toggle.querySelector('div.rounded-full');
      expect(bg?.className).toContain('bg-bg-elevated');
    });
  });

  it('shows the saved tray values it is given', async () => {
    render(<Section initial={{ ...APP_DEFAULTS, minimiseToTray: true, startMinimised: true }} />);
    // Resolve the toggles by label (toggleFor queries the DOM synchronously).
    await screen.findByText('Minimise to Tray');
    const minimiseToggle = toggleFor('Minimise to Tray');
    const startToggle = toggleFor('Start Hidden in Tray');
    await waitFor(() => {
      expect(minimiseToggle.querySelector('div.rounded-full')?.className).toContain('bg-accent');
      expect(startToggle.querySelector('div.rounded-full')?.className).toContain('bg-accent');
    });
  });
});
