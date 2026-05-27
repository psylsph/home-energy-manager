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
    case 'paused': return 'Paused';
    default: return state;
  }
}

export default function BatteryPanel({ snapshot: s }: Props) {
  const [modulesOpen, setModulesOpen] = useState(false);
  const color = socColor(s.soc);
  const storedKwh = (s.soc / 100) * s.battery_capacity_kwh;

  return (
    <div className="bg-bg-surface rounded-xl p-5 flex flex-col gap-4">
      <h2 className="text-text-primary text-lg font-semibold font-sans">Battery</h2>

      {/* SOC bar */}
      <div>
        <div className="flex justify-between text-sm mb-1">
          <span className="text-text-secondary">State of Charge</span>
          <span className="text-text-primary font-mono font-semibold">{formatPercent(s.soc)}</span>
        </div>
        <div className="h-4 bg-bg-elevated rounded-full overflow-hidden">
          <div
            className="h-full rounded-full transition-all duration-500"
            style={{ width: `${s.soc}%`, backgroundColor: color }}
          />
        </div>
      </div>

      {/* Power */}
      <div className="flex items-center gap-2">
        <span className="text-text-secondary text-sm">Power</span>
        <span className="text-text-primary font-mono font-semibold">
          {s.battery_state === 'discharging' ? '▼' : s.battery_state === 'charging' ? '▲' : '—'}
        </span>
        <span className="text-text-primary font-mono font-semibold">
          {formatPower(Math.abs(s.battery_power))}
        </span>
      </div>

      {/* State */}
      <div className="flex items-center gap-2">
        <span className="text-text-secondary text-sm">State</span>
        <span className="text-sm text-text-primary font-semibold font-sans">
          {stateLabel(s.battery_state)}
        </span>
      </div>

      {/* Temperature */}
      <div className="flex items-center gap-2">
        <span className="text-text-secondary text-sm">Temperature</span>
        <span className="text-text-primary font-mono text-sm">{formatTemp(s.battery_temperature)}</span>
      </div>

      {/* Capacity */}
      <div className="flex items-center gap-2">
        <span className="text-text-secondary text-sm">Capacity</span>
        <span className="text-text-primary font-mono text-sm">
          {formatEnergy(storedKwh)} / {formatEnergy(s.battery_capacity_kwh)}
        </span>
      </div>

      {/* Collapsible modules */}
      {s.battery_modules.length > 0 && (
        <div>
          <button
            type="button"
            className="text-sm text-text-secondary hover:text-text-primary transition-colors flex items-center gap-1"
            onClick={() => setModulesOpen((v) => !v)}
          >
            <span style={{ transform: modulesOpen ? 'rotate(90deg)' : 'none', transition: 'transform 0.2s' }}>▶</span>
            Modules ({s.battery_modules.length})
          </button>
          {modulesOpen && (
            <div className="mt-2 flex flex-col gap-1.5">
              {s.battery_modules.map((m) => (
                <div
                  key={m.index}
                  className="bg-bg-elevated rounded-lg px-3 py-2 flex items-center justify-between text-xs font-mono"
                >
                  <span className="text-text-secondary">#{m.index + 1}</span>
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
