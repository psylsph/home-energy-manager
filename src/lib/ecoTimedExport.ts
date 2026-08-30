/**
 * Eco and Timed Export state derivation for the Control page.
 *
 * The Eco / Timed Export redesign (issue #289) separates battery behaviour into:
 * - **Baseline**: Eco (normal match-demand mode)
 * - **Current behaviour**: Eco, charging, demand discharge, export, paused, blocked, or safety override
 * - **Schedules**: Off / Configured / Armed / Active now / Blocked / Error
 *
 * Raw HR27/HR59 register values are developer diagnostics; the UI presents
 * user-meaningful states derived from those registers plus schedule context.
 */

import type { InverterSnapshot, ScheduleSlot } from './types';

/**
 * Eco baseline presentation states.
 */
export type EcoPresentationState =
    | 'active' // Eco controls discharge now
    | 'temporarily_overridden' // Eco is baseline but Timed Export/Force Discharge/pause currently owns behaviour
    | 'off'; // Eco explicitly disabled and not scheduled for restoration

/**
 * Timed Export schedule presentation states.
 */
export type TimedExportPresentationState =
    | 'off' // No schedule configured or disabled
    | 'configured' // Schedule configured, waiting for window
    | 'armed' // Inside window, registers confirmed, telemetry not yet consistent with export
    | 'active_now' // Inside window, export confirmed, telemetry consistent with export
    | 'entering' // Machine state: Entering (entry writes issued, awaiting readback)
    | 'exiting' // Machine state: Exiting (exit writes issued, awaiting readback)
    | 'blocked_by_pause' // Inside window but HR318 pause is currently blocking discharge
    | 'error'; // Write failure or unexpected state

/**
 * Thresholds (in watts) for deciding whether telemetry confirms an active
 * export. Conservative values: a discharge >50 W and/or a grid export
 * >30 W is enough evidence; small numbers can be noise on cloudy days.
 */
const TELEMETRY_DISCHARGE_W = 50;
const TELEMETRY_EXPORT_W = 30;

/**
 * Check if a schedule slot has a valid non-zero window.
 */
export function isSlotConfigured(slot: ScheduleSlot | undefined): boolean {
    if (!slot || !slot.enabled) {
        return false;
    }
    // Zero-length slot (start == end) is disabled
    return !(slot.start_hour === slot.end_hour && slot.start_minute === slot.end_minute);
}

/**
 * Check if any slot in the array is configured (enabled with non-zero window).
 */
export function hasConfiguredSlot(slots: ScheduleSlot[] | undefined): boolean {
    if (!slots || slots.length === 0) {
        return false;
    }
    return slots.some(isSlotConfigured);
}

/**
 * A merged effective export window on the circular 24-hour timeline.
 */
export interface MergedWindow {
    /** Start minute-of-day (inclusive). */
    start: number;
    /** End minute-of-day (exclusive). */
    end: number;
}

/**
 * Merge overlapping and adjacent schedule slots into effective windows on a
 * circular 24-hour timeline. Adjacent windows such as 16:00-17:00 and
 * 17:00-18:00 merge into 16:00-18:00 so `findNextTransition` can report a
 * single "Eco resumes" time instead of the spurious transition at the
 * internal boundary. Overnight slots are unwrapped, merged, and re-wrapped.
 *
 * Returns an empty list when no slots are configured.
 */
