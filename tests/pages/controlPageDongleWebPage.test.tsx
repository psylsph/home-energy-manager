import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';

// Issue #452: the Dongle Web Page option sits in Developer Controls beside
// Inverter Reboot, in both Control page layouts, and only in developer mode.

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    if (path === '/api/dongle-page') return { ok: true, url: 'http://192.168.1.50/home.html' };
    if (path === '/api/agile') return { ok: true, enabled: false };
    if (path === '/api/cosy') return { ok: true, enabled: false, slots: [] };
    if (path === '/api/settings') {
      return { ok: true, data: { import_tariff: 0.285, export_tariff: 0.15, import_tariff_config: null } };
    }
    return { ok: true, data: {} };
  }),
  apiPost: vi.fn(),
  getApiBase: () => 'http://localhost:7337',
  getServerPort: () => 7337,
  fetchHistory: vi.fn().mockResolvedValue({}),
  isTauri: false,
}));

import ControlPage from '../../src/pages/ControlPage';
import { useInverterStore } from '../../src/store/useInverterStore';
import { makeSnapshot } from '../fixtures/snapshot';
import type { InverterSnapshot } from '../../src/lib/types';

function connect(overrides: Partial<InverterSnapshot>, developerMode: boolean) {
  useInverterStore.setState({
    snapshot: makeSnapshot(overrides),
    developerMode,
    connectionState: 'connected',
  });
}

beforeEach(() => {
  vi.clearAllMocks();
});

afterEach(() => {
  cleanup();
  useInverterStore.setState({ snapshot: null, developerMode: false, connectionState: 'disconnected' });
});

describe('<ControlPage/> — Dongle Web Page (issue #452)', () => {
  it.each([
    ['a single-phase inverter', {}],
    ['a Gateway', { device_type_code: '7001' }],
  ])('is offered beside Inverter Reboot in developer mode on %s', async (_name, overrides) => {
    connect(overrides, true);
    render(<ControlPage />);
    expect(screen.getByRole('heading', { name: 'Inverter Reboot' })).toBeTruthy();
    expect(screen.getByRole('heading', { name: 'Dongle Web Page' })).toBeTruthy();
    expect(await screen.findByRole('button', { name: 'Open Dongle Page' })).toBeTruthy();
  });

  it.each([
    ['a single-phase inverter', {}],
    ['a Gateway', { device_type_code: '7001' }],
  ])('is hidden, and not probed, outside developer mode on %s', async (_name, overrides) => {
    connect(overrides, false);
    render(<ControlPage />);
    expect(screen.queryByRole('heading', { name: 'Dongle Web Page' })).toBeNull();
    const { apiGet } = await import('../../src/lib/api');
    expect(vi.mocked(apiGet)).not.toHaveBeenCalledWith('/api/dongle-page');
  });
});
