import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, act } from '@testing-library/react';

const apiMocks = vi.hoisted(() => ({
  apiPost: vi.fn().mockResolvedValue({ ok: true }),
}));

vi.mock('../../src/lib/api', () => ({
  apiPost: apiMocks.apiPost,
}));

import { ConnectionIndicator } from '../../src/App';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot } from '../../src/lib/types';

// ---------------------------------------------------------------------------
// The header connection indicator is the one reconnect affordance that's
// visible on every page and in every connection state. These tests pin the
// two behaviours the old UI lacked:
//
//   1. A "Reconnect" button must appear when the connection is connected-but-
//      stale (the classic zombie dongle: TCP up, data frozen). Previously
//      there was no reconnect control at all in that state.
//   2. A "Reconnect requested at HH:MM:SS" notice must appear after a click,
//      so a forced retry against an unreachable dongle produces visible
//      feedback instead of looking inert.
// ---------------------------------------------------------------------------

const RECONNECT_BTN = { name: 'Reconnect' };

describe('<ConnectionIndicator/> — reconnect control', () => {
  beforeEach(() => {
    // The per-second setInterval (armed while connected / a notice is live)
    // trips act() warnings under jsdom; silence it like the sibling mobile
    // layout test does.
    vi.spyOn(console, 'error').mockImplementation(() => {});
    apiMocks.apiPost.mockClear();
    apiMocks.apiPost.mockResolvedValue({ ok: true });
    useInverterStore.setState({
      connectionState: 'disconnected',
      connectedHost: null,
      snapshot: null,
      reconnectRequestedAt: null,
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    useInverterStore.setState({
      connectionState: 'disconnected',
      connectedHost: null,
      snapshot: null,
      reconnectRequestedAt: null,
    });
  });

  function seedConnectedFresh(host: string) {
    useInverterStore.setState({
      connectionState: 'connected',
      connectedHost: host,
      snapshot: { timestamp: Math.floor(Date.now() / 1000) - 30 } as InverterSnapshot,
    });
  }

  function seedConnectedStale(host: string, ageSeconds = 660) {
    useInverterStore.setState({
      connectionState: 'connected',
      connectedHost: host,
      // 11 minutes old → past the 10-minute staleness threshold.
      snapshot: {
        timestamp: Math.floor(Date.now() / 1000) - ageSeconds,
      } as InverterSnapshot,
    });
  }

  it('hides the Reconnect button when connected and healthy', () => {
    seedConnectedFresh('192.168.1.42:8899');
    render(<ConnectionIndicator />);
    expect(screen.queryByRole('button', RECONNECT_BTN)).toBeNull();
  });

  it('shows the Reconnect button when connected but data is stale (zombie dongle)', () => {
    seedConnectedStale('192.168.1.42:8899');
    render(<ConnectionIndicator />);
    // This is the gap the change closes: previously there was NO reconnect
    // affordance at all while connected, so a frozen dongle meant restart
    // the app. Now the button shows right next to the pulsing red time.
    expect(screen.getByRole('button', RECONNECT_BTN)).toBeDefined();
  });

  it('shows the Reconnect button while disconnected', () => {
    useInverterStore.setState({
      connectionState: 'disconnected',
      connectedHost: '192.168.1.42:8899',
    });
    render(<ConnectionIndicator />);
    expect(screen.getByRole('button', RECONNECT_BTN)).toBeDefined();
  });

  it('shows the Reconnect button while reconnecting', () => {
    useInverterStore.setState({
      connectionState: 'reconnecting',
      connectedHost: '192.168.1.42:8899',
    });
    render(<ConnectionIndicator />);
    expect(screen.getByRole('button', RECONNECT_BTN)).toBeDefined();
  });

  it('hides the "Reconnect requested" notice until a reconnect is triggered', () => {
    seedConnectedStale('192.168.1.42:8899');
    render(<ConnectionIndicator />);
    expect(screen.queryByTitle('Reconnect requested')).toBeNull();
  });

  it('shows the "Reconnect requested" notice with a timestamp after a click', async () => {
    useInverterStore.setState({
      connectionState: 'reconnecting',
      connectedHost: '192.168.1.42:8899',
    });
    render(<ConnectionIndicator />);

    await act(async () => {
      fireEvent.click(screen.getByRole('button', RECONNECT_BTN));
    });
    expect(apiMocks.apiPost).toHaveBeenCalledWith('/api/reconnect');

    // The notice reads the shared store timestamp stamped by the hook. It
    // carries the ↻ glyph and a HH:MM:SS clock so the user can see the click
    // registered even though the connection state itself doesn't change.
    const notice = screen.getByTitle('Reconnect requested');
    expect(notice.textContent ?? '').toMatch(/↻\s*\d{1,2}:\d{2}:\d{2}/);
  });

  it('disables the button and shows "…" while a reconnect request is in flight', async () => {
    // A never-resolving promise pins the hook in its in-flight state.
    let resolvePost!: (v: unknown) => void;
    apiMocks.apiPost.mockReturnValue(
      new Promise((res) => {
        resolvePost = res;
      }),
    );
    useInverterStore.setState({ connectionState: 'disconnected' });
    render(<ConnectionIndicator />);

    const btn = screen.getByRole('button', RECONNECT_BTN);
    fireEvent.click(btn);
    expect(btn).toHaveProperty('disabled', true);
    expect(btn.textContent).toBe('…');

    // Let the request settle so the test doesn't leave a dangling promise.
    await Promise.resolve(resolvePost({ ok: true }));
  });
});
