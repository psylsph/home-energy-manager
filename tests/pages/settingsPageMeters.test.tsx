import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
}));

import MetersSection from '../../src/components/settings/MetersSection';
import { apiPost } from '../../src/lib/api';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot, PollSettings } from '../../src/lib/types';

function snapshotWithMeters(addresses: number[]): InverterSnapshot {
  return { meters: addresses.map((address) => ({ address })) } as unknown as InverterSnapshot;
}

const Section = ({
  initial = {},
  flash = () => {},
}: {
  initial?: Partial<PollSettings>;
  flash?: (text: string, ok: boolean) => void;
}) => <MetersSection flash={flash} initial={initial} />;

describe('<MetersSection/> — Meter Names (issue #473)', () => {
  beforeEach(() => {
    vi.mocked(apiPost).mockClear();
    vi.mocked(apiPost).mockResolvedValue({ ok: true, data: {} });
    useInverterStore.setState({
      snapshot: null,
      meterLabels: { names: {}, solarArrays: [] },
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  it('says there are no meters, with nothing to save, when none are on the bus or named', () => {
    // It still renders, so the Settings nav's Meters link lands somewhere.
    useInverterStore.setState({ snapshot: snapshotWithMeters([0]) });
    render(<Section />);
    expect(screen.getByRole('heading', { name: 'Meters' })).toBeTruthy();
    expect(screen.getByTestId('meter-names-empty').textContent).toBe('No external CT meters detected.');
    expect(screen.queryByRole('textbox')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Save Meter Names' })).toBeNull();
  });

  it('lists each external meter, plus an offline one that has a name', () => {
    useInverterStore.setState({ snapshot: snapshotWithMeters([0, 2, 1]) });
    render(<Section initial={{ meter_names: { '5': 'Garage' } }} />);
    expect(screen.getByRole('heading', { name: 'Meters' })).toBeTruthy();
    const fields = screen.getAllByRole('textbox') as HTMLInputElement[];
    // The built-in grid CT keeps its own title, so it isn't listed.
    expect(fields.map((f) => f.labels?.[0]?.textContent)).toEqual([
      'Meter 0x01',
      'Meter 0x02',
      'Meter 0x05',
    ]);
    expect(fields.map((f) => f.value)).toEqual(['', '', 'Garage']);
    expect(fields[0].maxLength).toBe(32);
  });

  it("suggests the Solar Arrays name as the placeholder, since that's the fallback", () => {
    useInverterStore.setState({
      snapshot: snapshotWithMeters([1, 2]),
      meterLabels: { names: {}, solarArrays: [{ meter_address: 2, name: 'Solar Small Roof', rated_kw: 2 }] },
    });
    render(<Section />);
    const fields = screen.getAllByRole('textbox') as HTMLInputElement[];
    expect(fields[1].placeholder).toBe('Solar Small Roof');
  });

  it("drops a deleted array's name at once, not on the next reload", () => {
    // The page loaded with the array; Solar Arrays then saved none.
    useInverterStore.setState({
      snapshot: snapshotWithMeters([2]),
      meterLabels: { names: {}, solarArrays: [] },
    });
    render(<Section initial={{ solar_arrays: [{ meter_address: 2, name: 'East roof', rated_kw: 2 }] }} />);
    expect((screen.getByRole('textbox') as HTMLInputElement).placeholder).toBe('e.g. Mains In');
  });

  it('saves trimmed names, drops blanks, and shares them with the Meters page', async () => {
    useInverterStore.setState({ snapshot: snapshotWithMeters([1, 2, 3]) });
    const flash = vi.fn();
    render(<Section initial={{ meter_names: { '3': 'Old Name' } }} flash={flash} />);
    const fields = screen.getAllByRole('textbox');
    fireEvent.change(fields[0], { target: { value: '  Mains In ' } });
    fireEvent.change(fields[2], { target: { value: '   ' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save Meter Names' }));
    await waitFor(() => {
      expect(apiPost).toHaveBeenCalledWith('/api/settings', { meter_names: { '1': 'Mains In' } });
    });
    await waitFor(() => {
      expect(useInverterStore.getState().meterLabels.names).toEqual({ '1': 'Mains In' });
    });
    expect(flash).toHaveBeenCalledWith('Meter names saved', true);
    // A cleared name's meter is still listed while it's on the bus.
    expect(screen.getAllByRole('textbox')).toHaveLength(3);
  });

  it('keeps the old names everywhere when the save fails', async () => {
    useInverterStore.setState({
      snapshot: snapshotWithMeters([1]),
      meterLabels: { names: { '1': 'Mains In' }, solarArrays: [] },
    });
    vi.mocked(apiPost).mockRejectedValueOnce(new Error('Meter 1\'s name is longer than 32 characters'));
    const flash = vi.fn();
    render(<Section initial={{ meter_names: { '1': 'Mains In' } }} flash={flash} />);
    fireEvent.change(screen.getByRole('textbox'), { target: { value: 'Grid' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save Meter Names' }));
    await waitFor(() => expect(flash).toHaveBeenCalled());
    expect(flash.mock.calls[0][1]).toBe(false);
    expect(useInverterStore.getState().meterLabels.names).toEqual({ '1': 'Mains In' });
  });
});