export function mergeScheduleWindows(slots: ScheduleSlot[]): MergedWindow[] {
    const arcs: Array<{ start: number; end: number }> = [];
    for (const slot of slots) {
        if (!isSlotConfigured(slot)) continue;
        const start = slot.start_hour * 60 + slot.start_minute;
        const end = slot.end_hour * 60 + slot.end_minute;
        if (start < end) {
            arcs.push({ start, end });
        } else if (start > end) {
            // Overnight: unwrap into two linear arcs.
            arcs.push({ start, end: 1440 });
            arcs.push({ start: 0, end });
        } else {
            // Zero-length — already filtered by isSlotConfigured, skip.
        }
    }
    if (arcs.length === 0) return [];

    // Sort by start; merge adjacent/overlapping arcs.
    arcs.sort((a, b) => a.start - b.start);
    const merged: Array<{ start: number; end: number }> = [];
    for (const arc of arcs) {
        const last = merged[merged.length - 1];
        if (last && arc.start <= last.end) {
            if (arc.end > last.end) last.end = arc.end;
        } else {
            merged.push({ ...arc });
        }
    }

    // Re-wrap on the 24-hour circle: if the merged arcs cover a full day
    // there is no "off" gap, return a single full window. Otherwise join
    // the two pieces which meet at midnight into one wrapped interval. This
    // preserves the real end transition for a window such as 22:00-02:00;
    // returning the linear 22:00-24:00 piece would incorrectly report 00:00.
    const total = merged.reduce((acc, m) => acc + (m.end - m.start), 0);
    if (total >= 1440) return [{ start: 0, end: 1440 }];

    const first = merged[0];
    const last = merged[merged.length - 1];
    if (first.start === 0 && last.end === 1440 && merged.length > 1) {
        return [
            { start: last.start, end: first.end },
            ...merged.slice(1, -1),
        ].sort((a, b) => a.start - b.start);
    }

    return merged;
}

/**
 * Whether the current minute-of-day falls inside any merged export window.
 */
export function isInMergedWindow(windows: MergedWindow[], minuteOfDay: number): boolean {
    for (const w of windows) {
        if (w.start <= w.end) {
            if (minuteOfDay >= w.start && minuteOfDay < w.end) return true;
        } else {
            if (minuteOfDay >= w.start || minuteOfDay < w.end) return true;
        }
    }
    return false;
}

/**
 * Check if the current time is inside an enabled slot window.
 * Handles overnight slots (e.g., 22:00–06:00).
 *
 * Kept for backwards compatibility with code that still binds to
 * `isInSlotWindow` directly; new callers should prefer `isInMergedWindow`
 * with merged effective windows.
 */
export function isInSlotWindow(
    slots: ScheduleSlot[] | undefined,
    hour: number,
    minute: number
): boolean {
    if (!slots || slots.length === 0) {
        return false;
    }

    const minuteOfDay = hour * 60 + minute;

    return slots.some((slot) => {
        if (!isSlotConfigured(slot)) {
            return false;
        }
        const start = slot.start_hour * 60 + slot.start_minute;
        const end = slot.end_hour * 60 + slot.end_minute;

        if (start < end) {
            // Normal slot: start <= minute < end
            return minuteOfDay >= start && minuteOfDay < end;
        } else {
            // Overnight slot: crosses midnight
            return minuteOfDay >= start || minuteOfDay < end;
        }
    });
}

/**
 * Find the next transition time after the given time, taking merged
 * effective windows into account. Returns the time string (HH:MM) or null
 * if there are no transitions. For adjacent slots such as 16:00-17:00 and
 * 17:00-18:00 the union is one continuous export window so the reported
 * "Eco resumes" time is 18:00, not the spurious 17:00.
 */
export function findNextTransition(
    slots: ScheduleSlot[] | undefined,
    hour: number,
    minute: number
): string | null {
    if (!slots || slots.length === 0) {
        return null;
    }
    const windows = mergeScheduleWindows(slots);
    if (windows.length === 0) return null;
    if (windows.length === 1 && windows[0].start === 0 && windows[0].end === 1440) {
        // Schedule covers the full day; no transition to Eco.
        return null;
    }

    const minuteOfDay = hour * 60 + minute;
    if (isInMergedWindow(windows, minuteOfDay)) {
        // Find the current window's end (exclusive) on the circular timeline.
        for (const w of windows) {
            if (w.start <= minuteOfDay && minuteOfDay < w.end) {
                return formatHHMM(w.end);
            }
            if (w.start > w.end && (minuteOfDay >= w.start || minuteOfDay < w.end)) {
                return formatHHMM(w.end);
            }
        }
        return null;
    }
    // Outside any window — find the next start on the circular timeline.
    let best: number | null = null;
    for (const w of windows) {
        for (const candidate of [w.start, w.start - 1440]) {
            if (candidate > minuteOfDay) {
                if (best === null || candidate < best) best = candidate;
            }
        }
    }
    if (best === null) {
        // Wrap to the first start of the next day.
        const firstStart = windows[0].start;
        best = firstStart + 1440;
    }
    return formatHHMM(((best % 1440) + 1440) % 1440);
}

