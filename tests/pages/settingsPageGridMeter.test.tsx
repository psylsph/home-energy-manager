import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// `localStorage` is polyfilled by tests/setup.ts (jsdom 29 ships a stub that
// lacks the actual storage methods). We can therefore use it directly here.

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    if (path === '/api/settings') {
      return {
        ok: true,
        data: {
          host: '', port: 8899, serial: '', interval_secs: 20, http_port: 7337,
          evc_port: 502, import_tariff_config: null, export_tariff_config: null,
          evc_host: '',
        },
      };
    }
    if (path === '/api/alerts') {
      return { ok: true, data: { config: { enabled: false, telegram: { bot_token: '', chat_id: '', enabled: false }, ntfy: { topic: '', server: 'https://ntfy.sh', enabled: false }, thresholds: {} } } };
    }
    if (path === '/api/weather') {
      return { ok: true, data: { config: { enabled: false, latitude: null, longitude: null, update_interval_mins: 30 }, current: null, history: [] } };
    }
    if (path === '/api/status') return { ok: true, lan_ip: null, clients: [], client_count: 0 };
    if (path === '/api/discover') return { ok: true, subnets: [], inverters: [] };
    if (path === '/api/evc/discover') return { ok: true, subnets: [], chargers: [] };
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
import type { InverterSnapshot, MeterData } from '../../src/lib/types';

function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

function meter(address: number, overrides: Partial<MeterData> = {}): MeterData {
  return {
    address,
    v_phase_1: 0, v_phase_2: 0, v_phase_3: 0,
    i_phase_1: 0, i_phase_2: 0, i_phase_3: 0, i_total: 0,
    p_active_phase_1: 0, p_active_phase_2: 0, p_active_phase_3: 0,
    p_active_total: 0, p_reactive_total: 0, p_apparent_total: 0,
    pf_total: 0, frequency: 0,
    e_import_active_kwh: 0, e_export_active_kwh: 0,
    ...overrides,
  };
}

/** Snapshot carrying only the fields SettingsPage reads from it. */
function snapshotWith(meters: MeterData[]): InverterSnapshot {
  return {
    meters,
    inverter_serial: '',
    device_type_code: '2201',
    solar_arrays: [],
  } as unknown as InverterSnapshot;
}

describe('<SettingsPage/> — Grid CT meter picker (issue #192)', () => {
  beforeEach(() => {
    silenceConsoleError();
    localStorage.removeItem('gridMeterAddress');
    useInverterStore.setState({ gridMeterAddress: 0, snapshot: null });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    localStorage.removeItem('gridMeterAddress');
  });

  it('hides the picker when there are no external CT meters', async () => {
    // No external CTs (only the built-in 0x00, or nothing) → nothing to
    // override; Auto resolves to the built-in grid CT and the control stays
    // out of the way.
    useInverterStore.setState({ snapshot: snapshotWith([meter(0x00, { i_total: 5 })]) });
    render(<SettingsPage />);
    await screen.findByText('Show Node Status Words');
    expect(screen.queryByTestId('grid-ct-meter-select')).toBeNull();
  });

  it('lists each external CT and defaults to Auto', async () => {
    useInverterStore.setState({ snapshot: snapshotWith([meter(0x01), meter(0x02)]) });
    render(<SettingsPage />);
    const select = (await screen.findByTestId('grid-ct-meter-select')) as HTMLSelectElement;
    // Built-in (address 0) is the default selection.
    expect(select.value).toBe('0');
    const labels = Array.from(select.options).map((o) => o.textContent);
    expect(labels).toContain('Auto (recommended)');
    expect(labels).toContain('Meter 0x01');
    expect(labels).toContain('Meter 0x02');
  });

  it('selecting an external CT updates the store and persists to localStorage', async () => {
    useInverterStore.setState({ snapshot: snapshotWith([meter(0x01, { i_total: 41.2 })]) });
    render(<SettingsPage />);
    const select = (await screen.findByTestId('grid-ct-meter-select')) as HTMLSelectElement;
    fireEvent.change(select, { target: { value: '1' } });
    await waitFor(() => {
      expect(useInverterStore.getState().gridMeterAddress).toBe(1);
      expect(localStorage.getItem('gridMeterAddress')).toBe('1');
    });
  });

  // -------------------------------------------------------------------------
  // Mobile layout. The long description ("Which CT clamp measures your grid
  // point. "Auto" uses the built-in grid CT when present, otherwise meter
  // 0x01. Used to show grid amps instead of frequency on the energy wheel.")
  // used to live in a fixed-width left column next to the select, which on a
  // phone compressed it to a single-word-per-line unreadable sliver. The fix
  // stacks label + select vertically on mobile (`flex-col`) and goes
  // side-by-side on `sm+`. jsdom doesn't compute flexbox layout, so we assert
  // on the responsive class structure — same approach as
  // connectionIndicatorMobile.test.tsx.
  // -------------------------------------------------------------------------
  it('stacks the label and the select vertically on mobile (flex-col)', async () => {
    useInverterStore.setState({ snapshot: snapshotWith([meter(0x01)]) });
    render(<SettingsPage />);
    const select = await screen.findByTestId('grid-ct-meter-select');
    // The outer row must collapse to a column on narrow screens so the
    // long description text isn't crammed into a 1/3-width gutter.
    const row = select.parentElement!;
    expect(row.className).toContain('flex-col');
    expect(row.className).toContain('sm:flex-row');
  });

  it('makes the select full-width on mobile and intrinsic on sm+', async () => {
    useInverterStore.setState({ snapshot: snapshotWith([meter(0x01)]) });
    render(<SettingsPage />);
    const select = await screen.findByTestId('grid-ct-meter-select');
    // `w-full` so it spans the available row width on mobile; `sm:w-auto`
    // releases it back to its content width on wider screens.
    expect(select.className).toContain('w-full');
    expect(select.className).toContain('sm:w-auto');
  });

  it('caps the description column at 60% width on sm+ so it does not crowd the select', async () => {
    useInverterStore.setState({ snapshot: snapshotWith([meter(0x01)]) });
    render(<SettingsPage />);
    const select = await screen.findByTestId('grid-ct-meter-select');
    // The label/description column (sibling of the select inside the row).
    const labelCol = select.previousElementSibling as HTMLElement | null;
    expect(labelCol).not.toBeNull();
    expect(labelCol!.className).toContain('sm:max-w-[60%]');
  });
});
