import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, act } from '@testing-library/react';

const mocks = vi.hoisted(() => ({
  apiGet: vi.fn(),
  apiPost: vi.fn().mockResolvedValue({ ok: true }),
  openExternal: vi.fn().mockResolvedValue(undefined),
}));

vi.mock('../../src/lib/api', () => ({
  apiGet: mocks.apiGet,
  apiPost: mocks.apiPost,
}));
vi.mock('../../src/lib/openExternal', () => ({
  openExternal: mocks.openExternal,
}));

import AwaitingConnection from '../../src/components/AwaitingConnection';
import { useInverterStore } from '../../src/store/useInverterStore';

// ---------------------------------------------------------------------------
// Issue #452: when the dongle can't be reached, the banner offers to restart
// just the dongle from its own web page (if the backend finds one answering),
// and keeps the isolator power-cycle as the way out.
// ---------------------------------------------------------------------------

const DONGLE_URL = 'http://192.168.1.50/home.html';

afterEach(() => {
  cleanup();
  useInverterStore.setState({ reconnectRequestedAt: null });
});

beforeEach(() => {
  mocks.apiGet.mockReset();
  mocks.apiPost.mockClear();
  mocks.openExternal.mockClear();
});

const renderBanner = (props: Partial<React.ComponentProps<typeof AwaitingConnection>> = {}) =>
  render(<AwaitingConnection connectionState="disconnected" connectFailures={6} {...props} />);

describe('<AwaitingConnection/> dongle page (issue #452)', () => {
  it('offers the dongle page, with the isolator advice as the fallback', async () => {
    mocks.apiGet.mockResolvedValue({ ok: true, url: DONGLE_URL });
    renderBanner();
    const open = await screen.findByRole('button', { name: 'Open Dongle Page' });
    expect(mocks.apiGet).toHaveBeenCalledWith('/api/dongle-page');
    expect(screen.getByText(/Device Management, then Restart/)).toBeTruthy();
    expect(screen.getByText(/If that doesn't help/).textContent).toMatch(/power-cycle the inverter/);
    fireEvent.click(open);
    expect(mocks.openExternal).toHaveBeenCalledWith(DONGLE_URL);
  });

  it('puts the login to try behind an info tip beside the button', async () => {
    mocks.apiGet.mockResolvedValue({ ok: true, url: DONGLE_URL });
    renderBanner();
    await screen.findByRole('button', { name: 'Open Dongle Page' });
    const tip = screen.getByRole('button', { name: /More about the dongle login/i });
    expect(tip).toBeTruthy();
    const text = document.getElementById(tip.getAttribute('aria-describedby') ?? '')?.textContent ?? '';
    expect(text).toMatch(/admin.*admin/);
    expect(text).toMatch(/unless you've changed it/);
    expect(text).toMatch(/123456/);
    expect(text).toMatch(/serial/i);
  });

  it('says the page is not answering, and falls back to the power-cycle, when no page answers', async () => {
    mocks.apiGet.mockResolvedValue({ ok: true, url: null });
    renderBanner();
    expect(await screen.findByText(/web page isn't answering either/)).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Open Dongle Page' })).toBeNull();
    expect(screen.getByText(/Power-cycle the inverter/)).toBeTruthy();
  });

  it('treats a failed probe like no answer', async () => {
    mocks.apiGet.mockRejectedValue(new Error('backend down'));
    renderBanner();
    expect(await screen.findByText(/web page isn't answering either/)).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Open Dongle Page' })).toBeNull();
  });

  it('holds back the isolator advice, and claims nothing, while the probe is out', async () => {
    // Acting on "power-cycle" before the page answers would skip the gentler restart.
    mocks.apiGet.mockReturnValue(new Promise(() => {}));
    renderBanner();
    expect(screen.getByText(/Checking whether its web page answers/)).toBeTruthy();
    expect(screen.queryByText(/ower-cycle the inverter/)).toBeNull();
    expect(screen.queryByText(/isn't answering/)).toBeNull();
    expect(screen.queryByRole('button', { name: 'Open Dongle Page' })).toBeNull();
    // Retry Now stays available meanwhile.
    expect(screen.getByRole('button', { name: 'Retry Now' })).toBeTruthy();
  });

  it.each([
    ['fewer than 5 failed connects', { connectFailures: 4 }],
    ['a connection that is reconnecting', { connectionState: 'reconnecting' as const }],
  ])('makes no probe for %s', async (_name, props) => {
    mocks.apiGet.mockResolvedValue({ ok: true, url: DONGLE_URL });
    renderBanner(props);
    await act(async () => {});
    expect(mocks.apiGet).not.toHaveBeenCalled();
  });

  it('probes again on Retry Now, and uses the new answer', async () => {
    mocks.apiGet.mockResolvedValueOnce({ ok: true, url: null });
    renderBanner();
    await screen.findByText(/web page isn't answering either/);
    expect(mocks.apiGet).toHaveBeenCalledTimes(1);

    mocks.apiGet.mockResolvedValueOnce({ ok: true, url: DONGLE_URL });
    fireEvent.click(screen.getByRole('button', { name: 'Retry Now' }));
    expect(await screen.findByRole('button', { name: 'Open Dongle Page' })).toBeTruthy();
    expect(mocks.apiGet).toHaveBeenCalledTimes(2);
    await waitFor(() => expect(mocks.apiPost).toHaveBeenCalledWith('/api/reconnect'));
  });

  it('does not probe on a timer', async () => {
    vi.useFakeTimers();
    try {
      mocks.apiGet.mockResolvedValue({ ok: true, url: DONGLE_URL });
      renderBanner();
      await act(async () => {});
      await act(async () => {
        await vi.advanceTimersByTimeAsync(10 * 60_000);
      });
      expect(mocks.apiGet).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });
});