function formatHHMM(minute: number): string {
    const m = ((minute % 1440) + 1440) % 1440;
    const h = Math.floor(m / 60);
    const mm = m % 60;
    return `${h.toString().padStart(2, '0')}:${mm.toString().padStart(2, '0')}`;
}

/**
 * Parse the inverter wall-clock ("YYYY-MM-DD HH:MM:SS") into a minute-of-
 * day value. Returns `null` when the registers are absent or malformed.
 *
 * Export windows must be evaluated on the **inverter's** clock, not the
 * host/browser clock: HEM may run in a UTC container while the inverter
 * (and the user) are on local time. The backend exposes
 * `snapshot.inverter_minute_of_day` evaluated on the same clock.
 */
export function inverterMinuteOfDay(snapshot: InverterSnapshot | null | undefined): number | null {
    if (!snapshot || !snapshot.inverter_time) return null;
    const timePart = snapshot.inverter_time.split(' ')[1];
    if (!timePart) return null;
    const parts = timePart.split(':');
    if (parts.length < 2) return null;
    const hour = parseInt(parts[0], 10);
    const minute = parseInt(parts[1], 10);
    if (!Number.isFinite(hour) || !Number.isFinite(minute)) return null;
    if (hour < 0 || hour > 23 || minute < 0 || minute > 59) return null;
    return hour * 60 + minute;
}

/**
 * The HR318 pause-mode + HR319/HR320 pause-window logic, shared between
 * Rust and TypeScript. Pause is *armed* by HR318 (modes 2 and 3); discharge
 * is actually *blocked* while the current inverter-local minute falls
 * inside the pause window stored in HR319/320.
 *
 * During an enabled Timed Discharge window (the complement of the pause
 * window), HR318 does not block discharge. A misconfigured pause slot
 * (disabled or zero-length) blocks nothing.
 */
export function hr318BlocksDischarge(
    snapshot: InverterSnapshot,
    minuteOfDay: number
): boolean {
    const mode = snapshot.battery_pause_mode;
    if (mode !== 2 && mode !== 3) return false;
    const slot = snapshot.battery_pause_slot;
    if (!slot || !slot.enabled) return false;
    const start = slot.start_hour * 60 + slot.start_minute;
    const end = slot.end_hour * 60 + slot.end_minute;
    if (start === end) return false; // zero-length window
    if (start < end) {
        return minuteOfDay >= start && minuteOfDay < end;
    }
    // Overnight pause window.
    return minuteOfDay >= start || minuteOfDay < end;
}

/**
 * Check if the current snapshot represents a genuine Timed Export state.
 *
 * HR59 alone is NOT sufficient: HR27=1/HR59=1 is Timed Demand (demand-matched
 * discharge), not full-power export. Timed Export requires HR27=0 AND HR59=1.
 */
export function isTimedExportActive(snapshot: InverterSnapshot | null): boolean {
    if (!snapshot) return false;
    return snapshot.battery_power_mode === 0 && snapshot.enable_discharge === true;
}

/**
 * Check if the current snapshot represents Timed Demand (demand-matched discharge).
 *
 * HR27=1/HR59=1 means the battery follows home demand; it does not deliberately
 * export surplus. This is NOT Timed Export.
 */
export function isTimedDemandActive(snapshot: InverterSnapshot | null): boolean {
    if (!snapshot) return false;
    return snapshot.battery_power_mode === 1 && snapshot.enable_discharge === true;
}

/**
 * Whether the live telemetry is consistent with active maximum-power
 * export. HEM convention: battery_power > 0 means discharging; grid_power
 * > 0 means exporting (IR30 signed +export/-import).
 */
export function telemetryConfirmsExport(snapshot: InverterSnapshot): boolean {
    const discharging = (snapshot.battery_power ?? 0) > TELEMETRY_DISCHARGE_W;
    const exporting = (snapshot.grid_power ?? 0) > TELEMETRY_EXPORT_W;
    return discharging || exporting;
}

/**
 * Whether the managed Timed Export schedule owns the current inverter-local
 * minute. This is deliberately separate from register readback: a transition
 * can be pending, or HR27/enable_discharge can be only partially written.
 */
