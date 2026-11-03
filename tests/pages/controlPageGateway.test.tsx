import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async () => ({ ok: true, data: {} })),
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
  getApiBase: () => 'http://localhost:7337',
  getServerPort: () => 7337,
  fetchHistory: vi.fn().mockResolvedValue({}),
  isTauri: false,
}));

import ControlPage from '../../src/pages/ControlPage';
import { useInverterStore } from '../../src/store/useInverterStore';
import { apiPost } from '../../src/lib/api';
import { makeSnapshot } from '../fixtures/snapshot';

// Issue #428: a Gateway keeps its own copy of the schedule and battery
// settings and doesn't pass writes to the AIO, so the backend refuses those
// controls. The page offers only what still works there: Sync Clock, and
// Reboot in developer mode.

const BATTERY_HEADINGS = [
  'Quick Actions',
  'Battery Mode',
  'Charge Schedule',
  'Timed Export',
  'Battery and Power Controls',
];

describe('<ControlPage/> on a Gateway', () => {
  beforeEach(() => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.stubGlobal(
      'matchMedia',
      vi.fn().mockImplementation((query: string) => ({
        matches: false,
        media: query,
        onchange: null,
        addListener: vi.fn(),
        removeListener: vi.fn(),
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
        dispatchEvent: vi.fn(),
      })),
    );
    window.localStorage.clear();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    cleanup();
    useInverterStore.setState({ snapshot: null, connectionState: 'disconnected', developerMode: false });
  });

  function renderOn(code: string, developerMode = false) {
    useInverterStore.setState({
      snapshot: makeSnapshot({ device_type_code: code, device_type_display: code === '7001' ? 'Gateway' : 'All-in-One 6kW' }),
      connectionState: 'connected',
      developerMode,
    });
    render(<ControlPage />);
  }

  it('hides every battery control and says why', () => {
    renderOn('7001');
    for (const heading of BATTERY_HEADINGS) {
      expect(screen.queryByRole('heading', { name: heading }), heading).toBeNull();
    }
    expect(screen.getByText(/connect HEM to the AIO/i)).toBeDefined();
  });

  it('says automations wait for the AIO, behind the ⓘ', () => {
    renderOn('7001');
    fireEvent.click(screen.getByRole('button', { name: 'More about Gateway battery controls' }));
    expect(screen.getByRole('tooltip').textContent).toMatch(
      /automations you've set up wait until HEM is connected to the AIO/i,
    );
  });

  it('still offers Sync Clock', () => {
    renderOn('7001');
    fireEvent.click(screen.getByRole('button', { name: /Sync Clock/ }));
    expect(apiPost).toHaveBeenCalledWith('/api/control/sync-clock', undefined);
  });

  it('still offers Reboot in developer mode', () => {
    renderOn('7001', true);
    expect(screen.getByRole('heading', { name: 'Developer Controls' })).toBeDefined();
  });

  it('shows the battery controls on an AIO', () => {
    renderOn('8001');
    expect(screen.getByRole('heading', { name: 'Quick Actions' })).toBeDefined();
    expect(screen.getByRole('heading', { name: 'Charge Schedule' })).toBeDefined();
    expect(screen.queryByText(/connect HEM to the AIO/i)).toBeNull();
  });
});
