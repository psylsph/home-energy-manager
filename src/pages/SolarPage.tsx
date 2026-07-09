import { useInverterStore } from '../store/useInverterStore';
import { formatPower, formatEnergy, formatVoltage, formatCurrent } from '../lib/format';
import { percentOfRated, arrayLabel, formatPercent, solarArrayColor, SOLAR_PV1_COLOR, SOLAR_PV2_COLOR } from '../lib/solarArrays';
import SolarPowerChart from '../components/SolarPowerChart';
import AwaitingConnection from '../components/AwaitingConnection';
import type { SolarArraySummary } from '../lib/types';

function pvColor(pv: number): string {
  return pv === 1 ? SOLAR_PV1_COLOR : SOLAR_PV2_COLOR;
}

export default function SolarPage() {
  const { snapshot, connectionState, panelGraphsEnabled } = useInverterStore();

  // See BatteryPage / ControlPage for why this gates on connectionState, not
  // just snapshot presence. The shared placeholder keeps wording aligned.
  if (!snapshot || connectionState !== 'connected') {
    return <AwaitingConnection connectionState={connectionState} showFaq />;
  }

  const hasPv2 = snapshot.pv2_power > 0 || snapshot.pv2_current > 0;

  return (
    <div className="flex flex-col gap-4 max-w-4xl mx-auto">

      {/* Summary bar */}
      <section className="bg-bg-surface rounded-2xl p-5">
        <h2 className="text-text-primary font-semibold text-lg mb-1">Solar Overview</h2>
        <p className="text-4xl font-bold text-amber-400 mb-1">{formatPower(snapshot.solar_power)}</p>
        <p className="text-text-secondary text-xs">Total Solar Power</p>
        <p className="text-text-primary font-mono text-sm mt-2">
          Today: {formatEnergy(snapshot.today_solar_kwh ?? 0)}
          {snapshot.today_pv1_kwh != null && snapshot.today_pv2_kwh != null && (
            <span className="text-text-secondary text-xs ml-2">
              (PV1 {formatEnergy(snapshot.today_pv1_kwh)} · PV2 {formatEnergy(snapshot.today_pv2_kwh)})
            </span>
          )}
        </p>
      </section>

      {/* Solar Arrays — "% of max" per array (issue #110). Surfaced only
          when the user has configured rated capacities in Settings (hybrid
          DC strings with a kWp, or AC-coupled CT meters labelled as solar).
          Empty by default so nothing changes for users who haven't opted
          in. Built server-side into `snapshot.solar_arrays`. */}
      {snapshot.solar_arrays && snapshot.solar_arrays.length > 0 && (
        <section className="bg-bg-surface rounded-2xl p-5" data-testid="solar-arrays">
          <h2 className="text-text-primary font-semibold text-lg mb-1">Solar Arrays</h2>
          <p className="text-text-secondary text-xs mb-3">
            Output as a percentage of each array's rated peak capacity (kWp)
          </p>
          <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
            {snapshot.solar_arrays.map((arr, i) => (
              <SolarArrayCard key={`${arr.source}-${arr.meter_address ?? i}`} arr={arr} />
            ))}
          </div>
        </section>
      )}


      {/* Detail cards */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        {/* PV1 Card */}
        <section className={`bg-bg-surface rounded-2xl p-5 ${hasPv2 ? '' : 'md:col-span-2'}`}>
          <div className="flex items-center gap-2 mb-4">
            <div className="w-3 h-3 rounded-full" style={{ backgroundColor: pvColor(1) }} />
            <h2 className="text-text-primary font-semibold text-lg">PV1</h2>
          </div>
          <div className="grid grid-cols-2 gap-x-6 gap-y-3 text-sm">
            <span className="text-text-secondary">Power</span>
            <span className="text-text-primary font-mono text-right font-semibold">{formatPower(snapshot.pv1_power)}</span>
            <span className="text-text-secondary">Voltage</span>
            <span className="text-text-primary font-mono text-right">{formatVoltage(snapshot.pv1_voltage)}</span>
            <span className="text-text-secondary">Current</span>
            <span className="text-text-primary font-mono text-right">{formatCurrent(snapshot.pv1_current)}</span>
            <span className="text-text-secondary">Today</span>
            <span className="text-text-primary font-mono text-right">{formatEnergy(snapshot.today_pv1_kwh ?? 0)}</span>
          </div>
        </section>

        {/* PV2 Card */}
        {hasPv2 && (
          <section className="bg-bg-surface rounded-2xl p-5">
            <div className="flex items-center gap-2 mb-4">
              <div className="w-3 h-3 rounded-full" style={{ backgroundColor: pvColor(2) }} />
              <h2 className="text-text-primary font-semibold text-lg">PV2</h2>
            </div>
            <div className="grid grid-cols-2 gap-x-6 gap-y-3 text-sm">
              <span className="text-text-secondary">Power</span>
              <span className="text-text-primary font-mono text-right font-semibold">{formatPower(snapshot.pv2_power)}</span>
              <span className="text-text-secondary">Voltage</span>
              <span className="text-text-primary font-mono text-right">{formatVoltage(snapshot.pv2_voltage)}</span>
              <span className="text-text-secondary">Current</span>
              <span className="text-text-primary font-mono text-right">{formatCurrent(snapshot.pv2_current)}</span>
              <span className="text-text-secondary">Today</span>
              <span className="text-text-primary font-mono text-right">{formatEnergy(snapshot.today_pv2_kwh ?? 0)}</span>
            </div>
          </section>
        )}
      </div>

      {/* Solar power trend — replicates the History → Solar "PV Power"
          chart so the Solar tab is self-contained (issue #81). Hidden when the
          user disables the "Panel Graphs" toggle in Settings. */}
      {panelGraphsEnabled && <SolarPowerChart />}

    </div>
  );
}

/** One solar array card in the "% of max" grid (issue #110). Shows the
 *  array's live output in kW plus a progress bar to its rated peak (kWp)
 *  when a rating is configured. The bar caps at 100% for visual sanity —
 *  a bright edge-of-cloud day can momentarily exceed nameplate, but a
 *  bar drawn past its track reads as a glitch. */
function SolarArrayCard({ arr }: { arr: SolarArraySummary }) {
  const pct = percentOfRated(arr.power_w, arr.rated_kw);
  const hasRating = arr.rated_kw > 0;
  // Clamp the bar fill at 100% for display; the numeric % (which can
  // exceed 100) is shown alongside so the real figure is never hidden.
  const barPct = pct == null ? 0 : Math.min(100, Math.max(0, pct));
  // Colour matches the PV Power graph (PV1 amber, PV2 blue) so a string
  // reads the same in the card and the trend chart (issue #192).
  const color = solarArrayColor(arr.source);
  return (
    <div className="bg-bg-elevated rounded-xl p-4 flex flex-col gap-2">
      <div className="flex items-center justify-between">
        <span className="text-text-primary font-semibold text-sm">{arrayLabel(arr)}</span>
        {hasRating && (
          <span className="text-text-secondary text-xs font-mono">{arr.rated_kw} kWp</span>
        )}
      </div>
      <div className="flex items-baseline gap-2">
        <span className="text-2xl font-bold font-mono" style={{ color }}>{formatPower(arr.power_w)}</span>
        {pct != null && (
          <span className="text-sm text-text-secondary">{formatPercent(pct)} of max</span>
        )}
      </div>
      {hasRating && (
        <div className="w-full h-2 rounded-full bg-white/10 overflow-hidden" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct == null ? 0 : Math.round(pct)}>
          <div
            className="h-full rounded-full transition-[width] duration-500"
            style={{ width: `${barPct}%`, backgroundColor: color }}
          />
        </div>
      )}
      {arr.today_kwh != null && (
        <div className="text-text-secondary text-xs font-mono">
          Today: {formatEnergy(arr.today_kwh)}
        </div>
      )}
    </div>
  );
}
