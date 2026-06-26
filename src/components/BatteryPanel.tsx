import { memo } from 'react';
import type { InverterSnapshot } from '../lib/types';
import { formatPower, formatPercent, formatVoltage, formatCurrent, formatTemp, formatEnergy } from '../lib/format';
import { batteryModeDisplayLabel } from '../lib/energyFlow';
import BatteryGauge from './BatteryGauge';

interface Props {
  snapshot: InverterSnapshot;
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
  const modeLabel = batteryModeDisplayLabel(
    s.battery_mode, s.cosy_active, s.cosy_enabled,
    s.enable_charge, s.enable_discharge,
    s.charge_slots, s.discharge_slots,
  );

  return (
    <section className="bg-bg-surface rounded-2xl p-6 h-full flex flex-col sm:flex-row items-center gap-6">
      <div className="relative w-32 h-32 shrink-0 flex items-center justify-center">
        <BatteryGauge soc={s.soc} width={88} />
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
          <span className="text-text-primary font-mono text-right">{modeLabel}</span>
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
