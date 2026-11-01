import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { renderHook } from '@testing-library/react';
import { useEnergyFlowViewModel } from '../../src/hooks/useEnergyFlowViewModel';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot } from '../../src/lib/types';

/**
 * The orbit and the house view both build their view model through this hook
 * (issue #422), so the store-driven options (noise threshold, grid CT) and the
 * EV charger label logic can't differ between them.
 */

function snap(over: Partial<InverterSnapshot> = {}): InverterSnapshot {
  return {
    timestamp: 0,
    solar_power: 0, pv1_power: 0, pv2_power: 0,
    pv1_voltage: 0, pv2_voltage: 0, pv1_current: 0, pv2_current: 0,
    battery_power: 0, soc: 50, battery_voltage: 50, battery_current: 0,
    battery_state: 'idle', battery_temperature: 20, battery_capacity_kwh: 9.5,
    eps_power_w: 0, grid_power: 0, grid_voltage: 230, grid_frequency: 50,
    grid_online: true, grid_loss: false, inverter_trip: false,
    battery_over_temp: false, home_power: 0, inverter_temperature: 30,
    inverter_time: '',
    today_solar_kwh: 0, today_pv1_kwh: 0, today_pv2_kwh: 0,
    today_import_kwh: 0, today_export_kwh: 0, today_charge_kwh: 0,
    total_import_kwh: 0, total_export_kwh: 0, total_solar_kwh: 0,
    total_charge_kwh: 0, total_discharge_kwh: 0, total_throughput_kwh: 0,
    ...over,
  } as InverterSnapshot;
}

describe('useEnergyFlowViewModel', () => {
  let savedNoise: number;

  beforeEach(() => {
    savedNoise = useInverterStore.getState().visualNoiseThreshold;
  });

  afterEach(() => {
    useInverterStore.setState({ visualNoiseThreshold: savedNoise });
  });

  it('applies the noise threshold from the store', () => {
    useInverterStore.setState({ visualNoiseThreshold: 100 });
    const s = snap({ solar_power: 80, pv1_power: 80, home_power: 80 });
    const { result } = renderHook(() => useEnergyFlowViewModel({ snapshot: s }));
    expect(result.current.flows).toEqual([]);
    expect(result.current.nodes.find((n) => n.id === 'solar')!.status).toBe('Idle');
  });

  it('shows flows above a lower store threshold', () => {
    useInverterStore.setState({ visualNoiseThreshold: 20 });
    const s = snap({ solar_power: 80, pv1_power: 80, home_power: 80 });
    const { result } = renderHook(() => useEnergyFlowViewModel({ snapshot: s }));
    expect(result.current.flows.map((f) => f.id)).toContain('solar');
  });

  it('omits the EV node when no charger is configured', () => {
    const { result } = renderHook(() => useEnergyFlowViewModel({ snapshot: snap(), evcPower: 7000 }));
    expect(result.current.nodes.find((n) => n.id === 'ev')).toBeUndefined();
  });

  it('shows the cable state only while the charger is reachable', () => {
    const online = renderHook(() => useEnergyFlowViewModel({
      snapshot: snap(), showEvc: true, evcConnected: true, evcCableConnected: true, evcEverConnected: true,
    }));
    expect(online.result.current.nodes.find((n) => n.id === 'ev')!.subLabel).toBe('Cable In');

    const offline = renderHook(() => useEnergyFlowViewModel({
      snapshot: snap(), showEvc: true, evcConnected: false, evcCableConnected: true, evcEverConnected: true,
    }));
    const ev = offline.result.current.nodes.find((n) => n.id === 'ev')!;
    expect(ev.subLabel).toBeUndefined();
    expect(ev.unit).toBe('Disconnected');
  });

  it('labels a charger that was never reached as Not Found', () => {
    const { result } = renderHook(() => useEnergyFlowViewModel({ snapshot: snap(), showEvc: true }));
    expect(result.current.nodes.find((n) => n.id === 'ev')!.unit).toBe('Not Found');
  });
});
