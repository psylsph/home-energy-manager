import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent } from '@testing-library/react';

const mocks = vi.hoisted(() => ({ openExternal: vi.fn().mockResolvedValue(undefined) }));
vi.mock('../../src/lib/openExternal', () => ({ openExternal: mocks.openExternal }));

import DongleSecuritySection from '../../src/components/settings/DongleSecuritySection';

beforeEach(() => mocks.openExternal.mockClear());
afterEach(cleanup);

describe('<DongleSecuritySection/> (issue #397)', () => {
  it('lists the passive checks', () => {
    render(<DongleSecuritySection id="settings-dongle-security" />);
    expect(screen.getByRole('heading', { name: 'Dongle Security', level: 2 })).toBeTruthy();
    for (const item of [
      "Change the dongle's Wi-Fi password",
      "Change the dongle's web admin login",
      'Put the inverter on its own network',
      "Don't port-scan the network it's on",
    ]) {
      expect(screen.getByText(item)).toBeTruthy();
    }
  });

  it('runs nothing: no form controls, no requests', () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch');
    render(<DongleSecuritySection />);
    expect(screen.queryAllByRole('checkbox')).toHaveLength(0);
    expect(screen.queryAllByRole('textbox')).toHaveLength(0);
    expect(fetchSpy).not.toHaveBeenCalled();
    fetchSpy.mockRestore();
  });

  it('opens the Pen Test Partners write-up outside the app', () => {
    render(<DongleSecuritySection />);
    fireEvent.click(screen.getByRole('button', { name: /Pen Test Partners Write-Up/ }));
    expect(mocks.openExternal).toHaveBeenCalledWith(
      expect.stringContaining('pentestpartners.com/security-blog/givenergy-enters-administration'),
    );
  });

  it('keeps the details behind the info tip, outside the heading', () => {
    render(<DongleSecuritySection />);
    const heading = screen.getByRole('heading', { name: 'Dongle Security' });
    expect(heading.querySelector('button')).toBeNull();
    const tip = screen.getByRole('button', { name: /More about Dongle Security/i });
    const text = document.getElementById(tip.getAttribute('aria-describedby') ?? '')?.textContent ?? '';
    expect(text).toMatch(/never probes the dongle's other ports/);
  });
});
