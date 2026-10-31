import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, act } from '@testing-library/react';

const apiPost = vi.fn();
vi.mock('../../src/lib/api', () => ({
  apiPost: (...args: unknown[]) => apiPost(...args),
}));

import LoadAboveSetting from '../../src/components/LoadAboveSetting';
import { DEFERRED_SAVE_DELAY_MS } from '../../src/hooks/useDeferredSave';

const input = () => screen.getByTestId('forecast-load-above-input') as HTMLInputElement;

describe('LoadAboveSetting', () => {
  beforeEach(() => {
    apiPost.mockReset();
    apiPost.mockResolvedValue({ ok: true });
  });
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it('shows the saved threshold, or Off when there is none', () => {
    render(<LoadAboveSetting savedKw={3.5} onSaved={() => {}} />);
    expect(input().value).toBe('3.5');
    cleanup();
    render(<LoadAboveSetting savedKw={null} onSaved={() => {}} />);
    expect(input().value).toBe('');
    expect(input().placeholder).toBe('Off');
  });

  it('saves a typed threshold after a pause, then tells the page', async () => {
    vi.useFakeTimers();
    const onSaved = vi.fn();
    render(<LoadAboveSetting savedKw={null} onSaved={onSaved} />);
    fireEvent.change(input(), { target: { value: '4' } });
    expect(apiPost).not.toHaveBeenCalled();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(DEFERRED_SAVE_DELAY_MS + 10);
    });
    expect(apiPost).toHaveBeenCalledWith('/api/settings', { forecast_exclude_load_above_kw: 4 });
    expect(onSaved).toHaveBeenCalled();
  });

  it('turns it off when the field is emptied and left, not while typing', async () => {
    vi.useFakeTimers();
    render(<LoadAboveSetting savedKw={3.5} onSaved={() => {}} />);
    fireEvent.change(input(), { target: { value: '' } });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(DEFERRED_SAVE_DELAY_MS * 2);
    });
    expect(apiPost).not.toHaveBeenCalled();
    fireEvent.blur(input());
    expect(apiPost).toHaveBeenCalledWith('/api/settings', { forecast_exclude_load_above_kw: null });
  });

  it('refuses a value out of range without saving', async () => {
    render(<LoadAboveSetting savedKw={null} onSaved={() => {}} />);
    fireEvent.change(input(), { target: { value: '0.5' } });
    fireEvent.blur(input());
    expect(await screen.findByTestId('forecast-load-above-error')).toHaveTextContent(
      /between 1 and 25 kW/,
    );
    expect(apiPost).not.toHaveBeenCalled();
  });

  it('treats a mistyped number as an error, never as off', async () => {
    render(<LoadAboveSetting savedKw={3} onSaved={() => {}} />);
    fireEvent.change(input(), { target: { value: '3,5' } });
    fireEvent.blur(input());
    expect(await screen.findByTestId('forecast-load-above-error')).toHaveTextContent(
      /between 1 and 25 kW/,
    );
    expect(apiPost).not.toHaveBeenCalled();
  });

  it('shows why a save failed', async () => {
    apiPost.mockRejectedValue(new Error('Leave out load above must be between 1 and 25 kW'));
    const onSaved = vi.fn();
    render(<LoadAboveSetting savedKw={null} onSaved={onSaved} />);
    fireEvent.change(input(), { target: { value: '6' } });
    fireEvent.keyDown(input(), { key: 'Enter' });
    await waitFor(() =>
      expect(screen.getByTestId('forecast-load-above-error')).toHaveTextContent(
        'Leave out load above must be between 1 and 25 kW',
      ),
    );
    expect(onSaved).not.toHaveBeenCalled();
  });
});
