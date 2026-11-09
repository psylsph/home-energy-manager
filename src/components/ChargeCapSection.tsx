import { useEffect, useRef, useState } from 'react';
import { HeadingWithHelp } from './HelpTip';
import Toggle from './Toggle';
import { apiGet, apiPost } from '../lib/api';
import { useInverterStore } from '../store/useInverterStore';
import { useNow } from '../hooks/useNow';
import {
  chargeCapError,
  describeChargeCapState,
  describeLastFullCharge,
  parseSoc,
} from '../lib/chargeCap';
import type { ChargeCapStatus } from '../lib/chargeCap';
import { DEFAULT_ALERTS_CONFIG } from './settings/notifications/alertsConfig';

const PATH = '/api/charge-cap';

/**
 * Control page: the optional charge cap (issue #426). Shown only on
 * inverters that can pause charging on their own (the All-in-Ones, the
 * Gen3 hybrid on newer firmware and the AC-coupled, as GivTCP supports it).
 */
export default function ChargeCapSection() {
  const snapshot = useInverterStore((s) => s.snapshot);
  const [status, setStatus] = useState<ChargeCapStatus | null>(null);
  const [loadFailed, setLoadFailed] = useState(false);
  // Drafts: null means "as saved".
  const [enabledDraft, setEnabledDraft] = useState<boolean | null>(null);
  const [capText, setCapText] = useState<string | null>(null);
  const [resumeText, setResumeText] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [feedback, setFeedback] = useState<'saved' | string | null>(null);
  const now = useNow(60 * 60 * 1000);

  // Until the backend has read the inverter it can't say whether the cap is
  // supported (it may be mid-reconnect while this page still shows the old
  // snapshot), so ask again with each new snapshot until it can, and only
  // once per snapshot.
  const snapshotAt = snapshot?.timestamp ?? null;
  const connected = status?.connected ?? null;
  const askedFor = useRef<number | null | undefined>(undefined);
  useEffect(() => {
    if (connected === true || askedFor.current === snapshotAt) return;
    askedFor.current = snapshotAt;
    let cancelled = false;
    apiGet<ChargeCapStatus>(PATH)
      .then((data) => {
        if (!cancelled) setStatus(data);
      })
      .catch(() => {
        if (!cancelled) setLoadFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [snapshotAt, connected]);

  if (!status) {
    return loadFailed ? (
      <p className="text-text-secondary text-xs">Couldn&apos;t load the charge cap.</p>
    ) : null;
  }
  // Hidden where the inverter can't pause charging, unless a pause the cap
  // started is still being handed back.
  if (!status.supported && !status.holding) return null;

  const enabled = enabledDraft ?? status.enabled;
  const capInput = capText ?? String(status.cap_soc);
  const resumeInput = resumeText ?? String(status.resume_soc);
  const cap = parseSoc(capInput);
  const resume = parseSoc(resumeInput);
  const levelsError = chargeCapError(cap, resume, status);
  const error = enabled ? levelsError : null;
  const dirty = enabledDraft !== null || capText !== null || resumeText !== null;
  // The snapshot follows each poll; the fetched status is only the fallback
  // (also when a snapshot arrives without the cap's state, as "").
  const liveState = snapshot?.charge_cap_state || status.state;
  const days = status.full_charge_days ?? DEFAULT_ALERTS_CONFIG.full_charge_alert_days;

  const save = async () => {
    setSaving(true);
    setFeedback(null);
    try {
      // Switching off with levels that can't be saved (their fields are
      // hidden once it's off) keeps the saved ones, so switching off always
      // goes through.
      const keepSaved = levelsError !== null;
      const saved = await apiPost<ChargeCapStatus>(PATH, {
        enabled,
        cap_soc: keepSaved ? status.cap_soc : cap,
        resume_soc: keepSaved ? status.resume_soc : resume,
      });
      setStatus((prev) => ({ ...(prev ?? saved), ...saved }));
      setEnabledDraft(null);
      setCapText(null);
      setResumeText(null);
      setFeedback('saved');
    } catch (e) {
      setFeedback(e instanceof Error ? e.message : 'Failed to save the charge cap');
    }
    setSaving(false);
  };

  return (
    <section className="space-y-3" data-testid="charge-cap-section">
      <HeadingWithHelp
        as="h3"
        className="text-text-primary font-medium text-base"
        title="Charge Cap"
        help={
          <>
            Some setups misbehave when the battery is completely full, such as an island-mode switchover
            that only fails at 100%. The cap pauses charging at the level you choose, so solar goes to
            the house and the grid instead, and lets the battery charge again once it has fallen to the
            resume level. A charge slot (Timed Charge, Force Charge, Cosy, Agile, the Planner) or a
            calibration cycle still charges past the cap; the cap waits until it ends. A Timed Discharge
            or another pause set on the inverter also takes priority. HEM does this with the
            inverter&apos;s own charge pause, so it only works while HEM is running.
          </>
        }
      />
      <div className="bg-bg-surface rounded-xl p-4 space-y-4">
        <div className="flex items-center justify-between">
          <div>
            <span className="text-text-primary text-sm font-medium">Enable</span>
            <p className="text-text-secondary text-xs mt-0.5">Stop charging below 100% and charge again lower down.</p>
          </div>
          <Toggle checked={enabled} onChange={setEnabledDraft} ariaLabel="Enable Charge Cap" />
        </div>

        <div className="space-y-0.5">
          <span className="text-text-primary text-xs font-medium" data-testid="charge-cap-state-label">
            Current State
          </span>
          <p className="text-text-secondary text-xs" data-testid="charge-cap-state">
            {describeChargeCapState(liveState, status.cap_soc, status.resume_soc, snapshot?.battery_pause_read_failing === true)}
          </p>
        </div>

        {enabled && (
          <div className="grid grid-cols-[max-content_auto] items-center gap-x-3 gap-y-2">
            <label htmlFor="charge-cap-soc" className="text-text-secondary text-xs">
              Stop Charging at (%)
            </label>
            <input
              id="charge-cap-soc"
              type="text"
              inputMode="numeric"
              value={capInput}
              onChange={(e) => setCapText(e.target.value)}
              className="w-16 bg-bg-elevated text-text-primary rounded-lg px-2 py-1 text-xs font-mono"
            />
            <label htmlFor="charge-cap-resume" className="text-text-secondary text-xs">
              Resume Charging at (%)
            </label>
            <input
              id="charge-cap-resume"
              type="text"
              inputMode="numeric"
              value={resumeInput}
              onChange={(e) => setResumeText(e.target.value)}
              className="w-16 bg-bg-elevated text-text-primary rounded-lg px-2 py-1 text-xs font-mono"
            />
          </div>
        )}
        {error && <p className="text-amber-400 text-xs">{error}</p>}

        {enabled && (
          <div className="space-y-1">
            <p className="text-amber-300/80 text-xs" data-testid="charge-cap-hem-warning">
              Only works while HEM runs: if HEM stops mid-pause, charging stays paused until it&apos;s back.
            </p>
            <p className="text-amber-300/80 text-xs" data-testid="charge-cap-balance-warning">
              Let the battery reach 100% every {days} days so the cells balance.{' '}
              {describeLastFullCharge(status.last_full_charge_at, now)}
            </p>
          </div>
        )}

        <div className="flex items-center gap-3">
          <button
            type="button"
            onClick={() => void save()}
            disabled={saving || !dirty || error != null}
            className="py-1.5 px-3 bg-accent/20 text-accent rounded-lg text-xs font-medium hover:bg-accent/30 transition disabled:opacity-50"
          >
            {saving ? 'Saving…' : 'Save'}
          </button>
          {feedback === 'saved' && <span className="text-green-400 text-xs">Saved</span>}
          {feedback && feedback !== 'saved' && <span className="text-red-400 text-xs">{feedback}</span>}
        </div>
      </div>
    </section>
  );
}
