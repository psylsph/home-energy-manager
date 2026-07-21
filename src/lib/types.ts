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
  /**
   * Instantaneous Emergency Power Supply (EPS) output power in watts
   * (IR(31) `p_backup`). Only populated on device families with an AC
   * output stage (AC-coupled, All-in-One). On grid-connected systems
   * this reads 0; during a grid outage the EPS-leg load increases as
   * backup circuits come online. Hidden from the Battery panel when
   * the value is 0 — see `BatteryPanel.tsx`.
   */
  eps_power_w: number;
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
  today_pv1_kwh: number;
  today_pv2_kwh: number;
  today_import_kwh: number;
  today_export_kwh: number;
  today_charge_kwh: number;
  total_import_kwh: number;
  total_export_kwh: number;
  total_solar_kwh: number;
  total_charge_kwh: number;
  total_discharge_kwh: number;
  total_throughput_kwh: number;
  /**
   * Cumulative inverter operating hours (IR(47-48) `work_time_total`).
   * Monotonically non-decreasing over the inverter's lifetime; the backend
   * caps this at 876 000 hours (100 years) to reject uint32 rollovers and
   * uninitialised-register garbage. Drives the "Inverter: 3y 4m old"
   * display on the Inverter page. 0 means the inverter hasn't reported a
   * value yet — UI hides the row in that case.
   */
  operating_hours: number;
  today_discharge_kwh: number;
  today_consumption_kwh: number;
  /**
   * Cumulative home energy consumption today (kWh), integrated from
   * `home_power`. Always monotonic during the day; resets at midnight.
   * Use this for "Consumption Today" displays instead of the formula-
   * derived `today_consumption_kwh` which can decrease when the battery
   * keeps AC-charging from the grid after solar stops.
   */
  home_energy_today_kwh: number;
  battery_modules: BatteryModule[];
  battery_mode: 'unknown' | 'eco' | 'eco_paused' | 'timed_demand' | 'timed_export' | 'export_paused';
  /** Raw Eco / self-consumption register HR27: 1 = Eco, 0 = export/max-power mode. */
  battery_power_mode?: number;
  battery_reserve: number;
  charge_rate: number;
  discharge_rate: number;
  active_power_rate: number;
  max_battery_power_w: number;
  max_ac_power_w: number;
  /**
   * Export power limit in watts (0 = unlimited / not configured).
   * Populated only for models that expose the relevant register:
   *   - Single-phase / AC-coupled: read from HR(26) `grid_port_max_power_output`
   *   - Three-phase / HV / AIO: read from HR(1063) `p_export_limit` (deci-W)
   *   - EMS / Gateway: read from HR(2071)
   * On other models this stays at its default of 0.
   */
  export_limit_w: number;
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
  /**
   * Active Agile Octopus scope. Replaces the boolean `agile_enabled`
   * for new code paths. Front-end derives its `chargeMode` from this
   * field plus the cosy flag. Defaults to `'off'` for backends that
   * predate the slot-based refactor.
   */
  agile_scope?: 'off' | 'full' | 'charge_only' | 'discharge_only';
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
  ac_eps_enabled: boolean;
  ac_export_priority: number;
  /** Battery pause mode HR318: 0 disabled, 1 pause charge, 2 pause discharge, 3 pause both. */
  battery_pause_mode?: number;
  /** Battery pause slot HR319/320. For Timed Discharge, HEM displays the inverse as the demand window. */
  battery_pause_slot?: ScheduleSlot;

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

  // -- Solar arrays (issue #110: "% of max" display) --
  /** Configured solar arrays with rated capacity (kWp), built server-side
   *  from the user's Settings. Combines DC strings (PV1/PV2) and external
   *  CT meters labelled as solar (AC-coupled / separate inverters). Empty
   *  until the user opts in. See `SolarArraySummary`. */
  solar_arrays?: SolarArraySummary[];
  /** PV1 output as a percentage of its rated peak capacity (kWp). Null when
   *  no rated capacity is configured. Stored in history for charting. */
  pv1_pct?: number | null;
  /** PV2 output as a percentage of its rated peak capacity (kWp). */
  pv2_pct?: number | null;
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

/** Where a surfaced solar array's power reading comes from (issue #110).
 *  - `pv1` / `pv2`: DC strings on a hybrid / DC-coupled inverter.
 *  - `meter`: external CT clamp at device address 1-8 (AC-coupled /
 *    separate inverter); `meter_address` carries the address. */
export type SolarArraySource = 'pv1' | 'pv2' | 'meter';

/** One solar array surfaced for "% of max" display (issue #110). Built each
 *  poll from the user's configured arrays (DC strings with a rated kWp, or
 *  external CT meters labelled as solar). */
