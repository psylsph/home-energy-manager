import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn().mockResolvedValue({ ok: true, subnets: [], inverters: [] }),
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
}));

import ConnectionSection from '../../src/components/settings/ConnectionSection';

/** No saved inverter: host and serial blank, default port. */
const Section = () => <ConnectionSection flash={() => {}} initial={{}} />;
import { apiPost } from '../../src/lib/api';

function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

/** Returns the <input> rendered under the "Inverter Address" label. */
async function inverterAddressInput(): Promise<HTMLInputElement> {
  const label = await screen.findByText('Inverter Address');
  const input = label.closest('label')?.querySelector('input');
  if (!input) throw new Error('Inverter Address input not found');
  return input as HTMLInputElement;
}

/** Returns the Connect button in the Inverter Connection section. */
function connectButton(): HTMLButtonElement {
  const text = screen.getByText('Connect');
  const btn = text.closest('button');
  if (!btn) throw new Error('Connect button not found');
  return btn as HTMLButtonElement;
}

function validationHint(): HTMLElement | null {
  return screen.queryByText(/Must be four numbers/i);
}

describe('<ConnectionSection/> — Inverter Address validation (issue #153)', () => {
  beforeEach(() => {
    silenceConsoleError();
    localStorage.clear();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    localStorage.clear();
  });

  it('disables Connect while the field is empty and shows no error styling', async () => {
    render(<Section />);
    const input = await inverterAddressInput();

    expect(input.value).toBe('');
    expect(input.getAttribute('aria-invalid')).toBe('false');
    expect(connectButton().disabled).toBe(true);
    expect(validationHint()).toBeNull();
  });

  it('marks an invalid address and disables Connect', async () => {
    render(<Section />);
    const input = await inverterAddressInput();

    fireEvent.change(input, { target: { value: '999.1.1.1' } });

    await waitFor(() => {
      expect(input.getAttribute('aria-invalid')).toBe('true');
    });
    expect(validationHint()).not.toBeNull();
    expect(connectButton().disabled).toBe(true);

    // A disabled button must never persist the bad host.
    fireEvent.click(connectButton());
    expect(apiPost).not.toHaveBeenCalled();
  });

  it('rejects malformed entries like a missing octet or non-digits', async () => {
    render(<Section />);
    const input = await inverterAddressInput();

    for (const bad of ['10.1.71', '192.168.1.50.', 'abc', '01.2.3.4', '256.1.1.1']) {
      fireEvent.change(input, { target: { value: bad } });
      await waitFor(() => expect(input.getAttribute('aria-invalid')).toBe('true'));
      expect(connectButton().disabled).toBe(true);
      expect(validationHint()).not.toBeNull();
    }
  });

  it('accepts a valid dotted-quad, clears the error, and lets Connect save it', async () => {
    render(<Section />);
    const input = await inverterAddressInput();

    fireEvent.change(input, { target: { value: '192.168.1.50' } });

    await waitFor(() => {
      expect(input.getAttribute('aria-invalid')).toBe('false');
    });
    expect(validationHint()).toBeNull();
    expect(connectButton().disabled).toBe(false);

    fireEvent.click(connectButton());
    await waitFor(() => {
      expect(apiPost).toHaveBeenCalledWith('/api/settings', {
        host: '192.168.1.50',
        port: 8899,
        serial: '',
      });
    });
  });

  it('clears the validation state when a bad entry is corrected', async () => {
    render(<Section />);
    const input = await inverterAddressInput();

    fireEvent.change(input, { target: { value: 'not-an-ip' } });
    await waitFor(() => expect(input.getAttribute('aria-invalid')).toBe('true'));
    expect(validationHint()).not.toBeNull();

    fireEvent.change(input, { target: { value: '10.0.0.9' } });
    await waitFor(() => expect(input.getAttribute('aria-invalid')).toBe('false'));
    expect(validationHint()).toBeNull();
    expect(connectButton().disabled).toBe(false);
  });
});
