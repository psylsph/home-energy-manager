import { useState } from 'react';
import { useInverterStore } from '../store/useInverterStore';
import { formatPower, formatPercent, formatVoltage, formatCurrent, formatTemp, formatEnergy } from '../lib/format';
import ColdBatteryWarning from '../components/ColdBatteryWarning';

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

/** Override mode label when cosy mode is enabled, or "Override" when */
/** force charge or force discharge is active.                         */
function modeDisplayLabel(
  mode: string, cosyActive: boolean, cosyEnabled: boolean,
  enableCharge: boolean, enableDischarge: boolean, inChargeWindow: boolean,
): string {
  if (cosyActive) return 'Cosy';
  if (cosyEnabled && (mode === 'eco' || mode === 'eco_paused')) return 'Cosy';
  // Force charge is active only when the master charge-enable flag is set
  // AND the current time falls within an active charge slot window. The
  // enable_charge register (HR 96 / HR 1123) is a sticky schedule-enable
  // flag, not an instantaneous "charging now" signal.
  const forceChargeActive = enableCharge && inChargeWindow;
  const forceDischargeActive = enableDischarge;
  if (forceChargeActive || forceDischargeActive) return 'Override';
  return BATTERY_MODE_LABELS[mode] ?? mode;
}

export default function BatteryPage() {
  const { snapshot } = useInverterStore();
  const [expandedModule, setExpandedModule] = useState<number | null>(null);

  if (!snapshot) {
    return (
      <div className="flex flex-col items-center justify-center min-h-[60vh] gap-4">
        <div className="w-10 h-10 border-4 border-flow-active border-t-transparent rounded-full animate-spin" />
        <p className="text-text-secondary text-sm font-sans">Waiting for data…</p>
        <p className="text-text-secondary/60 text-xs font-sans text-center max-w-xs">
          If data doesn't appear, try restarting the app and check your firewall settings.
          See the <a href="https://github.com/psylsph/home-energy-manager/blob/master/FAQ.md" target="_blank" rel="noopener noreferrer" className="text-flow-active hover:underline">FAQ</a> for help.
        </p>
      </div>
    );
  }

  const s = snapshot;
  const color = socColor(s.soc);
  const storedKwh = (s.soc / 100) * s.battery_capacity_kwh;
  const chargeSlotActive = (s.charge_slots ?? []).some(slot => {
    if (!slot.enabled) return false;
    const now = new Date();
    const curMin = now.getHours() * 60 + now.getMinutes();
    const startMin = slot.start_hour * 60 + slot.start_minute;
    const endMin = slot.end_hour * 60 + slot.end_minute;
    return startMin < endMin
      ? curMin >= startMin && curMin < endMin
      : curMin >= startMin || curMin < endMin;
  });

  return (
    <div className="flex flex-col gap-6 max-w-2xl mx-auto">
      <ColdBatteryWarning />
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
            <span className="text-text-primary font-mono text-right">{modeDisplayLabel(s.battery_mode, s.cosy_active, s.cosy_enabled, s.enable_charge, s.enable_discharge, chargeSlotActive)}</span>
            <span className="text-text-secondary">Reserve</span>
            <span className="text-text-primary font-mono text-right">{formatPercent(s.battery_reserve)}</span>
            <span className="text-text-secondary">Charged Today</span>
            <span className="text-text-primary font-mono text-right">{formatEnergy(s.today_charge_kwh)}</span>
            <span className="text-text-secondary">Discharged Today</span>
            <span className="text-text-primary font-mono text-right">{formatEnergy(s.today_discharge_kwh)}</span>
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
            {s.battery_modules.map((m) => {
              const isExpanded = expandedModule === m.index;
              const cellCount = m.cell_voltages?.length ?? 0;
              return (
                <div key={m.index} className="flex flex-col">
                  {/* Module header row */}
                  <button
                    type="button"
                    onClick={() => setExpandedModule(isExpanded ? null : m.index)}
                    className="bg-bg-elevated rounded-xl px-4 py-3 grid grid-cols-5 gap-2 text-sm text-left w-full"
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
                    <div className="flex items-center justify-end">
                      <svg
                        className={`w-4 h-4 text-text-secondary transition-transform ${isExpanded ? 'rotate-180' : ''}`}
                        fill="currentColor"
                        viewBox="0 0 20 20"
                      >
                        <path d="M5.293 7.293a1 1 0 011.414 0L10 10.586l3.293-3.293a1 1 0 111.414 1.414l-4 4a1 1 0 01-1.414 0l-4-4a1 1 0 010-1.414z" />
                      </svg>
                    </div>
                  </button>

                  {/* Expanded details */}
                  {isExpanded && (
                    <div className="bg-bg-elevated/50 rounded-b-xl px-4 pb-4 pt-2 space-y-3 border-t border-bg-elevated">
                      {/* Module info row */}
                      <div className="grid grid-cols-2 gap-x-6 gap-y-1 text-xs">
                        {m.serial && (
                          <>
                            <span className="text-text-secondary">Serial</span>
                            <span className="text-text-primary font-mono text-right">{m.serial}</span>
                          </>
                        )}
                        <span className="text-text-secondary">Cells</span>
                        <span className="text-text-primary font-mono text-right">{m.num_cells || cellCount}</span>
                        <span className="text-text-secondary">Cycles</span>
                        <span className="text-text-primary font-mono text-right">{m.num_cycles ?? '—'}</span>
                        {m.bms_firmware > 0 && (
                          <>
                            <span className="text-text-secondary">BMS FW</span>
                            <span className="text-text-primary font-mono text-right">{m.bms_firmware}</span>
                          </>
                        )}
                        {m.design_capacity_ah > 0 && (
                          <>
                            <span className="text-text-secondary">Design Capacity</span>
                            <span className="text-text-primary font-mono text-right">{m.design_capacity_ah.toFixed(1)} Ah</span>
                          </>
                        )}
                        {m.design_capacity_ah > 0 && m.capacity_ah > 0 && (
                          <>
                            <span className="text-text-secondary">State of Health</span>
                            <span className="text-text-primary font-mono text-right">{(m.capacity_ah / m.design_capacity_ah * 100).toFixed(1)}%</span>
                          </>
                        )}
                        {m.capacity_ah > 0 && (
                          <>
                            <span className="text-text-secondary">Capacity</span>
                            <span className="text-text-primary font-mono text-right">{m.capacity_ah.toFixed(1)} Ah</span>
                          </>
                        )}
                        {m.remaining_capacity_ah > 0 && (
                          <>
                            <span className="text-text-secondary">Remaining</span>
                            <span className="text-text-primary font-mono text-right">{m.remaining_capacity_ah.toFixed(1)} Ah</span>
                          </>
                        )}
                      </div>

                      {/* Cell voltage chart */}
                      {cellCount > 0 && (
                        <div className="space-y-1">
                          <div className="text-text-secondary text-xs">Cell Voltages</div>
                          <div className="flex items-end gap-px h-8">
                            {m.cell_voltages.map((v, i) => {
                              // Typical LFP cell: 2.5V–3.65V. Scale to bar height.
                              const pct = Math.max(0, Math.min(100, ((v - 2.5) / 1.15) * 100));
                              return (
                                <div key={i} className="flex-1 flex flex-col items-center">
                                  <div
                                    className="w-full rounded-t-sm"
                                    style={{
                                      height: `${pct}%`,
                                      backgroundColor: v < 2.8 ? '#EF4444' : v < 3.0 ? '#F59E0B' : '#22C55E',
                                      minHeight: '2px',
                                    }}
                                    title={`Cell ${i + 1}: ${v.toFixed(3)}V`}
                                  />
                                </div>
                              );
                            })}
                          </div>
                          <div className="flex justify-between text-text-secondary text-[10px] font-mono">
                            <span>{m.cell_voltages[0]?.toFixed(2)}V</span>
                            <span>{m.cell_voltages[cellCount - 1]?.toFixed(2)}V</span>
                          </div>
                        </div>
                      )}

                      {/* Cell temperature probes */}
                      {m.cell_temperatures && m.cell_temperatures.length > 0 && (
                        <div className="space-y-1">
                          <div className="text-text-secondary text-xs">
                            {m.cell_temperatures.length > 8 ? 'Cell Temps' : 'Cell Group Temps'}
                          </div>
                          <div className="flex flex-wrap gap-1">
                            {m.cell_temperatures.map((t, i) => (
                              <div
                                key={i}
                                className="inline-flex items-center justify-center gap-1.5 bg-bg-elevated rounded-lg px-2 py-1 text-xs font-mono tabular-nums"
                              >
                                <span className="text-text-secondary w-[2ch] text-right">
                                  {m.cell_temperatures.length > 8 ? i + 1 : `G${i + 1}`}
                                </span>
                                <span className="text-text-primary w-[7ch] text-right">{formatTemp(t)}</span>
                              </div>
                            ))}
                          </div>
                        </div>
                      )}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        </section>
      )}
    </div>
  );
}
