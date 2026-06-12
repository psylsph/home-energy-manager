import { useInverterStore } from '../store/useInverterStore';
import EnergyFlowDiagram from '../components/EnergyFlowDiagram';
import BatteryPanel from '../components/BatteryPanel';
import SummaryTiles from '../components/SummaryTiles';
import { formatPercent, formatPower } from '../lib/format';
import { gridFaultAdvice, gridFaultReason, gridFaultTitle, hasGridFault } from '../lib/gridFault';

export default function StatusPage() {
  const { snapshot, connectionState, evcHost, evcPower, evcCharging, evcConnected } = useInverterStore();

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
          If you've recently factory-reset your dongle, make sure the <strong>WiFi-UART</strong>
          setting is <strong>Server</strong> (not Client).
          See the <a href="https://github.com/psylsph/home-energy-manager/blob/master/FAQ.md" target="_blank" rel="noopener noreferrer" className="text-flow-active hover:underline">FAQ</a> for help.
        </p>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4 max-w-4xl mx-auto">
      {hasGridFault(snapshot) && (
        <section className="rounded-2xl border border-red-500/40 bg-red-950/50 px-4 py-3 text-red-100 shadow-lg shadow-red-950/20">
          <div className="flex items-start gap-3">
            <span className="text-2xl" aria-hidden="true">⚠️</span>
            <div className="flex flex-col gap-1">
              <h2 className="text-sm font-semibold uppercase tracking-wide">
                {gridFaultTitle(snapshot)}
              </h2>
              <p className="text-sm text-red-100/90">
                The inverter is reporting <strong>{gridFaultReason(snapshot)}</strong>.
                Battery SOC is {formatPercent(snapshot.soc)}
                {snapshot.battery_power < 0 ? ` and the battery is discharging at ${formatPower(Math.abs(snapshot.battery_power))}` : ''}.{gridFaultAdvice(snapshot)}
              </p>
            </div>
          </div>
        </section>
      )}

      {/* Energy flow diagram — full width card */}
      <section className="bg-bg-surface rounded-2xl p-4">
        <EnergyFlowDiagram
          snapshot={snapshot}
          evcPower={evcPower}
          evcCharging={evcCharging}
          evcConnected={evcConnected}
          showEvc={!!evcHost}
        />
      </section>

      {/* Battery + Summary side by side on md+ */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <BatteryPanel snapshot={snapshot} />
        <SummaryTiles snapshot={snapshot} />
      </div>

      {/* Data accuracy warning */}
      <p className="text-text-secondary/40 text-xs text-center leading-relaxed mx-auto">
        Data is polled from the inverter based on the Refresh Interval on the Settings pane. The app attempts to filter out
        erroneous values, which can slow updates.
      </p>
    </div>
  );
}
