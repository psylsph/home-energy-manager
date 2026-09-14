import { useInverterStore } from '../store/useInverterStore';
import type { InverterSnapshot, MeterData } from '../lib/types';
import AwaitingConnection from '../components/AwaitingConnection';

/** A phase is "active" if it has a plausible non-zero voltage (>10 V). */
const VOLTAGE_THRESHOLD = 10;

function MeterCard({ meter }: { meter: MeterData }) {
  const dir = meter.p_active_total >= 0 ? '↓ Import' : '↑ Export';
  const absPower = Math.abs(meter.p_active_total);

  const hasL2 = meter.v_phase_2 > VOLTAGE_THRESHOLD;
  const hasL3 = meter.v_phase_3 > VOLTAGE_THRESHOLD;
  const showThreePhase = hasL2 || hasL3;

  // Per-phase power is unavailable for the synthetic built-in CT (address 0x00)
  // — three-phase inverters have no per-phase signed grid power registers.
  // A synthetic meter never represents an external clamp, so its zeroed
  // per-phase fields must not be rendered as a "broken external meter".
  const isSynthetic = meter.address === 0;
  const showPerPhasePower = !isSynthetic;

  const phaseCols = showThreePhase ? 'grid-cols-3' : 'grid-cols-1';

  return (
    <div className="bg-bg-surface rounded-xl p-4 space-y-3">
      <div className="flex items-center justify-between">
        <span className="text-text-primary font-medium">
          {isSynthetic ? 'Built-in Grid CT' : `Meter 0x${meter.address.toString(16).padStart(2, '0')}`}
        </span>
        <span className="text-xs text-text-secondary font-mono">{meter.frequency.toFixed(2)} Hz</span>
      </div>

      {/* Power summary */}
      <div className="flex items-baseline gap-2">
        <span className="text-2xl font-bold font-mono text-flow-active">{absPower.toFixed(0)}</span>
        <span className="text-sm text-text-secondary">W {dir}</span>
      </div>

      {/* Per-phase readings — only show phases with real voltage */}
      <div className={`grid ${phaseCols} gap-2 text-center`}>
        <div>
          <div className="text-xs text-text-secondary">L1</div>
          <div className="font-mono text-sm text-text-primary">{meter.v_phase_1.toFixed(1)}V</div>
          <div className="font-mono text-xs text-text-secondary">{meter.i_phase_1.toFixed(2)}A</div>
          {showPerPhasePower && <div className="font-mono text-xs text-text-secondary">{meter.p_active_phase_1 >= 0 ? '+' : ''}{meter.p_active_phase_1}W</div>}
        </div>
        {showThreePhase && (
          <div>
            <div className="text-xs text-text-secondary">L2</div>
            <div className="font-mono text-sm text-text-primary">{meter.v_phase_2.toFixed(1)}V</div>
            <div className="font-mono text-xs text-text-secondary">{meter.i_phase_2.toFixed(2)}A</div>
            {showPerPhasePower && <div className="font-mono text-xs text-text-secondary">{meter.p_active_phase_2 >= 0 ? '+' : ''}{meter.p_active_phase_2}W</div>}
          </div>
        )}
        {showThreePhase && (
          <div>
            <div className="text-xs text-text-secondary">L3</div>
            <div className="font-mono text-sm text-text-primary">{meter.v_phase_3.toFixed(1)}V</div>
            <div className="font-mono text-xs text-text-secondary">{meter.i_phase_3.toFixed(2)}A</div>
            {showPerPhasePower && <div className="font-mono text-xs text-text-secondary">{meter.p_active_phase_3 >= 0 ? '+' : ''}{meter.p_active_phase_3}W</div>}
          </div>
        )}
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

      {/* Raw meter energy counters are uint16 deci-kWh registers and wrap
          every 6553.6 kWh. They are intentionally not labelled as lifetime
          totals; use the Inverter page for those. */}
      <div className="grid grid-cols-2 gap-2 text-center border-t border-white/5 pt-3">
        <div>
          <div className="text-xs text-text-secondary">Meter Import Counter</div>
          <div className="font-mono text-sm text-green-400">{meter.e_import_active_kwh.toFixed(1)} kWh</div>
        </div>
        <div>
          <div className="text-xs text-text-secondary">Meter Export Counter</div>
          <div className="font-mono text-sm text-amber-400">{meter.e_export_active_kwh.toFixed(1)} kWh</div>
        </div>
      </div>
    </div>
  );
}

