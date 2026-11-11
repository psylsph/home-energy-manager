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

  describe('estimated car charging (issue #454, no charger polled)', () => {
    const ev = (r: { current: ReturnType<typeof useEnergyFlowViewModel> }) =>
      r.current.nodes.find((n) => n.id === 'ev');

    it('draws the post and the flow from the estimate, without inventing charger details', () => {
      const s = snap({ home_power: 9000, grid_power: -9000 });
      const { result } = renderHook(() =>
        useEnergyFlowViewModel({ snapshot: s, evcPower: 7360, showEvc: true, evcEstimated: true }),
      );
      const node = ev(result)!;
      expect(node.unit).toBe('Charging');
      expect(node.active).toBe(true);
      // No cable state or session energy: there is no charger to ask.
      expect(node.subLabel).toBeUndefined();
      expect(node.value).not.toMatch(/\(/);
      expect(result.current.flows.map((f) => f.id)).toContain('ev');
    });

    it('shows the post idle while the load is under the threshold', () => {
      const { result } = renderHook(() =>
        useEnergyFlowViewModel({ snapshot: snap({ home_power: 800 }), evcPower: 0, showEvc: true, evcEstimated: true }),
      );
      const node = ev(result)!;
      expect(node.unit).toBe('Idle');
      expect(node.active).toBe(false);
      expect(result.current.flows.map((f) => f.id)).not.toContain('ev');
    });

    it('does not claim a missing cable the way an unreachable charger does', () => {
      const { result } = renderHook(() =>
        useEnergyFlowViewModel({
          snapshot: snap({ home_power: 9000 }), evcPower: 7360, showEvc: true, evcEstimated: true,
          evcConnected: true, evcCableConnected: false,
        }),
      );
      expect(ev(result)!.subLabel).toBeUndefined();
    });
  });

  describe('Home without the car (issue #457, store toggle)', () => {
    let saved: boolean;

    beforeEach(() => {
      saved = useInverterStore.getState().homeExcludesCar;
    });

    afterEach(() => {
      useInverterStore.setState({ homeExcludesCar: saved });
    });

    it('keeps the gross Home reading while the toggle is off', () => {
      const s = snap({ home_power: 9000, grid_power: -9000 });
      const { result } = renderHook(() =>
        useEnergyFlowViewModel({ snapshot: s, evcPower: 7360, showEvc: true }),
      );
      expect(result.current.nodes.find((n) => n.id === 'home')!.value).toBe('9.0kW');
    });

    it('drops the charger\'s power from Home when the toggle is on', () => {
      useInverterStore.setState({ homeExcludesCar: true });
      const s = snap({ home_power: 9000, grid_power: -9000 });
      const { result } = renderHook(() =>
        useEnergyFlowViewModel({ snapshot: s, evcPower: 7360, showEvc: true }),
      );
      expect(result.current.nodes.find((n) => n.id === 'home')!.value).toBe('1.6kW');
      // The car's node still shows the draw.
      expect(result.current.nodes.find((n) => n.id === 'ev')!.value).toBe('7.4kW');
    });

    it('drops the threshold estimate too — the estimate arrives as evcPower either way', () => {
      // StatusPage passes the estimate as `evcPower` with `evcEstimated`
      // (issue #454); the exclusion reads the same input, so a thresholded
      // house with no charger polled gets the same Home (Excl. Car) split.
      useInverterStore.setState({ homeExcludesCar: true });
      const s = snap({ home_power: 9000, grid_power: -9000 });
      const { result } = renderHook(() =>
        useEnergyFlowViewModel({ snapshot: s, evcPower: 7680, showEvc: true, evcEstimated: true }),
      );
      expect(result.current.nodes.find((n) => n.id === 'home')!.value).toBe('1.3kW');
      expect(result.current.nodes.find((n) => n.id === 'ev')!.value).toBe('7.7kW');
    });
  });

  describe('EV charger label follows the power, not the state register (issue #458)', () => {
    // A charger whose state register says Charging, as Brian's bridge does.
    const charger = {
      showEvc: true, evcConnected: true, evcEverConnected: true,
      evcChargingState: 'Charging',
    };
    const ev = (over: Record<string, unknown>) => {
      const { result } = renderHook(() => useEnergyFlowViewModel({ snapshot: snap(), ...charger, ...over }));
      return result.current.nodes.find((n) => n.id === 'ev')!;
    };

    it('does not say Charging at 0 W with the cable in', () => {
      const node = ev({ evcPower: 0, evcCableConnected: true });
      expect(node.unit).toBe('Connected');
      expect(node.subLabel).toBe('Cable In');
      expect(node.active).toBe(false);
    });

    it('does not say Charging at 0 W with no cable (Brian\'s bridge)', () => {
      const node = ev({ evcPower: 0, evcCableConnected: false });
      expect(node.unit).toBe('Connected');
      expect(node.subLabel).toBe('No Cable');
    });

    it('does not say Charging for power below the noise threshold, matching the flow line', () => {
      useInverterStore.setState({ visualNoiseThreshold: 50 });
      const node = ev({ evcPower: 30, evcCableConnected: true });
      expect(node.unit).toBe('Connected');
      expect(node.active).toBe(false);
    });

    it('says Charging once the power is above the threshold, whatever the state says', () => {
      useInverterStore.setState({ visualNoiseThreshold: 50 });
      const node = ev({ evcPower: 7000, evcChargingState: 'Connected', evcCableConnected: true });
      expect(node.unit).toBe('Charging');
      expect(node.active).toBe(true);
    });

    it('still says Idle at 0 W when the charger reports Idle', () => {
      expect(ev({ evcPower: 0, evcChargingState: 'Idle' }).unit).toBe('Idle');
    });

    it('says Disconnected at 0 W once the charger has dropped off the network', () => {
      expect(ev({ evcPower: 0, evcConnected: false }).unit).toBe('Disconnected');
    });
  });
});
