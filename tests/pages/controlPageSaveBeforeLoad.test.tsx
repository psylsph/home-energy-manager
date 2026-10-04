import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, within, act } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Each Control-page section that loads its saved configuration on mount holds
// defaults until that load finishes. Saving in that window posts the defaults
// over what the user configured. Each Save must stay disabled until its own
// load has settled, and a load that FAILS must not lock the section for good.
// ---------------------------------------------------------------------------

type Deferred = { resolve: () => void; reject: (e: unknown) => void };
const held = vi.hoisted(() => ({ paths: [] as string[], pending: {} as Record<string, Deferred | undefined> }));

const bodies: Record<string, () => unknown> = {
  '/api/auto-winter': () => ({
    ok: true,
    data: { config: { enabled: true, cold_threshold: 6, recovery_threshold: 14, target_soc: 70, debounce_readings: 4 } },
  }),
  '/api/load-limiter': () => ({
    ok: true,
    data: { config: { enabled: true, threshold_w: 3500, trigger_delay_minutes: 7, start_hour: 1, start_minute: 30, end_hour: 6, end_minute: 0 } },
  }),
  '/api/temperature-limiter': () => ({
    ok: true,
    data: { config: { enabled: true, high_threshold: 65, recovery_threshold: 55, confirmation_readings: 5 } },
  }),
  '/api/discharge-floor': () => ({ ok: true, data: { config: { enabled: true, floor_soc: 40 } } }),
  '/api/adaptive-charge': () => ({
    ok: true,
    data: {
      config: {
        periods: [{
          enabled: true, start_hour: 0, start_minute: 0, end_hour: 6, end_minute: 0, all_day: false,
          low_soc: 25, recovery_soc: 35, preferred_rate_percent: 20, recovery_rate_percent: 40,
        }],
        confirmation_readings: 2,
      },
    },
  }),
  '/api/agile': () => ({ ok: true, enabled: false, region: 'C', charge_threshold: 12, discharge_threshold: 33 }),
  '/api/cosy': () => ({ ok: true, enabled: false, slots: [] }),
  '/api/settings': () => ({ ok: true, data: { import_tariff: 0.285, export_tariff: 0.15, import_tariff_config: null } }),
};

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn((path: string) => {
    const body = bodies[path];
    if (held.paths.includes(path)) {
      return new Promise((resolve, reject) => {
        held.pending[path] = { resolve: () => resolve(body()), reject };
      });
    }
    return Promise.resolve(body ? body() : { ok: true, data: {} });
  }),
  apiPost: vi.fn(),
  getApiBase: () => 'http://localhost:7337',
  getServerPort: () => 7337,
  fetchHistory: vi.fn().mockResolvedValue({}),
  isTauri: false,
}));

import ControlPage from '../../src/pages/ControlPage';
import { useInverterStore } from '../../src/store/useInverterStore';
import { apiPost } from '../../src/lib/api';
import { makeSnapshot } from '../fixtures/snapshot';

const release = (path: string) => act(async () => held.pending[path]!.resolve());
const fail = (path: string) => act(async () => held.pending[path]!.reject(new Error('backend not ready')));

beforeEach(() => {
  vi.spyOn(console, 'error').mockImplementation(() => {});
  vi.spyOn(console, 'warn').mockImplementation(() => {});
  vi.stubGlobal(
    'matchMedia',
    vi.fn().mockImplementation((query: string) => ({
      matches: false, media: query, onchange: null, addListener: vi.fn(), removeListener: vi.fn(),
      addEventListener: vi.fn(), removeEventListener: vi.fn(), dispatchEvent: vi.fn(),
    })),
  );
  vi.spyOn(globalThis, 'fetch').mockResolvedValue({ ok: true, json: async () => ({ results: [] }) } as Response);
  window.localStorage.clear();
  held.paths = [];
  held.pending = {};
  vi.mocked(apiPost).mockReset();
  vi.mocked(apiPost).mockResolvedValue({ ok: true, data: {} });
  useInverterStore.setState({
    snapshot: makeSnapshot({ adaptive_charge_enabled: true }),
    developerMode: true,
    connectionState: 'connected',
    connectedHost: '192.168.1.36:8899',
    batteryModePending: null,
    batteryModeError: null,
  });
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  cleanup();
  useInverterStore.setState({ snapshot: null, connectionState: 'disconnected', developerMode: false });
});

const SAVE = /^(Save|Saving\.\.\.|\.\.\.|✓ Saved|✗ Error)$/;

interface SectionCase {
  name: string;
  path: string;
  /** The Save button, once the section is on the page. */
  save: () => HTMLElement;
  /** The endpoint that Save posts to. */
  posts: string;
  /** Part of the loaded configuration that must reach the post. */
  loaded: Record<string, unknown>;
}

const heading = (name: string) => screen.getByRole('heading', { name }).parentElement as HTMLElement;

