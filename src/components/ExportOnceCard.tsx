import { useEffect, useState } from 'react';
import { apiGet, apiPost } from '../lib/api';
import {
  defaultBooking,
  formatEventWindow,
  formatSlotRange,
  splitEvents,
  stateLabel,
  validateBooking,
} from '../lib/exportOnce';
import type { ExportOnceBooking } from '../lib/exportOnce';
import type { ExportOnceEvent, ExportOnceListResponse, ScheduleSlot } from '../lib/types';

const REFRESH_MS = 15_000;

interface ExportOnceCardProps {
  /** How many discharge slots the inverter model has. */
  maxDischargeSlots: number;
  /** The slots as the Control page shows them, for "currently ..." hints. */
  dischargeSlots: ScheduleSlot[];
}

/**
 * Book a one-off Timed Export: a dated window applied to a slot the user
 * picks shortly before it starts, put back to the slot's previous setting
 * when it ends. The backend does the work; this only books and lists.
 */
export default function ExportOnceCard({ maxDischargeSlots, dischargeSlots }: ExportOnceCardProps) {
  const [events, setEvents] = useState<ExportOnceEvent[]>([]);
  const [nowLocal, setNowLocal] = useState<string | null>(null);
  const [form, setForm] = useState<ExportOnceBooking | null>(null);
  const [loadFailed, setLoadFailed] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Bumped after a booking or cancellation to reload straight away.
  const [reloadTick, setReloadTick] = useState(0);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const res = await apiGet<ExportOnceListResponse>('/api/export-once');
        if (cancelled) return;
        if (!res || !Array.isArray(res.events) || typeof res.now !== 'string') {
          setLoadFailed(true);
          return;
        }
        setLoadFailed(false);
        setEvents(res.events);
        setNowLocal(res.now);
        // Seed the form once, from the inverter's own clock.
        setForm((current) => current ?? defaultBooking(res.now));
      } catch {
        if (!cancelled) setLoadFailed(true);
      }
    };
    void load();
    const timer = setInterval(() => void load(), REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [reloadTick]);

  const { upcoming, recent } = splitEvents(events);
  const showFloor = maxDischargeSlots > 2;

  async function book() {
    if (!form || !nowLocal) return;
    const problem = validateBooking(form, nowLocal, maxDischargeSlots);
    if (problem) {
      setError(problem);
      return;
    }
    setSaving(true);
    setError(null);
    try {
      await apiPost('/api/export-once', form);
      setForm(defaultBooking(nowLocal));
      setReloadTick((tick) => tick + 1);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Could not book the export.');
    } finally {
      setSaving(false);
    }
  }

  async function cancel(event: ExportOnceEvent) {
    setSaving(true);
    setError(null);
    try {
      await apiPost('/api/export-once/cancel', { id: event.id });
      setReloadTick((tick) => tick + 1);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Could not cancel the export.');
    } finally {
      setSaving(false);
    }
  }

  const update = (patch: Partial<ExportOnceBooking>) =>
    setForm((current) => (current ? { ...current, ...patch } : current));

  return (
    <div className="space-y-3 border-t border-bg-elevated pt-4" data-testid="export-once-card">
      <h3 className="text-text-primary font-semibold">Export Once</h3>
      <p className="text-text-secondary/60 text-xs">
        Book a single export on a date. HEM puts it into the slot you pick shortly before it
        starts, then puts that slot back to how it was when it ends. Keep HEM running until then.
      </p>

      {loadFailed && (
        <div role="status" className="rounded-lg border border-yellow-500/30 bg-yellow-500/10 px-3 py-2 text-xs text-text-primary">
          Booked exports could not be loaded. They will appear here once HEM reconnects.
        </div>
      )}

      {upcoming.length > 0 && (
        <ul className="space-y-2" aria-label="Booked Exports">
          {upcoming.map((event) => (
            <li key={event.id} className="bg-bg-surface rounded-xl p-3 space-y-1">
              <div className="flex items-center justify-between gap-2">
                <span className="text-text-primary text-sm">
                  Slot {event.slot} · {formatEventWindow(event)}
                </span>
                <span className="text-xs text-text-secondary">{stateLabel(event.state)}</span>
              </div>
              {event.state === 'active' && event.original && (
                <p className="text-xs text-text-secondary">
                  Slot {event.slot} is set for this export for now and goes back to{' '}
                  {formatSlotRange(event.original.slot)} afterwards.
                </p>
              )}
              {event.note && <p className="text-xs text-yellow-300">{event.note}</p>}
              <button
                onClick={() => void cancel(event)}
                disabled={saving}
                aria-label={`Cancel export in slot ${event.slot} on ${formatEventWindow(event)}`}
                className="text-xs text-red-300 hover:text-red-200 disabled:opacity-50"
              >
                {event.state === 'active' ? 'Stop And Put The Slot Back' : 'Cancel'}
              </button>
            </li>
          ))}
        </ul>
      )}

      {form && (
        <div className="bg-bg-surface rounded-xl p-3 space-y-3">
          {maxDischargeSlots > 1 && (
            <label className="flex items-center justify-between gap-3 text-sm text-text-secondary">
              Slot To Use
              <select
                value={form.slot}
                onChange={(e) => update({ slot: Number(e.target.value) })}
                disabled={saving}
                className="bg-bg-elevated text-text-primary rounded-lg px-2 py-1 text-sm"
              >
                {Array.from({ length: maxDischargeSlots }, (_, i) => {
                  const current = dischargeSlots[i];
                  return (
                    <option key={i} value={i + 1}>
                      Slot {i + 1}
                      {current ? ` (now ${formatSlotRange(current)})` : ''}
                    </option>
                  );
                })}
              </select>
            </label>
          )}
          <div className="grid grid-cols-1 sm:grid-cols-3 gap-3">
            <label className="flex flex-col gap-1 text-sm text-text-secondary">
              Date
              <input
                type="date"
                value={form.date}
                onChange={(e) => update({ date: e.target.value })}
                disabled={saving}
                className="bg-bg-elevated text-text-primary rounded-lg px-2 py-1"
              />
            </label>
            <label className="flex flex-col gap-1 text-sm text-text-secondary">
              Start
              <input
                type="time"
                value={form.start}
                onChange={(e) => update({ start: e.target.value })}
                disabled={saving}
                className="bg-bg-elevated text-text-primary rounded-lg px-2 py-1"
              />
            </label>
            <label className="flex flex-col gap-1 text-sm text-text-secondary">
              End
              <input
                type="time"
                value={form.end}
                onChange={(e) => update({ end: e.target.value })}
                disabled={saving}
                className="bg-bg-elevated text-text-primary rounded-lg px-2 py-1"
              />
            </label>
          </div>
          {showFloor && (
            <div className="space-y-1">
              <div className="flex items-center justify-between">
                <span className="text-text-secondary text-sm">Stop Exporting At</span>
                <span className="font-mono text-text-primary text-sm">{form.target_soc}%</span>
              </div>
              <input
                type="range"
                min={4}
                max={99}
                step={1}
                value={form.target_soc}
                aria-label="Stop Exporting At"
                onChange={(e) => update({ target_soc: Number(e.target.value) })}
                disabled={saving}
                className="w-full"
              />
            </div>
          )}
          {error && (
            <div role="alert" className="rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-sm text-red-300">
              {error}
            </div>
          )}
          <button
            onClick={() => void book()}
            disabled={saving || !nowLocal}
            className="w-full py-2 bg-accent/20 text-accent rounded-lg text-sm font-medium hover:bg-accent/30 transition disabled:opacity-50"
          >
            {saving ? 'Working…' : 'Book Export'}
          </button>
        </div>
      )}

      {recent.length > 0 && (
        <details className="text-xs text-text-secondary">
          <summary className="cursor-pointer">Recent Exports</summary>
          <ul className="mt-2 space-y-1">
            {recent.map((event) => (
              <li key={event.id}>
                Slot {event.slot} · {formatEventWindow(event)} · {stateLabel(event.state)}
                {event.note ? ` — ${event.note}` : ''}
              </li>
            ))}
          </ul>
        </details>
      )}
    </div>
  );
}
