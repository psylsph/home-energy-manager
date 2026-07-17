import { render, screen, waitFor, fireEvent } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(),
  apiPost: vi.fn(),
}));

vi.mock('recharts', () => ({
  ResponsiveContainer: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  AreaChart: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  Area: () => null,
  CartesianGrid: () => null,
  Legend: () => null,
  Tooltip: () => null,
  XAxis: () => null,
  YAxis: () => null,
}));

import OctopusPage from '../../src/pages/OctopusPage';
import { apiGet, apiPost } from '../../src/lib/api';

const status = {
  ok: true,
  configured: true,
  data: {
    syncing: false,
    last_sync_at: '2026-07-17T12:00:00Z',
    last_error: null,
    backfill_complete: false,
    discovered_streams: 3,
    imported_intervals: 20,
  },
  bounds: null,
  gas_unit_note: 'Gas values are supplier-reported units.',
};

describe('OctopusPage', () => {
  beforeEach(() => {
    vi.mocked(apiGet).mockImplementation(async (path: string) => {
      if (path === '/api/octopus/status') return status;
      return {
        ok: true,
        data: {
          electricity_import: [{ t: 1_700_000_000_000, v: 1.25 }],
          electricity_export: [{ t: 1_700_000_000_000, v: 0.5 }],
          gas: [{ t: 1_700_000_000_000, v: 3.5 }],
        },
      };
    });
    vi.mocked(apiPost).mockResolvedValue({ ok: true });
  });

  it('renders supplier electricity and gas on their own page with the unit warning', async () => {
    render(<OctopusPage />);
    expect(await screen.findByText('Electricity consumption')).toBeDefined();
    expect(screen.getByText('Gas consumption')).toBeDefined();
    expect(screen.getByText('Cumulative electricity')).toBeDefined();
    expect(screen.getByText('Cumulative gas')).toBeDefined();
    expect(screen.getByText('1.250 kWh')).toBeDefined();
    expect(screen.getByText('0.500 kWh')).toBeDefined();
    expect(screen.getByText('3.500')).toBeDefined();
    expect(screen.getByText('Gas values are supplier-reported units.')).toBeDefined();
    expect(screen.getByText('3 meter stream(s)')).toBeDefined();
    expect(screen.getByText('Older history backfilling')).toBeDefined();
  });

  it('starts a manual sync and reflects its in-progress state', async () => {
    render(<OctopusPage />);
    const button = await screen.findByRole('button', { name: 'Sync now' });
    fireEvent.click(button);
    await waitFor(() => expect(apiPost).toHaveBeenCalledWith('/api/octopus/sync'));
    expect(screen.getByRole('button', { name: 'Syncing…' }).hasAttribute('disabled')).toBe(true);
  });
});
