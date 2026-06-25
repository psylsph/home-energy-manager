import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';

import { ConnectionIndicator } from '../../src/App';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot } from '../../src/lib/types';

// ---------------------------------------------------------------------------
// The top-of-app header's connection indicator used to render the inverter IP
// and the last-updated time on a single row. On a phone that forced the header
// to overflow / wrap awkwardly. The fix stacks the time *under* the IP on
// narrow screens (`flex-col`) and keeps them side-by-side on `sm+` with the
// middot separator restored. jsdom doesn't compute flexbox layout, so these
// tests assert on the responsive class structure and DOM order instead — the
// same way the existing grid-lines / settings tests pin down CSS behaviour.
// ---------------------------------------------------------------------------

describe('<ConnectionIndicator/> — mobile header layout', () => {
  beforeEach(() => {
    // Silence React act() warnings from the per-second setInterval that the
    // component arms while connected (matches the settings-page test setup).
    vi.spyOn(console, 'error').mockImplementation(() => {});
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    useInverterStore.setState({
      connectionState: 'disconnected',
      connectedHost: null,
      snapshot: null,
    });
  });

  function seedConnected(host: string, ageSeconds: number) {
    useInverterStore.setState({
      connectionState: 'connected',
      connectedHost: host,
      snapshot: {
        timestamp: Math.floor(Date.now() / 1000) - ageSeconds,
      } as InverterSnapshot,
    });
  }

  it('stacks the last-updated time under the inverter IP on mobile', () => {
    seedConnected('192.168.1.42:8899', 120);
    render(<ConnectionIndicator />);

    // Port stripped, exactly as it shows in the header.
    const ip = screen.getByText('192.168.1.42');
    expect(ip).toBeDefined();

    // The IP and its time share a wrapper that lays out as a column on
    // mobile and a row on sm+. This is the actual fix: the time drops
    // beneath the IP on a phone instead of being crammed onto one line.
    const wrapper = ip.parentElement;
    expect(wrapper).not.toBeNull();
    expect(wrapper!.className).toContain('flex-col');
    expect(wrapper!.className).toContain('sm:flex-row');

    // Both IP and time are font-mono spans inside that wrapper, with the
    // IP first (on top) and the time second (underneath).
    const monoSpans = wrapper!.querySelectorAll('span.font-mono');
    expect(monoSpans.length).toBe(2);
    expect(monoSpans[0]).toBe(ip);
    expect(monoSpans[1].textContent ?? '').toMatch(/\d{1,2}:\d{2}:\d{2}/);
  });

  it('hides the middot separator on mobile and restores it on sm+', () => {
    seedConnected('10.0.0.5:8899', 60);
    render(<ConnectionIndicator />);

    // The separator is a child of the time span. It must carry `hidden`
    // (collapsed on mobile where IP/time are stacked) together with
    // `sm:inline` so it reappears when they sit side-by-side.
    const ip = screen.getByText('10.0.0.5');
    const wrapper = ip.parentElement!;
    const timeSpan = wrapper.querySelectorAll('span.font-mono')[1];
    const separator = timeSpan.querySelector('span');
    expect(separator).not.toBeNull();
    expect(separator!.className).toContain('hidden');
    expect(separator!.className).toContain('sm:inline');
  });

  it('still renders just the connection state word when not connected', () => {
    useInverterStore.setState({
      connectionState: 'reconnecting',
      connectedHost: '192.168.1.42:8899',
      snapshot: null,
    });
    render(<ConnectionIndicator />);

    // Regression guard: the responsive wrapper only applies in the
    // connected branch. When reconnecting we still show the bare state.
    // (The `capitalize` class is a CSS text-transform that jsdom doesn't
    // apply, so the DOM text stays lowercase.)
    expect(screen.getByText('reconnecting')).toBeDefined();
    expect(screen.queryByText('192.168.1.42')).toBeNull();
  });
});
