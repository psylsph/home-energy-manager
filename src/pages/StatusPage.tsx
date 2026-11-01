import { useInverterStore } from '../store/useInverterStore';
import { useReconnect } from '../hooks/useReconnect';
import { useNow } from '../hooks/useNow';
import EnergyOrbitDiagram from '../components/EnergyOrbitDiagram';
import HouseEnergyView from '../components/house/HouseEnergyView';
import SummaryTiles from '../components/SummaryTiles';
import HouseStatusCards from '../components/house/HouseStatusCards';
import ColdBatteryWarning from '../components/ColdBatteryWarning';
import AwaitingConnection from '../components/AwaitingConnection';
import { formatPercent, formatPower } from '../lib/format';
import { gridFaultAdvice, gridFaultReason, gridFaultTitle, hasGridFault } from '../lib/gridFault';

/** Format a duration in seconds to a human-readable string. */
function formatDuration(totalSec: number): string {
  if (totalSec < 60) return `${Math.floor(totalSec)}s`;
  if (totalSec < 3600) {
    const m = Math.floor(totalSec / 60);
    const s = Math.floor(totalSec % 60);
    return `${m}m ${s}s`;
  }
  const h = Math.floor(totalSec / 3600);
  const m = Math.floor((totalSec % 3600) / 60);
  return `${h}h ${m}m`;
}

/** Compute elapsed seconds since an epoch-millis timestamp. */
function elapsedSec(epochMs: number): number {
  return (Date.now() - epochMs) / 1000;
}

export default function StatusPage() {
  const {
    snapshot,
    connectionState,
    connectedHost,
    connectedSince,
    lastConnectedDurationSec,
    connectFailures,
    evcHost,
    evcPower,
    evcChargingState,
    evcCharging,
    evcConnected,
    evcCableConnected,
    evcSessionEnergyKwh,
    evcEverConnected,
    energyFlowDisplay,
    showTodayStats,
  } = useInverterStore();
  const house = energyFlowDisplay === 'house';

  // Re-compute uptime every second while connected.
  const now = useNow(connectionState === 'connected' ? 1000 : null);

  const { reconnect, reconnecting } = useReconnect();

  // Re-compute duration every tick (driven by the interval that updates `now`).
  const durationSec =
    connectionState === 'connected' && connectedSince != null
      ? elapsedSec(connectedSince)
      : lastConnectedDurationSec ?? 0;
  const hasConnectionDuration = connectedSince != null || lastConnectedDurationSec != null;
  void now; // used to trigger re-renders for live uptime counter

  if (!snapshot) {
    return (
      <AwaitingConnection
        connectionState={connectionState}
        connectedHost={connectedHost}
        showRetry={connectionState !== 'disconnected'}
        retryLabel="Reconnect"
        connectFailures={connectFailures}
        extraNote={hasConnectionDuration ? `Last connected for ${formatDuration(durationSec)}` : undefined}
        showFaq
        faqExtraNote="If you've recently factory-reset your dongle, make sure the WiFi-UART setting is Server (not Client)."
      />
    );
  }

  const flowProps = {
    snapshot,
    evcPower,
    evcChargingState,
    evcCharging,
    evcConnected,
    evcCableConnected,
    evcSessionEnergyKwh,
    evcEverConnected,
    showEvc: !!evcHost,
  };

  return (
    <div className="flex flex-col gap-3 mx-auto w-full max-w-4xl lg:max-w-6xl">
      {/* Connection status bar — only show when disconnected */}
      {connectionState !== 'connected' && (
        <section className="flex items-center justify-between gap-2 px-4 py-2 bg-bg-surface/50 rounded-2xl text-xs text-text-secondary/70">
          <div className="flex items-center gap-2">
            <span
              className={`inline-block w-2 h-2 rounded-full ${
                connectionState === 'reconnecting'
                  ? 'bg-amber-500'
                  : 'bg-red-500'
              }`}
            />
            <span className="capitalize">{connectionState}</span>
            {connectedHost && (
              <span className="font-mono ml-1">
                {connectedHost.replace(/:.*$/, '')}
              </span>
            )}
            {hasConnectionDuration && (
              <span className="ml-1 text-text-secondary/50">
                &middot; last connected for {formatDuration(durationSec)}
              </span>
            )}
          </div>
          <button
            onClick={reconnect}
            disabled={reconnecting}
            className="px-3 py-1 text-xs font-medium rounded-lg bg-white/5 hover:bg-white/10 border border-white/10 transition-colors disabled:opacity-40"
          >
            {reconnecting ? '…' : 'Reconnect'}
          </button>
        </section>
      )}

      {/* Grid fault banner */}
      {hasGridFault(snapshot) && (
        <section className="rounded-2xl border border-red-500/40 bg-red-950/50 px-4 py-3 text-red-100 shadow-lg shadow-red-950/20">
          <div className="flex items-start gap-3">
            <span className="text-2xl" aria-hidden="true">⚠️</span>
            <div className="flex flex-col gap-1">
              <h2 className="text-sm font-semibold uppercase tracking-wide">
                {gridFaultTitle()}
              </h2>
              <p className="text-sm text-red-100/90">
                The inverter is reporting <strong>{gridFaultReason(snapshot)}</strong>.
                Battery SOC is {formatPercent(snapshot.soc)}
                {snapshot.battery_power > 0 ? ` and the battery is discharging at ${formatPower(Math.abs(snapshot.battery_power))}` : ''}.{gridFaultAdvice()}
              </p>
            </div>
          </div>
        </section>
      )}

      <ColdBatteryWarning />

      {/* Landscape layout on lg+ (≥1024px): energy diagram on the left
          with the house cards and summary tiles on the right, so the whole
          status page fits in roughly 800px tall instead of forcing a scroll.
          Below lg everything stacks under the diagram. The full battery card
          lives on the Battery page (issue #422); the diagram already shows
          battery power, SOC and mode. The Today tiles can be hidden in
          Settings → Panel Controls. */}
      <div
        className={`grid grid-cols-1 ${house || showTodayStats ? 'lg:grid-cols-2' : ''} gap-3 lg:gap-4 items-start`}
        data-testid="status-landscape-grid"
      >
        {/* Energy flow diagram — full width card (left column on lg+).
            Orbit by default; the illustrated house when chosen in Settings →
            Panel Controls (issue #422). Both render the same view model. */}
        <section className="bg-bg-surface rounded-lg p-1">
          {house ? (
            <HouseEnergyView
              {...flowProps}
              connectionState={connectionState}
              gridFault={hasGridFault(snapshot)}
            />
          ) : (
            <EnergyOrbitDiagram {...flowProps} />
          )}
        </section>

        {(house || showTodayStats) && (
          <div className="flex flex-col gap-3 lg:gap-4 min-w-0">
            {/* House view: the charge slot and battery level cards. */}
            {house && <HouseStatusCards snapshot={snapshot} />}
            {showTodayStats && <SummaryTiles snapshot={snapshot} />}
          </div>
        )}
      </div>

    </div>
  );
}
