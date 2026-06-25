import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Mocks — same shape as settingsPageGridLines.test.tsx so SettingsPage can
// mount under jsdom. The `/api/settings` response includes the new
// `minimal_telemetry_mode` field; the test asserts that the toggle is
// hydrated from it and that subsequent toggles POST the right payload.
// ---------------------------------------------------------------------------

const apiPost = vi.fn().mockResolvedValue({ ok: true, data: {} });

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
          minimal_telemetry_mode: false,
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
  apiPost: (...args: unknown[]) => apiPost(...args),
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

/**
 * Returns the inner toggle control for the given section title. The
 * SettingsPage `Toggle` component renders as a `div.cursor-pointer` that
 * is a *sibling* of the column containing the label + description, so we
 * walk up to the row (`div.flex.items-center`) that contains both the
 * label span and the toggle, then return the toggle element.
 */
function toggleFor(container: HTMLElement, label: string): HTMLElement {
  const labelEl = Array.from(container.querySelectorAll('span')).find(
    (s) => s.textContent?.trim() === label,
  );
  if (!labelEl) throw new Error(`label not found: ${label}`);
  // The label lives inside `<div className="flex flex-col gap-0.5">`.
  // The toggle lives in the *parent* row `<div className="flex items-center justify-between ...">`.
  // So the row we want is `labelEl.parentElement?.parentElement`.
  const row = labelEl.parentElement?.parentElement;
  if (!row) throw new Error(`row not found for label: ${label}`);
  const toggle = row.querySelector('div.cursor-pointer');
  if (!toggle) throw new Error(`toggle not found for label: ${label}`);
  return toggle as HTMLElement;
}

