import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// SettingsPage "History Chart Averages" sub-section (issue #345).
//
// Mirrors settingsPageGridLines.test.tsx: the side-effecting libs are stubbed
// so the page can mount, while the real Zustand store stays the subject under
// test so the toggle's persistence wiring is exercised end-to-end.
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
          config: { enabled: false, latitude: null, longitude: null, update_interval_mins: 30 },
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

function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

describe('<SettingsPage/> — History Chart Averages (issue #345)', () => {
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
    render(<SettingsPage />);
    expect(
      await screen.findByRole('heading', { name: 'History Chart Averages', level: 3 }),
    ).toBeDefined();
  });

  it('defaults the toggle to off', async () => {
    render(<SettingsPage />);
    const toggle = await screen.findByRole('switch', {
      name: 'Show average lines on History charts',
    });
    expect(toggle.getAttribute('aria-checked')).toBe('false');
  });

  it('clicking the toggle enables averages and persists to localStorage', async () => {
    render(<SettingsPage />);
    const toggle = await screen.findByRole('switch', {
      name: 'Show average lines on History charts',
    });
    fireEvent.click(toggle);

    await waitFor(() => {
      expect(useInverterStore.getState().showHistoryAverages).toBe(true);
      expect(localStorage.getItem('showHistoryAverages')).toBe('true');
    });
    expect(toggle.getAttribute('aria-checked')).toBe('true');
  });

  it('sits between Chart Grid Lines and Energy Flow Diagram', async () => {
    render(<SettingsPage />);
    const panelControls = await screen.findByRole('heading', { name: 'Panel Controls', level: 2 });
    const allHeadings = await screen.findAllByRole('heading', { level: 3 });
    const section = panelControls.closest('section');
    expect(section).not.toBeNull();
    const order = allHeadings
      .filter((h) => section!.contains(h))
      .map((h) => h.textContent ?? '');

    expect(order.indexOf('Chart Grid Lines')).toBeLessThan(order.indexOf('History Chart Averages'));
    expect(order.indexOf('History Chart Averages')).toBeLessThan(order.indexOf('Energy Flow Diagram'));
  });
});
