import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
}));

import EvChargerSection from '../../src/components/settings/EvChargerSection';
import { apiPost } from '../../src/lib/api';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { PollSettings } from '../../src/lib/types';

const flash = vi.fn();

const Section = ({ initial = {} }: { initial?: Partial<PollSettings> }) => (
  <EvChargerSection flash={flash} initialHost="" initialPort={502} initial={initial} />
);

const threshold = () => screen.getByLabelText('Car Charging Threshold') as HTMLInputElement;
const amps = () => screen.getByLabelText('Charger Current') as HTMLInputElement;
const save = () => screen.getByRole('button', { name: 'Save Car Charging' });

describe('<CarChargingSection/> — Settings → EV Charger → Car Charging Estimate (issue #454)', () => {
  beforeEach(() => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(apiPost).mockReset();
    vi.mocked(apiPost).mockResolvedValue({ ok: true, data: {} });
    flash.mockReset();
    useInverterStore.setState({ snapshot: null });
  });
  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  it('shows the saved threshold and charger current', () => {
    render(<Section initial={{ forecast_exclude_load_above_kw: 7.4, car_charger_current_a: 16 }} />);
    expect(screen.getByRole('heading', { name: 'EV Charger', level: 2 })).toBeTruthy();
    expect(screen.getByRole('heading', { name: 'Car Charging Estimate', level: 3 })).toBeTruthy();
    expect(threshold().value).toBe('7.4');
    expect(amps().value).toBe('16');
  });

  it('is off and 32 A when nothing is saved', () => {
    render(<Section />);
    expect(threshold().value).toBe('');
    expect(threshold().placeholder).toBe('Off');
    expect(amps().value).toBe('32');
  });

  it('shows the live grid voltage and the power it gives at the entered current, without saving', () => {
    useInverterStore.setState({ snapshot: { grid_voltage: 241.2 } as never });
    render(<Section initial={{ car_charger_current_a: 32 }} />);
    expect(screen.getByTestId('car-charging-live-voltage')).toHaveTextContent('241.2V');
    expect(screen.getByTestId('car-charging-live-power')).toHaveTextContent('7.7kW');
    expect(screen.getByTestId('car-charging-live-formula')).toHaveTextContent('32 A × 241.2V');
    // The display follows the field as it is edited, but never saves it.
    fireEvent.change(amps(), { target: { value: '16' } });
    expect(screen.getByTestId('car-charging-live-power')).toHaveTextContent('3.9kW');
    expect(apiPost).not.toHaveBeenCalled();
  });

  it('shows 230 V (default) when the inverter has no usable voltage', () => {
    render(<Section initial={{ car_charger_current_a: 10 }} />);
    expect(screen.getByTestId('car-charging-live-voltage')).toHaveTextContent('230.0V (default)');
    expect(screen.getByTestId('car-charging-live-power')).toHaveTextContent('2.3kW');
  });

  it('treats an out-of-range voltage as no reading', () => {
    useInverterStore.setState({ snapshot: { grid_voltage: 300 } as never });
    render(<Section initial={{ car_charger_current_a: 32 }} />);
    expect(screen.getByTestId('car-charging-live-voltage')).toHaveTextContent('230.0V (default)');
  });

  it('shows no live power while the current is invalid', () => {
    useInverterStore.setState({ snapshot: { grid_voltage: 240 } as never });
    render(<Section />);
    fireEvent.change(amps(), { target: { value: '16.5' } });
    expect(screen.getByTestId('car-charging-live-power')).toHaveTextContent('—');
    expect(screen.queryByTestId('car-charging-live-formula')).toBeNull();
    expect(apiPost).not.toHaveBeenCalled();
  });

  it('keeps the explanation behind an ⓘ outside the labels and heading', () => {
    render(<Section />);
    const tip = screen.getByRole('button', { name: 'More about Car Charging Estimate' });
    expect(tip.closest('label')).toBeNull();
    expect(tip.closest('h3')).toBeNull();
  });

  it('saves both settings together', async () => {
    render(<Section initial={{ forecast_exclude_load_above_kw: null }} />);
    fireEvent.change(threshold(), { target: { value: '7.4' } });
    fireEvent.change(amps(), { target: { value: '16' } });
    fireEvent.click(save());
    await waitFor(() =>
      expect(apiPost).toHaveBeenCalledWith('/api/settings', {
        forecast_exclude_load_above_kw: 7.4,
        car_charger_current_a: 16,
      }),
    );
    await waitFor(() => expect(flash).toHaveBeenCalledWith('Car charging saved', true));
  });

  it('updates the store so the Status diagrams follow without a reload', async () => {
    useInverterStore.setState({ carCharging: {} });
    render(<Section />);
    fireEvent.change(threshold(), { target: { value: '7.4' } });
    fireEvent.change(amps(), { target: { value: '16' } });
    fireEvent.click(save());
    await waitFor(() =>
      expect(useInverterStore.getState().carCharging).toEqual({
        forecast_exclude_load_above_kw: 7.4,
        car_charger_current_a: 16,
      }),
    );
  });

  it('leaves the store alone when the save fails', async () => {
    vi.mocked(apiPost).mockRejectedValue(new Error('nope'));
    useInverterStore.setState({ carCharging: { forecast_exclude_load_above_kw: 5 } });
    render(<Section initial={{ forecast_exclude_load_above_kw: 5 }} />);
    fireEvent.change(threshold(), { target: { value: '7.4' } });
    fireEvent.click(save());
    await waitFor(() => expect(flash).toHaveBeenCalledWith('nope', false));
    expect(useInverterStore.getState().carCharging).toEqual({ forecast_exclude_load_above_kw: 5 });
  });

  it('turns the feature off when the threshold is emptied', async () => {
    render(<Section initial={{ forecast_exclude_load_above_kw: 7.4 }} />);
    fireEvent.change(threshold(), { target: { value: '' } });
    fireEvent.click(save());
    await waitFor(() =>
      expect(apiPost).toHaveBeenCalledWith('/api/settings', {
        forecast_exclude_load_above_kw: null,
        car_charger_current_a: 32,
      }),
    );
  });

  it.each(['0.5', '26', '7,4', 'abc'])('refuses a threshold of %s without saving', async (value) => {
    render(<Section initial={{ forecast_exclude_load_above_kw: 7.4 }} />);
    fireEvent.change(threshold(), { target: { value } });
    fireEvent.click(save());
    expect(await screen.findByTestId('car-charging-threshold-error')).toHaveTextContent(
      /between 1 and 25 kW/,
    );
    expect(apiPost).not.toHaveBeenCalled();
  });

  it.each(['5', '81', '16.5', '', 'abc'])('refuses a charger current of %s without saving', async (value) => {
    render(<Section initial={{ forecast_exclude_load_above_kw: 7.4 }} />);
    fireEvent.change(amps(), { target: { value } });
    fireEvent.click(save());
    expect(await screen.findByTestId('car-charging-current-error')).toHaveTextContent(
      /between 6 and 80 A/,
    );
    expect(apiPost).not.toHaveBeenCalled();
  });

  it('clears an error once the value is fixed and saved', async () => {
    render(<Section initial={{ forecast_exclude_load_above_kw: 7.4 }} />);
    fireEvent.change(threshold(), { target: { value: '99' } });
    fireEvent.click(save());
    await screen.findByTestId('car-charging-threshold-error');
    fireEvent.change(threshold(), { target: { value: '8' } });
    fireEvent.click(save());
    await waitFor(() => expect(apiPost).toHaveBeenCalledTimes(1));
    expect(screen.queryByTestId('car-charging-threshold-error')).toBeNull();
  });

  it('says why a save failed', async () => {
    vi.mocked(apiPost).mockRejectedValue(new Error('Charger current must be between 6 and 80 A'));
    render(<Section initial={{ forecast_exclude_load_above_kw: 7.4 }} />);
    fireEvent.click(save());
    await waitFor(() =>
      expect(flash).toHaveBeenCalledWith('Charger current must be between 6 and 80 A', false),
    );
  });
});
