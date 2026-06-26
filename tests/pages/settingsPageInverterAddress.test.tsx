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
import { apiPost } from '../../src/lib/api';

function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

/** Returns the <input> rendered under the "Inverter Address" label. */
async function inverterAddressInput(): Promise<HTMLInputElement> {
  const label = await screen.findByText('Inverter Address');
  const input = label.closest('label')?.querySelector('input');
  if (!input) throw new Error('Inverter Address input not found');
  return input as HTMLInputElement;
}

/** Returns the Connect button in the Inverter Connection section. */
function connectButton(): HTMLButtonElement {
  const text = screen.getByText('Connect');
  const btn = text.closest('button');
  if (!btn) throw new Error('Connect button not found');
  return btn as HTMLButtonElement;
}

function validationHint(): HTMLElement | null {
  return screen.queryByText(/Must be four numbers/i);
}

describe('<SettingsPage/> — Inverter Address validation (issue #153)', () => {
  beforeEach(() => {
    silenceConsoleError();
    localStorage.clear();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    localStorage.clear();
  });

  it('disables Connect while the field is empty and shows no error styling', async () => {
    render(<SettingsPage />);
    const input = await inverterAddressInput();

    expect(input.value).toBe('');
    expect(input.getAttribute('aria-invalid')).toBe('false');
    expect(connectButton().disabled).toBe(true);
    expect(validationHint()).toBeNull();
  });

  it('marks an invalid address and disables Connect', async () => {
    render(<SettingsPage />);
    const input = await inverterAddressInput();

    fireEvent.change(input, { target: { value: '999.1.1.1' } });

    await waitFor(() => {
      expect(input.getAttribute('aria-invalid')).toBe('true');
    });
    expect(validationHint()).not.toBeNull();
    expect(connectButton().disabled).toBe(true);

    // A disabled button must never persist the bad host.
    fireEvent.click(connectButton());
    expect(apiPost).not.toHaveBeenCalled();
  });

  it('rejects malformed entries like a missing octet or non-digits', async () => {
    render(<SettingsPage />);
    const input = await inverterAddressInput();

    for (const bad of ['10.1.71', '192.168.1.50.', 'abc', '01.2.3.4', '256.1.1.1']) {
      fireEvent.change(input, { target: { value: bad } });
      await waitFor(() => expect(input.getAttribute('aria-invalid')).toBe('true'));
      expect(connectButton().disabled).toBe(true);
      expect(validationHint()).not.toBeNull();
    }
  });

  it('accepts a valid dotted-quad, clears the error, and lets Connect save it', async () => {
    render(<SettingsPage />);
    const input = await inverterAddressInput();

    fireEvent.change(input, { target: { value: '192.168.1.50' } });

    await waitFor(() => {
      expect(input.getAttribute('aria-invalid')).toBe('false');
    });
    expect(validationHint()).toBeNull();
    expect(connectButton().disabled).toBe(false);

    fireEvent.click(connectButton());
    await waitFor(() => {
      expect(apiPost).toHaveBeenCalledWith('/api/settings', {
        host: '192.168.1.50',
        port: 8899,
        serial: '',
      });
    });
  });

  it('clears the validation state when a bad entry is corrected', async () => {
    render(<SettingsPage />);
    const input = await inverterAddressInput();

    fireEvent.change(input, { target: { value: 'not-an-ip' } });
    await waitFor(() => expect(input.getAttribute('aria-invalid')).toBe('true'));
    expect(validationHint()).not.toBeNull();

    fireEvent.change(input, { target: { value: '10.0.0.9' } });
    await waitFor(() => expect(input.getAttribute('aria-invalid')).toBe('false'));
    expect(validationHint()).toBeNull();
    expect(connectButton().disabled).toBe(false);
  });
});
