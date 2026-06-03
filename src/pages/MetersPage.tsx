import { useInverterStore } from '../store/useInverterStore';
import type { MeterData } from '../lib/types';

function MeterCard({ meter }: { meter: MeterData }) {
  const dir = meter.p_active_total >= 0 ? '↓ Import' : '↑ Export';
  const absPower = Math.abs(meter.p_active_total);
  return (
    <div className="bg-bg-surface rounded-xl p-4 space-y-3">
      <div className="flex items-center justify-between">
        <span className="text-text-primary font-medium">Meter 0x{meter.address.toString(16).padStart(2, '0')}</span>
        <span className="text-xs text-text-secondary font-mono">{meter.frequency.toFixed(2)} Hz</span>
      </div>

      {/* Power summary */}
      <div className="flex items-baseline gap-2">
        <span className="text-2xl font-bold font-mono text-flow-active">{absPower.toFixed(0)}</span>
        <span className="text-sm text-text-secondary">W {dir}</span>
      </div>

      {/* Voltages */}
      <div className="grid grid-cols-3 gap-2 text-center">
        <div>
          <div className="text-xs text-text-secondary">L1</div>
          <div className="font-mono text-sm text-text-primary">{meter.v_phase_1.toFixed(1)}V</div>
          <div className="font-mono text-xs text-text-secondary">{meter.i_phase_1.toFixed(2)}A</div>
          <div className="font-mono text-xs text-text-secondary">{meter.p_active_phase_1 >= 0 ? '+' : ''}{meter.p_active_phase_1}W</div>
        </div>
        <div>
          <div className="text-xs text-text-secondary">L2</div>
          <div className="font-mono text-sm text-text-primary">{meter.v_phase_2.toFixed(1)}V</div>
          <div className="font-mono text-xs text-text-secondary">{meter.i_phase_2.toFixed(2)}A</div>
          <div className="font-mono text-xs text-text-secondary">{meter.p_active_phase_2 >= 0 ? '+' : ''}{meter.p_active_phase_2}W</div>
        </div>
        <div>
          <div className="text-xs text-text-secondary">L3</div>
          <div className="font-mono text-sm text-text-primary">{meter.v_phase_3.toFixed(1)}V</div>
          <div className="font-mono text-xs text-text-secondary">{meter.i_phase_3.toFixed(2)}A</div>
          <div className="font-mono text-xs text-text-secondary">{meter.p_active_phase_3 >= 0 ? '+' : ''}{meter.p_active_phase_3}W</div>
        </div>
      </div>

      {/* Totals */}
      <div className="grid grid-cols-3 gap-2 text-center border-t border-white/5 pt-3">
        <div>
          <div className="text-xs text-text-secondary">Total Current</div>
          <div className="font-mono text-sm text-text-primary">{meter.i_total.toFixed(2)}A</div>
        </div>
        <div>
          <div className="text-xs text-text-secondary">Power Factor</div>
          <div className="font-mono text-sm text-text-primary">{meter.pf_total.toFixed(3)}</div>
        </div>
        <div>
          <div className="text-xs text-text-secondary">Apparent</div>
          <div className="font-mono text-sm text-text-primary">{meter.p_apparent_total.toFixed(0)} VA</div>
        </div>
      </div>

      {/* Energy */}
      <div className="grid grid-cols-2 gap-2 text-center border-t border-white/5 pt-3">
        <div>
          <div className="text-xs text-text-secondary">Import Today</div>
          <div className="font-mono text-sm text-green-400">{meter.e_import_active_kwh.toFixed(1)} kWh</div>
        </div>
        <div>
          <div className="text-xs text-text-secondary">Export Today</div>
          <div className="font-mono text-sm text-amber-400">{meter.e_export_active_kwh.toFixed(1)} kWh</div>
        </div>
      </div>
    </div>
  );
}

export default function MetersPage() {
  const snapshot = useInverterStore((s) => s.snapshot);
  const meters = snapshot?.meters;

  return (
    <div className="flex flex-col gap-4 max-w-2xl mx-auto px-4 py-6">
      <h2 className="text-text-primary font-semibold text-lg">External CT Meters</h2>

      {!meters || meters.length === 0 ? (
        <div className="bg-bg-surface rounded-xl p-6 text-center">
          <p className="text-text-secondary">
            {snapshot
              ? 'No external CT meters detected on your system.'
              : 'Connect to an inverter to see meter data.'}
          </p>
        </div>
      ) : (
        <div className="space-y-4">
          {meters.map((m) => (
            <MeterCard key={m.address} meter={m} />
          ))}
        </div>
      )}
    </div>
  );
}
