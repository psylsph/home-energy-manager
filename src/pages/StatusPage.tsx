import { useState, useEffect, useCallback } from 'react';
import { useInverterStore } from '../store/useInverterStore';
import { apiPost } from '../lib/api';
import EnergyFlowDiagram from '../components/EnergyFlowDiagram';
import BatteryPanel from '../components/BatteryPanel';
import SummaryTiles from '../components/SummaryTiles';
import ColdBatteryWarning from '../components/ColdBatteryWarning';
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
    connectFailures,
    evcHost,
    evcPower,
    evcCharging,
    evcConnected,
  } = useInverterStore();

  // Re-compute uptime every second while connected.
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (connectionState !== 'connected') return;
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [connectionState]);

  const [reconnecting, setReconnecting] = useState(false);
  const handleReconnect = useCallback(async () => {
    setReconnecting(true);
    try {
      await apiPost('/api/reconnect');
    } catch {
      // Ignore — the connection will drop and the WS hook handles reconnection.
    }
    // Reset after a timeout in case the request doesn't trigger a state change.
    setTimeout(() => setReconnecting(false), 5000);
  }, []);

  // Re-compute duration every tick (driven by the interval that updates `now`).
  const durationSec =
    connectionState === 'connected' && connectedSince != null
      ? elapsedSec(connectedSince)
      : 0;
  void now; // used to trigger re-renders for live uptime counter

  const showFailureAdvice =
    connectionState === 'disconnected' && connectFailures >= 5;

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

        {/* Prolonged failure advice banner */}
        {showFailureAdvice && (
          <div className="rounded-2xl border border-amber-500/40 bg-amber-950/30 px-5 py-4 text-amber-100 shadow-lg max-w-md">
            <div className="flex items-start gap-3">
              <span className="text-2xl" aria-hidden="true">💡</span>
              <div className="flex flex-col gap-2">
                <p className="text-sm font-semibold">
                  Can't reach the dongle after {Math.min(connectFailures, 99)}+ attempts
                </p>
                <p className="text-xs text-amber-100/80 leading-relaxed">
                  This is usually because the GivEnergy dongle has locked up.
                  Try <strong>power-cycling the inverter</strong> (turn off the
                  AC isolator, wait 30&nbsp;seconds, turn it back on). The dongle
                  will reboot and should reconnect within a few minutes.
                </p>
                <button
                  onClick={handleReconnect}
                  disabled={reconnecting}
                  className="self-start mt-1 px-4 py-1.5 text-xs font-semibold rounded-lg bg-amber-600/30 hover:bg-amber-600/50 border border-amber-500/30 transition-colors disabled:opacity-50"
                >
                  {reconnecting ? 'Reconnecting…' : 'Retry now'}
                </button>
              </div>
            </div>
          </div>
        )}

        <div className="flex flex-col items-center gap-2">
          {connectedHost && (
            <p className="text-text-secondary/60 text-xs font-sans">
              Host: {connectedHost}
            </p>
          )}
          {connectedSince != null && (
            <p className="text-text-secondary/60 text-xs font-sans">
              Last connected for {formatDuration(durationSec)}
            </p>
          )}
          {connectionState !== 'disconnected' && (
            <button
              onClick={handleReconnect}
              disabled={reconnecting}
              className="px-4 py-1.5 text-xs font-semibold rounded-lg bg-bg-surface hover:bg-white/10 border border-white/10 transition-colors disabled:opacity-50"
            >
              {reconnecting ? 'Reconnecting…' : 'Reconnect'}
            </button>
          )}
        </div>

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
            {connectedSince != null && (
              <span className="ml-1 text-text-secondary/50">
                &middot; last connected for {formatDuration(durationSec)}
              </span>
            )}
          </div>
          <button
            onClick={handleReconnect}
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
                {gridFaultTitle(snapshot)}
              </h2>
              <p className="text-sm text-red-100/90">
                The inverter is reporting <strong>{gridFaultReason(snapshot)}</strong>.
                Battery SOC is {formatPercent(snapshot.soc)}
                {snapshot.battery_power > 0 ? ` and the battery is discharging at ${formatPower(Math.abs(snapshot.battery_power))}` : ''}.{gridFaultAdvice(snapshot)}
              </p>
            </div>
          </div>
        </section>
      )}

      <ColdBatteryWarning />

      {/* Energy flow diagram — full width card */}
      <section className="bg-bg-surface rounded-2xl p-2">
        <EnergyFlowDiagram
          snapshot={snapshot}
          evcPower={evcPower}
          evcCharging={evcCharging}
          evcConnected={evcConnected}
          showEvc={!!evcHost}
        />
      </section>

      {/* Battery + Summary side by side on md+ */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4 items-stretch">
        <SummaryTiles snapshot={snapshot} />
        <BatteryPanel snapshot={snapshot} />
      </div>

    </div>
  );
}