export function isTimedExportWindowActive(
    scheduleEnabled: boolean,
    slots: ScheduleSlot[] | undefined,
    minuteOfDay: number,
): boolean {
    return scheduleEnabled
        && hasConfiguredSlot(slots)
        && isInMergedWindow(mergeScheduleWindows(slots ?? []), minuteOfDay);
}

/**
 * Whether an export-shaped readback belongs to the manual Force Discharge
 * action rather than the managed Timed Export schedule. Physical slots remain
 * part of this check because an armed register pair without a slot is an
 * invalid/stale state, not a running Force Discharge action.
 */
export function isForceDischargeActive(
    snapshot: InverterSnapshot | null,
    scheduleEnabled: boolean,
    scheduledSlots: ScheduleSlot[] | undefined,
    minuteOfDay: number,
): boolean {
    if (!snapshot || !isTimedExportActive(snapshot)) return false;
    if (!isInSlotWindow(snapshot.discharge_slots, Math.floor(minuteOfDay / 60), minuteOfDay % 60)) {
        return false;
    }
    return !isTimedExportWindowActive(scheduleEnabled, scheduledSlots, minuteOfDay);
}

/**
 * Derive the Eco baseline presentation state.
 */
export function deriveEcoState(
    snapshot: InverterSnapshot | null,
    isTimedExportActive: boolean,
    isForceDischargeActive: boolean,
    isPaused: boolean,
    options?: {
        /** Evaluated inverter-local minute, injected by the caller/tests. */
        minuteOfDay?: number;
        /** Host fallback clock when the inverter clock is unavailable. */
        now?: Date;
    }
): EcoPresentationState {
    if (!snapshot) {
        return 'off';
    }

    const isEcoMode = snapshot.battery_power_mode === 1;
    const now = options?.now ?? new Date();
    const minuteOfDay = options?.minuteOfDay
        ?? inverterMinuteOfDay(snapshot)
        ?? (now.getHours() * 60 + now.getMinutes());
    const isPausedByRegister = hr318BlocksDischarge(snapshot, minuteOfDay);

    if (isTimedExportActive || isForceDischargeActive) {
        return 'temporarily_overridden';
    }

    if (isPaused || isPausedByRegister) {
        return 'temporarily_overridden';
    }

    if (isEcoMode) {
        return 'active';
    }

    return 'off';
}

/**
 * Extract the Timed Export machine-state discriminant from the backend's
 * serialized machine_state value. Struct variants serialise as
 * `{VariantName: {...}}`; unit variants as a plain string. The legacy
 * `state` fallback can also carry whole Debug formatting such as
 * `"Entering { polls_waiting: 0, retries: 1 }"` — split on the brace and
 * trim so transitional/error presentation keeps working verbatim during a
 * rolling upgrade (CODE_REVIEW.md finding 3).
 */
export function extractMachineStateName(machineState: unknown): string {
    if (typeof machineState === 'string') {
        const brace = machineState.indexOf('{');
        const name = (brace === -1 ? machineState : machineState.slice(0, brace)).trim();
        return name.length > 0 ? name : 'Off';
    }
    if (machineState && typeof machineState === 'object') {
        const keys = Object.keys(machineState);
        if (keys.length === 1) return keys[0];
    }
    return 'Off';
}

/**
 * Derive the Timed Export schedule presentation state.
 *
 * `scheduleEnabled` is the boolean managed-schedule flag from the backend
 * (NOT a derived "has any configured slot" check — a disabled schedule
 * whose desired slots are retained must still show as **Off**).
 *
 * `machineStateName` is the discriminant of the backend's machine state
 * (e.g. "Off", "Active", "Entering", "Exiting", "Configured",
 * "BlockedByPause", "Error") — used to surface transitional states.
 *
 * Telemetry semantics (code-review finding): `active_now` requires the
 * registers to be export-confirmed AND the live telemetry to show
 * discharge / export within defined tolerances. When only the registers
 * are confirmed (e.g. battery at reserve or grid not yet reflecting
 * export), the state is `armed`.
 */
