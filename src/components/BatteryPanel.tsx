import { memo } from 'react';
import type { InverterSnapshot } from '../lib/types';
import { formatPower, formatPercent, formatTemp, formatEnergy, formatVoltage } from '../lib/format';
import ColdBatteryWarning from './ColdBatteryWarning';

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

function BatteryPanelInner({ snapshot: s }: Props) {
  const color = socColor(s.soc);
  const storedKwh = (s.soc / 100) * s.battery_capacity_kwh;

  return (
    <div className="bg-bg-surface rounded-2xl p-6 flex flex-col gap-5">
      <ColdBatteryWarning />
      {/* Header row */}
      <div className="flex items-center justify-between">
        <h2 className="text-text-primary text-base font-semibold tracking-wide">Battery</h2>
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

      {/* SOC ring + power */}
      <div className="flex items-center gap-5">
        {/* Circular SOC indicator */}
        <div className="relative w-24 h-24 shrink-0">
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
            <span className="text-text-primary text-xl font-bold font-mono leading-none">
              {formatPercent(s.soc)}
            </span>
          </div>
        </div>

        {/* Power detail */}
        <div className="flex flex-col gap-2 flex-1 min-w-0">
          <div className="flex items-baseline justify-between">
            <span className="text-text-secondary text-xs">Power</span>
            <span className="text-text-primary text-sm font-mono">
              {s.battery_state === 'discharging' ? '−' : s.battery_state === 'charging' ? '+' : ''}
              {formatPower(Math.abs(s.battery_power))}
            </span>
          </div>
          <div className="flex items-baseline justify-between">
            <span className="text-text-secondary text-xs">Voltage</span>
            <span className="text-text-primary text-sm font-mono">{formatVoltage(s.battery_voltage)}</span>
          </div>
          <div className="flex items-baseline justify-between">
            <span className="text-text-secondary text-xs">Temp</span>
            <span className="text-text-primary text-sm font-mono">{formatTemp(s.battery_temperature)}</span>
          </div>
          <div className="flex items-baseline justify-between">
            <span className="text-text-secondary text-xs">Charged Today</span>
            <span className="text-text-primary text-sm font-mono">{formatEnergy(s.today_charge_kwh)}</span>
           </div>
          <div className="flex items-baseline justify-between">
            <span className="text-text-secondary text-xs">Discharged Today</span>
            <span className="text-text-primary text-sm font-mono">{formatEnergy(s.today_discharge_kwh)}</span>
        </div>
        </div>
      </div>

      {/* Capacity bar */}
      <div className="space-y-1">
        <div className="flex justify-between text-xs">
          <span className="text-text-secondary">Capacity</span>
          <span className="text-text-primary font-mono">
            {formatEnergy(storedKwh)} <span className="text-text-secondary">/ {formatEnergy(s.battery_capacity_kwh)}</span>
          </span>
        </div>
        <div className="h-1.5 bg-bg-elevated rounded-full overflow-hidden">
          <div
            className="h-full rounded-full transition-all duration-500"
            style={{ width: `${s.soc}%`, backgroundColor: color }}
          />
        </div>
      </div>

    </div>
  );
}

const BatteryPanel = memo(BatteryPanelInner);
export default BatteryPanel;
