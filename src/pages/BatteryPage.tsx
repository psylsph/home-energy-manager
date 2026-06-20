import { useState } from 'react';
import { useInverterStore } from '../store/useInverterStore';
import { formatPercent, formatVoltage, formatTemp } from '../lib/format';
import ColdBatteryWarning from '../components/ColdBatteryWarning';
import BatteryPanel from '../components/BatteryPanel';
import BatterySocChart from '../components/BatterySocChart';

const BMS_REGISTER_LABELS = ['IR90', 'IR91', 'IR92', 'IR93', 'IR94'];
const BMS_STATUS_LABELS = ['status_1', 'status_2', 'status_3', 'status_4', 'status_5', 'status_6', 'status_7'];
const BMS_WARNING_LABELS = ['warning_1', 'warning_2'];

function formatHex(value: number, width: number): string {
  return `0x${Math.trunc(value).toString(16).toUpperCase().padStart(width, '0')}`;
}

export default function BatteryPage() {
  const { snapshot, developerMode, panelGraphsEnabled } = useInverterStore();
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

  return (
    <div className="flex flex-col gap-6 max-w-2xl mx-auto">
      <ColdBatteryWarning />
      {/* SOC overview card */}
      <BatteryPanel snapshot={s} />

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
                      <div className="grid grid-cols-[20rem_1fr] gap-x-6 gap-y-1 text-xs">
                        {m.serial && (
                          <>
                            <span className="text-text-secondary">Serial</span>
                            <span className="text-text-primary font-mono text-right">{m.serial}</span>
                          </>
                        )}
                        <span className="text-text-secondary">Cells</span>
                        <span className="text-text-primary font-mono text-right">{m.num_cells || cellCount}</span>
                        {m.num_cycles > 0 && (
                          <>
                            <span className="text-text-secondary">Cycles</span>
                            <span className="text-text-primary font-mono text-right">{m.num_cycles}</span>
                          </>
                        )}
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
                        {m.design_capacity_ah > 0 && m.capacity_ah > 0 && (
                          <>
                            <span className="text-text-secondary">State of Health (current vs design capacity)</span>
                            <span className="text-text-primary font-mono text-right">{(m.capacity_ah / m.design_capacity_ah * 100).toFixed(0)}%</span>
                          </>
                        )}
                        {/* Total Throughput + Battery Life Remaining.
                            Decoded from IR(6-7) `e_battery_throughput` (uint32 /10 kWh)
                            and re-derived from IR(180)+IR(181) for AC coupled / AIO
                            where the dongle leaves IR(6-7) empty.

                            Caveat for AC coupled inverters (DTC 3001 / 3002): GivEnergy
                            firmware does NOT populate any lifetime throughput register
                            on these models — neither IR(6-7) nor IR(180)/IR(181) report
                            a real value. Confirmed against the givenergy-modbus
                            reference library's `_BATTERY_ENERGY_SOURCE` table, which
                            deliberately leaves the `total` slot undeclared for
                            `Model.AC` / `Model.AC_3PH` (only `today` is mapped), and
                            AIO follows the same shape. The rows are therefore hidden
                            entirely when the meter is empty (zero value, never
                            populated) — showing two "–" rows suggests a bug rather
                            than a missing firmware feature.

                            A zero value can also occur transiently on a fresh
                            Hybrid install before any charge cycle has completed;
                            hiding the rows in that case is also fine because there
                            is nothing meaningful to display yet. */}
                        {s.battery_capacity_kwh > 0 && s.total_throughput_kwh > 0 && (
                          <>
                            <span className="text-text-secondary">Total Throughput</span>
                            <span className="text-text-primary font-mono text-right">{`${s.total_throughput_kwh.toFixed(0)} kWh`}</span>
                            <span className="text-text-secondary">Battery Life Remaining (warranty throughput remaining)</span>
                            <span className="text-text-primary font-mono text-right">{(() => {
                              const RATED_THROUGHPUT_MWH_PER_KWH = 10;
                              const throughputUsed = s.total_throughput_kwh / (s.battery_capacity_kwh * RATED_THROUGHPUT_MWH_PER_KWH * 1000);
                              const remainingPct = Math.max(0, Math.min(1, 1 - throughputUsed)) * 100;
                              return remainingPct.toFixed(0) + '%';
                            })()}</span>
                          </>
                        )}
                      </div>

                      {/* Raw LV BMS status/warning bytes — developer diagnostics only. */}
                      {developerMode && (m.bms_status_registers?.length ?? 0) > 0 && (
                        <div className="space-y-2 rounded-lg border border-yellow-500/25 bg-yellow-500/5 p-3">
                          <div>
                            <div className="text-yellow-400 text-xs font-semibold">Developer: Raw BMS Status Registers</div>
                            <div className="text-text-secondary text-[11px]">
                              Undocumented LV BMS bytes from IR90–IR94. Use these for before/after comparisons.
                            </div>
                          </div>
                          <div className="grid grid-cols-5 gap-1 text-[11px] font-mono">
                            {(m.bms_status_registers ?? []).map((value, i) => (
                              <div key={BMS_REGISTER_LABELS[i] ?? i} className="rounded bg-bg-elevated px-2 py-1 text-center">
                                <div className="text-text-secondary">{BMS_REGISTER_LABELS[i] ?? `IR${90 + i}`}</div>
                                <div className="text-text-primary">{formatHex(value, 4)}</div>
                                <div className="text-text-secondary">{value}</div>
                              </div>
                            ))}
                          </div>
                          <div className="grid grid-cols-3 sm:grid-cols-5 gap-1 text-[11px] font-mono">
                            {(m.bms_status ?? []).map((value, i) => (
                              <div key={BMS_STATUS_LABELS[i] ?? i} className="rounded bg-bg-elevated px-2 py-1">
                                <span className="text-text-secondary">{BMS_STATUS_LABELS[i] ?? `status_${i + 1}`}</span>
                                <span className="float-right text-text-primary">{value} / {formatHex(value, 2)}</span>
                              </div>
                            ))}
                            {(m.bms_warnings ?? []).map((value, i) => (
                              <div key={BMS_WARNING_LABELS[i] ?? i} className="rounded bg-bg-elevated px-2 py-1">
                                <span className="text-text-secondary">{BMS_WARNING_LABELS[i] ?? `warning_${i + 1}`}</span>
                                <span className="float-right text-yellow-400">{value} / {formatHex(value, 2)}</span>
                              </div>
                            ))}
                          </div>
                        </div>
                      )}

                      {/* Cell voltage chart */}
                      {cellCount > 0 && (
                        <div className="space-y-1">
                          <div className="text-text-secondary text-xs">Cell Voltages</div>
                          <div className="flex gap-2">
                            {/* Y-axis labels */}
                            {(() => {
                              const voltages = m.cell_voltages;
                              const minV = Math.min(...voltages);
                              const maxV = Math.max(...voltages);
                              return (
                                <div className="flex flex-col justify-between text-[10px] font-mono text-text-secondary pb-5 select-none">
                                  <span>{Math.floor(maxV * 1000) / 1000}V</span>
                                  <span>{Math.floor(((maxV + minV) / 2) * 1000) / 1000}V</span>
                                  <span>{Math.floor(minV * 1000) / 1000}V</span>
                                </div>
                              );
                            })()}
                            {/* Bars */}
                            <div className="flex-1">
                              <div className="flex items-end gap-px h-24">
                                {(() => {
                                  const voltages = m.cell_voltages;
                                  const minV = Math.min(...voltages);
                                  const maxV = Math.max(...voltages);
                                  const range = maxV - minV;
                                  const scale = (v: number) =>
                                    range > 0.001
                                      ? Math.max(0, Math.min(100, ((v - minV) / range) * 100))
                                      : 100;
                                  return voltages.map((v, i) => {
                                    const pct = scale(v);
                                    return (
                                      <div key={i} className="flex-1 self-stretch flex flex-col items-center justify-end">
                                        <div
                                          className="w-full rounded-t-sm"
                                          style={{
                                            height: `${pct}%`,
                                            backgroundColor:
                                              v < 2.8 ? '#EF4444' : v < 3.0 ? '#F59E0B' : '#22C55E',
                                            minHeight: '2px',
                                          }}
                                          title={`Cell ${i + 1}: ${v.toFixed(3)}V (${pct.toFixed(0)}%)`}
                                        />
                                      </div>
                                    );
                                  });
                                })()}
                              </div>
                              {/* X-axis cell labels — show every Nth label to avoid crowding */}
                              <div className="flex text-[10px] font-mono text-text-secondary mt-0.5 select-none">
                                {(() => {
                                  const count = m.cell_voltages.length;
                                  const step = count > 16 ? Math.floor(count / 8) : 1;
                                  return Array.from({ length: count }).map((_, i) => (
                                    <div key={i} className="flex-1 text-center">
                                      {i % step === 0 && i + 1}
                                    </div>
                                  ));
                                })()}
                              </div>
                            </div>
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
      {/* No battery modules — show explanation for batteryless devices */}
      {s.battery_modules.length === 0 && (
        <section className="bg-bg-surface rounded-2xl p-6">
          <div className="bg-bg-elevated/50 rounded-lg px-4 py-3">
            <p className="text-text-secondary text-sm">
              No battery module data available.
            </p>
            <p className="text-text-secondary/60 text-xs mt-2">
              {s.device_type_code?.startsWith('70')
                ? 'The Gateway does not expose per-cell telemetry. Battery cell data requires a direct connection to each AIO unit (not yet supported).'
                : 'Battery module data will appear once detected by the inverter.'}
            </p>
          </div>
        </section>
      )}

      {/* SOC trend — replicates the History → Battery "SOC %" chart
          so the Battery tab is self-contained (issue #81). Hidden when the
          user disables the "Panel Graphs" toggle in Settings. */}
      {panelGraphsEnabled && <BatterySocChart />}
    </div>
  );
}