export function deriveTimedExportState(
    snapshot: InverterSnapshot | null,
    scheduleEnabled: boolean,
    slots: ScheduleSlot[] | undefined,
    options?: Date | {
        now?: Date;
        machineStateName?: string;
    }
): TimedExportPresentationState {
    if (!snapshot || !scheduleEnabled) {
        return 'off';
    }
    if (!hasConfiguredSlot(slots)) {
        return 'off';
    }

    // Surface transitional machine states from the backend so the UI can
    // present pending transitions rather than falling back to a wrong
    // steady-state label.
    const machineStateName = options instanceof Date ? undefined : options?.machineStateName;
    if (machineStateName === 'Error') return 'error';
    if (machineStateName === 'Entering') return 'entering';
    if (machineStateName === 'Exiting') return 'exiting';

    const nowDate = options instanceof Date ? options : options?.now ?? new Date();
    const minuteOfDay = inverterMinuteOfDay(snapshot)
        ?? (nowDate.getHours() * 60 + nowDate.getMinutes());

    const inWindow = isInMergedWindow(mergeScheduleWindows(slots ?? []), minuteOfDay);
    const pauseBlocking = hr318BlocksDischarge(snapshot, minuteOfDay);
    const isExportConfirmed = isTimedExportActive(snapshot);

    if (inWindow && pauseBlocking) {
        return 'blocked_by_pause';
    }
    if (inWindow && isExportConfirmed) {
        return telemetryConfirmsExport(snapshot) ? 'active_now' : 'armed';
    }
    if (inWindow && !snapshot.enable_discharge) {
        // A just-cleared HR318 pause can leave the fetched backend metadata
        // at Configured until the next poll advances the machine. The window
        // is already open, so calling its end the "next export start" is
        // both misleading and an hour late for a 10:00-11:00 slot. Present
        // the live condition as entry pending; the backend will either
        // confirm Entering/Active on the next poll or surface Error.
        return 'entering';
    }
    return 'configured';
}

/**
 * Format the Eco state for display.
 */
export function formatEcoState(state: EcoPresentationState): string {
    switch (state) {
        case 'active':
            return 'Eco — Active';
        case 'temporarily_overridden':
            return 'Eco — Temporarily overridden';
        case 'off':
            return 'Eco — Off';
    }
}

/**
 * Format the Timed Export state for display.
 */
export function formatTimedExportState(state: TimedExportPresentationState): string {
    switch (state) {
        case 'off':
            return 'Timed Export — Off';
        case 'configured':
            return 'Timed Export — Configured';
        case 'armed':
            return 'Timed Export — Armed';
        case 'entering':
            return 'Timed Export — Entering…';
        case 'exiting':
            return 'Timed Export — Exiting…';
        case 'active_now':
            return 'Timed Export — Exporting now';
        case 'blocked_by_pause':
            return 'Timed Export — Blocked by Pause Discharge';
        case 'error':
            return 'Timed Export — Error';
    }
}

/**
 * Visual variant of the Timed Export control button.
 *
 * - `neutral` — nothing scheduled; the control offers Arm
 * - `scheduled` — schedule configured and will fire, but the physical
 *   enable registers are not (yet) confirmed (schedule intent colour)
 * - `active` — readback confirmed HR27=0 + discharge-enable (and telemetry
 *   agrees for `active_now`); the control offers Stop
 * - `pending` — a boundary transition is in flight (`Entering`/`Exiting`/API
 *   apply), shown as pending rather than active
 * - `blocked` — in-window but HR318 pause is blocking discharge
 * - `error` — the arm failed or the machine surfaced an error
 */
export type TimedExportButtonVariant =
    | 'neutral'
    | 'scheduled'
    | 'active'
    | 'pending'
    | 'blocked'
    | 'error';

/** Presentation of the single Timed Export toggle (CODE_REVIEW.md). */
export interface TimedExportButtonPresentation {
    /** Visible button label carrying the explicit start/stop semantics. */
    label: string;
    /** Visual variant distinguishing schedule intent from confirmed state. */
    variant: TimedExportButtonVariant;
    /** What clicking does: arm the schedule or stop/disarm it. */
    action: 'arm' | 'stop';
    /** Accessible name including the presentation state and the action. */
    ariaLabel: string;
    /** Whether the control is disabled (Arm requires a configured slot). */
    disabled: boolean;
}

