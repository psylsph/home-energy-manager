import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent } from '@testing-library/react';

const apiMocks = vi.hoisted(() => ({
  apiPost: vi.fn().mockResolvedValue({ ok: true }),
}));

vi.mock('../../src/lib/api', () => ({
  apiPost: apiMocks.apiPost,
}));

import AwaitingConnection from '../../src/components/AwaitingConnection';
import { awaitingConnectionMessage } from '../../src/lib/awaitingConnection';
import type { ConnectionState } from '../../src/lib/types';

// ---------------------------------------------------------------------------
// AwaitingConnection is the shared placeholder every tab renders while the
// backend has no usable connection to the inverter. It exists because the
// inline copies in Battery / Solar / Inverter / Control / Status had
// drifted apart in both wording and gating. These tests pin the message
// strings and the Retry-button behaviour so they can't drift again.
// ---------------------------------------------------------------------------

afterEach(() => {
  vi.restoreAllMocks();
  cleanup();
});

describe('awaitingConnectionMessage() — word by connection state', () => {
  it.each([
    ['reconnecting', 'Connection lost — reconnecting…'],
    ['disconnected', 'Disconnected — will retry automatically'],
    ['connected', 'Waiting for data…'],
  ] as const)('returns the canonical string for %s', (state, expected) => {
    expect(awaitingConnectionMessage(state as ConnectionState)).toBe(expected);
  });
});

describe('<AwaitingConnection/>', () => {
  beforeEach(() => {
    apiMocks.apiPost.mockClear();
  });

  it('renders the spinner and the message for the given state', () => {
    render(<AwaitingConnection connectionState="reconnecting" />);
    expect(
      screen.getByText('Connection lost — reconnecting…'),
    ).toBeDefined();
    // Spinner is present.
    expect(document.querySelector('.animate-spin')).not.toBeNull();
  });

  it('shows the connected host with the port stripped', () => {
    render(
      <AwaitingConnection
        connectionState="reconnecting"
        connectedHost="192.168.1.36:8899"
      />,
    );
    // The host <p> reads "Host: 192.168.1.36" with the port stripped.
    expect(screen.getByText(/Host: 192\.168\.1\.36/)).toBeDefined();
    // The :8899 suffix must not leak through anywhere on the page.
    expect(screen.queryByText(/8899/)).toBeNull();
  });

  it('omits the host line when no host is provided', () => {
    render(<AwaitingConnection connectionState="disconnected" />);
    expect(screen.queryByText(/Host:/)).toBeNull();
  });

  it('renders the Retry button only when showRetry is set', () => {
    const { rerender } = render(<AwaitingConnection connectionState="reconnecting" />);
    expect(screen.queryByRole('button', { name: /Retry now/i })).toBeNull();

    rerender(<AwaitingConnection connectionState="reconnecting" showRetry />);
    expect(screen.getByRole('button', { name: /Retry now/i })).toBeDefined();
  });

  it('POSTs to /api/reconnect and flips to the Reconnecting… label on click', async () => {
    render(<AwaitingConnection connectionState="reconnecting" showRetry />);
    fireEvent.click(screen.getByRole('button', { name: /Retry now/i }));

    expect(apiMocks.apiPost).toHaveBeenCalledWith('/api/reconnect');
    expect(screen.getByText(/Reconnecting…/)).toBeDefined();
  });

  it('does not crash when /api/reconnect rejects (the poll loop back-off retries anyway)', async () => {
    apiMocks.apiPost.mockRejectedValueOnce(new Error('network down'));
    render(<AwaitingConnection connectionState="reconnecting" showRetry />);

    expect(() =>
      fireEvent.click(screen.getByRole('button', { name: /Retry now/i })),
    ).not.toThrow();
  });

  it('renders the extra page-specific note when provided', () => {
    render(
      <AwaitingConnection
        connectionState="disconnected"
        extraNote="Controls are disabled while the inverter is unreachable."
      />,
    );
    expect(
      screen.getByText(
        'Controls are disabled while the inverter is unreachable.',
      ),
    ).toBeDefined();
  });

  it('renders the FAQ help paragraph when showFaq is set', () => {
    render(<AwaitingConnection connectionState="reconnecting" showFaq />);
    const faqLink = screen.getByRole('link', { name: 'FAQ' });
    expect(faqLink).toBeDefined();
    expect(faqLink.getAttribute('href')).toBe(
      'https://github.com/psylsph/home-energy-manager/blob/master/FAQ.md',
    );
  });

  it('omits the FAQ paragraph by default', () => {
    render(<AwaitingConnection connectionState="reconnecting" />);
    expect(screen.queryByRole('link', { name: 'FAQ' })).toBeNull();
  });
});
