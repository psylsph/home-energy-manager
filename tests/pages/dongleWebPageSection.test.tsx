import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, act } from '@testing-library/react';

const mocks = vi.hoisted(() => ({
  apiGet: vi.fn(),
  openExternal: vi.fn().mockResolvedValue(undefined),
}));

vi.mock('../../src/lib/api', () => ({ apiGet: mocks.apiGet }));
vi.mock('../../src/lib/openExternal', () => ({ openExternal: mocks.openExternal }));

import DongleWebPageSection from '../../src/components/DongleWebPageSection';

const URL_OK = 'https://192.168.1.50/web/info';

beforeEach(() => {
  mocks.apiGet.mockReset();
  mocks.openExternal.mockClear();
});
afterEach(cleanup);

describe('<DongleWebPageSection/> (issue #452)', () => {
  it('opens the page the backend found', async () => {
    mocks.apiGet.mockResolvedValue({ ok: true, url: URL_OK });
    render(<DongleWebPageSection />);
    fireEvent.click(await screen.findByRole('button', { name: 'Open Dongle Page' }));
    expect(mocks.apiGet).toHaveBeenCalledWith('/api/dongle-page');
    expect(mocks.openExternal).toHaveBeenCalledWith(URL_OK);
  });

  it('keeps the hint to one line and the login behind the info tip, outside the heading', async () => {
    mocks.apiGet.mockResolvedValue({ ok: true, url: URL_OK });
    render(<DongleWebPageSection />);
    await screen.findByRole('button', { name: 'Open Dongle Page' });
    const heading = screen.getByRole('heading', { name: 'Dongle Web Page' });
    expect(heading.querySelector('button')).toBeNull();
    expect(screen.getByText(/the ⓘ has the login/)).toBeTruthy();
    const tip = screen.getByRole('button', { name: /More about the dongle login/i });
    const text = document.getElementById(tip.getAttribute('aria-describedby') ?? '')?.textContent ?? '';
    // Leads with the default, then the fallbacks.
    expect(text.indexOf('admin')).toBeLessThan(text.indexOf('123456'));
    expect(text).toMatch(/unless you've changed it/);
  });

  it('shows a physical-reboot line, not a dead link, when nothing answers, and can check again', async () => {
    mocks.apiGet.mockResolvedValueOnce({ ok: true, url: null });
    render(<DongleWebPageSection />);
    expect(await screen.findByText(/web page isn't answering/)).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Open Dongle Page' })).toBeNull();
    expect(screen.queryByRole('link')).toBeNull();

    mocks.apiGet.mockResolvedValueOnce({ ok: true, url: URL_OK });
    fireEvent.click(screen.getByRole('button', { name: 'Check Again' }));
    expect(await screen.findByRole('button', { name: 'Open Dongle Page' })).toBeTruthy();
    expect(mocks.apiGet).toHaveBeenCalledTimes(2);
  });

  it('shows it is checking again while Check Again is out, then settles', async () => {
    mocks.apiGet.mockResolvedValueOnce({ ok: true, url: null });
    render(<DongleWebPageSection />);
    await screen.findByText(/web page isn't answering/);

    let answer!: (v: unknown) => void;
    mocks.apiGet.mockReturnValueOnce(new Promise((resolve) => { answer = resolve; }));
    fireEvent.click(screen.getByRole('button', { name: 'Check Again' }));
    const busy = screen.getByRole('button', { name: 'Checking…' }) as HTMLButtonElement;
    expect(busy.disabled).toBe(true);

    await act(async () => { answer({ ok: true, url: null }); });
    expect((screen.getByRole('button', { name: 'Check Again' }) as HTMLButtonElement).disabled).toBe(false);
  });

  it('treats a failed request as no answer', async () => {
    mocks.apiGet.mockRejectedValue(new Error('backend down'));
    render(<DongleWebPageSection />);
    expect(await screen.findByText(/web page isn't answering/)).toBeTruthy();
  });

  it('says it is checking while the first request is out', () => {
    mocks.apiGet.mockReturnValue(new Promise(() => {}));
    render(<DongleWebPageSection />);
    expect(screen.getByText(/Checking for the dongle/)).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Open Dongle Page' })).toBeNull();
  });
});
