/**
 * Control page and the Intelligent Octopus Go dispatch hold (issue #394).
 *
 * While the hold pauses the battery for a smart charge, the Pause/Resume
 * button must offer the way out and say why the battery is paused. Resume
 * turns the hold off on the backend (like the load and temperature limiters),
 * so it cannot re-pause on the next poll.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    if (path === '/api/agile') return { ok: true, enabled: false };
    if (path === '/api/auto-winter')
      return {
        ok: true,
        data: {
          config: {
            enabled: false,
            cold_threshold: 8,
            recovery_threshold: 12,
            target_soc: 80,
            debounce_readings: 10,
          },
        },
      };
    if (path === '/api/cosy') return { ok: true, enabled: false, slots: [] };
    if (path === '/api/settings')
      return {
        ok: true,
        data: { import_tariff: 0.285, export_tariff: 0.15, import_tariff_config: null },
      };
    return { ok: true, data: {} };
  }),
  apiPost: vi.fn(async () => ({ ok: true, data: {} })),
  getApiBase: () => 'http://localhost:7337',
  getServerPort: () => 7337,
  fetchHistory: vi.fn().mockResolvedValue({}),
  isTauri: false,
}));

import ControlPage from '../../src/pages/ControlPage';
import { apiPost } from '../../src/lib/api';
import { useInverterStore } from '../../src/store/useInverterStore';
import { makeSnapshot } from '../fixtures/snapshot';
import type { InverterSnapshot } from '../../src/lib/types';

function show(overrides: Partial<InverterSnapshot>) {
  useInverterStore.setState({
    snapshot: makeSnapshot(overrides),
    developerMode: false,
    connectionState: 'connected',
  });
  render(<ControlPage />);
}

describe('<ControlPage/> — dispatch hold', () => {
  beforeEach(() => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(apiPost).mockClear();
  });
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it('offers to turn the hold off and resume, and says why the battery is paused', async () => {
    show({ battery_mode: 'eco_paused', battery_reserve: 100, dispatch_hold_active: true });

    const button = await screen.findByRole('button', {
      name: /Turn Off Dispatch Hold & Resume Discharge/i,
    });
    expect(screen.getByRole('status', { name: 'Dispatch hold' }).textContent).toMatch(
      /Intelligent Octopus Go hold has paused battery discharge/i,
    );

    fireEvent.click(button);
    await waitFor(() => {
      expect(apiPost).toHaveBeenCalledWith('/api/control/unpause', undefined);
    });
  });

  it('still offers Resume while the hold is restoring and the mode already reads Eco', async () => {
    show({ battery_mode: 'eco', battery_reserve: 4, dispatch_hold_active: true });
    expect(
      await screen.findByRole('button', { name: /Turn Off Dispatch Hold & Resume Discharge/i }),
    ).toBeDefined();
  });

  it('names both when a limiter and the hold are pausing together', async () => {
    show({
      battery_mode: 'eco_paused',
      battery_reserve: 100,
      dispatch_hold_active: true,
      load_limiter_active: true,
    });
    expect(
      await screen.findByRole('button', { name: /Stop Automatic Pauses & Resume Discharge/i }),
    ).toBeDefined();
  });

  it('says nothing about the hold when it is not active', async () => {
    show({ battery_mode: 'eco_paused', battery_reserve: 100, dispatch_hold_active: false });
    expect(await screen.findByRole('button', { name: /Resume Discharge/i })).toBeDefined();
    expect(screen.queryByRole('status', { name: 'Dispatch hold' })).toBeNull();
    expect(screen.queryByText(/Dispatch Hold/i)).toBeNull();
  });

  it('keeps the limiter wording when only a limiter is pausing', async () => {
    show({ battery_mode: 'eco_paused', battery_reserve: 100, load_limiter_active: true });
    expect(
      await screen.findByRole('button', { name: /Disable Limiter & Resume Discharge/i }),
    ).toBeDefined();
  });
});
