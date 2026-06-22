import { memo } from 'react';
import type { InverterSnapshot, ScheduleSlot } from '../lib/types';
import { formatPower, formatPercent, formatVoltage, formatCurrent, formatTemp, formatEnergy } from '../lib/format';

interface Props {
  snapshot: InverterSnapshot;
}

function socColor(soc: number): string {
  if (soc < 20) return '#EF4444';
  if (soc < 50) return '#F59E0B';
  return '#22C55E';
}

function stateLabel(state: string): string {
  switch (state) {
    case 'charging': return 'Charging';
    case 'discharging': return 'Discharging';
    case 'idle': return 'Idle';
    default: return state;
  }
}

function stateColor(state: string): string {
  switch (state) {
    case 'charging': return '#22C55E';
    case 'discharging': return '#F59E0B';
    default: return '#8B949E';
  }
}

const BATTERY_MODE_LABELS: Record<string, string> = {
  unknown: 'Unknown',
  eco: 'Eco',
  eco_paused: 'Eco Paused',
  timed_demand: 'Timed Demand',
  timed_export: 'Timed Export',
  export_paused: 'Export Paused',
};

function isAnySlotActive(slots: ScheduleSlot[]): boolean {
  const now = new Date();
  const curMin = now.getHours() * 60 + now.getMinutes();

  return slots.some((slot) => {
    if (!slot.enabled) return false;
    const startMin = slot.start_hour * 60 + slot.start_minute;
    const endMin = slot.end_hour * 60 + slot.end_minute;
    return startMin < endMin
      ? curMin >= startMin && curMin < endMin
      : curMin >= startMin || curMin < endMin;
  });
}

/** Override mode label when cosy mode is enabled, or "Override" when */
/** force charge or force discharge is active.                         */
function modeDisplayLabel(
  mode: string,
  cosyActive: boolean,
  cosyEnabled: boolean,
  enableCharge: boolean,
  enableDischarge: boolean,
  inChargeWindow: boolean,
  inDischargeWindow: boolean,
): string {
  if (cosyActive) return 'Cosy';
  if (cosyEnabled && (mode === 'eco' || mode === 'eco_paused')) return 'Cosy';
  // Force charge is active only when the master charge-enable flag is set
  // AND the current time falls within an active charge slot window. The
  // enable_charge register (HR 96 / HR 1123) is a sticky schedule-enable
  // flag, not an instantaneous "charging now" signal.
  const forceChargeActive = enableCharge && inChargeWindow;
  // Same logic for discharge: the enable_discharge flag (HR 59) enables
  // timed slots; force discharge is only active when inside a window.
  const forceDischargeActive = enableDischarge && inDischargeWindow;
  if (forceChargeActive || forceDischargeActive) return 'Override';
  return BATTERY_MODE_LABELS[mode] ?? mode;
}

function BatteryPanelInner({ snapshot: s }: Props) {
  const color = socColor(s.soc);
  const chargeSlotActive = isAnySlotActive(s.charge_slots ?? []);
  const dischargeSlotActive = isAnySlotActive(s.discharge_slots ?? []);

  return (
    <section className="bg-bg-surface rounded-2xl p-6 h-full flex flex-col sm:flex-row items-center gap-6">
      <div className="relative w-32 h-32 shrink-0">
        <svg viewBox="0 0 100 100" className="w-full h-full -rotate-90">
          <circle cx="50" cy="50" r="42" fill="none" stroke="#21262D" strokeWidth="8" />
          <circle
            cx="50" cy="50" r="42"
            fill="none"
            stroke={color}
            strokeWidth="8"
            strokeLinecap="round"
            strokeDasharray={`${(s.soc / 100) * 264} 264`}
            className="transition-all duration-700"
          />
        </svg>
        <div className="absolute inset-0 flex flex-col items-center justify-center">
          <span className="text-text-primary text-2xl font-bold font-mono leading-none">
            {formatPercent(s.soc)}
          </span>
        </div>
      </div>
      <div className="flex flex-col gap-2 flex-1">
        <div className="flex items-center gap-3">
          <h2 className="text-text-primary text-lg font-semibold">Battery</h2>
          <span
            className="text-xs font-semibold px-2.5 py-1 rounded-full"
            style={{
              backgroundColor: stateColor(s.battery_state) + '20',
              color: stateColor(s.battery_state),
            }}
          >
            {stateLabel(s.battery_state)}
          </span>
        </div>
        <div className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-1.5 text-sm">
          <span className="text-text-secondary">Power</span>
          <span className="text-text-primary font-mono text-right">
            {s.battery_state === 'discharging' ? '−' : s.battery_state === 'charging' ? '+' : ''}
            {formatPower(Math.abs(s.battery_power))}
          </span>
          {/* EPS (Emergency Power Supply) output — only visible when the
              backup leg is actively feeding loads. IR(31) reads 0 on
              grid-connected systems and on devices that don't support
              EPS (DC hybrids, pure three-phase); the row stays hidden
              in both cases so the panel doesn't grow when there's
              nothing to show. */}
          {s.eps_power_w > 0 && (
            <>
              <span className="text-text-secondary">EPS Power</span>
              <span className="text-text-primary font-mono text-right">
                {formatPower(s.eps_power_w)}
              </span>
            </>
          )}
          <span className="text-text-secondary">Voltage</span>
          <span className="text-text-primary font-mono text-right">{formatVoltage(s.battery_voltage)}</span>
          <span className="text-text-secondary">Current</span>
          <span className="text-text-primary font-mono text-right">{formatCurrent(Math.abs(s.battery_current))}</span>
          <span className="text-text-secondary">Temperature</span>
          <span className="text-text-primary font-mono text-right">{formatTemp(s.battery_temperature)}</span>
          <span className="text-text-secondary">Mode</span>
          <span className="text-text-primary font-mono text-right">{modeDisplayLabel(s.battery_mode, s.cosy_active, s.cosy_enabled, s.enable_charge, s.enable_discharge, chargeSlotActive, dischargeSlotActive)}</span>
          <span className="text-text-secondary">Reserve</span>
          <span className="text-text-primary font-mono text-right">{formatPercent(s.battery_reserve)}</span>
          <span className="text-text-secondary">Charged Today</span>
          <span className="text-text-primary font-mono text-right">{formatEnergy(s.today_charge_kwh)}</span>
          <span className="text-text-secondary">Discharged Today</span>
          <span className="text-text-primary font-mono text-right">{formatEnergy(s.today_discharge_kwh)}</span>
        </div>
      </div>
    </section>
  );
}

const BatteryPanel = memo(BatteryPanelInner);
export default BatteryPanel;
