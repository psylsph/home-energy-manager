import { useInverterStore } from '../store/useInverterStore';
import { formatPower, formatVoltage, formatCurrent, formatTemp, formatEnergy, formatPercent, formatFrequency } from '../lib/format';
import ColdBatteryWarning from '../components/ColdBatteryWarning';

export default function InverterPage() {
  const { snapshot, connectionState } = useInverterStore();

  if (!snapshot) {
    return (
      <div className="flex flex-col items-center justify-center min-h-[60vh] gap-4">
        <div className="w-10 h-10 border-4 border-flow-active border-t-transparent rounded-full animate-spin" />
        <p className="text-text-secondary text-sm font-sans">
          {connectionState === 'reconnecting'
            ? 'Connection lost — reconnecting…'
            : connectionState === 'disconnected'
              ? 'Disconnected — will retry automatically'
              : 'Waiting for data'}
        </p>
      </div>
    );
  }

  const s = snapshot;

  return (
    <div className="flex flex-col gap-4 max-w-4xl mx-auto">
      <ColdBatteryWarning />

      {/* Device Info */}
      <section className="bg-bg-surface rounded-2xl p-5">
        <h2 className="text-text-primary font-semibold text-lg mb-4">Device Info</h2>
        <div className="grid grid-cols-2 gap-x-6 gap-y-2 text-sm">
          <span className="text-text-secondary">Inverter Type</span>
          <span className="text-text-primary font-mono text-right">{s.device_type_display || '—'}</span>
          <span className="text-text-secondary">Device Type Code</span>
          <span className="text-text-primary font-mono text-right">0x{s.device_type_code || '—'}</span>
          <span className="text-text-secondary">Serial Number</span>
          <span className="text-text-primary font-mono text-right">{s.inverter_serial || '—'}</span>
          <span className="text-text-secondary">Firmware Version</span>
          <span className="text-text-primary font-mono text-right">{s.firmware_version || '—'}</span>
          <span className="text-text-secondary">Max Battery Power</span>
          <span className="text-text-primary font-mono text-right">{formatPower(s.max_battery_power_w)}</span>
          <span className="text-text-secondary">Battery Capacity</span>
          <span className="text-text-primary font-mono text-right">{s.battery_capacity_kwh.toFixed(1)} kWh</span>
        </div>
      </section>

      {/* Inverter Metrics */}
      <section className="bg-bg-surface rounded-2xl p-5">
        <h2 className="text-text-primary font-semibold text-lg mb-4">Inverter</h2>
        <div className="grid grid-cols-2 gap-x-6 gap-y-2 text-sm">
          <span className="text-text-secondary">Temperature</span>
          <span className="text-text-primary font-mono text-right">{formatTemp(s.inverter_temperature)}</span>
          <span className="text-text-secondary">Active Power Rate</span>
          <span className="text-text-primary font-mono text-right">{formatPercent(s.active_power_rate)}</span>
        </div>
      </section>

      {/* PV Inputs */}
      <section className="bg-bg-surface rounded-2xl p-5">
        <h2 className="text-text-primary font-semibold text-lg mb-4">Solar Inputs</h2>
        <div className="grid grid-cols-2 gap-x-6 gap-y-2 text-sm">
          <span className="text-text-secondary">Total Solar Power</span>
          <span className="text-text-primary font-mono text-right">{formatPower(s.solar_power)}</span>
          <span className="text-text-secondary">PV1 Power</span>
          <span className="text-text-primary font-mono text-right">{formatPower(s.pv1_power)}</span>
          <span className="text-text-secondary">PV1 Voltage</span>
          <span className="text-text-primary font-mono text-right">{formatVoltage(s.pv1_voltage)}</span>
          <span className="text-text-secondary">PV1 Current</span>
          <span className="text-text-primary font-mono text-right">{formatCurrent(s.pv1_current)}</span>
          {(s.pv2_voltage > 0 || s.pv2_power > 0) && (
            <>
              <span className="text-text-secondary">PV2 Power</span>
              <span className="text-text-primary font-mono text-right">{formatPower(s.pv2_power)}</span>
              <span className="text-text-secondary">PV2 Voltage</span>
              <span className="text-text-primary font-mono text-right">{formatVoltage(s.pv2_voltage)}</span>
              <span className="text-text-secondary">PV2 Current</span>
              <span className="text-text-primary font-mono text-right">{formatCurrent(s.pv2_current)}</span>
            </>
          )}
          <span className="text-text-secondary">Solar Today</span>
          <span className="text-text-primary font-mono text-right">{formatEnergy(s.today_solar_kwh)}</span>
        </div>
      </section>

      {/* Grid */}
      <section className="bg-bg-surface rounded-2xl p-5">
        <h2 className="text-text-primary font-semibold text-lg mb-4">Grid</h2>
        <div className="grid grid-cols-2 gap-x-6 gap-y-2 text-sm">
          <span className="text-text-secondary">Grid Power</span>
          <span className="text-text-primary font-mono text-right">{formatPower(s.grid_power)}</span>
          <span className="text-text-secondary">Grid Voltage</span>
          <span className="text-text-primary font-mono text-right">{formatVoltage(s.grid_voltage)}</span>
          <span className="text-text-secondary">Grid Frequency</span>
          <span className="text-text-primary font-mono text-right">{formatFrequency(s.grid_frequency)}</span>
          <span className="text-text-secondary">Import Today</span>
          <span className="text-text-primary font-mono text-right">{formatEnergy(s.today_import_kwh)}</span>
          <span className="text-text-secondary">Export Today</span>
          <span className="text-text-primary font-mono text-right">{formatEnergy(s.today_export_kwh)}</span>
        </div>
      </section>

      {/* Battery Configuration */}
      <section className="bg-bg-surface rounded-2xl p-5">
        <h2 className="text-text-primary font-semibold text-lg mb-4">Battery</h2>
        <div className="grid grid-cols-2 gap-x-6 gap-y-2 text-sm">
          <span className="text-text-secondary">Battery Mode</span>
          <span className="text-text-primary font-mono text-right">{s.battery_mode}</span>
          <span className="text-text-secondary">SOC</span>
          <span className="text-text-primary font-mono text-right">{formatPercent(s.soc)}</span>
          <span className="text-text-secondary">Voltage</span>
          <span className="text-text-primary font-mono text-right">{formatVoltage(s.battery_voltage)}</span>
          <span className="text-text-secondary">Current</span>
          <span className="text-text-primary font-mono text-right">{formatCurrent(s.battery_current)}</span>
          <span className="text-text-secondary">Power</span>
          <span className="text-text-primary font-mono text-right">{formatPower(s.battery_power)}</span>
          <span className="text-text-secondary">Temperature</span>
          <span className="text-text-primary font-mono text-right">{formatTemp(s.battery_temperature)}</span>
          <span className="text-text-secondary">Capacity</span>
          <span className="text-text-primary font-mono text-right">{s.battery_capacity_kwh.toFixed(1)} kWh</span>
          <span className="text-text-secondary">SOC Reserve</span>
          <span className="text-text-primary font-mono text-right">{formatPercent(s.battery_reserve)}</span>
          <span className="text-text-secondary">Target SOC</span>
          <span className="text-text-primary font-mono text-right">{formatPercent(s.target_soc)}</span>
          <span className="text-text-secondary">Charge Rate</span>
          <span className="text-text-primary font-mono text-right">{formatPercent(s.charge_rate)}</span>
          <span className="text-text-secondary">Discharge Rate</span>
          <span className="text-text-primary font-mono text-right">{formatPercent(s.discharge_rate)}</span>
          <span className="text-text-secondary">Enable Charge</span>
          <span className={`font-mono text-right ${s.enable_charge ? 'text-battery' : 'text-text-secondary'}`}>{s.enable_charge ? 'Yes' : 'No'}</span>
          <span className="text-text-secondary">Enable Charge Target</span>
          <span className={`font-mono text-right ${s.enable_charge_target ? 'text-battery' : 'text-text-secondary'}`}>{s.enable_charge_target ? 'Yes' : 'No'}</span>
          <span className="text-text-secondary">Enable Discharge</span>
          <span className={`font-mono text-right ${s.enable_discharge ? 'text-battery' : 'text-text-secondary'}`}>{s.enable_discharge ? 'Yes' : 'No'}</span>
          <span className="text-text-secondary">Modules</span>
          <span className="text-text-primary font-mono text-right">{s.battery_modules.length}</span>
          <span className="text-text-secondary">Charge Today</span>
          <span className="text-text-primary font-mono text-right">{formatEnergy(s.today_charge_kwh)}</span>
          <span className="text-text-secondary">Discharge Today</span>
          <span className="text-text-primary font-mono text-right">{formatEnergy(s.today_discharge_kwh)}</span>
        </div>
      </section>

      {/* Features & Status */}
      <section className="bg-bg-surface rounded-2xl p-5">
        <h2 className="text-text-primary font-semibold text-lg mb-4">Features & Status</h2>
        <div className="grid grid-cols-2 gap-x-6 gap-y-2 text-sm">
          <span className="text-text-secondary">Auto Winter</span>
          <span className={`font-mono text-right ${s.auto_winter_active ? 'text-blue-400' : 'text-text-secondary'}`}>{s.auto_winter_active ? 'Active' : 'Inactive'}</span>
          <span className="text-text-secondary">Cosy Mode</span>
          <span className={`font-mono text-right ${s.cosy_active ? 'text-battery' : s.cosy_enabled ? 'text-amber-400' : 'text-text-secondary'}`}>
            {s.cosy_active ? 'Active' : s.cosy_enabled ? 'Enabled' : 'Disabled'}
          </span>
          <span className="text-text-secondary">Battery Calibration</span>
          <span className="text-text-primary font-mono text-right">{s.battery_calibration_stage > 0 ? `Stage ${s.battery_calibration_stage}` : 'Off'}</span>
          <span className="text-text-secondary">Home Power</span>
          <span className="text-text-primary font-mono text-right">{formatPower(s.home_power)}</span>
          <span className="text-text-secondary">Consumption Today</span>
          <span className="text-text-primary font-mono text-right">{formatEnergy(s.today_consumption_kwh)}</span>
        </div>
      </section>
    </div>
  );
}
