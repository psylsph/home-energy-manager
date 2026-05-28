import { useInverterStore } from '../store/useInverterStore';
import { formatPower, formatPercent, formatVoltage, formatCurrent, formatTemp, formatEnergy } from '../lib/format';

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

export default function BatteryPage() {
  const { snapshot } = useInverterStore();

  if (!snapshot) {
    return (
      <div className="flex flex-col items-center justify-center min-h-[60vh] gap-4">
        <div className="w-10 h-10 border-4 border-flow-active border-t-transparent rounded-full animate-spin" />
        <p className="text-text-secondary text-sm font-sans">Waiting for data…</p>
      </div>
    );
  }

  const s = snapshot;
  const color = socColor(s.soc);
  const storedKwh = (s.soc / 100) * s.battery_capacity_kwh;

  return (
    <div className="flex flex-col gap-6 max-w-2xl mx-auto">
      {/* SOC overview card */}
      <section className="bg-bg-surface rounded-2xl p-6 flex flex-col sm:flex-row items-center gap-6">
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
          <div className="grid grid-cols-2 gap-x-6 gap-y-1.5 text-sm">
            <span className="text-text-secondary">Power</span>
            <span className="text-text-primary font-mono text-right">
              {s.battery_state === 'discharging' ? '−' : s.battery_state === 'charging' ? '+' : ''}
              {formatPower(Math.abs(s.battery_power))}
            </span>
            <span className="text-text-secondary">Voltage</span>
            <span className="text-text-primary font-mono text-right">{formatVoltage(s.battery_voltage)}</span>
            <span className="text-text-secondary">Current</span>
            <span className="text-text-primary font-mono text-right">{formatCurrent(s.battery_current)}</span>
            <span className="text-text-secondary">Temperature</span>
            <span className="text-text-primary font-mono text-right">{formatTemp(s.battery_temperature)}</span>
            <span className="text-text-secondary">Mode</span>
            <span className="text-text-primary font-mono text-right">{BATTERY_MODE_LABELS[s.battery_mode] ?? s.battery_mode}</span>
            <span className="text-text-secondary">Reserve</span>
            <span className="text-text-primary font-mono text-right">{formatPercent(s.battery_reserve)}</span>
          </div>
        </div>
      </section>

      {/* Stored energy bar */}
      <section className="bg-bg-surface rounded-2xl p-6 space-y-3">
        <h3 className="text-text-primary text-sm font-semibold tracking-wide">Stored Energy</h3>
        <div className="flex justify-between text-sm">
          <span className="text-text-secondary">Capacity</span>
          <span className="text-text-primary font-mono">{formatEnergy(s.battery_capacity_kwh)}</span>
        </div>
        <div className="flex justify-between text-sm">
          <span className="text-text-secondary">Available</span>
          <span className="text-text-primary font-mono">{formatEnergy(storedKwh)}</span>
        </div>
        <div className="h-2 bg-bg-elevated rounded-full overflow-hidden">
          <div
            className="h-full rounded-full transition-all duration-500"
            style={{ width: `${s.soc}%`, backgroundColor: color }}
          />
        </div>
      </section>

      {/* Battery modules */}
      {s.battery_modules.length > 0 && (
        <section className="bg-bg-surface rounded-2xl p-6 space-y-4">
          <h3 className="text-text-primary text-sm font-semibold tracking-wide">
            Modules ({s.battery_modules.length})
          </h3>
          <div className="flex flex-col gap-2">
            {s.battery_modules.map((m) => (
              <div
                key={m.index}
                className="bg-bg-elevated rounded-xl px-4 py-3 grid grid-cols-4 gap-2 text-sm"
              >
                <div>
                  <div className="text-text-secondary text-xs">Module</div>
                  <div className="text-text-primary font-mono">#{m.index + 1}</div>
                </div>
                <div>
                  <div className="text-text-secondary text-xs">SOC</div>
                  <div className="text-text-primary font-mono">{formatPercent(m.soc)}</div>
                </div>
                <div>
                  <div className="text-text-secondary text-xs">Voltage</div>
                  <div className="text-text-primary font-mono">{formatVoltage(m.voltage)}</div>
                </div>
                <div>
                  <div className="text-text-secondary text-xs">Temp</div>
                  <div className="text-text-primary font-mono">{formatTemp(m.temperature)}</div>
                </div>
              </div>
            ))}
          </div>
        </section>
      )}

      {/* Today's energy */}
      <section className="bg-bg-surface rounded-2xl p-6 space-y-3">
        <h3 className="text-text-primary text-sm font-semibold tracking-wide">Today</h3>
        <div className="grid grid-cols-2 gap-x-6 gap-y-1.5 text-sm">
          <span className="text-text-secondary">Charged</span>
          <span className="text-text-primary font-mono text-right">{formatEnergy(s.today_charge_kwh)}</span>
          <span className="text-text-secondary">Discharged</span>
          <span className="text-text-primary font-mono text-right">{formatEnergy(s.today_discharge_kwh)}</span>
        </div>
      </section>
    </div>
  );
}
