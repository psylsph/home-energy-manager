import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

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

describe('<SettingsPage/> — Energy Flow diagram word preferences', () => {
  beforeEach(() => {
    silenceConsoleError();
    localStorage.removeItem('showFlowSummary');
    localStorage.removeItem('showFlowStatusWords');
    useInverterStore.setState({ showFlowSummary: false, showFlowStatusWords: false });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    localStorage.removeItem('showFlowSummary');
    localStorage.removeItem('showFlowStatusWords');
    useInverterStore.setState({ showFlowSummary: false, showFlowStatusWords: false });
  });

  it('renders the overview toggle inside the Energy Flow Diagram sub-section', async () => {
    render(<SettingsPage />);

    const label = await screen.findByText('Show Overview Sentence');
    const section = screen
      .getByRole('heading', { name: 'Energy Flow Diagram', level: 3 })
      .closest('div');

    expect(section).not.toBeNull();
    expect(section!.contains(label)).toBe(true);
  });

  it('defaults the overview sentence and node status words to off', async () => {
    render(<SettingsPage />);

    await screen.findByText('Show Overview Sentence');
    await screen.findByText('Show Node Status Words');

    expect(useInverterStore.getState().showFlowSummary).toBe(false);
    expect(useInverterStore.getState().showFlowStatusWords).toBe(false);
    expect(localStorage.getItem('showFlowSummary')).toBeNull();
    expect(localStorage.getItem('showFlowStatusWords')).toBeNull();
  });

  it('clicking the overview toggle shows/hides the overview, persisting each choice', async () => {
    render(<SettingsPage />);
    await screen.findByText('Show Overview Sentence');

    fireEvent.click(toggleFor('Show Overview Sentence'));

    await waitFor(() => {
      expect(useInverterStore.getState().showFlowSummary).toBe(true);
      expect(localStorage.getItem('showFlowSummary')).toBe('true');
    });

    fireEvent.click(toggleFor('Show Overview Sentence'));

    await waitFor(() => {
      expect(useInverterStore.getState().showFlowSummary).toBe(false);
      expect(localStorage.getItem('showFlowSummary')).toBe('false');
    });
  });

  it('clicking the node-status toggle shows/hides node words, persisting each choice', async () => {
    render(<SettingsPage />);
    await screen.findByText('Show Node Status Words');

    fireEvent.click(toggleFor('Show Node Status Words'));

    await waitFor(() => {
      expect(useInverterStore.getState().showFlowStatusWords).toBe(true);
      expect(localStorage.getItem('showFlowStatusWords')).toBe('true');
    });

    fireEvent.click(toggleFor('Show Node Status Words'));

    await waitFor(() => {
      expect(useInverterStore.getState().showFlowStatusWords).toBe(false);
      expect(localStorage.getItem('showFlowStatusWords')).toBe('false');
    });
  });
});
