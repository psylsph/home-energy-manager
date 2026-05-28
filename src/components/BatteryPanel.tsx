import { useState } from 'react';
import type { InverterSnapshot } from '../lib/types';
import { formatPower, formatPercent, formatTemp, formatEnergy, formatVoltage } from '../lib/format';

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

export default function BatteryPanel({ snapshot: s }: Props) {
  const [modulesOpen, setModulesOpen] = useState(false);
  const color = socColor(s.soc);
  const storedKwh = (s.soc / 100) * s.battery_capacity_kwh;

  return (
    <div className="bg-bg-surface rounded-2xl p-6 flex flex-col gap-5">
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
            <span className="text-text-primary text-lg font-bold font-mono">
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
        </div>
      </div>

      {/* Capacity bar */}
      <div className="space-y-1">
        <div className="flex justify-between text-xs">
          <span className="text-text-secondary">Stored</span>
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

      {/* Collapsible modules */}
      {s.battery_modules.length > 0 && (
        <div>
          <button
            type="button"
            className="text-xs text-text-secondary hover:text-text-primary transition-colors flex items-center gap-1.5 py-1"
            onClick={() => setModulesOpen((v) => !v)}
          >
            <svg
              className={`w-3 h-3 transition-transform ${modulesOpen ? 'rotate-90' : ''}`}
              fill="currentColor"
              viewBox="0 0 20 20"
            >
              <path d="M6 4l8 6-8 6V4z" />
            </svg>
            {s.battery_modules.length} module{s.battery_modules.length > 1 ? 's' : ''}
          </button>
          {modulesOpen && (
            <div className="mt-2 flex flex-col gap-1">
              {s.battery_modules.map((m) => (
                <div
                  key={m.index}
                  className="bg-bg-elevated rounded-lg px-3 py-2 flex items-center justify-between text-xs font-mono"
                >
                  <span className="text-text-secondary w-8">#{m.index + 1}</span>
                  <span className="text-text-primary">{formatVoltage(m.voltage)}</span>
                  <span className="text-text-primary">{formatPercent(m.soc)}</span>
                  <span className="text-text-secondary">{formatTemp(m.temperature)}</span>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