const cases: SectionCase[] = [
  {
    name: 'Auto Winter',
    path: '/api/auto-winter',
    save: () => within(screen.getByRole('heading', { name: 'Auto Winter Mode' }).closest('section') as HTMLElement).getByRole('button', { name: SAVE }),
    posts: '/api/auto-winter',
    loaded: { enabled: true, cold_threshold: 6, target_soc: 70 },
  },
  {
    name: 'Load Discharge Limiter',
    path: '/api/load-limiter',
    save: () => within(heading('Load Discharge Limiter')).getByRole('button', { name: SAVE }),
    posts: '/api/load-limiter',
    loaded: { enabled: true, threshold_w: 3500, trigger_delay_minutes: 7 },
  },
  {
    name: 'Inverter Temperature Limiter',
    path: '/api/temperature-limiter',
    save: () => within(heading('Inverter Temperature Limiter')).getByRole('button', { name: SAVE }),
    posts: '/api/temperature-limiter',
    loaded: { enabled: true, high_threshold: 65, confirmation_readings: 5 },
  },
  {
    name: 'Discharge Schedule Minimum SOC',
    path: '/api/discharge-floor',
    save: () => within(heading('Discharge Schedule Minimum SOC')).getByRole('button', { name: SAVE }),
    posts: '/api/discharge-floor',
    loaded: { enabled: true, floor_soc: 40 },
  },
  {
    name: 'Adaptive Charge',
    path: '/api/adaptive-charge',
    save: () => screen.getByRole('button', { name: /Save Adaptive Charge|Saving|Saved|Check settings/ }),
    posts: '/api/adaptive-charge',
    loaded: { config: expect.objectContaining({ periods: [expect.objectContaining({ low_soc: 25, recovery_soc: 35 })] }) },
  },
];

describe('<ControlPage/> — saves wait for the section to load', () => {
  for (const c of cases) {
    describe(c.name, () => {
      it('has Save disabled until its configuration has loaded', async () => {
        held.paths = [c.path];
        render(<ControlPage />);
        await waitFor(() => expect(c.save()).toBeInTheDocument());
        expect(c.save()).toBeDisabled();

        await release(c.path);

        await waitFor(() => expect(c.save()).toBeEnabled());
      });

      it('posts nothing when Save is clicked before the load', async () => {
        held.paths = [c.path];
        render(<ControlPage />);
        await waitFor(() => expect(c.save()).toBeInTheDocument());

        fireEvent.click(c.save());

        expect(vi.mocked(apiPost).mock.calls.filter((x) => x[0] === c.posts)).toHaveLength(0);
      });

      it('then posts the loaded configuration, not the defaults', async () => {
        held.paths = [c.path];
        render(<ControlPage />);
        await waitFor(() => expect(c.save()).toBeInTheDocument());
        await release(c.path);
        await waitFor(() => expect(c.save()).toBeEnabled());

        fireEvent.click(c.save());

        await waitFor(() => expect(apiPost).toHaveBeenCalledWith(c.posts, expect.objectContaining(c.loaded)));
      });

      it('is not locked out when the load fails', async () => {
        held.paths = [c.path];
        render(<ControlPage />);
        await waitFor(() => expect(c.save()).toBeInTheDocument());
        expect(c.save()).toBeDisabled();

        await fail(c.path);

        await waitFor(() => expect(c.save()).toBeEnabled());
      });
    });
  }

  describe('Agile thresholds', () => {
    const save = () => {
      const root = screen.getByText('Postcode').closest('div.space-y-4') as HTMLElement;
      return within(root).getByRole('button', { name: SAVE });
    };
    async function openAgile() {
      render(<ControlPage />);
      const select = (await screen.findAllByRole('combobox'))[0] as HTMLSelectElement;
      await waitFor(() => expect(select).toBeEnabled());
      fireEvent.change(select, { target: { value: 'agile' } });
      await screen.findByText('Postcode');
    }

    it('has Save disabled until the saved region and thresholds have loaded', async () => {
      held.paths = ['/api/agile'];
      useInverterStore.setState({ snapshot: makeSnapshot({ adaptive_charge_enabled: false }) });
      await openAgile();
      expect(save()).toBeDisabled();

      await release('/api/agile');

      await waitFor(() => expect(save()).toBeEnabled());
      fireEvent.click(save());
      await waitFor(() =>
        expect(apiPost).toHaveBeenCalledWith('/api/agile', { region: 'C', charge_threshold: 12, discharge_threshold: 33 }),
      );
    });

    it('is not locked out when the load fails', async () => {
      held.paths = ['/api/agile'];
      useInverterStore.setState({ snapshot: makeSnapshot({ adaptive_charge_enabled: false }) });
      await openAgile();
      expect(save()).toBeDisabled();
      await fail('/api/agile');
      await waitFor(() => expect(save()).toBeEnabled());
    });
  });
});
