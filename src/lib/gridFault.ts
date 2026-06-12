import type { InverterSnapshot } from './types';

type GridFaultSnapshot = Pick<InverterSnapshot, 'grid_loss' | 'grid_online' | 'inverter_trip' | 'battery_over_temp'>;

export function hasGridFault(snapshot: GridFaultSnapshot): boolean {
  return snapshot.grid_loss || snapshot.inverter_trip || !snapshot.grid_online || snapshot.battery_over_temp;
}

export function gridFaultTitle(snapshot: GridFaultSnapshot): string {
  if (snapshot.grid_loss || !snapshot.grid_online) return 'Grid power lost';
  if (snapshot.battery_over_temp) return 'Battery over temperature';
  return 'Inverter trip detected';
}

export function gridFaultReason(snapshot: GridFaultSnapshot): string {
  if (snapshot.grid_loss) return 'No Utility';
  if (!snapshot.grid_online) return 'no live grid AC reference';
  if (snapshot.battery_over_temp) return 'Battery over temp warning';
  return 'Inverter fault';
}

export function gridFaultAdvice(snapshot: GridFaultSnapshot): string {
  if (snapshot.inverter_trip && !snapshot.grid_loss && snapshot.grid_online && !snapshot.battery_over_temp) {
    return ' Check the inverter fault state.';
  }
  if (snapshot.battery_over_temp) {
    return ' The battery temperature is critical — check ventilation and allow the system to cool.';
  }
  return ' Conserve battery power until the grid is restored.';
}
