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
  grid_online: boolean;
  grid_loss: boolean;
  inverter_trip: boolean;
  battery_over_temp: boolean;
  home_power: number;
  inverter_temperature: number;
  inverter_time: string;
  today_solar_kwh: number;
  today_import_kwh: number;
  today_export_kwh: number;
  today_charge_kwh: number;
  total_import_kwh: number;
  total_export_kwh: number;
  today_discharge_kwh: number;
  today_consumption_kwh: number;
  battery_modules: BatteryModule[];
  battery_mode: 'unknown' | 'eco' | 'eco_paused' | 'timed_demand' | 'timed_export' | 'export_paused';
  battery_reserve: number;
  charge_rate: number;
  discharge_rate: number;
  active_power_rate: number;
  max_battery_power_w: number;
  max_ac_power_w: number;
  target_soc: number;
  enable_charge_target: boolean;
  enable_charge: boolean;
  enable_discharge: boolean;
  auto_winter_active: boolean;
  load_limiter_active: boolean;
  cosy_active: boolean;
  cosy_enabled: boolean;
  agile_active: boolean;
  agile_state: 'idle' | 'charging' | 'discharging';
  agile_enabled: boolean;
  max_charge_slots: number;
  max_discharge_slots: number;
  charge_slots: ScheduleSlot[];
  discharge_slots: ScheduleSlot[];
  meters: MeterData[];
  inverter_serial: string;
  firmware_version: string;
  dsp_firmware_version: string;
  dc_dsp_firmware_version: string;
  device_type: string;
  device_type_display: string;
  device_type_code: string;
  battery_calibration_stage: number;
  enable_ammeter: boolean;
  enable_reversed_ct_clamp: boolean;
  meter_type: number;
  supports_battery_calibration: boolean;

  // -- Gateway-specific (absent on every other device; optional for backward compat) --
  parallel_aio_count?: number;
  parallel_aio_online?: number;
  per_aio_soc?: number[];
  per_aio_power?: number[];
  per_aio_charge_today_kwh?: number[];
  per_aio_discharge_today_kwh?: number[];
  per_aio_serial?: string[];
  gateway_software_version?: string;
  gateway_is_v2?: boolean;
  gateway_work_mode?: number;
  gateway_fault_codes?: string[];
  first_inverter_serial?: string;
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
  capacity_ah: number;
  design_capacity_ah: number;
  remaining_capacity_ah: number;
  bms_status_registers?: number[];
  bms_status?: number[];
  bms_warnings?: number[];
}

export interface ScheduleSlot {
  enabled: boolean;
  start_hour: number;
  start_minute: number;
  end_hour: number;
  end_minute: number;
  target_soc: number;
}

export interface TariffConfig {
  peak_rate: number;
  off_peak_rate: number;
  off_peak_start: string;
  off_peak_end: string;
}

export interface PollSettings {
  host: string;
  port: number;
  serial: string;
  interval_secs: number;
  http_port: number;
  import_tariff: number;
  export_tariff: number;
  import_tariff_config: TariffConfig | null;
  export_tariff_config: TariffConfig | null;
  hidden_panels: string[];
  evc_host: string;
  evc_port: number;
  disable_auto_discovery: boolean;
}

export interface DiscoveredInverter {
  host: string;
  port: number;
  serial: string | null;
  generation: string | null;
}

export interface DiscoveredEvc {
  host: string;
  port: number;
  serial: string | null;
}

export type ConnectionState = 'connected' | 'reconnecting' | 'disconnected';

export interface StatusResponse {
  ok: boolean;
  connection: ConnectionState;
  host: string;
  lan_ip: string | null;
  connected_since_epoch_ms: number | null;
  connect_failures: number;
}

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

export interface MeterData {
  address: number;
  v_phase_1: number;
  v_phase_2: number;
  v_phase_3: number;
  i_phase_1: number;
  i_phase_2: number;
  i_phase_3: number;
  i_total: number;
  p_active_phase_1: number;
  p_active_phase_2: number;
  p_active_phase_3: number;
  p_active_total: number;
  p_reactive_total: number;
  p_apparent_total: number;
  pf_total: number;
  frequency: number;
  e_import_active_kwh: number;
  e_export_active_kwh: number;
}

export interface TimePoint {
  t: number;
  v: number;
}

export type HistoryRange = '1h' | '6h' | '12h' | '24h' | 'today' | '7d' | '30d' | 'month' | '6m' | '1y';
