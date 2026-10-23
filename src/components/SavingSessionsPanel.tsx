import { useEffect, useId, useState } from 'react';
import { apiGet, apiPost } from '../lib/api';
import Toggle from './Toggle';
import { exportStatusLabel, formatReward, formatSessionWindow, readSessionsResponse } from '../lib/savingSessions';
import type { SavingSessionsMode, SavingSessionsResponse } from '../lib/types';

/** What /api/octopus/sessions/mode answers: the mode as saved. */
type ModeReply = { ok?: boolean; mode?: SavingSessionsMode };

const REFRESH_MS = 60_000;

const MODES: { value: SavingSessionsMode; label: string }[] = [
  { value: 'off', label: 'Off' },
  { value: 'alert', label: 'Alert Only' },
  { value: 'export', label: 'Alert And Export' },
];

/**
 * Saving Sessions settings and the list of joined sessions, shown at the top
 * of the Octopus page. Notices use role="status": a failure here is never
 * allowed to look like the page's own billing errors.
 *
 * Two separate settings: the mode says what HEM does about sessions the
 * account has joined, and the Joining tick is the only thing that joins. Off
 * stops both, since it means no requests are made to Octopus at all (#377).
 */
export default function SavingSessionsPanel() {
  const joiningHeadingId = useId();
  const modesLabelId = useId();
  const [data, setData] = useState<SavingSessionsResponse | null>(null);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  // Set when a save worked but did not do what the page assumed.
  const [saveNotice, setSaveNotice] = useState<string | null>(null);
  // Unsaved edits to the export slot and floor; null means "as saved".
  const [slotDraft, setSlotDraft] = useState<number | null>(null);
  const [floorDraft, setFloorDraft] = useState<number | null>(null);
  // Bumped after the mode changes to reload straight away.
  const [reloadTick, setReloadTick] = useState(0);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const res = readSessionsResponse(await apiGet('/api/octopus/sessions'));
        if (!cancelled) setData(res);
      } catch {
        if (!cancelled) setData(null);
      }
    };
    void load();
    const timer = setInterval(() => void load(), REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [reloadTick]);

  // The backend answers an unreadable or failed request with nothing usable;
  // offer no controls rather than guess at the current mode.
  if (!data) return null;

  async function post(
    path: string,
    body: Record<string, unknown>,
    then?: (reply: ModeReply | undefined) => void,
  ) {
    setSaving(true);
    setSaveError(null);
    setSaveNotice(null);
    try {
      const reply = await apiPost<ModeReply | undefined>(path, body);
      then?.(reply);
      setReloadTick((tick) => tick + 1);
    } catch (cause) {
      setSaveError(cause instanceof Error ? cause.message : 'Could not change the setting.');
    } finally {
      setSaving(false);
    }
  }

  const choose = (mode: SavingSessionsMode) => post('/api/octopus/sessions/mode', { mode });

  const maxSlots = data.max_discharge_slots;
  // A saved slot the inverter does not have (e.g. chosen on a previous
  // inverter) would make every booking fail, so show a real one and let Save
  // correct it.
  const slot = slotDraft ?? Math.min(data.export_slot, maxSlots);
  const floor = floorDraft ?? data.export_floor_soc;
  const exportChanged = slot !== data.export_slot || floor !== data.export_floor_soc;
  // Only offer what can actually be chosen: nothing for a single-slot
  // inverter (the one slot is used), no floor on models that cannot hold one.
  const showSlotPicker = maxSlots > 1 || data.export_slot > maxSlots;
  const showFloor = maxSlots > 2;
  // Neither this nor the join tick sends a mode: the one on the page may be
  // up to a minute old, and re-sending it could undo a change made elsewhere
  // and stop a running export.
  const saveExportSettings = () =>
    post(
      '/api/octopus/sessions/mode',
      { export_slot: slot, export_floor_soc: floor },
      (reply) => {
        setSlotDraft(null);
        setFloorDraft(null);
        // Saved from a page that still showed Alert And Export after the
        // mode was changed elsewhere: the values are kept but not used yet.
        const now = MODES.find((item) => item.value === reply?.mode);
        if (now && now.value !== 'export') {
          setSaveNotice(
            `Export settings saved, but Saving Sessions is set to ${now.label}, so nothing will ` +
              'be exported. Choose Alert And Export to use them.',
          );
        }
      },
    );

  const watching = data.mode !== 'off';
  const exporting = data.mode === 'export';
  // Joining only happens with a working login to an account known to be
  // signed up; a failed check keeps the last enrolment answer, so check both.
  const quietJoinOnly =
    data.mode === 'alert' && data.auto_join && data.enrolled === true && !data.error;

  return (
    <div className="mt-4 border-t border-bg-elevated pt-4" data-testid="saving-sessions-panel">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h3 className="font-medium text-text-primary">Saving Sessions</h3>
          <p className="mt-1 text-xs text-text-secondary">
            HEM can message you when you have joined a Saving Session and again shortly before
            it starts, and can also export from your battery during it. Joining is separate:
            join in the Octopus app, or let HEM join them for you. Off turns all of this off,
            joining included.
          </p>
        </div>
        <div className="flex flex-col items-start gap-1">
          <span id={modesLabelId} className="text-[11px] text-text-secondary">
            For Sessions You Have Joined · Off also stops joining
          </span>
          <div className="flex flex-wrap gap-1" role="group" aria-labelledby={modesLabelId}>
            {MODES.map((item) => (
              <button
                key={item.value}
                type="button"
                aria-pressed={data.mode === item.value}
                disabled={saving}
                onClick={() => void choose(item.value)}
                className={`rounded-lg px-3 py-1.5 text-xs font-medium disabled:opacity-50 ${
                  data.mode === item.value
                    ? 'bg-accent text-on-accent'
                    : 'bg-bg-elevated text-text-secondary hover:text-text-primary'
                }`}
              >
                {item.label}
              </button>
            ))}
          </div>
        </div>
      </div>

      {saveError && (
        <p role="status" className="mt-2 text-xs text-red-300">{saveError}</p>
      )}

      {saveNotice && (
        <p role="status" className="mt-2 text-xs text-text-secondary">{saveNotice}</p>
      )}

      {/* Offered while Off too: choosing a mode starts a check straight away,
          so the tick must be changeable before then. Saving it while Off makes
          no request to Octopus. */}
      {data.configured && (
        <div
          role="group"
          aria-labelledby={joiningHeadingId}
          className="mt-3 rounded-lg bg-bg-elevated p-3"
        >
          <h4 id={joiningHeadingId} className="mb-2 text-xs font-medium text-text-secondary">
            Joining
          </h4>
          <div className="flex items-start justify-between gap-3 text-sm text-text-primary">
            <span>
              Join Sessions For Me
              <span className="mt-1 block text-xs text-text-secondary">
                HEM asks Octopus to join each Saving Session on offer to your account that it has
                not tried before, including any already on offer when you turn this on. Each one is
                tried once, and only counts as joined after checking your account shows it.
                This works the same whichever of Alert Only or Alert And Export is chosen. Your
                account must already be signed up to Octoplus Saving Sessions; HEM will not sign
                it up. If a join fails, HEM shows it here, messages you if an alert channel is
                set up, and does not try that session again.
              </span>
            </span>
            <div className="mt-0.5 shrink-0">
              <Toggle
                checked={data.auto_join}
                disabled={saving}
                onChange={(auto_join) => void post('/api/octopus/sessions/mode', { auto_join })}
                ariaLabel="Join Sessions For Me"
              />
            </div>
          </div>
          {!watching && data.auto_join && (
            <p role="status" className="mt-2 text-xs text-text-secondary">
              Joining is paused while Saving Sessions is Off. Choosing Alert Only or Alert And
              Export starts a check, which straight away joins any session on offer that it has
              not tried before, if your account is signed up to Octoplus Saving Sessions.
            </p>
          )}
          {watching && data.auto_join && data.join_failures.length > 0 && (
            <ul className="mt-3 space-y-1" aria-label="Sessions HEM could not join">
              {data.join_failures.map((failure) => (
                <li
                  key={failure.id}
                  role="status"
                  className="rounded-lg border border-yellow-500/30 bg-yellow-500/10 px-3 py-2 text-xs text-text-primary"
                >
                  Could not join {formatSessionWindow(failure.start, failure.end)}: {failure.error}
                  {' '}HEM will not try this one again; you can join it in the Octopus app.
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      {exporting && (
        <div className="mt-3 space-y-3 rounded-lg bg-bg-elevated p-3">
          <p className="text-xs text-text-primary">
            HEM will export from your battery during each Saving Session you have joined.{' '}
            {showSlotPicker
              ? 'The slot below is used for the session'
              : 'Your inverter has a single export slot, which is used for the session'}{' '}
            and put back to how it was afterwards. HEM must be running and connected to the
            inverter at the time.
          </p>
          {(showSlotPicker || showFloor) && (
          <div className="flex flex-wrap items-center gap-4">
            {showSlotPicker && (
            <label className="flex items-center gap-2 text-sm text-text-secondary">
              Slot To Use
              <select
                value={slot}
                onChange={(e) => setSlotDraft(Number(e.target.value))}
                disabled={saving}
                className="rounded-lg bg-bg-surface px-2 py-1 text-sm text-text-primary"
              >
                {Array.from({ length: maxSlots }, (_, i) => (
                  <option key={i} value={i + 1}>{i + 1}</option>
                ))}
              </select>
            </label>
            )}
            {showFloor && (
              <label className="flex flex-1 items-center gap-2 text-sm text-text-secondary">
                Stop Exporting At
                <input
                  type="range"
                  min={4}
                  max={99}
                  step={1}
                  value={floor}
                  aria-label="Stop Exporting At"
                  onChange={(e) => setFloorDraft(Number(e.target.value))}
                  disabled={saving}
                  className="min-w-24 flex-1"
                />
                <span className="font-mono text-text-primary">{floor}%</span>
              </label>
            )}
            <button
              type="button"
              disabled={saving || !exportChanged}
              onClick={() => void saveExportSettings()}
              className="rounded-lg bg-accent/20 px-3 py-1.5 text-xs font-medium text-accent disabled:opacity-50"
            >
              Save Export Settings
            </button>
          </div>
          )}
        </div>
      )}

      {watching && !data.configured && (
        <p role="status" className="mt-3 rounded-lg border border-yellow-500/30 bg-yellow-500/10 px-3 py-2 text-xs text-text-primary">
          Saving Sessions needs your Octopus account number and API key. Add them in Settings.
        </p>
      )}

      {/* Alert Only with joining on and no channel may be deliberate (join but
          stay quiet), so it is a note rather than a warning. In Alert And Export
          the export messages would be lost too, and nothing is joined until a
          check has succeeded for an account signed up to Octoplus, so those
          keep the warning. alerts_ready is also false when a channel exists
          but alerts are switched off. */}
      {watching && data.configured && !data.alerts_ready && (
        <p
          role="status"
          className={`mt-3 text-xs ${
            quietJoinOnly
              ? 'text-text-secondary'
              : 'rounded-lg border border-yellow-500/30 bg-yellow-500/10 px-3 py-2 text-text-primary'
          }`}
        >
          Alerts are off or no alert channel is set up
          {quietJoinOnly
            ? '. HEM will still join sessions for you, but you will not get messages about them; failed joins still show here.'
            : ', so you will not be told about sessions.'}
          {' '}Turn alerts on and set up Telegram, ntfy or Pushover under Notifications in Settings
          {quietJoinOnly ? ' if you want messages.' : '.'}
        </p>
      )}

      {watching && data.configured && data.error && (
        <p role="status" className="mt-3 rounded-lg border border-yellow-500/30 bg-yellow-500/10 px-3 py-2 text-xs text-text-primary">
          Saving Sessions could not be checked just now: {data.error}
          {data.sessions.length > 0 ? ' The list below is from the last successful check.' : ''}
        </p>
      )}

      {watching && data.configured && data.enrolled === false && (
        <p role="status" className="mt-3 text-xs text-text-secondary">
          This Octopus account has not joined Octoplus Saving Sessions yet.
          {data.auto_join ? ' Nothing will be joined until it has.' : ''}
        </p>
      )}

      {watching && data.configured && (
        data.sessions.length > 0 ? (
          <ul className="mt-3 space-y-2" aria-label="Joined Saving Sessions">
            {data.sessions.map((session) => {
              const reward = formatReward(session);
              const window = formatSessionWindow(session.start, session.end);
              const status = exporting ? exportStatusLabel(session) : null;
              const canSkip = exporting
                && session.export?.state !== 'running'
                && session.export?.state !== 'done';
              return (
                <li key={session.id} className="flex flex-wrap items-center justify-between gap-2 text-sm text-text-primary">
                  <span>
                    {window}
                    {reward ? <span className="text-text-secondary"> · {reward}</span> : null}
                    {status ? (
                      <span
                        className={`ml-2 text-xs ${
                          status.tone === 'good'
                            ? 'text-green-400'
                            : status.tone === 'bad'
                              ? 'text-red-300'
                              : 'text-text-secondary'
                        }`}
                      >
                        {status.text}
                      </span>
                    ) : null}
                  </span>
                  {canSkip && (
                    <button
                      type="button"
                      disabled={saving}
                      aria-label={`${session.skipped ? "Don't Skip" : 'Skip'} Session ${window}`}
                      onClick={() =>
                        void post('/api/octopus/sessions/skip', { id: session.id, skip: !session.skipped })
                      }
                      className="rounded-lg bg-bg-elevated px-2 py-1 text-xs text-text-secondary hover:text-text-primary disabled:opacity-50"
                    >
                      {session.skipped ? "Don't Skip" : 'Skip'}
                    </button>
                  )}
                </li>
              );
            })}
          </ul>
        ) : (
          !data.error && (
            <p className="mt-3 text-xs text-text-secondary">
              No joined Saving Sessions coming up. Weekend Happy Hours and other events are not
              shown.
            </p>
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
