import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Settings page — "Minimise to Tray" / "Start Hidden in Tray" toggles (#217).
//
// Both toggles are gated behind `autostartSupported`, which is true only when
// the page is running inside the Tauri shell (`'__TAURI_INTERNALS__' in
// window`). jsdom doesn't provide that marker, so each test injects it before
// render and removes it after. Persisting is a plain `apiPost('/api/settings')`
// (no Tauri command), so the assertion is on the mocked apiPost payload.
// ---------------------------------------------------------------------------

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    if (path === '/api/settings') {
      return {
        ok: true,
        data: {
          host: '',
          port: 8899,
          serial: '',
          interval_secs: 20,
          http_port: 7337,
          evc_port: 502,
          import_tariff_config: null,
          export_tariff_config: null,
          evc_host: '',
        },
      };
    }
    if (path === '/api/alerts') {
      return {
        ok: true,
        data: {
          config: {
            enabled: false,
            telegram: { bot_token: '', chat_id: '', enabled: false },
            ntfy: { topic: '', server: 'https://ntfy.sh', enabled: false },
            thresholds: {},
          },
        },
      };
    }
    if (path === '/api/weather') {
      return {
        ok: true,
        data: {
          config: {
            enabled: false,
            latitude: null,
            longitude: null,
            update_interval_mins: 30,
          },
          current: null,
          history: [],
        },
      };
    }
    if (path === '/api/status') {
      return { ok: true, lan_ip: null, clients: [], client_count: 0 };
    }
    if (path === '/api/discover') {
      return { ok: true, subnets: [], inverters: [] };
    }
    if (path === '/api/evc/discover') {
      return { ok: true, subnets: [], chargers: [] };
    }
    return { ok: true, data: {} };
  }),
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
  getApiBase: () => 'http://localhost:7337',
  getServerPort: () => 7337,
  fetchHistory: vi.fn().mockResolvedValue({}),
  isTauri: false,
}));

vi.mock('../../src/lib/openExternal', () => ({
  openExternal: vi.fn().mockResolvedValue(undefined),
}));

import SettingsPage from '../../src/pages/SettingsPage';
import { useInverterStore } from '../../src/store/useInverterStore';
import { apiPost, apiGet } from '../../src/lib/api';

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

describe('<SettingsPage/> — Minimise to Tray toggles (issue #217)', () => {
  beforeEach(() => {
    silenceConsoleError();
    localStorage.removeItem('gridLineWeight');
    useInverterStore.setState({ gridLineWeight: 'standard' });
    // Pretend we're inside the Tauri shell so the desktop-only toggles render.
    (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    localStorage.removeItem('gridLineWeight');
    delete (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  it('renders both tray toggles when running inside the Tauri shell', async () => {
    render(<SettingsPage />);
    expect(await screen.findByText('Minimise to Tray')).toBeDefined();
    expect(await screen.findByText('Start Hidden in Tray')).toBeDefined();
  });

  it('hides both tray toggles in headless mode (no Tauri shell)', async () => {
    delete (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
    render(<SettingsPage />);
    // Wait for settings to load so the absence isn't just a not-yet-rendered
    // race — the Start on Login toggle is under the same gate, so use it as
    // a sentinel that the page has settled past the loading spinner.
    await waitFor(() => {
      expect(screen.queryByText('Start on Login')).toBeNull();
    });
    expect(screen.queryByText('Minimise to Tray')).toBeNull();
    expect(screen.queryByText('Start Hidden in Tray')).toBeNull();
  });

  it('clicking Minimise to Tray persists minimise_to_tray=true', async () => {
    render(<SettingsPage />);
    await screen.findByText('Minimise to Tray');
    fireEvent.click(toggleFor('Minimise to Tray'));
    await waitFor(() => {
      expect(apiPost).toHaveBeenCalledWith('/api/settings', { minimise_to_tray: true });
    });
  });

  it('clicking Start Hidden in Tray persists start_minimised=true', async () => {
    render(<SettingsPage />);
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
    render(<SettingsPage />);
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

  it('reflects persisted values from /api/settings on load', async () => {
    // Seed both tray prefs as enabled. The override is path-aware so the
    // seed lands on /api/settings (not whichever fetch fires first) and
    // every other path keeps a valid shape the page can render.
    vi.mocked(apiGet).mockImplementation(async (path: string) => {
      if (path === '/api/settings') {
        return {
          ok: true,
          data: {
            host: '',
            port: 8899,
            serial: '',
            interval_secs: 20,
            http_port: 7337,
            evc_port: 502,
            import_tariff_config: null,
            export_tariff_config: null,
            evc_host: '',
            minimise_to_tray: true,
            start_minimised: true,
          },
        };
      }
      if (path === '/api/alerts') {
        return {
          ok: true,
          data: {
            config: {
              enabled: false,
              telegram: { bot_token: '', chat_id: '', enabled: false },
              ntfy: { topic: '', server: 'https://ntfy.sh', enabled: false },
              thresholds: {},
            },
          },
        };
      }
      if (path === '/api/weather') {
        return {
          ok: true,
          data: {
            config: { enabled: false, latitude: null, longitude: null, update_interval_mins: 30 },
            current: null,
            history: [],
          },
        };
      }
      if (path === '/api/status') return { ok: true, lan_ip: null, clients: [], client_count: 0 };
      if (path === '/api/discover') return { ok: true, subnets: [], inverters: [] };
      if (path === '/api/evc/discover') return { ok: true, subnets: [], chargers: [] };
      return { ok: true, data: {} };
    });
    render(<SettingsPage />);
    // Wait for the page to settle past the loading spinner, then resolve
    // the toggles by label (toggleFor queries the DOM synchronously).
    await screen.findByText('Minimise to Tray');
    const minimiseToggle = toggleFor('Minimise to Tray');
    const startToggle = toggleFor('Start Hidden in Tray');
    await waitFor(() => {
      expect(minimiseToggle.querySelector('div.rounded-full')?.className).toContain('bg-flow-active');
      expect(startToggle.querySelector('div.rounded-full')?.className).toContain('bg-flow-active');
    });
  });
});
