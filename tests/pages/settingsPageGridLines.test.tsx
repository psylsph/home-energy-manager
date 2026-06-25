import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Mocks — SettingsPage pulls in many libs (api, openExternal, tariff,
// validators) plus the Zustand store. Stub the side-effecting ones so the
// component can mount under jsdom; the real `useInverterStore` is the
// subject under test, so we use it as-is and reset state between tests.
// ---------------------------------------------------------------------------

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    // Return a shape that satisfies every SettingsPage fetcher so the
    // page can mount past its "Loading settings…" spinner without
    // crashing on undefined nested fields.
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

// Imported after the vi.mock() calls above (factories are hoisted regardless).
import SettingsPage from '../../src/pages/SettingsPage';
import { useInverterStore } from '../../src/store/useInverterStore';

/** Silence noisy React act() warnings from async setState in useEffects. */
function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

describe('<SettingsPage/> — Chart Grid Lines sub-section (issue #111)', () => {
  beforeEach(() => {
    silenceConsoleError();
    // Reset between tests so the latch / state from one test doesn't leak.
    localStorage.removeItem('gridLineWeight');
    useInverterStore.setState({ gridLineWeight: 'standard' });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    localStorage.removeItem('gridLineWeight');
  });

  describe('render', () => {
    it('renders the sub-section heading inside Panel Controls', async () => {
      render(<SettingsPage />);
      // SettingsPage shows a "Loading settings…" spinner until the first
      // /api/settings fetch resolves. The mock apiGet resolves immediately,
      // but the resulting setState is async — use findBy* to wait for the
      // real heading to appear.
      expect(
        await screen.findByRole('heading', { name: 'Chart Grid Lines', level: 3 }),
      ).toBeDefined();
    });

    it('renders both Standard and Subtle buttons with the swatch SVGs', async () => {
      render(<SettingsPage />);
      const standardBtn = await screen.findByRole('button', { name: /Standard/i });
      const subtleBtn = await screen.findByRole('button', { name: /Subtle/i });

      // Each button must carry a visual swatch so the user previews the
      // grid style before committing. We don't assert rendered colour
      // (that needs CSS evaluation), just that the <svg> lives inside
      // each button. The swatches use `aria-hidden="true"` so they're
      // skipped by the accessibility tree — query the container for an
      // <svg> instead.
      expect(standardBtn.querySelector('svg')).not.toBeNull();
      expect(subtleBtn.querySelector('svg')).not.toBeNull();
    });

    it('marks Standard as the default selected preset', async () => {
      render(<SettingsPage />);
      const standardBtn = await screen.findByRole('button', { name: /Standard/i });
      const subtleBtn = await screen.findByRole('button', { name: /Subtle/i });
      // aria-pressed is the canonical a11y signal for a toggle button.
      // Standard is the default per issue #111 (existing users see no
      // visual change).
      expect(standardBtn.getAttribute('aria-pressed')).toBe('true');
      expect(subtleBtn.getAttribute('aria-pressed')).toBe('false');
    });
  });

  describe('interaction', () => {
    it('clicking Subtle switches the store to "subtle" and toggles aria-pressed', async () => {
      render(<SettingsPage />);
      const subtleBtn = await screen.findByRole('button', { name: /Subtle/i });

      fireEvent.click(subtleBtn);

      await waitFor(() => {
        expect(useInverterStore.getState().gridLineWeight).toBe('subtle');
      });
      expect(subtleBtn.getAttribute('aria-pressed')).toBe('true');
      expect(
        screen.getByRole('button', { name: /Standard/i }).getAttribute('aria-pressed'),
      ).toBe('false');
    });

    it('clicking Subtle persists the choice to localStorage', async () => {
      render(<SettingsPage />);
      const subtleBtn = await screen.findByRole('button', { name: /Subtle/i });
      fireEvent.click(subtleBtn);
      await waitFor(() => {
        expect(localStorage.getItem('gridLineWeight')).toBe('subtle');
      });
    });

    it('clicking Standard after Subtle restores the baseline state and storage', async () => {
      // Start in Subtle so we can prove the click goes back to Standard
      // rather than no-op'ing on the already-selected button.
      useInverterStore.getState().setGridLineWeight('subtle');
      localStorage.setItem('gridLineWeight', 'subtle');

      render(<SettingsPage />);
      const standardBtn = await screen.findByRole('button', { name: /Standard/i });
      fireEvent.click(standardBtn);

      await waitFor(() => {
        expect(useInverterStore.getState().gridLineWeight).toBe('standard');
        expect(localStorage.getItem('gridLineWeight')).toBe('standard');
      });
    });

    it('reflects an externally-set store value on next render', async () => {
      // The user can also flip the preference from devtools / future
      // settings sync. The next render must reflect the change without
      // needing a re-mount.
      const { rerender } = render(<SettingsPage />);
      // Wait for first paint of the buttons before mutating store state.
      await screen.findByRole('button', { name: /Standard/i });
      useInverterStore.getState().setGridLineWeight('subtle');
      rerender(<SettingsPage />);

      const subtleBtn = await screen.findByRole('button', { name: /Subtle/i });
      const standardBtn = screen.getByRole('button', { name: /Standard/i });
      expect(subtleBtn.getAttribute('aria-pressed')).toBe('true');
      expect(standardBtn.getAttribute('aria-pressed')).toBe('false');
    });
  });

  describe('position in Panel Controls', () => {
    it('Chart Grid Lines sits between Panel Graphs and Energy Flow Diagram', async () => {
      // Issue #111 specifically asks for the control to be inside
      // Panel Controls. Verify the section ordering so a future refactor
      // can't silently relocate it to a new section.
      render(<SettingsPage />);

      const panelControls = await screen.findByRole('heading', {
        name: 'Panel Controls',
        level: 2,
      });
      // Only look at headings inside the Panel Controls section so other
      // sections (App, Notifications, etc.) can't reorder our comparison.
      const allHeadings = await screen.findAllByRole('heading', { level: 3 });
      const section = panelControls.closest('section');
      expect(section).not.toBeNull();
      const sectionHeadings = allHeadings.filter((h) => section!.contains(h));
      const order = sectionHeadings.map((h) => h.textContent ?? '');
      const idxPanelGraphs = order.indexOf('Panel Graphs');
      const idxGridLines = order.indexOf('Chart Grid Lines');
      const idxEnergyFlow = order.indexOf('Energy Flow Diagram');

      expect(idxPanelGraphs).toBeGreaterThanOrEqual(0);
      expect(idxGridLines).toBeGreaterThanOrEqual(0);
      expect(idxEnergyFlow).toBeGreaterThanOrEqual(0);
      expect(idxPanelGraphs).toBeLessThan(idxGridLines);
      expect(idxGridLines).toBeLessThan(idxEnergyFlow);
    });
  });
});
