export interface InverterSnapshot {
  timestamp: number;
  solar_power: number;
  pv1_power: number;
  pv2_power: number;
  pv1_voltage: number;
  pv2_voltage: number;
  pv1_current: number;
  pv2_current: number;
  battery_power: number;
  soc: number;
  battery_voltage: number;
  battery_current: number;
  battery_state: 'idle' | 'charging' | 'discharging';
  battery_temperature: number;
  battery_capacity_kwh: number;
  grid_power: number;
  grid_voltage: number;
  grid_frequency: number;
  home_power: number;
  inverter_temperature: number;
  today_solar_kwh: number;
  today_import_kwh: number;
  today_export_kwh: number;
  today_charge_kwh: number;
  today_discharge_kwh: number;
  today_consumption_kwh: number;
  battery_modules: BatteryModule[];
  battery_mode: 'unknown' | 'eco' | 'eco_paused' | 'timed_demand' | 'timed_export' | 'export_paused';
  battery_reserve: number;
  charge_rate: number;
  discharge_rate: number;
  target_soc: number;
  charge_slots: ScheduleSlot[];
  discharge_slots: ScheduleSlot[];
  inverter_serial: string;
  firmware_version: string;
  device_type: string;
}

export interface BatteryModule {
  index: number;
  soc: number;
  temperature: number;
  voltage: number;
  current: number;
  serial: string;
  num_cycles: number;
  num_cells: number;
  cell_voltages: number[];
  cell_temperatures: number[];
  bms_firmware: number;
}

export interface ScheduleSlot {
  enabled: boolean;
  start_hour: number;
  start_minute: number;
  end_hour: number;
  end_minute: number;
  target_soc: number;
}

export interface PollSettings {
  host: string;
  port: number;
  serial: string;
  interval_secs: number;
}

export interface DiscoveredInverter {
  host: string;
  port: number;
  serial: string | null;
  generation: string | null;
}

export type ConnectionState = 'connected' | 'reconnecting' | 'disconnected';

export interface WsSnapshotMessage {
  type: 'snapshot';
  // All InverterSnapshot fields at top level
  [key: string]: unknown;
}

export interface WsConnectionMessage {
  type: 'connection';
  state: ConnectionState;
  host: string;
}
