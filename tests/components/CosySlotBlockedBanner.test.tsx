import { describe, it, expect, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import { HashRouter } from 'react-router';
import CosySlotBlockedBanner from '../../src/components/CosySlotBlockedBanner';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot } from '../../src/lib/types';

function show(overrides: Partial<InverterSnapshot> | null) {
  useInverterStore.setState({
    snapshot: overrides === null ? null : ({ ...overrides } as InverterSnapshot),
  });
  return render(
    <HashRouter>
      <CosySlotBlockedBanner />
    </HashRouter>,
  );
}

afterEach(() => {
  cleanup();
  useInverterStore.setState({ snapshot: null });
});

describe('CosySlotBlockedBanner', () => {
  it('raises an alert when a Cosy slot is at risk and says why', () => {
    show({ cosy_enabled: true, cosy_slot_at_risk: true, cosy_blocked_by: 'pause' });
    const banner = screen.getByRole('alert');
    expect(banner).toHaveTextContent(/Cosy Charging Is Being Held Back/);
    expect(banner).toHaveTextContent(/battery pause/i);
  });

  it('explains each hard override', () => {
    const cases: Array<[string, RegExp]> = [
      ['pause', /battery pause/i],
      ['force', /Force Charge or Force Discharge/],
      ['safety', /safety limit/i],
    ];
    for (const [code, text] of cases) {
      cleanup();
      show({ cosy_enabled: true, cosy_slot_at_risk: true, cosy_blocked_by: code });
      expect(screen.getByRole('alert')).toHaveTextContent(text);
    }
  });

  it('links to the Control page where the cause can be cleared', () => {
    show({ cosy_enabled: true, cosy_slot_at_risk: true, cosy_blocked_by: 'force' });
    const link = screen.getByRole('link', { name: /Control/ });
    expect(link.getAttribute('href')).toBe('#/control');
  });

  it('stays hidden when no slot is at risk, even though Cosy is blocked', () => {
    // Between slots the on-page notice is enough; the app-wide banner is
    // reserved for a charge the user is relying on right now.
    show({ cosy_enabled: true, cosy_slot_at_risk: false, cosy_blocked_by: 'pause' });
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('stays hidden when Cosy is off, nothing blocks it, or there is no snapshot', () => {
    show({ cosy_enabled: false, cosy_slot_at_risk: true, cosy_blocked_by: 'pause' });
    expect(screen.queryByRole('alert')).toBeNull();
    cleanup();
    show({ cosy_enabled: true, cosy_slot_at_risk: true, cosy_blocked_by: '' });
    expect(screen.queryByRole('alert')).toBeNull();
    cleanup();
    show({ cosy_enabled: true, cosy_slot_at_risk: true });
    expect(screen.queryByRole('alert')).toBeNull();
    cleanup();
    show(null);
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('disappears as soon as the snapshot says the slot is no longer at risk', () => {
    const { rerender } = show({
      cosy_enabled: true,
      cosy_slot_at_risk: true,
      cosy_blocked_by: 'pause',
    });
    expect(screen.getByRole('alert')).toBeInTheDocument();
    useInverterStore.setState({
      snapshot: { cosy_enabled: true, cosy_slot_at_risk: false, cosy_blocked_by: '' } as InverterSnapshot,
    });
    rerender(
      <HashRouter>
        <CosySlotBlockedBanner />
      </HashRouter>,
    );
    expect(screen.queryByRole('alert')).toBeNull();
  });
});