function CtConfigCard({ snapshot }: { snapshot: InverterSnapshot }) {
  const meterTypeLabel = (() => {
    switch (snapshot.meter_type) {
      case 0: return 'CT / EM418';
      case 1: return 'EM115';
      default: return snapshot.meter_type > 0 ? `Unknown (${snapshot.meter_type})` : null;
    }
  })();

  return (
    <div className="bg-bg-surface rounded-xl p-4 space-y-3">
      <h3 className="text-text-primary font-medium">CT Clamp Configuration</h3>
      <div className="grid grid-cols-3 gap-3 text-center">
        <div>
          <div className="text-xs text-text-secondary">Ammeter Enabled</div>
          <div className={`font-mono text-sm font-medium ${snapshot.enable_ammeter ? 'text-green-400' : 'text-red-400'}`}>
            {snapshot.enable_ammeter ? 'Yes' : 'No'}
          </div>
        </div>
        <div>
          <div className="text-xs text-text-secondary">Reversed CT</div>
          <div className={`font-mono text-sm font-medium ${snapshot.enable_reversed_ct_clamp ? 'text-amber-400' : 'text-text-primary'}`}>
            {snapshot.enable_reversed_ct_clamp ? 'Yes' : 'No'}
          </div>
        </div>
        <div>
          <div className="text-xs text-text-secondary">Meter Type</div>
          <div className="font-mono text-sm text-text-primary">
            {meterTypeLabel ?? '—'}
          </div>
        </div>
      </div>
      {!snapshot.enable_ammeter && (
        <p className="text-xs text-text-secondary">
          External CT ammeter is disabled. Enable it in the inverter settings if you have a CT clamp installed.
        </p>
      )}
    </div>
  );
}

export default function MetersPage() {
  const snapshot = useInverterStore((s) => s.snapshot);
  const connectionState = useInverterStore((s) => s.connectionState);
  const meters = snapshot?.meters;

  // Same gate as Battery / Solar / Inverter / Control: while the backend
  // has no usable connection, render the shared placeholder instead of a
  // half-populated meter list bound to stale data. Previously this page
  // fell through to an inline "Connect to an inverter" card, whose wording
  // and behaviour didn't match the other tabs.
  if (!snapshot || connectionState !== 'connected') {
    return <AwaitingConnection connectionState={connectionState} showFaq />;
  }

  return (
    <div className="flex flex-col gap-4 max-w-2xl mx-auto px-4 py-6">
      <h2 className="text-text-primary font-semibold text-lg">External CT Meters</h2>
      <CtConfigCard snapshot={snapshot} />

      {!meters || meters.length === 0 ? (
        <div className="bg-bg-surface rounded-xl p-6 text-center">
          <p className="text-text-secondary">
            No external CT meters detected on your system.
          </p>
        </div>
      ) : (
        <div className="space-y-4">
          {meters.map((m) => (
            <MeterCard key={m.address} meter={m} />
          ))}
        </div>
      )}

      <aside
        role="note"
        aria-label="Meter counter note"
        className="rounded-xl border border-amber-500/30 bg-amber-500/10 p-4 shadow-sm"
      >
        <div className="flex items-start gap-3">
          <div className="mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-amber-500/15 text-amber-500">
            <svg aria-hidden="true" className="h-4 w-4" viewBox="0 0 24 24" fill="currentColor">
              <path d="M12 3.5 1.8 20.5h20.4L12 3.5Zm0 4.2 6.5 10.8h-13L12 7.7Zm-1 3.1v4.1h2v-4.1h-2Zm0 5.2v2h2v-2h-2Z" />
            </svg>
          </div>
          <div className="min-w-0">
            <p className="text-xs leading-relaxed text-text-secondary">
              The import and export counters values wrap. Check the Inverter page for accurate lifetime Import and Export totals.
            </p>
          </div>
        </div>
      </aside>
    </div>
  );
}
