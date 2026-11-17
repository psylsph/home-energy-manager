/**
 * The Control page's notice for a dongle factory reset (issue #397): it shows
 * only while the backend flags one, offers Restore and Dismiss, and writes
 * nothing until the user presses a button.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

const mocks = vi.hoisted(() => ({ apiPost: vi.fn() }));
vi.mock('../../src/lib/api', () => ({ apiPost: mocks.apiPost }));

import ScheduleResetBanner from '../../src/components/ScheduleResetBanner';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot } from '../../src/lib/types';

const SAVED_AT = new Date(2026, 10, 14, 18, 2).getTime() / 1000;

function setSnapshot(extra: Partial<InverterSnapshot> | null) {
  useInverterStore.setState({
    snapshot: extra === null ? null : ({ ...extra } as InverterSnapshot),
    readOnly: false,
  });
}

const FLAGGED = { schedule_reset: { detected_at: SAVED_AT + 600, saved_at: SAVED_AT } };

beforeEach(() => {
  mocks.apiPost.mockReset();
  mocks.apiPost.mockResolvedValue({ ok: true });
  window.history.replaceState(null, '', '/');
});
afterEach(() => {
  cleanup();
  setSnapshot(null);
});

describe('<ScheduleResetBanner/> (issue #397)', () => {
  it('shows nothing without a flagged reset, or before any snapshot', () => {
    setSnapshot({});
    const { container, rerender } = render(<ScheduleResetBanner />);
    expect(container.textContent).toBe('');
    setSnapshot(null);
    rerender(<ScheduleResetBanner />);
    expect(container.textContent).toBe('');
  });

  it('names the saved schedule and offers both buttons', () => {
    setSnapshot(FLAGGED);
    render(<ScheduleResetBanner />);
    expect(screen.getByRole('alert').textContent).toContain('Restore the schedule from 14 Nov 18:02?');
    expect(screen.getByRole('button', { name: 'Restore Schedule' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Dismiss' })).toBeTruthy();
    expect(mocks.apiPost).not.toHaveBeenCalled();
  });

  it('restores through the restore endpoint and says the notice clears on read-back', async () => {
    setSnapshot(FLAGGED);
    render(<ScheduleResetBanner />);
    fireEvent.click(screen.getByRole('button', { name: 'Restore Schedule' }));
    await waitFor(() =>
      expect(mocks.apiPost).toHaveBeenCalledWith('/api/control/schedule-reset/restore'),
    );
    expect(await screen.findByRole('status')).toBeTruthy();
    expect(screen.getByRole('status').textContent).toContain('clears once the inverter reads back');
    // Still flagged until the snapshot says otherwise.
    expect(screen.getByRole('alert')).toBeTruthy();
  });

  it('dismisses through the dismiss endpoint and hides at once, before the next snapshot', async () => {
    setSnapshot(FLAGGED);
    render(<ScheduleResetBanner />);
    fireEvent.click(screen.getByRole('button', { name: 'Dismiss' }));
    await waitFor(() =>
      expect(mocks.apiPost).toHaveBeenCalledWith('/api/control/schedule-reset/dismiss'),
    );
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
  });

  it('keeps the banner when the dismiss fails', async () => {
    mocks.apiPost.mockRejectedValueOnce(new Error('boom'));
    setSnapshot(FLAGGED);
    render(<ScheduleResetBanner />);
    fireEvent.click(screen.getByRole('button', { name: 'Dismiss' }));
    expect((await screen.findByRole('status')).textContent).toContain('boom');
    expect(screen.getByRole('alert')).toBeTruthy();
  });

  it('shows a later reset again after an earlier one was dismissed', async () => {
    setSnapshot(FLAGGED);
    const { rerender } = render(<ScheduleResetBanner />);
    fireEvent.click(screen.getByRole('button', { name: 'Dismiss' }));
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
    setSnapshot({ schedule_reset: { detected_at: SAVED_AT + 9000, saved_at: SAVED_AT } });
    rerender(<ScheduleResetBanner />);
    expect(screen.getByRole('alert')).toBeTruthy();
  });

  it('shows the failure and lets the user try again', async () => {
    mocks.apiPost.mockRejectedValueOnce(new Error('The schedule could not be restored: exception 67. Try again.'));
    setSnapshot(FLAGGED);
    render(<ScheduleResetBanner />);
    fireEvent.click(screen.getByRole('button', { name: 'Restore Schedule' }));
    expect((await screen.findByRole('status')).textContent).toContain('could not be restored');
    const retry = screen.getByRole('button', { name: 'Restore Schedule' }) as HTMLButtonElement;
    expect(retry.disabled).toBe(false);
  });

  it('disappears when the snapshot stops flagging the reset', () => {
    setSnapshot(FLAGGED);
    const { container, rerender } = render(<ScheduleResetBanner />);
    expect(screen.getByRole('alert')).toBeTruthy();
    setSnapshot({});
    rerender(<ScheduleResetBanner />);
    expect(container.textContent).toBe('');
  });

  it('keeps the buttons off a read-only link but still says what happened', () => {
    window.history.replaceState(null, '', '/?RO');
    setSnapshot(FLAGGED);
    render(<ScheduleResetBanner />);
    expect(screen.getByRole('alert')).toBeTruthy();
    expect(screen.queryByRole('button')).toBeNull();
  });
});
