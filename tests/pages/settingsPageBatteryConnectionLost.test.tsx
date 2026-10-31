import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    if (path === '/api/alerts') {
      return {
        ok: true,
        data: {
          config: {
            enabled: true,
            telegram_bot_token: '',
            telegram_chat_id: '',
            cooldown_minutes: 30,
            batt_temp_min: 0, batt_temp_max: 0,
            inverter_temp_min: 8, inverter_temp_max: 60,
            soc_min: 4, soc_max: 100,
            grid_offline_enabled: false, inverter_trip_enabled: false,
            battery_over_temp_enabled: false,
            battery_connection_lost_enabled: true,
            connection_lost_enabled: false,
            solar_clipping_enabled: false, solar_clipping_ceiling_w: 0,
            ntfy_topic: '', ntfy_server: 'https://ntfy.sh',
            pushover_app_token: '', pushover_user_key: '',
          },
        },
      };
    }
    return { ok: true, data: {} };
  }),
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
}));

vi.mock('../../src/lib/openExternal', () => ({
  openExternal: vi.fn().mockResolvedValue(undefined),
}));

import NotificationsSection from '../../src/components/settings/NotificationsSection';

const Section = () => <NotificationsSection flash={() => {}} serial="TEST123" />;

function silenceConsoleError() {
  vi.spyOn(console, 'error').mockImplementation(() => {});
}

describe('<NotificationsSection/> — Battery Connection Lost toggle wording (issue #272)', () => {
  beforeEach(() => {
    silenceConsoleError();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  it('describes both detection modes: BMS silence and DC breaker trip', async () => {
    render(<Section />);
    // The row is always rendered once alerts config loads (no collapsible).
    const label = await screen.findByText('Battery Connection Lost');
    // The description block is inside the same <span> parent.
    const row = label.closest('span') ?? label.parentElement;
    expect(row).not.toBeNull();
    const text = (row as HTMLElement).textContent ?? '';
    // Covers the per-battery BMS-silence mode…
    expect(text).toMatch(/stops responding/i);
    // …and the breaker-trip voltage-mismatch mode added for issue #272.
    expect(text).toMatch(/breaker/i);
    expect(text).toMatch(/voltage collapses/i);
  });
});
