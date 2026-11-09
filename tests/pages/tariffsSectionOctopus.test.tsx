import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, within } from '@testing-library/react';
import type { TariffPreview } from '../../src/lib/tariffSource';

const apiGet = vi.fn();
const apiPost = vi.fn();
vi.mock('../../src/lib/api', () => ({
  apiGet: (...args: unknown[]) => apiGet(...args),
  apiPost: (...args: unknown[]) => apiPost(...args),
}));

import TariffsSection from '../../src/components/settings/TariffsSection';

const GO = [
  { start: '00:00', end: '00:30', rate: 0.245 },
  { start: '00:30', end: '05:30', rate: 0.075 },
  { start: '05:30', end: '23:59', rate: 0.245 },
];

function preview(over: Partial<TariffPreview> = {}): TariffPreview {
  return {
    source: 'manual',
    applied: { import: null, export: null, gas: null, at: null },
    gas_enabled: true,
    import: { ok: true, tariff_code: 'E-1R-GO', slots: GO, standing_charge_p_per_day: 54.86 },
    export: { ok: false, reason: 'The rates change 48 times a day, too often for fixed windows (as on Agile)' },
    gas: { ok: true, tariff_code: 'G-1R-X', unit_rate_p_per_kwh: 6.1, standing_charge_p_per_day: 29.4, unit_rate_used: true },
    ...over,
  };
}

/** What the server returns after switching to Octopus with `preview()`'s rates. */
function followed(): TariffPreview {
  return preview({
    source: 'octopus',
    applied: { import: 'E-1R-GO', export: null, gas: 'G-1R-X', at: 1_800_000_000 },
  });
}

function serve(p: TariffPreview | unknown = preview()) {
  apiGet.mockImplementation(async (path: string) => {
    if (path === '/api/history/reprice') {
      return { ok: true, data: { running: false, kind: null, done: 0, total: 0, last: null } };
    }
    if (path !== '/api/octopus/tariff-preview') throw new Error(`unexpected GET ${path}`);
    return { ok: true, data: p };
  });
}

const flash = vi.fn();
const octopus = { octopus_api_key_configured: true } as const;

function renderSection(initial: Record<string, unknown> = {}) {
  return render(<TariffsSection flash={flash} initial={{ ...octopus, ...initial }} />);
}

const importEditor = () => screen.getByRole('heading', { name: 'Import' }).closest('div')!.parentElement!;
const rateInputs = (root: HTMLElement) =>
  within(root).getAllByRole('spinbutton') as HTMLInputElement[];

