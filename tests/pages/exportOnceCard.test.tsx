import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';
import type { ExportOnceEvent, ScheduleSlot } from '../../src/lib/types';

const apiGet = vi.fn();
const apiPost = vi.fn();
vi.mock('../../src/lib/api', () => ({
  apiGet: (...args: unknown[]) => apiGet(...args),
  apiPost: (...args: unknown[]) => apiPost(...args),
}));

import ExportOnceCard from '../../src/components/ExportOnceCard';

const NOW = '2026-10-17 12:34';

const slots: ScheduleSlot[] = [
  { enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 4 },
  { enabled: true, start_hour: 22, start_minute: 0, end_hour: 23, end_minute: 0, target_soc: 30 },
];

function event(overrides: Partial<ExportOnceEvent> = {}): ExportOnceEvent {
  return {
    id: 'e1',
    slot: 2,
    start: '2026-10-17 18:00',
    end: '2026-10-17 19:00',
    target_soc: 20,
    source: 'manual',
    state: 'pending',
    ...overrides,
  };
}

function serve(events: ExportOnceEvent[]) {
  apiGet.mockImplementation(async (path: string) => {
    if (path === '/api/export-once') return { ok: true, now: NOW, events };
    throw new Error(`unexpected GET ${path}`);
  });
}

function renderCard(max = 2) {
  return render(<ExportOnceCard maxDischargeSlots={max} dischargeSlots={slots} />);
}

describe('ExportOnceCard', () => {
  beforeEach(() => {
    apiGet.mockReset();
    apiPost.mockReset();
  });
  afterEach(cleanup);

  it('seeds the form from the inverter clock and lets the user pick a slot', async () => {
    serve([]);
    renderCard();
    expect(await screen.findByRole('heading', { name: 'Export Once' })).toBeTruthy();
    const start = (await screen.findByLabelText('Start')) as HTMLInputElement;
    expect(start.value).toBe('13:00');
    expect((screen.getByLabelText('End') as HTMLInputElement).value).toBe('14:00');
    expect((screen.getByLabelText('Date') as HTMLInputElement).value).toBe('2026-10-17');
    // The picker shows what each slot holds now, since it will be put back.
    expect(screen.getByRole('option', { name: 'Slot 1 (now not set)' })).toBeTruthy();
    expect(screen.getByRole('option', { name: 'Slot 2 (now 22:00–23:00)' })).toBeTruthy();
  });

  it('books the chosen window and refreshes the list', async () => {
    serve([]);
    apiPost.mockResolvedValue({ ok: true });
    renderCard();
    await screen.findByLabelText('Start');
    fireEvent.change(screen.getByLabelText('Slot To Use'), { target: { value: '2' } });
    fireEvent.change(screen.getByLabelText('Start'), { target: { value: '18:00' } });
    fireEvent.change(screen.getByLabelText('End'), { target: { value: '19:00' } });
    fireEvent.click(screen.getByRole('button', { name: 'Book Export' }));

    await waitFor(() => expect(apiPost).toHaveBeenCalledTimes(1));
    expect(apiPost).toHaveBeenCalledWith('/api/export-once', {
      slot: 2,
      date: '2026-10-17',
      start: '18:00',
      end: '19:00',
      target_soc: 20,
    });
    // Refreshed after booking: initial load plus one more.
    await waitFor(() => expect(apiGet.mock.calls.length).toBeGreaterThanOrEqual(2));
  });

  it('explains a bad window without posting it', async () => {
    serve([]);
    renderCard();
    await screen.findByLabelText('Start');
    fireEvent.change(screen.getByLabelText('Start'), { target: { value: '19:00' } });
    fireEvent.change(screen.getByLabelText('End'), { target: { value: '18:00' } });
    fireEvent.click(screen.getByRole('button', { name: 'Book Export' }));
    expect((await screen.findByRole('alert')).textContent).toMatch(/end must be after the start/i);
    expect(apiPost).not.toHaveBeenCalled();
  });

  it('shows the server error when booking is refused', async () => {
    serve([]);
    apiPost.mockRejectedValue(new Error('Slot 1 already has an export booked that overlaps this window'));
    renderCard();
    await screen.findByLabelText('Start');
    fireEvent.click(screen.getByRole('button', { name: 'Book Export' }));
    expect((await screen.findByRole('alert')).textContent).toMatch(/already has an export booked/);
  });

  it('lists booked exports and cancels one', async () => {
    serve([event()]);
    apiPost.mockResolvedValue({ ok: true });
    renderCard();
    expect(await screen.findByText(/Slot 2 · Sat 17 Oct, 18:00–19:00/)).toBeTruthy();
    expect(screen.getByText('Booked')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: /Cancel export in slot 2/ }));
    await waitFor(() =>
      expect(apiPost).toHaveBeenCalledWith('/api/export-once/cancel', { id: 'e1' }),
    );
  });

  it('says the slot is temporary while an export is running', async () => {
    serve([
      event({
        state: 'active',
        applied: true,
        original: { slot: slots[1], schedule_enabled: true },
      }),
    ]);
    renderCard();
    expect(await screen.findByText('Running')).toBeTruthy();
    expect(screen.getByText(/goes back to 22:00–23:00 afterwards/)).toBeTruthy();
    expect(screen.getByRole('button', { name: /Cancel export in slot 2/ }).textContent).toBe(
      'Stop And Put The Slot Back',
    );
  });

  it('shows why an export failed and keeps finished ones under a disclosure', async () => {
    serve([
      event({ id: 'f1', state: 'failed', note: 'Missed: HEM did not apply this export' }),
      event({ id: 'd1', state: 'done', start: '2026-10-10 18:00', end: '2026-10-10 19:00' }),
    ]);
    renderCard();
    expect(await screen.findByText(/Recent Exports/)).toBeTruthy();
    expect(screen.getByText(/Missed: HEM did not apply this export/)).toBeTruthy();
    expect(screen.queryByRole('button', { name: /Cancel export/ })).toBeNull();
  });

  it('hides the slot picker with a single slot and the floor on models without per-slot SOC', async () => {
    serve([]);
    renderCard(1);
    await screen.findByLabelText('Start');
    expect(screen.queryByLabelText('Slot To Use')).toBeNull();
    expect(screen.queryByLabelText('Stop Exporting At')).toBeNull();
  });

  it('offers the discharge floor on models with per-slot SOC', async () => {
    serve([]);
    renderCard(10);
    await screen.findByLabelText('Start');
    expect(screen.getByLabelText('Stop Exporting At')).toBeTruthy();
  });

  it('warns when the list cannot be loaded and does not offer a form', async () => {
    apiGet.mockRejectedValue(new Error('offline'));
    renderCard();
    expect((await screen.findByRole('status')).textContent).toMatch(/could not be loaded/i);
    // A passive load problem is not an alert: it must not compete with the page's own.
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Book Export' })).toBeNull();
  });
});
