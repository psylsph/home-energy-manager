import { describe, it, expect, vi, afterEach, beforeEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Settings → Inverter Connection → Second HEM Address (issue #456), mounted
// alone: saving, clearing, a rejected address, and the status line.
// ---------------------------------------------------------------------------

const apiPost = vi.fn();
vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn().mockResolvedValue({ ok: true, subnets: [], inverters: [] }),
  apiPost: (...args: unknown[]) => apiPost(...args),
}));

import ConnectionSection from '../../src/components/settings/ConnectionSection';
import SecondHemField from '../../src/components/settings/SecondHemField';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot, SecondUnitInfo } from '../../src/lib/types';

const flash = vi.fn();

const withSecondUnit = (second_unit: SecondUnitInfo | undefined) =>
  useInverterStore.setState({ snapshot: { second_unit } as InverterSnapshot });

describe('<SecondHemField/> (issue #456)', () => {
  beforeEach(() => {
    apiPost.mockReset().mockResolvedValue({ ok: true });
    flash.mockReset();
    useInverterStore.setState({ snapshot: null });
  });
  afterEach(cleanup);

  it('starts empty with Save disabled until something changes', () => {
    render(<SecondHemField initialUrl="" flash={flash} />);
    const input = screen.getByLabelText(/^Second HEM Address/) as HTMLInputElement;
    expect(input.value).toBe('');
    expect((screen.getByRole('button', { name: 'Save' }) as HTMLButtonElement).disabled).toBe(true);
  });

  it('saves the address trimmed, on its own, without the inverter fields', async () => {
    render(<SecondHemField initialUrl="" flash={flash} />);
    fireEvent.change(screen.getByLabelText(/^Second HEM Address/), {
      target: { value: ' http://192.168.1.50:7338 ' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(flash).toHaveBeenCalledWith('Second HEM saved', true));
    expect(apiPost).toHaveBeenCalledWith('/api/settings', {
      second_hem_url: 'http://192.168.1.50:7338',
    });
  });

  it('clears the address by saving it empty', async () => {
    render(<SecondHemField initialUrl="http://hem-ac:7338" flash={flash} />);
    fireEvent.change(screen.getByLabelText(/^Second HEM Address/), { target: { value: '' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(flash).toHaveBeenCalledWith('Second HEM removed', true));
    expect(apiPost).toHaveBeenCalledWith('/api/settings', { second_hem_url: '' });
  });

  it('shows the backend’s reason when it rejects the address', async () => {
    apiPost.mockRejectedValue(new Error('The second HEM address must start with http:// or https://'));
    render(<SecondHemField initialUrl="" flash={flash} />);
    fireEvent.change(screen.getByLabelText(/^Second HEM Address/), { target: { value: '192.168.1.50' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() =>
      expect(flash).toHaveBeenCalledWith(
        'The second HEM address must start with http:// or https://',
        false,
      ),
    );
    // Still unsaved, so the button stays available for a retry.
    expect((screen.getByRole('button', { name: 'Save' }) as HTMLButtonElement).disabled).toBe(false);
  });

  it('says what the house figures are doing once an address is saved', () => {
    withSecondUnit({
      state: 'stale',
      solar_power: 0,
      battery_power: 0,
      soc: 0,
      battery_capacity_kwh: 0,
      today_charge_kwh: 0,
      today_discharge_kwh: 0,
    });
    render(<SecondHemField initialUrl="http://hem-ac:7338" flash={flash} />);
    expect(screen.getByText(/went quiet/)).toBeDefined();
  });

  it('shows no status line when no address is saved', () => {
    withSecondUnit(undefined);
    render(<SecondHemField initialUrl="" flash={flash} />);
    expect(screen.queryByText(/second inverter|went quiet|hasn’t answered/)).toBeNull();
  });
});

// The field is a developer-mode feature for now: the Inverter Connection
// section only offers it once Developer Mode is on.
describe('<ConnectionSection/> and the second HEM address (developer mode)', () => {
  beforeEach(() => {
    useInverterStore.setState({ snapshot: null, developerMode: false });
  });
  afterEach(() => {
    useInverterStore.setState({ developerMode: false });
    cleanup();
  });

  const mount = () =>
    render(<ConnectionSection flash={flash} initial={{ second_hem_url: 'http://pc2:7338' }} />);

  it('hides the field while Developer Mode is off', () => {
    mount();
    expect(screen.queryByLabelText(/^Second HEM Address/)).toBeNull();
  });

  it('shows the field, with the saved address, once Developer Mode is on', () => {
    useInverterStore.setState({ developerMode: true });
    mount();
    expect((screen.getByLabelText(/^Second HEM Address/) as HTMLInputElement).value).toBe(
      'http://pc2:7338',
    );
  });
});