describe('<SettingsPage/> — Minimal Telemetry Mode toggle', () => {
  beforeEach(() => {
    silenceConsoleError();
    apiPost.mockClear();
    apiPost.mockResolvedValue({ ok: true, data: {} });
    localStorage.removeItem('devMode');
    // Developer mode off by default — toggle must be hidden initially.
    useInverterStore.setState({ developerMode: false });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    localStorage.removeItem('devMode');
    useInverterStore.setState({ developerMode: false });
  });

  describe('visibility (gated on developerMode)', () => {
    it('does NOT render the toggle when Developer Mode is off', async () => {
      render(<SettingsPage />);
      // Wait for the page to finish its initial settings fetch so we
      // don't race the lazy-render of the Developer section.
      await screen.findByText('Developer', { exact: true });
      expect(screen.queryByText('Minimal Telemetry Mode')).toBeNull();
    });

    it('renders the toggle inside the Developer section when Developer Mode is on', async () => {
      useInverterStore.setState({ developerMode: true });
      render(<SettingsPage />);
      const label = await screen.findByText('Minimal Telemetry Mode');
      // The toggle lives inside the Developer section (the one headed
      // "Developer"), not anywhere else — confirms the gating is on the
      // right section rather than e.g. some stray new section.
      const developerSection = screen
        .getByText('Developer', { exact: true })
        .closest('section');
      expect(developerSection).not.toBeNull();
      expect(developerSection!.contains(label)).toBe(true);
    });
  });

  describe('default state from GET /api/settings', () => {
    it('reflects minimal_telemetry_mode=false from the server by default', async () => {
      useInverterStore.setState({ developerMode: true });
      render(<SettingsPage />);
      // The toggle renders the on-state with a coloured indicator dot;
      // we can't query for that without rendered CSS, but we can verify
      // the POST path was not called on mount (the toggle should hydrate
      // from the GET response, not POST its initial value back).
      await screen.findByText('Minimal Telemetry Mode');
      await waitFor(() => {
        // The page should have finished the GET /api/settings fetch.
      });
      // Confirm the user hasn't been silently re-saving on every mount.
      expect(apiPost).not.toHaveBeenCalledWith(
        '/api/settings',
        expect.objectContaining({ minimal_telemetry_mode: expect.anything() }),
      );
    });
  });

  describe('interaction', () => {
    it('clicking the toggle POSTs {minimal_telemetry_mode: true}', async () => {
      useInverterStore.setState({ developerMode: true });
      render(<SettingsPage />);
      await screen.findByText('Minimal Telemetry Mode');

      const toggle = toggleFor(document.body, 'Minimal Telemetry Mode');
      fireEvent.click(toggle);

      await waitFor(() => {
        expect(apiPost).toHaveBeenCalledWith('/api/settings', {
          minimal_telemetry_mode: true,
        });
      });
    });

    it('clicking the toggle when it appears ON POSTs {minimal_telemetry_mode: false}', async () => {
      // Simulate the server returning minimal_telemetry_mode=true so the
      // toggle mounts in the "on" state, then verify a click sends the
      // right payload to turn it off.
      const apiMod = await import('../../src/lib/api');
      (apiMod.apiGet as unknown as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
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
          minimal_telemetry_mode: true,
        },
      });

      useInverterStore.setState({ developerMode: true });
      render(<SettingsPage />);
      await screen.findByText('Minimal Telemetry Mode');

      const toggle = toggleFor(document.body, 'Minimal Telemetry Mode');
      fireEvent.click(toggle);

      await waitFor(() => {
        expect(apiPost).toHaveBeenCalledWith('/api/settings', {
          minimal_telemetry_mode: false,
        });
      });
    });

    it('surfaces a success flash after the save resolves', async () => {
      useInverterStore.setState({ developerMode: true });
      render(<SettingsPage />);
      await screen.findByText('Minimal Telemetry Mode');

      const toggle = toggleFor(document.body, 'Minimal Telemetry Mode');
      fireEvent.click(toggle);

      // The flash message mentions "next poll" because the flag is
      // re-read by the poll loop on every iteration (poll.rs:773) — a
      // reconnect is NOT required. Match on the key phrase so the test
      // doesn't break on minor copy edits to the success message.
      expect(
        await screen.findByText(/optional blocks will be skipped on the next poll/i, {}, { timeout: 2000 }),
      ).toBeDefined();
    });

    it('surfaces an error flash when the save rejects', async () => {
      apiPost.mockRejectedValueOnce(new Error('disk full'));
      useInverterStore.setState({ developerMode: true });
      render(<SettingsPage />);
      await screen.findByText('Minimal Telemetry Mode');

      const toggle = toggleFor(document.body, 'Minimal Telemetry Mode');
      fireEvent.click(toggle);

      // Error path: the SettingsPage wraps the rejection with
      // `e.message ?? 'Failed to save'`, so we expect the rejected
      // Error's message to appear in the flash.
      expect(
        await screen.findByText('disk full', {}, { timeout: 2000 }),
      ).toBeDefined();
    });
  });

  describe('position inside Developer section', () => {
    it('sits above the Read-only API paragraph', async () => {
      // Layout invariant: the toggle must be the first item inside the
      // developer-only sub-block so it's the most discoverable dev knob.
      // If a future refactor shuffles it, this test catches it.
      useInverterStore.setState({ developerMode: true });
      render(<SettingsPage />);

      const developerHeading = await screen.findByText('Developer', { exact: true });
      const developerSection = developerHeading.closest('section')!;

      const toggleLabel = screen.getByText('Minimal Telemetry Mode');
      const apiParagraph = screen.getByText(/Read-only API for external access/i);

      // Compare DOM positions: the toggle label must come before the
      // API description paragraph.
      const position = toggleLabel.compareDocumentPosition(apiParagraph);
      // DOCUMENT_POSITION_FOLLOWING === 0x04 — the toggle is an
      // earlier sibling / ancestor of the API paragraph.
      expect(position & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
      expect(developerSection.contains(toggleLabel)).toBe(true);
      expect(developerSection.contains(apiParagraph)).toBe(true);
    });
  });
});
