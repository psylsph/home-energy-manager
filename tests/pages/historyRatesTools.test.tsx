import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';
import type { RatesJobStatus } from '../../src/lib/ratesJob';

const apiGet = vi.fn();
const apiPost = vi.fn();
vi.mock('../../src/lib/api', () => ({
  apiGet: (...args: unknown[]) => apiGet(...args),
  apiPost: (...args: unknown[]) => apiPost(...args),
}));

import HistoryRatesTools from '../../src/components/settings/HistoryRatesTools';

function status(over: Partial<RatesJobStatus> = {}): RatesJobStatus {
  return { running: false, kind: null, done: 0, total: 0, last: null, ...over };
}

function serve(...responses: RatesJobStatus[]) {
  let call = 0;
  apiGet.mockImplementation(async (path: string) => {
    if (path !== '/api/history/reprice') throw new Error(`unexpected GET ${path}`);
    const data = responses[Math.min(call, responses.length - 1)];
    call += 1;
    return { ok: true, data };
  });
}

const dateInput = () => screen.getByLabelText('Re-Price from Date') as HTMLInputElement;

describe('<HistoryRatesTools/> (issue #455)', () => {
  beforeEach(() => {
    apiGet.mockReset();
    apiPost.mockReset();
    apiPost.mockResolvedValue({ ok: true, data: { running: true } });
    serve(status());
  });
  afterEach(() => cleanup());

  it('keeps Re-Price History disabled until a real past date is chosen', async () => {
    render(<HistoryRatesTools octopusConfigured={false} />);
    const button = screen.getByRole('button', { name: 'Re-Price History' });
    expect((button as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(dateInput(), { target: { value: '2999-01-01' } });
    expect((button as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(dateInput(), { target: { value: '2026-10-01' } });
    expect((button as HTMLButtonElement).disabled).toBe(false);
  });

  it('asks for confirmation before anything is sent, and Cancel sends nothing', async () => {
    render(<HistoryRatesTools octopusConfigured={false} />);
    fireEvent.change(dateInput(), { target: { value: '2026-10-01' } });
    fireEvent.click(screen.getByRole('button', { name: 'Re-Price History' }));
    expect(screen.getByRole('alert').textContent).toMatch(/2026-10-01/);
    expect(screen.getByRole('alert').textContent).toMatch(/can.t be restored/);
    // The server resolves the date, and it may be in another time zone.
    expect(screen.getByRole('alert').textContent).toMatch(/time zone of the computer running HEM/);
    expect(apiPost).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('alert')).toBeNull();
    expect(apiPost).not.toHaveBeenCalled();
  });

  it('posts the chosen date once confirmed', async () => {
    render(<HistoryRatesTools octopusConfigured={false} />);
    fireEvent.change(dateInput(), { target: { value: '2026-10-01' } });
    fireEvent.click(screen.getByRole('button', { name: 'Re-Price History' }));
    fireEvent.click(screen.getByRole('button', { name: 'Confirm Re-Price' }));
    await waitFor(() => expect(apiPost).toHaveBeenCalledWith('/api/history/reprice', { from: '2026-10-01' }));
  });

  it('changing the date withdraws a pending confirmation', async () => {
    render(<HistoryRatesTools octopusConfigured={false} />);
    fireEvent.change(dateInput(), { target: { value: '2026-10-01' } });
    fireEvent.click(screen.getByRole('button', { name: 'Re-Price History' }));
    fireEvent.change(dateInput(), { target: { value: '2026-09-01' } });
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('shows the server error when a job is refused', async () => {
    apiPost.mockRejectedValue(new Error('A rates job is already running'));
    render(<HistoryRatesTools octopusConfigured={false} />);
    fireEvent.change(dateInput(), { target: { value: '2026-10-01' } });
    fireEvent.click(screen.getByRole('button', { name: 'Re-Price History' }));
    fireEvent.click(screen.getByRole('button', { name: 'Confirm Re-Price' }));
    expect((await screen.findByText('A rates job is already running')).textContent).toBeTruthy();
    // The confirmation is closed, so a retry needs a fresh confirm.
    expect(screen.queryByRole('button', { name: 'Confirm Re-Price' })).toBeNull();
  });

  it('offers the Octopus fill only for an Octopus account, and needs no confirmation', async () => {
    const { unmount } = render(<HistoryRatesTools octopusConfigured={false} />);
    expect(screen.queryByRole('button', { name: 'Fill Missing Rates from Octopus' })).toBeNull();
    unmount();
    render(<HistoryRatesTools octopusConfigured />);
    fireEvent.click(screen.getByRole('button', { name: 'Fill Missing Rates from Octopus' }));
    await waitFor(() => expect(apiPost).toHaveBeenCalledWith('/api/octopus/backfill-rates', undefined));
  });

  it('shows progress and locks the controls while a job runs', async () => {
    serve(status({ running: true, kind: 'reprice', done: 5000, total: 12000 }));
    render(<HistoryRatesTools octopusConfigured />);
    expect(await screen.findByRole('status')).toBeTruthy();
    expect(screen.getByRole('status').textContent).toBe('Re-pricing… 5,000 of 12,000 rows');
    expect((dateInput() as HTMLInputElement).disabled).toBe(true);
    expect(
      (screen.getByRole('button', { name: 'Fill Missing Rates from Octopus' }) as HTMLButtonElement).disabled,
    ).toBe(true);
  });

  it('shows how the last job ended once it is no longer running', async () => {
    serve(
      status({
        last: { kind: 'reprice', at: 1_800_000_000, ok: true, rows: 42, error: null },
      }),
    );
    render(<HistoryRatesTools octopusConfigured={false} />);
    expect(await screen.findByText('Re-priced 42 rows')).toBeTruthy();
  });

  it('copes with a status that cannot be loaded', async () => {
    apiGet.mockRejectedValue(new Error('offline'));
    render(<HistoryRatesTools octopusConfigured={false} />);
    await waitFor(() => expect(apiGet).toHaveBeenCalled());
    expect(screen.getByRole('button', { name: 'Re-Price History' })).toBeTruthy();
  });
});