export interface SolarArraySummary {
  source: SolarArraySource;
  /** Display name. Empty → the UI falls back to a label derived from
   *  `source` (PV1 / PV2 / the meter address). */
  name: string;
  /** Live AC power output in watts (unsigned). */
  power_w: number;
  /** Rated peak capacity in kW (kWp). 0 = not configured (hide the %). */
  rated_kw: number;
  /** Energy produced today in kWh, when known. `null` for meter-backed
   *  arrays (CT meters only expose cumulative totals). */
  today_kwh: number | null;
  /** CT meter device address for `meter`-source arrays (1-8). `null` for
   *  DC strings. */
  meter_address: number | null;
}

/** A user-configured external solar array measured by a GivEnergy CT clamp
 *  (issue #110). Stored in Settings and POSTed to `/api/settings`. */
export interface SolarArrayConfig {
  /** CT meter device address this array is wired to (1-8). */
  meter_address: number;
  /** Display name (e.g. "East roof"). */
  name: string;
  /** Rated peak capacity in kW (kWp). 0 hides the % display. */
  rated_kw: number;
}

export interface ScheduleSlot {
  enabled: boolean;
  start_hour: number;
  start_minute: number;
  end_hour: number;
  end_minute: number;
  target_soc: number;
}

/**
 * Response shape for POST /api/control/mode. The backend echoes a captured
 * discharge schedule in `discharge_slots_backup` when entering Eco / Pause /
 * Export Paused with a configured schedule, so the frontend can stage the
 * saved slots as pending edits and surface them in the Eco-mode UI after
 * an Eco→Timed→Eco round-trip. See issue #137.
 */
export interface SetModeResponse {
  ok: boolean;
  message?: string;
  error?: string;
  discharge_slots_backup?: ScheduleSlot[];
}

export interface TariffSlot {
  start: string;   // "HH:MM"
  end: string;     // "HH:MM" — "23:59" for final slot (inclusive, covers minute 1439)
  rate: number;    // £/kWh
}

export interface TariffConfig {
  slots: TariffSlot[];
}

export interface PollSettings {
  host: string;
  port: number;
  serial: string;
  interval_secs: number;
  http_port: number;
  import_tariff: number;
  export_tariff: number;
  /**
   * Optional daily fixed cost for the import direction, in pence/day (p/day).
   * Sourced from UK-style tariffs (Octopus Flux, etc.) that charge a flat
   * standing fee on top of the per-kWh rate. Defaults to 0 (no standing
   * charge) when absent from the persisted settings file — older installs
   * didn't carry the field. Issue #131.
   */
  import_standing_charge_p_per_day?: number;
  import_tariff_config: TariffConfig | null;
  export_tariff_config: TariffConfig | null;
  /** Authenticated Octopus customer-consumption integration (issue #212). */
  octopus_enabled?: boolean;
  octopus_account_number?: string;
  /** The secret itself is never returned by GET /api/settings. */
  octopus_api_key_configured?: boolean;
  octopus_gas_unit?: 'unknown' | 'kwh' | 'm3';
  octopus_economy7_start?: string;
  octopus_economy7_end?: string;
  hidden_panels: string[];
  evc_host: string;
  evc_port: number;
  disable_auto_discovery: boolean;
  /** Whether the user has opted in to launching the app on system login.
   *  Wired through tauri-plugin-autostart (HKCU\…\Run on Windows,
   *  LaunchAgent on macOS, ~/.config/autostart/*.desktop on Linux).
   *  See issue #117. */
  autostart_enabled: boolean;
  /** When true, the window's close button hides to the system tray instead
   *  of quitting (issue #217). Takes effect immediately; the close handler
   *  reads it live from the persisted settings. */
  minimise_to_tray: boolean;
  /** When true, the app launches with its window hidden in the tray
   *  (issue #217). Read once at startup, so it applies on the next launch. */
  start_minimised: boolean;
  /** API key for the read-only external API server (developer mode). */
  api_key: string;
  /** Port for the read-only external API server (0 = disabled). */
  api_port: number;
  // -- Solar array capacities (issue #110) --
  /** Rated peak capacity (kWp) of the PV1 DC string (hybrid). 0 = unset. */
  pv1_rated_kw?: number;
  /** Rated peak capacity (kWp) of the PV2 DC string. 0 = unset. */
  pv2_rated_kw?: number;
  /** External solar arrays measured by CT clamps (AC-coupled). */
  solar_arrays?: SolarArrayConfig[];
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
  /** Modbus device address. External CT clamps use 0x01-0x08; 0 marks the
   * synthetic built-in grid CT (three-phase / HV models, no external meter). */
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
