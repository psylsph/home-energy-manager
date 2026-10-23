import { useEffect, useState } from 'react';
import { apiGet, apiPost } from '../lib/api';
import { formatSessionWindow } from '../lib/savingSessions';
import { dispatchLabel, readDispatchesResponse } from '../lib/smartCharging';
import type { SmartChargingResponse } from '../lib/types';
import Toggle from './Toggle';

const REFRESH_MS = 60_000;

/**
 * Intelligent Octopus Go battery hold (issue #394), shown on the Octopus
 * page. Notices use role="status" so a failure here never looks like the
 * page's own billing errors.
 */
export default function SmartChargingPanel() {
  const [data, setData] = useState<SmartChargingResponse | null>(null);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  // Bumped after the hold is switched to reload straight away.
  const [reloadTick, setReloadTick] = useState(0);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const res = readDispatchesResponse(await apiGet('/api/octopus/dispatches'));
        // An unusable answer is no better than a failure: keep what was good.
        if (!cancelled && res) setData(res);
      } catch {
        // Keep the last good data: one failed refresh must not take the
        // switch away. If the first load fails there is nothing to show.
      }
    };
    void load();
    const timer = setInterval(() => void load(), REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [reloadTick]);

  if (!data) return null;

  async function setHold(enabled: boolean) {
    setSaving(true);
    setSaveError(null);
    try {
      await apiPost('/api/octopus/dispatches/hold', { enabled });
      // Show the saved position even if the reload below fails.
      setData((current) => current && { ...current, enabled });
      setReloadTick((tick) => tick + 1);
    } catch (cause) {
      setSaveError(cause instanceof Error ? cause.message : 'Could not change the setting.');
    } finally {
      setSaving(false);
    }
  }

  const watching = data.enabled && data.configured;
  const noDevice = watching && data.checked_at !== null && data.devices === 0;

  return (
    <div className="mt-4 border-t border-bg-elevated pt-4" data-testid="smart-charging-panel">
      <h3 className="font-medium text-text-primary">Intelligent Octopus Go</h3>
      <div className="mt-2 flex items-start justify-between gap-3 text-sm text-text-primary">
        <span>
          Hold The Battery While Octopus Smart-Charges Your Car
          <span className="mt-1 block text-xs text-text-secondary">
            Octopus can charge your car outside 23:30–05:30, and the battery would then empty
            itself into the car. With this on, HEM reads your planned charges from Octopus and
            pauses battery discharge while a smart charge runs, so the car charges from the cheap
            grid. When the charge ends the battery goes back to Eco with your previous reserve.
            Boost charges are billed at the peak rate, so the battery is left to help with those.
            If HEM cannot read your charges, the battery behaves as normal. HEM must be running and
            connected to the inverter at the time.
          </span>
        </span>
        <div className="mt-0.5 shrink-0">
          <Toggle
            checked={data.enabled}
            disabled={saving}
            onChange={(enabled) => void setHold(enabled)}
            ariaLabel="Hold The Battery While Octopus Smart-Charges Your Car"
          />
        </div>
      </div>

      {saveError && (
        <p role="status" className="mt-2 text-xs text-red-300">{saveError}</p>
      )}

      {data.enabled && !data.configured && (
        <p role="status" className="mt-3 rounded-lg border border-yellow-500/30 bg-yellow-500/10 px-3 py-2 text-xs text-text-primary">
          The battery hold needs your Octopus account number and API key. Add them in Settings.
        </p>
      )}

      {watching && data.holding && (
        <p role="status" className="mt-3 rounded-lg border border-green-500/30 bg-green-950/20 px-3 py-2 text-xs text-green-300">
          Holding the battery now: your car is smart-charging.
        </p>
      )}

      {watching && data.error && (
        <p role="status" className="mt-3 rounded-lg border border-yellow-500/30 bg-yellow-500/10 px-3 py-2 text-xs text-text-primary">
          Planned charges could not be checked just now: {data.error}. HEM keeps using the last
          list it got, and lets the battery behave as normal once that is more than{' '}
          {data.stale_after_minutes} minutes old.
        </p>
      )}

      {noDevice && (
        <p role="status" className="mt-3 text-xs text-text-secondary">
          No smart-charging car or charger was found on this Octopus account.
        </p>
      )}

      {watching && !noDevice && (
        data.dispatches.length > 0 ? (
          <ul className="mt-3 space-y-2" aria-label="Planned car charges">
            {data.dispatches.map((dispatch) => (
              <li
                key={`${dispatch.start}-${dispatch.end}-${dispatch.type ?? ''}`}
                className="flex flex-wrap items-center justify-between gap-2 text-sm text-text-primary"
              >
                <span>{formatSessionWindow(dispatch.start, dispatch.end)}</span>
                <span className={`text-xs ${dispatch.smart ? 'text-green-400' : 'text-text-secondary'}`}>
                  {dispatchLabel(dispatch)}
                </span>
              </li>
            ))}
          </ul>
        ) : (
          data.checked_at && !data.error && (
            <p className="mt-3 text-xs text-text-secondary">No smart charges planned.</p>
          )
        )
      )}

      {watching && data.checked_at && (
        <p className="mt-2 text-[11px] text-text-secondary/70">
          Last checked {new Date(data.checked_at).toLocaleString()}
        </p>
      )}
    </div>
  );
}