/**
 * Derive the Timed Export control-button presentation from the schedule
 * presentation state (see `deriveTimedExportState`).
 *
 * One toggle with explicit `Arm`/`Stop` semantics (the settled answer to the
 * CODE_REVIEW.md open question): schedule intent and confirmed inverter
 * state stay visually distinct through the `variant`, while the label always
 * names the action the button performs — never a bare mode name whose
 * start/stop meaning must be inferred from a possibly-stale register
 * snapshot. A failed or deferred arm surfaces as `error`/`pending`, never as
 * active Timed Export.
 */
export function deriveTimedExportButton(
    state: TimedExportPresentationState,
    options: {
        /** Whether any discharge slot is configured (gates the Arm action). */
        hasConfiguredSlot?: boolean;
        /** A mode-change API request is currently in flight. */
        applying?: boolean;
        /** The last arm/toggle request failed; the control must not look active. */
        armError?: boolean;
        /** Readback physically confirms export registers (HR27=0 + enable).
         *  Even when no slot is configured (a stale/invalid armed state), the
         *  control must offer Stop rather than a disabled Arm. */
        physicallyArmed?: boolean;
    } = {},
): TimedExportButtonPresentation {
    const stopHint = 'Press to stop Timed Export.';
    const armHint = 'Press to arm Timed Export.';

    if (options.applying) {
        return {
            label: 'Applying…',
            variant: 'pending',
            action: state === 'off' ? 'arm' : 'stop',
            ariaLabel: `Timed Export — applying. ${state === 'off' ? armHint : stopHint}`,
            disabled: false,
        };
    }

    if (state === 'off' && options.physicallyArmed) {
        // Readback-confirmed export with no configured window (e.g. an
        // externally created invalid state awaiting poll-loop repair).
        // Registers are confirmed, so the control offers Stop and never
        // disables — there must always be a way to leave export mode.
        return {
            label: 'Stop Timed Export',
            variant: 'active',
            action: 'stop',
            ariaLabel: `Timed Export — armed without a configured window. ${stopHint}`,
            disabled: false,
        };
    }

    if (state === 'off') {
        const disabled = !options.hasConfiguredSlot;
        const variant = options.armError ? 'error' : 'neutral';
        const ariaLabel = options.armError
            ? `Timed Export — last attempt failed. ${armHint}`
            : disabled
              ? 'Timed Export — Off. Configure a discharge slot to arm Timed Export.'
              : `Timed Export — Off. ${armHint}`;
        return {
            label: 'Arm Timed Export',
            variant,
            action: 'arm',
            ariaLabel,
            disabled,
        };
    }

    switch (state) {
        case 'entering':
            return {
                label: 'Arming…',
                variant: 'pending',
                action: 'stop',
                ariaLabel: `${formatTimedExportState(state)}. ${stopHint}`,
                disabled: false,
            };
        case 'exiting':
            return {
                label: 'Stopping…',
                variant: 'pending',
                action: 'stop',
                ariaLabel: `${formatTimedExportState(state)}. ${stopHint}`,
                disabled: false,
            };
        case 'error':
            return {
                label: 'Timed Export — Error',
                variant: 'error',
                action: 'stop',
                ariaLabel: `${formatTimedExportState(state)}. ${stopHint}`,
                disabled: false,
            };
        case 'blocked_by_pause':
            return {
                label: 'Timed Export — Blocked',
                variant: 'blocked',
                action: 'stop',
                ariaLabel: `${formatTimedExportState(state)}. ${stopHint}`,
                disabled: false,
            };
        case 'active_now':
        case 'armed':
            return {
                label: 'Stop Timed Export',
                variant: 'active',
                action: 'stop',
                ariaLabel: `${formatTimedExportState(state)}. ${stopHint}`,
                disabled: false,
            };
        case 'configured':
        default:
            // State label, not an action label: "Stop Timed Export" is
            // reserved for readback-confirmed export, and a future window
            // must not read as live. Mirrors the Schedule panel wording;
            // the stop action remains in the accessible name.
            return {
                label: 'Timed Export — Configured',
                variant: 'scheduled',
                action: 'stop',
                ariaLabel: `${formatTimedExportState('configured')}. ${stopHint}`,
                disabled: false,
            };
    }
}