describe('<TariffsSection/> — following Octopus (issue #455)', () => {
  beforeEach(() => {
    apiGet.mockReset();
    apiPost.mockReset();
    flash.mockReset();
    serve();
    apiPost.mockResolvedValue({ ok: true, data: followed() });
  });
  afterEach(() => cleanup());

  it('shows no source control without an Octopus account', () => {
    render(<TariffsSection flash={flash} initial={{}} />);
    expect(screen.queryByRole('group', { name: 'Tariff Source' })).toBeNull();
    expect(apiGet).not.toHaveBeenCalledWith('/api/octopus/tariff-preview');
  });

  it('offers the Octopus rates when some can be followed', async () => {
    renderSection();
    expect(await screen.findByText('Octopus rates are available for Import and Gas.')).toBeTruthy();
    const manual = screen.getByRole('button', { name: 'Manual' });
    expect(manual.getAttribute('aria-pressed')).toBe('true');
  });

  it('does not offer them when none can be followed', async () => {
    serve(
      preview({
        import: { ok: false, reason: 'x' },
        export: { ok: false, reason: 'y' },
        gas: { ok: false, reason: 'z' },
      }),
    );
    renderSection();
    await waitFor(() => expect(apiGet).toHaveBeenCalled());
    expect(screen.queryByRole('button', { name: 'Use My Octopus Rates' })).toBeNull();
  });

  it('ignores a reply that is not a preview', async () => {
    serve({});
    renderSection();
    await waitFor(() => expect(apiGet).toHaveBeenCalled());
    expect(screen.queryByRole('button', { name: 'Use My Octopus Rates' })).toBeNull();
  });

  it('switches straight away when nothing was typed by hand', async () => {
    renderSection();
    fireEvent.click(await screen.findByRole('button', { name: 'Use My Octopus Rates' }));
    await waitFor(() =>
      expect(apiPost).toHaveBeenCalledWith('/api/octopus/tariff-source', { source: 'octopus' }),
    );
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('asks first when it would replace typed rates, and Cancel changes nothing', async () => {
    renderSection({ import_tariff_config: { slots: [{ start: '00:00', end: '23:59', rate: 0.31 }] } });
    fireEvent.click(await screen.findByRole('button', { name: 'Use My Octopus Rates' }));
    const alert = screen.getByRole('alert');
    expect(alert.textContent).toMatch(/replaces your Import rate/);
    expect(apiPost).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('alert')).toBeNull();
    expect(apiPost).not.toHaveBeenCalled();
  });

  it('switches after the confirmation and fills in what Octopus charges', async () => {
    renderSection({
      import_tariff_config: { slots: [{ start: '00:00', end: '23:59', rate: 0.31 }] },
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Use My Octopus Rates' }));
    fireEvent.click(screen.getByRole('button', { name: 'Use Octopus Rates' }));
    await waitFor(() =>
      expect(apiPost).toHaveBeenCalledWith('/api/octopus/tariff-source', { source: 'octopus' }),
    );
    // Three Go windows now, read-only, and the standing charge filled in.
    await waitFor(() => expect(rateInputs(importEditor())).toHaveLength(3));
    expect(rateInputs(importEditor()).every((i) => i.disabled)).toBe(true);
    const standing = screen.getByLabelText('Import Standing Charge in Pence per Day') as HTMLInputElement;
    expect(standing.value).toBe('54.86');
    expect(standing.disabled).toBe(true);
    expect(flash).toHaveBeenCalledWith('Energy Tariffs now follow Octopus', true);
  });

  it('says which tariff each part follows and why another stays manual', async () => {
    renderSection();
    fireEvent.click(await screen.findByRole('button', { name: 'Use My Octopus Rates' }));
    expect(await screen.findByText('Import follows Octopus tariff E-1R-GO')).toBeTruthy();
    expect(screen.getByText(/^Export stays manual: The rates change 48 times/)).toBeTruthy();
    expect(screen.getByText('Gas follows Octopus tariff G-1R-X')).toBeTruthy();
    expect(screen.getByText(/^Last updated /)).toBeTruthy();
  });

  it('keeps the part Octopus could not fill editable, and saves only that part', async () => {
    renderSection();
    fireEvent.click(await screen.findByRole('button', { name: 'Use My Octopus Rates' }));
    await screen.findByText('Import follows Octopus tariff E-1R-GO');
    apiPost.mockClear();
    apiPost.mockResolvedValue({ ok: true });
    const exportEditor = screen.getByRole('heading', { name: 'Export' }).closest('div')!.parentElement!;
    const [rate] = rateInputs(exportEditor);
    expect(rate!.disabled).toBe(false);
    fireEvent.change(rate!, { target: { value: '16' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save Tariffs' }));
    await waitFor(() => expect(apiPost).toHaveBeenCalledTimes(1));
    const [path, body] = apiPost.mock.calls[0] as [string, Record<string, unknown>];
    expect(path).toBe('/api/settings');
    expect(Object.keys(body)).toEqual(['export_tariff_config']);
  });

  it('has nothing to save when every part follows Octopus', async () => {
    serve(followed());
    renderSection({
      tariff_source: 'octopus',
      octopus_tariff_applied: { import: 'E-1R-GO', export: 'E-1R-OUT', gas: 'G-1R-X', at: 1 },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Save Tariffs' }));
    await waitFor(() =>
      expect(flash).toHaveBeenCalledWith('Every rate follows Octopus, so there is nothing to save', true),
    );
    expect(apiPost).not.toHaveBeenCalled();
  });

  it('starts locked when Energy Tariffs already follow Octopus', async () => {
    serve(followed());
    renderSection({
      tariff_source: 'octopus',
      octopus_tariff_applied: { import: 'E-1R-GO', export: null, gas: null, at: 1 },
      import_tariff_config: { slots: GO },
    });
    expect(screen.getByRole('button', { name: 'From Octopus' }).getAttribute('aria-pressed')).toBe('true');
    expect(rateInputs(importEditor()).every((i) => i.disabled)).toBe(true);
  });

  it('switching back to Manual unlocks the editors and keeps the values', async () => {
    serve(followed());
    apiPost.mockResolvedValue({
      ok: true,
      data: preview({ source: 'manual' }),
    });
    renderSection({
      tariff_source: 'octopus',
      octopus_tariff_applied: { import: 'E-1R-GO', export: null, gas: null, at: 1 },
      import_tariff_config: { slots: GO },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Manual' }));
    await waitFor(() =>
      expect(apiPost).toHaveBeenCalledWith('/api/octopus/tariff-source', { source: 'manual' }),
    );
    await waitFor(() => expect(rateInputs(importEditor()).every((i) => !i.disabled)).toBe(true));
    expect(rateInputs(importEditor())).toHaveLength(3);
    expect(flash).toHaveBeenCalledWith('Energy Tariffs are manual again', true);
  });

  it('shows the server reason when the switch is refused and stays manual', async () => {
    apiPost.mockRejectedValue(new Error('Nothing to follow from Octopus yet: no rates'));
    renderSection();
    fireEvent.click(await screen.findByRole('button', { name: 'Use My Octopus Rates' }));
    expect((await screen.findByText(/Nothing to follow from Octopus yet/)).textContent).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Manual' }).getAttribute('aria-pressed')).toBe('true');
  });

  it('ignores a switch reply that is not a preview', async () => {
    apiPost.mockResolvedValue({ ok: true, data: {} });
    renderSection();
    fireEvent.click(await screen.findByRole('button', { name: 'Use My Octopus Rates' }));
    expect(await screen.findByText('Unexpected reply from the server')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Manual' }).getAttribute('aria-pressed')).toBe('true');
  });

  // ---- gas ----

  it('saves gas rates only when they were edited', async () => {
    apiPost.mockResolvedValue({ ok: true });
    render(<TariffsSection flash={flash} initial={{}} />);
    fireEvent.click(screen.getByRole('button', { name: 'Save Tariffs' }));
    await waitFor(() => expect(apiPost).toHaveBeenCalledTimes(1));
    expect(Object.keys(apiPost.mock.calls[0]![1] as object)).not.toContain('gas_tariff_p_per_kwh');

    apiPost.mockClear();
    fireEvent.change(screen.getByLabelText('Gas Unit Rate (p/kWh)'), { target: { value: '6.1' } });
    fireEvent.change(screen.getByLabelText('Gas Standing Charge (p/day)'), { target: { value: '29.4' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save Tariffs' }));
    await waitFor(() => expect(apiPost).toHaveBeenCalledTimes(1));
    expect(apiPost.mock.calls[0]![1]).toMatchObject({
      gas_tariff_p_per_kwh: 6.1,
      gas_standing_charge_p_per_day: 29.4,
    });
  });

  it('saves a gas rate that is cleared again after an earlier save', async () => {
    // The baseline for "was it edited" is what was last saved, not what the
    // page loaded with: type 6.1, save, clear it, save must send the clear.
    apiPost.mockResolvedValue({ ok: true });
    render(<TariffsSection flash={flash} initial={{}} />);
    fireEvent.change(screen.getByLabelText('Gas Unit Rate (p/kWh)'), { target: { value: '6.1' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save Tariffs' }));
    await waitFor(() => expect(apiPost).toHaveBeenCalledTimes(1));
    expect(apiPost.mock.calls[0]![1]).toMatchObject({ gas_tariff_p_per_kwh: 6.1 });

    apiPost.mockClear();
    fireEvent.change(screen.getByLabelText('Gas Unit Rate (p/kWh)'), { target: { value: '' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save Tariffs' }));
    await waitFor(() => expect(apiPost).toHaveBeenCalledTimes(1));
    expect(apiPost.mock.calls[0]![1]).toMatchObject({ gas_tariff_p_per_kwh: 0 });
  });

  it('keeps sending a gas rate that failed to save', async () => {
    apiPost.mockRejectedValueOnce(new Error('locked'));
    render(<TariffsSection flash={flash} initial={{}} />);
    fireEvent.change(screen.getByLabelText('Gas Unit Rate (p/kWh)'), { target: { value: '6.1' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save Tariffs' }));
    await waitFor(() => expect(flash).toHaveBeenCalledWith('locked', false));
    apiPost.mockClear();
    apiPost.mockResolvedValue({ ok: true });
    fireEvent.click(screen.getByRole('button', { name: 'Save Tariffs' }));
    await waitFor(() => expect(apiPost).toHaveBeenCalledTimes(1));
    expect(apiPost.mock.calls[0]![1]).toMatchObject({ gas_tariff_p_per_kwh: 6.1 });
  });

  it('leaves the gas unit rate editable when Octopus could only fill the standing charge', async () => {
    // Gas in cubic metres: Octopus fills the standing charge, never the rate.
    serve(
      preview({
        source: 'octopus',
        applied: { import: null, export: null, gas: 'G-1R-X', at: 1 },
        gas: { ok: true, tariff_code: 'G-1R-X', unit_rate_p_per_kwh: 6.1, standing_charge_p_per_day: 29.4, unit_rate_used: false },
      }),
    );
    apiPost.mockResolvedValue({ ok: true });
    renderSection({
      tariff_source: 'octopus',
      octopus_tariff_applied: { import: null, export: null, gas: 'G-1R-X', at: 1 },
      octopus_gas_enabled: true,
      octopus_gas_unit: 'm3',
      gas_standing_charge_p_per_day: 29.4,
    });
    const rate = screen.getByLabelText('Gas Unit Rate (p/kWh)') as HTMLInputElement;
    const standing = screen.getByLabelText('Gas Standing Charge (p/day)') as HTMLInputElement;
    expect(standing.disabled).toBe(true);
    expect(rate.disabled).toBe(false);
    fireEvent.change(rate, { target: { value: '7' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save Tariffs' }));
    await waitFor(() => expect(apiPost).toHaveBeenCalled());
    const body = apiPost.mock.calls.at(-1)![1] as Record<string, unknown>;
    expect(body.gas_tariff_p_per_kwh).toBe(7);
    expect(body).not.toHaveProperty('gas_standing_charge_p_per_day');
  });

  it('re-reads what follows Octopus when a save is refused because a sync locked a part', async () => {
    // The page loaded with Export editable; a later Octopus sync started
    // following it. The save is refused, and the page must then show Export as
    // followed (read-only, with Octopus's windows) rather than stay editable.
    let call = 0;
    apiGet.mockImplementation(async (path: string) => {
      if (path === '/api/history/reprice') {
        return { ok: true, data: { running: false, kind: null, done: 0, total: 0, last: null } };
      }
      call += 1;
      return {
        ok: true,
        data:
          call === 1
            ? preview()
            : preview({
                source: 'octopus',
                applied: { import: null, export: 'E-1R-OUT', gas: null, at: 1 },
                export: {
                  ok: true,
                  tariff_code: 'E-1R-OUT',
                  slots: [{ start: '00:00', end: '23:59', rate: 0.15 }],
                  standing_charge_p_per_day: null,
                },
              }),
      };
    });
    apiPost.mockRejectedValue(new Error('The export tariff follows Octopus. Switch Energy Tariffs to Manual to edit it.'));
    renderSection();
    await screen.findByText('Octopus rates are available for Import and Gas.');
    const exportEditor = screen.getByRole('heading', { name: 'Export' }).closest('div')!.parentElement!;
    expect(rateInputs(exportEditor)[0]!.disabled).toBe(false);

    fireEvent.click(screen.getByRole('button', { name: 'Save Tariffs' }));
    await waitFor(() =>
      expect(flash).toHaveBeenCalledWith(
        'The export tariff follows Octopus. Switch Energy Tariffs to Manual to edit it.',
        false,
      ),
    );
    await waitFor(() => expect(rateInputs(exportEditor)[0]!.disabled).toBe(true));
    expect(screen.getByRole('button', { name: 'From Octopus' }).getAttribute('aria-pressed')).toBe('true');
  });

  it('shows saved gas rates and leaves blank when there are none', () => {
    const { unmount } = render(<TariffsSection flash={flash} initial={{}} />);
    expect((screen.getByLabelText('Gas Unit Rate (p/kWh)') as HTMLInputElement).value).toBe('');
    unmount();
    render(
      <TariffsSection
        flash={flash}
        initial={{ gas_tariff_p_per_kwh: 6.1, gas_standing_charge_p_per_day: 29.4 }}
      />,
    );
    expect((screen.getByLabelText('Gas Unit Rate (p/kWh)') as HTMLInputElement).value).toBe('6.1');
    expect((screen.getByLabelText('Gas Standing Charge (p/day)') as HTMLInputElement).value).toBe('29.4');
  });
});
