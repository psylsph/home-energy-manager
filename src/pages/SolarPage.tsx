import { useInverterStore } from '../store/useInverterStore';
import { formatPower, formatEnergy, formatVoltage, formatCurrent } from '../lib/format';
import SolarPowerChart from '../components/SolarPowerChart';

function pvColor(pv: number): string {
  return pv === 1 ? '#F59E0B' : '#3B82F6';
}

export default function SolarPage() {
  const { snapshot, connectionState, panelGraphsEnabled } = useInverterStore();

  if (!snapshot) {
    return (
      <div className="flex flex-col items-center justify-center min-h-[60vh] gap-4">
        <div className="w-10 h-10 border-4 border-flow-active border-t-transparent rounded-full animate-spin" />
        <p className="text-text-secondary text-sm font-sans">
          {connectionState === 'reconnecting'
            ? 'Connection lost — reconnecting…'
            : connectionState === 'disconnected'
              ? 'Disconnected — will retry automatically'
              : 'Waiting for data'}
        </p>
        <p className="text-text-secondary/60 text-xs font-sans text-center max-w-xs">
          If data doesn't appear, try restarting the app and check your firewall settings.
          See the <a href="https://github.com/psylsph/home-energy-manager/blob/master/FAQ.md" target="_blank" rel="noopener noreferrer" className="text-flow-active hover:underline">FAQ</a> for help.
        </p>
      </div>
    );
  }

  const hasPv2 = snapshot.pv2_voltage > 0 || snapshot.pv2_power > 0;

  return (
    <div className="flex flex-col gap-4 max-w-4xl mx-auto">

      {/* Summary bar */}
      <section className="bg-bg-surface rounded-2xl p-5">
        <h2 className="text-text-primary font-semibold text-lg mb-1">Solar Overview</h2>
        <p className="text-4xl font-bold text-amber-400 mb-1">{formatPower(snapshot.solar_power)}</p>
        <p className="text-text-secondary text-xs">Total Solar Power</p>
      </section>


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
            <span className="text-text-primary font-mono text-right">{formatEnergy(snapshot.today_solar_kwh ?? 0)}</span>
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
