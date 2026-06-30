import { useInverterStore } from '../store/useInverterStore';
import { formatPower, formatVoltage, formatCurrent, formatTemp, formatEnergy, formatPercent, formatFrequency, formatOperatingHours, finiteAbs } from '../lib/format';
import ColdBatteryWarning from '../components/ColdBatteryWarning';
import BatteryModeSummary from '../components/BatteryModeSummary';
import { deviceSupportsExportLimit } from '../lib/deviceCapabilities';
import AwaitingConnection from '../components/AwaitingConnection';

export default function InverterPage() {
  const { snapshot, connectionState } = useInverterStore();

  // See BatteryPage / ControlPage for why this gates on connectionState, not
  // just snapshot presence. The shared placeholder keeps wording aligned and
  // adds the FAQ paragraph that used to be missing here.
  if (!snapshot || connectionState !== 'connected') {
    return <AwaitingConnection connectionState={connectionState} showFaq />;
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
          <span className="text-text-secondary">ARM Firmware</span>
          <span className="text-text-primary font-mono text-right">{s.firmware_version || '—'}</span>
          <span className="text-text-secondary">DC DSP Firmware</span>
          <span className="text-text-primary font-mono text-right">{s.dc_dsp_firmware_version || '—'}</span>
          <span className="text-text-secondary">DSP Firmware</span>
          <span className="text-text-primary font-mono text-right">{s.dsp_firmware_version || '—'}</span>
          <span className="text-text-secondary">Max Battery Power</span>
          <span className="text-text-primary font-mono text-right">{formatPower(s.max_battery_power_w)}</span>
          <span className="text-text-secondary">Max AC Output</span>
          <span className="text-text-primary font-mono text-right">{formatPower(s.max_ac_power_w)}</span>
          {/* Grid Export Limit — only shown for device families that actually
              implement a configurable export-limit register (three-phase /
              HV / AIO hybrids use HR 1063; Gateway / EMS use HR 2071).
              Single-phase / AC-coupled hybrids only expose HR(26)
              `grid_port_max_power_output`, which is the read-only rated
              hardware max output, not an export-limit setting — so the row is
              hidden for them rather than showing a misleading value. */}
          {deviceSupportsExportLimit(s) && (
            <>
              <span className="text-text-secondary">Grid Export Limit</span>
              <span className="text-text-primary font-mono text-right whitespace-nowrap">
                {s.export_limit_w > 0 ? `${(s.export_limit_w / 1000).toFixed(2)} kW` : 'No limit set'}
              </span>
            </>
          )}
          <span className="text-text-secondary">Battery Capacity</span>
          <span className="text-text-primary font-mono text-right">{s.battery_capacity_kwh.toFixed(1)} kWh</span>
          <span className="text-text-secondary">Inverter Time</span>
          <span className="text-text-primary font-mono text-right">{s.inverter_time || '—'}</span>
          {/* IR(47-48) work_time_total — hidden when the inverter hasn't
              populated the register (some firmware variants / first
              connect window). The formatOperatingHours helper turns raw
              hours into "3y 4m" style ages for a friendly read. */}
          {s.operating_hours > 0 && (() => {
            const age = formatOperatingHours(s.operating_hours);
            return (
              <>
                <span className="text-text-secondary">Operating Hours</span>
                <span className="text-text-primary font-mono text-right">
                  {age ? `${age} (${s.operating_hours.toLocaleString()} h)` : '—'}
                </span>
              </>
            );
          })()}
        </div>
      </section>

      {/* Gateway detail — only for Gateway devices */}
      {s.device_type_code.startsWith('70') && (
        <section className="bg-bg-surface rounded-2xl p-5">
          <div className="flex items-center gap-3 mb-4">
            <h2 className="text-text-primary font-semibold text-lg">Gateway</h2>
            <span className="text-xs font-semibold px-2.5 py-1 rounded-full bg-flow-active/20 text-flow-active">
              {s.gateway_is_v2 ? 'V2' : 'V1'}
            </span>
          </div>
          <div className="grid grid-cols-2 gap-x-6 gap-y-2 text-sm">
            <span className="text-text-secondary">Software Version</span>
            <span className="text-text-primary font-mono text-right">{s.gateway_software_version || '—'}</span>
            <span className="text-text-secondary">Work Mode</span>
            <span className="text-text-primary font-mono text-right">{s.gateway_work_mode === 2 ? 'On Grid' : s.gateway_work_mode || '—'}</span>
            {s.first_inverter_serial && (
              <>
                <span className="text-text-secondary">Primary AIO</span>
                <span className="text-text-primary font-mono text-right">{s.first_inverter_serial}</span>
              </>
            )}
            <span className="text-text-secondary">AIOs Configured</span>
            <span className="text-text-primary font-mono text-right">{s.parallel_aio_count ?? '—'}</span>
            <span className="text-text-secondary">AIOs Online</span>
            <span className={`font-mono text-right ${(s.parallel_aio_online ?? 0) === (s.parallel_aio_count ?? 0) && (s.parallel_aio_online ?? 0) > 0 ? 'text-battery' : 'text-warning'}`}>
              {s.parallel_aio_online ?? '—'}
            </span>
          </div>

          {/* Per-AIO rows */}
          {(s.parallel_aio_count ?? 0) > 0 && (
            <div className="mt-4 space-y-2">
              <h3 className="text-text-primary text-sm font-semibold tracking-wide">Connected AIOs</h3>
              {Array.from({ length: s.parallel_aio_count! }).map((_, i) => (
                <div key={i} className="bg-bg-elevated rounded-xl p-4 space-y-2">
                  <div className="flex items-center gap-2">
                    <span className="text-text-secondary text-xs font-medium">AIO #{i + 1}</span>
                    {s.per_aio_serial?.[i] && (
                      <span className="text-text-primary font-mono text-xs">{s.per_aio_serial[i]}</span>
                    )}
                  </div>
                  <div className="grid grid-cols-2 sm:grid-cols-4 gap-2 text-xs">
                    <div>
                      <span className="text-text-secondary">SOC </span>
                      <span className="text-text-primary font-mono">{formatPercent(s.per_aio_soc?.[i] ?? 0)}</span>
                    </div>
                    <div>
                      <span className="text-text-secondary">Power </span>
                      <span className="text-text-primary font-mono">{formatPower(s.per_aio_power?.[i] ?? 0)}</span>
                    </div>
                    <div>
                      <span className="text-text-secondary">Charge Today </span>
                      <span className="text-text-primary font-mono">{formatEnergy(s.per_aio_charge_today_kwh?.[i] ?? 0)}</span>
                    </div>
                    <div>
                      <span className="text-text-secondary">Discharge Today </span>
                      <span className="text-text-primary font-mono">{formatEnergy(s.per_aio_discharge_today_kwh?.[i] ?? 0)}</span>
                    </div>
                  </div>
                </div>
              ))}
            </div>
          )}

          {/* Faults */}
          {s.gateway_fault_codes && s.gateway_fault_codes.length > 0 && (
            <div className="mt-4">
              <h3 className="text-warning text-sm font-semibold tracking-wide mb-2">Faults</h3>
              <ul className="space-y-1">
                {s.gateway_fault_codes.map((fault, i) => (
                  <li key={i} className="bg-warning/10 text-warning rounded-lg px-3 py-1.5 text-xs font-mono">{fault}</li>
                ))}
              </ul>
            </div>
          )}
          {s.gateway_fault_codes && s.gateway_fault_codes.length === 0 && (
            <div className="mt-4 bg-bg-elevated rounded-lg px-3 py-2">
              <span className="text-battery text-xs font-medium">✓ No faults</span>
            </div>
          )}

          <div className="mt-4 bg-bg-elevated/50 rounded-lg px-3 py-2">
            <p className="text-text-secondary text-xs">
              Battery cell-level data requires a direct connection to each AIO (not yet supported).
              The Gateway aggregates battery data — only SOC, power, and energy totals are shown here.
            </p>
          </div>
        </section>
      )}

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
          {(s.pv2_power > 0 || s.pv2_current > 0) && (
            <>
              <span className="text-text-secondary">PV2 Power</span>
              <span className="text-text-primary font-mono text-right">{formatPower(s.pv2_power)}</span>
              <span className="text-text-secondary">PV2 Voltage</span>
              <span className="text-text-primary font-mono text-right">{formatVoltage(s.pv2_voltage)}</span>
              <span className="text-text-secondary">PV2 Current</span>
              <span className="text-text-primary font-mono text-right">{formatCurrent(s.pv2_current)}</span>
            </>
          )}
          <span className="text-text-secondary">PV1 Today</span>
          <span className="text-text-primary font-mono text-right">{formatEnergy(s.today_pv1_kwh)}</span>
          {(s.pv2_power > 0 || s.pv2_current > 0) && (
            <>
              <span className="text-text-secondary">PV2 Today</span>
              <span className="text-text-primary font-mono text-right">{formatEnergy(s.today_pv2_kwh)}</span>
            </>
          )}
          <span className="text-text-secondary">Solar Today</span>
          <span className="text-text-primary font-mono text-right">{formatEnergy(s.today_solar_kwh)}</span>
          <span className="text-text-secondary">Lifetime Solar Generation</span>
          <span className="text-text-primary font-mono text-right">{formatEnergy(s.total_solar_kwh)}</span>
        </div>
      </section>

      {/* Grid */}
      <section className="bg-bg-surface rounded-2xl p-5">
        <h2 className="text-text-primary font-semibold text-lg mb-4">Grid</h2>
        <div className="grid grid-cols-2 gap-x-6 gap-y-2 text-sm">
          <span className="text-text-secondary">Grid Power</span>
          <span className="text-text-primary font-mono text-right">{formatPower(finiteAbs(s.grid_power))}</span>
          <span className="text-text-secondary">Grid Voltage</span>
          <span className="text-text-primary font-mono text-right">{formatVoltage(s.grid_voltage)}</span>
          <span className="text-text-secondary">Grid Frequency</span>
          <span className="text-text-primary font-mono text-right">{formatFrequency(s.grid_frequency)}</span>
          <span className="text-text-secondary">Import Today</span>
          <span className="text-text-primary font-mono text-right">{formatEnergy(s.today_import_kwh)}</span>
          <span className="text-text-secondary">Export Today</span>
          <span className="text-text-primary font-mono text-right">{formatEnergy(s.today_export_kwh)}</span>
          <span className="text-text-secondary">Import Total</span>
          <span className="text-text-primary font-mono text-right">{formatEnergy(s.total_import_kwh)}</span>
          <span className="text-text-secondary">Export Total</span>
          <span className="text-text-primary font-mono text-right">{formatEnergy(s.total_export_kwh)}</span>
        </div>
      </section>

      {/* Battery Configuration */}
      <section className="bg-bg-surface rounded-2xl p-5">
        <h2 className="text-text-primary font-semibold text-lg mb-4">Battery</h2>
        <div className="grid grid-cols-2 gap-x-6 gap-y-2 text-sm">
          <span className="text-text-secondary col-span-2 sm:col-span-1">Modes</span>
          <div className="text-text-primary col-span-2 sm:col-span-1 sm:text-right">
            <BatteryModeSummary snapshot={s} />
          </div>
          <span className="text-text-secondary">SOC</span>
          <span className="text-text-primary font-mono text-right">{formatPercent(s.soc)}</span>
          <span className="text-text-secondary">Voltage</span>
          <span className="text-text-primary font-mono text-right">{formatVoltage(s.battery_voltage)}</span>
          <span className="text-text-secondary">Current</span>
          <span className="text-text-primary font-mono text-right">{formatCurrent(finiteAbs(s.battery_current))}</span>
          <span className="text-text-secondary">Power</span>
          <span className="text-text-primary font-mono text-right">{formatPower(finiteAbs(s.battery_power))}</span>
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
          <span className="text-text-primary font-mono text-right">{s.battery_modules.length > 0 ? s.battery_modules.length : '—'}</span>
          <span className="text-text-secondary">Charge Today</span>
          <span className="text-text-primary font-mono text-right">{formatEnergy(s.today_charge_kwh)}</span>
          <span className="text-text-secondary">Discharge Today</span>
          <span className="text-text-primary font-mono text-right">{formatEnergy(s.today_discharge_kwh)}</span>
        </div>
      </section>

      {/* Gateway battery note */}
      {s.device_type_code.startsWith('70') && (
        <div className="bg-bg-surface rounded-2xl p-5">
          <p className="text-text-secondary text-xs">
            Battery cell data not available on the Gateway. The Gateway aggregates battery telemetry
            from its child AIO(s) — only aggregate SOC, power and energy totals are reported.
            Per-cell voltages and module temperatures require a direct connection to each AIO (not yet supported).
          </p>
        </div>
      )}

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
          <span className="text-text-secondary">Agile Mode</span>
          <span className={`font-mono text-right ${s.agile_active ? 'text-battery' : s.agile_enabled ? 'text-amber-400' : 'text-text-secondary'}`}>
            {s.agile_active
              ? (s.agile_state === 'charging'
                  ? 'Charging'
                  : s.agile_state === 'discharging'
                    ? 'Discharging'
                    : 'Active')
              : s.agile_scope === 'charge_only'
                ? 'Enabled (charge only)'
                : s.agile_scope === 'discharge_only'
                  ? 'Enabled (discharge only)'
                  : s.agile_enabled
                    ? 'Enabled (waiting for slot)'
                    : 'Disabled'}
          </span>

          <span className="text-text-secondary">Battery Calibration</span>
          <span className="text-text-primary font-mono text-right">{s.battery_calibration_stage > 0 ? `Stage ${s.battery_calibration_stage}` : 'Off'}</span>
        </div>
      </section>
    </div>
  );
}
