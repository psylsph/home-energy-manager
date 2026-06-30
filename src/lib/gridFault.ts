import type { InverterSnapshot } from './types';

export type SystemAlertKind = 'grid' | 'inverter_trip' | 'battery_over_temp' | 'inverter_temp_low' | 'inverter_temp_high';

type GridFaultSnapshot = Pick<InverterSnapshot, 'grid_loss' | 'grid_online' | 'inverter_trip' | 'battery_over_temp'>;
type InverterTempSnapshot = Pick<InverterSnapshot, 'inverter_temperature'>;

export interface InverterTemperatureAlertConfig {
  inverter_temp_min: number;
  inverter_temp_max: number;
}

export interface SystemAlert {
  kind: SystemAlertKind;
  title: string;
  reason: string;
  advice: string;
}

export function hasGridFault(snapshot: GridFaultSnapshot): boolean {
  return snapshot.grid_loss || !snapshot.grid_online;
}

export function hasInverterTrip(snapshot: GridFaultSnapshot): boolean {
  return snapshot.inverter_trip;
}

export function hasBatteryOverTemp(snapshot: GridFaultSnapshot): boolean {
  return snapshot.battery_over_temp;
}

export function hasInverterTemperatureAlert(
  snapshot: InverterTempSnapshot,
  config: InverterTemperatureAlertConfig,
): boolean {
  return inverterTemperatureAlertKind(snapshot, config) !== null;
}

export function inverterTemperatureAlertKind(
  snapshot: InverterTempSnapshot,
  config: InverterTemperatureAlertConfig,
): 'inverter_temp_low' | 'inverter_temp_high' | null {
  const temp = snapshot.inverter_temperature;
  if (!Number.isFinite(temp)) return null;
  if (config.inverter_temp_min > 0 && temp < config.inverter_temp_min) return 'inverter_temp_low';
  if (config.inverter_temp_max > 0 && temp > config.inverter_temp_max) return 'inverter_temp_high';
  return null;
}

export function gridFaultTitle(): string {
  return 'Grid power lost';
}

export function gridFaultReason(snapshot: GridFaultSnapshot): string {
  if (snapshot.grid_loss) return 'No Utility';
  if (!snapshot.grid_online) return 'no live grid AC reference';
  return 'grid fault';
}

export function gridFaultAdvice(): string {
  return ' Conserve battery power until the grid is restored.';
}

export function buildSystemAlerts(
  snapshot: GridFaultSnapshot & InverterTempSnapshot,
  tempConfig: InverterTemperatureAlertConfig,
): SystemAlert[] {
  const alerts: SystemAlert[] = [];

  if (hasGridFault(snapshot)) {
    alerts.push({
      kind: 'grid',
      title: gridFaultTitle(),
      reason: gridFaultReason(snapshot),
      advice: 'Conserve battery power until the grid is restored.',
    });
  }

  if (hasInverterTrip(snapshot)) {
    alerts.push({
      kind: 'inverter_trip',
      title: 'Inverter trip detected',
      reason: 'Inverter fault',
      advice: 'Check the inverter fault state.',
    });
  }

  if (hasBatteryOverTemp(snapshot)) {
    alerts.push({
      kind: 'battery_over_temp',
      title: 'Battery over temperature',
      reason: 'Battery over temp warning',
      advice: 'The battery temperature is critical — check ventilation and allow the system to cool.',
    });
  }

  const tempKind = inverterTemperatureAlertKind(snapshot, tempConfig);
  if (tempKind === 'inverter_temp_low') {
    alerts.push({
      kind: 'inverter_temp_low',
      title: 'Inverter temperature low',
      reason: `Inverter temperature below ${tempConfig.inverter_temp_min}°C`,
      advice: 'Check the inverter environment and airflow.',
    });
  } else if (tempKind === 'inverter_temp_high') {
    alerts.push({
      kind: 'inverter_temp_high',
      title: 'Inverter temperature high',
      reason: `Inverter temperature above ${tempConfig.inverter_temp_max}°C`,
      advice: 'Check ventilation and allow the inverter to cool.',
    });
  }

  return alerts;
}
