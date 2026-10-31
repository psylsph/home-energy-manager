import { useCallback, useEffect, useState } from 'react';
import HelpTip, { HeadingWithHelp } from './HelpTip';
import { apiGet, apiPost } from '../lib/api';
import {
  describeLastCalibration,
  describePhase,
  formatCalibrationTime,
  holdRemainingMinutes,
  parseHoldHours,
} from '../lib/calibration';
import type { CalibrationStatus } from '../lib/calibration';

const PATH = '/api/battery/calibration';
const RUNNING_POLL_MS = 30_000;

function hours(minutes: number): string {
  return String(minutes / 60);
}

/**
 * Control page: the guided battery calibration cycle (issue #418). HEM
 * exports the battery down to the floor, charges it to 100%, holds it there
 * while the cells balance and then puts every setting back.
 */
export default function BatteryCalibrationCycle() {
  const [status, setStatus] = useState<CalibrationStatus | null>(null);
  /** When `status` arrived, for the time left in the hold. */
  const [fetchedAt, setFetchedAt] = useState(0);
  const [loadFailed, setLoadFailed] = useState(false);
  const [holdText, setHoldText] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setStatus(await apiGet<CalibrationStatus>(PATH));
      setFetchedAt(Date.now());
      setLoadFailed(false);
    } catch {
      setLoadFailed(true);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    apiGet<CalibrationStatus>(PATH)
      .then((data) => {
        if (cancelled) return;
        setStatus(data);
        setFetchedAt(Date.now());
      })
      .catch(() => {
        if (!cancelled) setLoadFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // A cycle takes hours; follow it while the page is open.
  const running = status?.running === true;
  useEffect(() => {
    if (!running) return;
    const timer = setInterval(() => void refresh(), RUNNING_POLL_MS);
    return () => clearInterval(timer);
  }, [running, refresh]);

  if (!status) {
    return loadFailed ? (
      <p className="text-text-secondary text-xs">Couldn&apos;t load the battery calibration status.</p>
    ) : null;
  }

  const minHours = hours(status.min_hold_minutes);
  const maxHours = hours(status.max_hold_minutes);
  const typedHold = holdText ?? hours(status.default_hold_minutes);
  const holdMinutes = parseHoldHours(typedHold, status.min_hold_minutes, status.max_hold_minutes);
  const view = status.calibration;
  const remaining = view ? holdRemainingMinutes(view, fetchedAt) : null;
  const last = describeLastCalibration(status.last);

  const act = async (path: string, body?: unknown) => {
    setBusy(true);
    setError(null);
    try {
      await (body === undefined ? apiPost(path) : apiPost(path, body));
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : 'The request failed');
    }
    setBusy(false);
  };

  const start = () => {
    if (holdMinutes == null) return;
    if (
      !confirm(
        `Start a battery calibration?\n\nHEM will export at full power until the battery reaches ${status.floor_soc}%, `
          + `charge it to 100% at full power, hold it there for ${hours(holdMinutes)} hours, then put your settings back. `
          + 'It can take most of a day, and Cosy, Agile and your other automations wait until it has finished.',
      )
    ) {
      return;
    }
    void act(PATH, { hold_minutes: holdMinutes });
  };

  const cancel = () => {
    if (!confirm('Cancel the battery calibration and put your settings back?')) return;
    void act(`${PATH}/cancel`);
  };

  return (
    <div className="space-y-3">
      <HeadingWithHelp
        as="h3"
        className="text-text-primary font-medium text-base"
        title="Battery Calibration Cycle"
        help={
          <>
            A full discharge and charge brings a drifting battery SOC back into line, which is what the
            GivEnergy BMS SOC bug needs. HEM empties the battery to {status.floor_soc}%, charges it to 100%,
            holds it there while the cells balance, then puts every setting it changed back as it was.
            Battery settings changed while it runs are put back to how they were when it started.
          </>
        }
      />
      <div className="bg-bg-surface border border-bg-elevated rounded-xl p-3 space-y-3">
        <p className="text-text-secondary text-xs">
          Empties to {status.floor_soc}%, charges to 100% and holds, then restores your settings.
        </p>

        {!status.supported ? (
          <p className="text-text-secondary text-xs">
            The calibration cycle isn&apos;t available for this inverter. HEM runs it on single-phase
            inverters with a low-voltage battery.
          </p>
        ) : view ? (
          <div className="space-y-2">
            <div className="text-text-primary text-sm font-medium">{describePhase(view, status.floor_soc)}</div>
            {remaining != null && (
              <div className="text-text-secondary text-xs">{remaining} min left</div>
            )}
            <div className="text-text-secondary text-xs">
              Started {formatCalibrationTime(view.started_at_ms)}, holding for {hours(view.hold_minutes)} hours at 100%.
            </div>
            {view.phase !== 'restoring' && (
              <button
                type="button"
                onClick={cancel}
                disabled={busy}
                className="py-1.5 px-3 bg-bg-elevated text-text-primary rounded-lg text-xs font-medium hover:opacity-80 transition disabled:opacity-40"
              >
                Cancel Calibration
              </button>
            )}
          </div>
        ) : (
          <div className="space-y-3">
            <div className="flex items-center gap-3">
              <label htmlFor="calibration-hold-hours" className="text-text-secondary text-xs">
                Hold at 100% for (Hours)
              </label>
              <input
                id="calibration-hold-hours"
                type="text"
                inputMode="decimal"
                value={typedHold}
                onChange={(e) => setHoldText(e.target.value)}
                className="w-16 bg-bg-elevated text-text-primary rounded-lg px-2 py-1 text-xs font-mono"
              />
            </div>
            {holdMinutes == null && (
              <p className="text-amber-400 text-xs">Choose between {minHours} and {maxHours} hours.</p>
            )}
            <p className="text-amber-300/80 text-xs">
              Exports at full power. Start a few hours before your cheap rate.{' '}
              <HelpTip label="the cost of a calibration cycle">
                It exports at full power, so on a time-of-use tariff the battery sells at your export rate and
                is bought back at whatever import rate applies while it recharges. Starting a few hours before
                your cheap rate keeps that cost down.
              </HelpTip>
            </p>
            <button
              type="button"
              onClick={start}
              disabled={busy || holdMinutes == null}
              className="py-1.5 px-3 bg-amber-500/20 text-text-primary rounded-lg text-xs font-medium hover:bg-amber-500/30 transition disabled:opacity-40 border border-amber-500/30"
            >
              Start Calibration
            </button>
          </div>
        )}

        {error && <p className="text-red-400 text-xs">{error}</p>}
        {last && <p className="text-text-secondary text-xs">{last}</p>}
      </div>
    </div>
  );
}
