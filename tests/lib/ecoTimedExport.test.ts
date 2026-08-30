/**
 * Tests for Eco / Timed Export state derivation helpers.
 */

import { describe, it, expect } from 'vitest';
import {
    deriveTimedExportButton,
    deriveEcoState,
    deriveTimedExportState,
    extractMachineStateName,
    findNextTransition,
    formatEcoState,
    formatTimedExportState,
    hasConfiguredSlot,
    hr318BlocksDischarge,
    isForceDischargeActive,
    isInSlotWindow,
    isSlotConfigured,
    isTimedExportWindowActive,
    isTimedDemandActive,
    isTimedExportActive,
    type EcoPresentationState,
    type TimedExportPresentationState,
} from '../../src/lib/ecoTimedExport';
import type { InverterSnapshot, ScheduleSlot } from '../../src/lib/types';

/** Minimal slot factory */
function slot(overrides: Partial<ScheduleSlot> = {}): ScheduleSlot {
    return {
        enabled: true,
        start_hour: 16,
        start_minute: 0,
        end_hour: 19,
        end_minute: 0,
        target_soc: 4,
        ...overrides,
    };
}

/** Minimal snapshot factory for Eco/Timed Export tests */
function snapshot(overrides: Partial<InverterSnapshot> = {}): InverterSnapshot {
    return {
        battery_power_mode: 1,
        enable_discharge: false,
        battery_pause_mode: 0,
        discharge_slots: [slot()],
        // Default telemetry that confirms active export — tests that
        // specifically want the "armed but not yet exporting" branch
        // override these to zero.
        battery_power: 100,
        grid_power: 50,
        ...overrides,
    } as InverterSnapshot;
}

describe('isSlotConfigured', () => {
    it('returns true for enabled slot with non-zero window', () => {
        expect(isSlotConfigured(slot())).toBe(true);
    });

    it('returns false for disabled slot', () => {
        expect(isSlotConfigured(slot({ enabled: false }))).toBe(false);
    });

    it('returns false for zero-length slot', () => {
        expect(
            isSlotConfigured(slot({ start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0 }))
        ).toBe(false);
    });

    it('returns false for undefined slot', () => {
        expect(isSlotConfigured(undefined)).toBe(false);
    });
});

describe('hasConfiguredSlot', () => {
    it('returns true when any slot is configured', () => {
        expect(hasConfiguredSlot([slot()])).toBe(true);
    });

    it('returns false when no slots are configured', () => {
        expect(hasConfiguredSlot([slot({ enabled: false })])).toBe(false);
    });

    it('returns false for empty array', () => {
        expect(hasConfiguredSlot([])).toBe(false);
    });

    it('returns false for undefined', () => {
        expect(hasConfiguredSlot(undefined)).toBe(false);
    });
});

describe('isInSlotWindow', () => {
    it('returns true when inside slot window', () => {
        expect(isInSlotWindow([slot()], 17, 0)).toBe(true);
    });

    it('returns false when outside slot window', () => {
        expect(isInSlotWindow([slot()], 10, 0)).toBe(false);
    });

    it('handles overnight slot (22:00–06:00)', () => {
        const overnight = slot({ start_hour: 22, start_minute: 0, end_hour: 6, end_minute: 0 });
        expect(isInSlotWindow([overnight], 23, 0)).toBe(true); // 23:00
        expect(isInSlotWindow([overnight], 2, 0)).toBe(true); // 02:00
        expect(isInSlotWindow([overnight], 12, 0)).toBe(false); // 12:00 (outside)
    });

    it('returns false for zero-length slot', () => {
        const zeroLength = slot({ start_hour: 16, start_minute: 0, end_hour: 16, end_minute: 0 });
        expect(isInSlotWindow([zeroLength], 16, 0)).toBe(false);
    });

    it('returns false when slots is undefined', () => {
        expect(isInSlotWindow(undefined, 17, 0)).toBe(false);
    });
});

describe('findNextTransition', () => {
    it('returns next slot start time', () => {
        // At 10:00, next transition is 16:00
        expect(findNextTransition([slot()], 10, 0)).toBe('16:00');
    });

    it('returns next slot end time when inside window', () => {
        // At 17:00 (inside 16:00–19:00), next transition is 19:00
        expect(findNextTransition([slot()], 17, 0)).toBe('19:00');
    });

    it('wraps to first transition of next day', () => {
        // At 20:00, next transition is 16:00 (next day)
        expect(findNextTransition([slot()], 20, 0)).toBe('16:00');
    });

    it('returns null for no slots', () => {
        expect(findNextTransition([], 10, 0)).toBe(null);
        expect(findNextTransition(undefined, 10, 0)).toBe(null);
    });
});

describe('deriveEcoState', () => {
    it('returns active when in Eco mode with no overrides', () => {
        const state = deriveEcoState(snapshot(), false, false, false);
        expect(state).toBe('active');
    });

    it('returns temporarily_overridden during Timed Export', () => {
        const state = deriveEcoState(snapshot(), true, false, false);
        expect(state).toBe('temporarily_overridden');
    });

    it('returns temporarily_overridden during Force Discharge', () => {
        const state = deriveEcoState(snapshot(), false, true, false);
        expect(state).toBe('temporarily_overridden');
    });

    it('returns temporarily_overridden when paused by HR318', () => {
        // A 24-hour pause window blocks discharge at every minute.
        const state = deriveEcoState(
            snapshot({
            battery_pause_mode: 2,
                battery_pause_slot: slot({ enabled: true, start_hour: 0, end_hour: 23, end_minute: 59 }),
            }),
            false,
            false,
            false,
            { minuteOfDay: 17 * 60 }
        );
        expect(state).toBe('temporarily_overridden');
    });

    it('returns off when not in Eco mode and no overrides', () => {
        const state = deriveEcoState(snapshot({ battery_power_mode: 0 }), false, false, false);
        expect(state).toBe('off');
    });

    it('returns off when snapshot is null', () => {
        const state = deriveEcoState(null, false, false, false);
        expect(state).toBe('off');
    });
});

describe('deriveTimedExportState', () => {
    it('returns off when schedule is disabled', () => {
        const state = deriveTimedExportState(snapshot(), false, [slot()]);
        expect(state).toBe('off');
    });

    it('returns off when no slots are configured', () => {
        const state = deriveTimedExportState(snapshot(), true, [slot({ enabled: false })]);
        expect(state).toBe('off');
    });

    it('returns configured when outside window', () => {
        const state = deriveTimedExportState(
            snapshot(),
            true,
            [slot()],
            new Date('2026-06-28T10:00:00')
        );
        expect(state).toBe('configured');
    });

    it('returns active_now when inside window with confirmed export', () => {
        const state = deriveTimedExportState(
            snapshot({ battery_power_mode: 0, enable_discharge: true }),
            true,
            [slot()],
            new Date('2026-06-28T17:00:00')
        );
        expect(state).toBe('active_now');
    });

    it('returns blocked_by_pause when inside window but paused', () => {
        // An overnight pause window 20:00→17:00 blocks discharge during
        // the export window; the UI must report blocked, not armed.
        const state = deriveTimedExportState(
            snapshot({
                battery_power_mode: 0,
                enable_discharge: true,
                    battery_pause_mode: 2,
                    battery_pause_slot: slot({
                        enabled: true,
                        start_hour: 20,
                        end_hour: 18,
                    }),
            }),
            true,
            [slot()],
            new Date('2026-06-28T17:00:00')
        );
        expect(state).toBe('blocked_by_pause');
    });

    it('returns configured when HR59=1 but HR27=1 (Timed Demand, not Export)', () => {
        // This is the key regression test: HR59 alone is NOT sufficient
        const state = deriveTimedExportState(
            snapshot({ battery_power_mode: 1, enable_discharge: true }), // HR27=1, HR59=1
            true,
            [slot()],
            new Date('2026-06-28T17:00:00')
        );
        expect(state).toBe('configured'); // NOT 'active_now'
    });

    it('uses current time when now is not provided', () => {
        const state = deriveTimedExportState(snapshot(), true, [slot()]);
        // We can't assert exact state without mocking Date, but verify it doesn't throw
        expect(['off', 'configured', 'active_now', 'blocked_by_pause']).toContain(state);
    });
});

describe('isTimedExportActive', () => {
    it('returns true for HR27=0 and HR59=1', () => {
        expect(
            isTimedExportActive(snapshot({ battery_power_mode: 0, enable_discharge: true }))
        ).toBe(true);
    });

    it('returns false for HR27=1 and HR59=1 (Timed Demand)', () => {
        expect(
            isTimedExportActive(snapshot({ battery_power_mode: 1, enable_discharge: true }))
        ).toBe(false);
    });

    it('returns false for HR27=0 and HR59=0', () => {
        expect(
            isTimedExportActive(snapshot({ battery_power_mode: 0, enable_discharge: false }))
        ).toBe(false);
    });

    it('returns false for null snapshot', () => {
        expect(isTimedExportActive(null)).toBe(false);
    });
});

describe('isTimedDemandActive', () => {
    it('returns true for HR27=1 and HR59=1', () => {
        expect(
            isTimedDemandActive(snapshot({ battery_power_mode: 1, enable_discharge: true }))
        ).toBe(true);
    });

    it('returns false for HR27=0 and HR59=1 (Timed Export)', () => {
        expect(
            isTimedDemandActive(snapshot({ battery_power_mode: 0, enable_discharge: true }))
        ).toBe(false);
    });

    it('returns false for null snapshot', () => {
        expect(isTimedDemandActive(null)).toBe(false);
    });
});

describe('discharge ownership', () => {
    it('does not label a managed Timed Export window as Force Discharge', () => {
        const state = snapshot({ battery_power_mode: 0, enable_discharge: true });
        expect(isTimedExportWindowActive(true, [slot()], 17 * 60)).toBe(true);
        expect(isForceDischargeActive(state, true, [slot()], 17 * 60)).toBe(false);
    });

    it('labels the same physical readback as Force Discharge outside the managed window', () => {
        const state = snapshot({ battery_power_mode: 0, enable_discharge: true });
        expect(isForceDischargeActive(state, true, [slot({ start_hour: 20, end_hour: 22 })], 17 * 60)).toBe(true);
    });

    it('does not label an unconfigured physical window as Force Discharge', () => {
        const state = snapshot({
            battery_power_mode: 0,
            enable_discharge: true,
            discharge_slots: [slot({ enabled: false })],
        });
        expect(isForceDischargeActive(state, false, undefined, 17 * 60)).toBe(false);
    });
});

describe('extractMachineStateName (legacy Debug-string fallback)', () => {
    it('passes unit-variant strings through', () => {
        expect(extractMachineStateName('Configured')).toBe('Configured');
        expect(extractMachineStateName('Off')).toBe('Off');
    });

    it('extracts the discriminant from a serialised struct-variant object', () => {
        expect(extractMachineStateName({ Entering: { polls_waiting: 0, retries: 1 } })).toBe('Entering');
        expect(extractMachineStateName({ Error: { reason: 'x' } })).toBe('Error');
    });

    it('splits whole Debug struct-variant strings on the brace and trims', () => {
        // The legacy `state` field carried whole Debug formatting such as
        // "Entering { polls_waiting: 0, retries: 1 }"; === 'Entering' must
        // still work during a rolling upgrade (CODE_REVIEW.md finding 3).
        expect(extractMachineStateName('Entering { polls_waiting: 0, retries: 1 }')).toBe('Entering');
        expect(extractMachineStateName('Error { reason: "write retries exhausted" }')).toBe('Error');
        expect(extractMachineStateName('Active')).toBe('Active');
    });

    it('falls back to Off for unrecognised input', () => {
        expect(extractMachineStateName(undefined)).toBe('Off');
        expect(extractMachineStateName(42)).toBe('Off');
        expect(extractMachineStateName({ A: 1, B: 2 })).toBe('Off');
    });
});

describe('formatEcoState', () => {
    it.each([
        ['active', 'Eco — Active'],
        ['temporarily_overridden', 'Eco — Temporarily overridden'],
        ['off', 'Eco — Off'],
    ] as const)('formats %s as %s', (state: EcoPresentationState, expected: string) => {
        expect(formatEcoState(state)).toBe(expected);
    });
});

describe('formatTimedExportState', () => {
    it.each([
        ['off', 'Timed Export — Off'],
        ['configured', 'Timed Export — Configured'],
        ['armed', 'Timed Export — Armed'],
        ['entering', 'Timed Export — Entering…'],
        ['exiting', 'Timed Export — Exiting…'],
        ['active_now', 'Timed Export — Exporting now'],
        ['blocked_by_pause', 'Timed Export — Blocked by Pause Discharge'],
        ['error', 'Timed Export — Error'],
    ] as const)('formats %s as %s', (state: TimedExportPresentationState, expected: string) => {
        expect(formatTimedExportState(state)).toBe(expected);
    });
});

describe('findNextTransition (merged windows)', () => {
    // Code-review finding: adjacent slots 16–17 and 17–18 form one
    // continuous effective export window; the next transition out of it
    // is 18:00, not the spurious 17:00.
    it('merges adjacent slots into a single effective window', () => {
        const slots = [
            slot({ start_hour: 16, end_hour: 17 }),
            slot({ start_hour: 17, end_hour: 18 }),
        ];
        // Inside the merged window → 18:00.
        expect(findNextTransition(slots, 16, 30)).toBe('18:00');
        // Outside → 16:00.
        expect(findNextTransition(slots, 10, 0)).toBe('16:00');
    });

    it('merges overlapping slots', () => {
        const slots = [
            slot({ start_hour: 16, end_hour: 18 }),
            slot({ start_hour: 17, end_hour: 19 }),
        ];
        expect(findNextTransition(slots, 16, 30)).toBe('19:00');
    });

    it('respects the circular boundary for overnight slots', () => {
        // 22:00–02:00 + 02:00–04:00 → one continuous 22:00–04:00 window.
        const slots = [
            slot({ start_hour: 22, end_hour: 2 }),
            slot({ start_hour: 2, end_hour: 4 }),
        ];
        expect(findNextTransition(slots, 1, 0)).toBe('04:00');
        expect(findNextTransition(slots, 10, 0)).toBe('22:00');
    });

    it('reports the real end of a single overnight window', () => {
        const slots = [slot({ start_hour: 22, end_hour: 2 })];
        expect(findNextTransition(slots, 23, 0)).toBe('02:00');
        expect(findNextTransition(slots, 10, 0)).toBe('22:00');
    });
});

describe('deriveTimedExportState telemetry semantics', () => {
    it('treats an unblocked open window as entering even if backend metadata is briefly Configured', () => {
        const state = deriveTimedExportState(
            snapshot({
                inverter_time: '2026-08-30 10:30:00',
                battery_power_mode: 1,
                enable_discharge: false,
                battery_pause_mode: 0,
            }),
            true,
            [slot({ start_hour: 10, end_hour: 11 })],
            { machineStateName: 'Configured' },
        );

        expect(state).toBe('entering');
    });

    it('returns armed (not active_now) when registers confirm but telemetry is flat', () => {
        // A battery at reserve with no grid export yet — the inverter
        // confirmed HR27=0/HR59=1 but no power is actually moving.
        const state = deriveTimedExportState(
            snapshot({
                battery_power_mode: 0,
                enable_discharge: true,
                battery_power: 0,
                grid_power: 0,
            }),
            true,
            [slot()],
            new Date('2026-06-28T17:00:00')
        );
        expect(state).toBe('armed');
    });

    it('returns entering when the backend machine state is Entering', () => {
        const state = deriveTimedExportState(
            snapshot({ battery_power_mode: 1, enable_discharge: false }),
            true,
            [slot()],
            { machineStateName: 'Entering' }
        );
        expect(state).toBe('entering');
    });

    it('returns error when the backend machine state is Error', () => {
        const state = deriveTimedExportState(
            snapshot(),
            true,
            [slot()],
            { machineStateName: 'Error' }
        );
        expect(state).toBe('error');
    });

    it('does not flip a disabled schedule into configured (finding #9)', () => {
        // scheduleEnabled=false must mean Off even when slots are
        // retained for a future enable.
        const state = deriveTimedExportState(
            snapshot(),
            false,
            [slot()],
            new Date('2026-06-28T17:00:00')
        );
        expect(state).toBe('off');
    });
});

describe('hr318BlocksDischarge (shared pause-window logic)', () => {
    // Mirror of the Rust helper: HR318 modes 2 and 3 arm a discharge pause
    // window in HR319/320; discharge is blocked only while the current
    // minute falls inside that pause window.
    it('does not block outside the pause window', () => {
        const state = snapshot({
            battery_pause_mode: 2,
            battery_pause_slot: slot({ start_hour: 20, end_hour: 4, enabled: true }),
        });
        // Pause 20:00→04:00 — daytime and the 04:00 boundary are outside.
        expect(hr318BlocksDischarge(state, 16 * 60)).toBe(false);
        expect(hr318BlocksDischarge(state, 4 * 60)).toBe(false);
    });

    it('blocks inside the pause window', () => {
        const state = snapshot({
            battery_pause_mode: 2,
            battery_pause_slot: slot({ start_hour: 20, end_hour: 4, enabled: true }),
        });
        expect(hr318BlocksDischarge(state, 20 * 60)).toBe(true);
        expect(hr318BlocksDischarge(state, 23 * 60 + 59)).toBe(true);
        expect(hr318BlocksDischarge(state, 3 * 60 + 59)).toBe(true);
    });

    it('does not block when HR318 is 1 (pause charge only)', () => {
        const state = snapshot({
            battery_pause_mode: 1,
            battery_pause_slot: slot({ start_hour: 20, end_hour: 4, enabled: true }),
        });
        expect(hr318BlocksDischarge(state, 20 * 60)).toBe(false);
    });
});

describe('deriveTimedExportButton (single toggle, dynamic label)', () => {
    it('disabled schedule with no configured slot offers Arm and stays neutral', () => {
        const button = deriveTimedExportButton('off', { hasConfiguredSlot: false });
        expect(button.label).toBe('Arm Timed Export');
        expect(button.variant).toBe('neutral');
        expect(button.action).toBe('arm');
        expect(button.disabled).toBe(true);
        expect(button.ariaLabel).toContain('Configure a discharge slot');
    });

    it('disabled schedule with a configured slot offers Arm as a retry affordance', () => {
        const button = deriveTimedExportButton('off', { hasConfiguredSlot: true });
        expect(button.label).toBe('Arm Timed Export');
        expect(button.variant).toBe('neutral');
        expect(button.action).toBe('arm');
        expect(button.disabled).toBe(false);
    });

    it('failed arm request shows the error variant without becoming active', () => {
        const button = deriveTimedExportButton('off', { hasConfiguredSlot: true, armError: true });
        expect(button.variant).toBe('error');
        expect(button.action).toBe('arm');
        expect(button.ariaLabel).toContain('last attempt failed');
    });

    it('configured future schedule uses the scheduled-to-fire variant and a state label', () => {
        // The button must not read "Stop Timed Export" while the window is
        // still in the future — that label means export is live. The state
        // label mirrors the Schedule panel; the stop action stays in the
        // accessible name.
        const button = deriveTimedExportButton('configured', {});
        expect(button.variant).toBe('scheduled');
        expect(button.label).toBe('Timed Export — Configured');
        expect(button.action).toBe('stop');
        expect(button.ariaLabel).toContain('Configured');
        expect(button.ariaLabel).toContain('Press to stop');
    });

    it('confirmed registers (armed) escalate to the active variant', () => {
        const button = deriveTimedExportButton('armed', {});
        expect(button.variant).toBe('active');
        expect(button.label).toBe('Stop Timed Export');
        expect(button.action).toBe('stop');
    });

    it('active export uses the active variant with an explicit Stop label', () => {
        const button = deriveTimedExportButton('active_now', {});
        expect(button.variant).toBe('active');
        expect(button.label).toBe('Stop Timed Export');
        expect(button.action).toBe('stop');
        expect(button.ariaLabel).toContain('Exporting now');
        expect(button.ariaLabel).toContain('Press to stop');
    });

    it('entering transition shows pending Arming state', () => {
        const button = deriveTimedExportButton('entering', {});
        expect(button.variant).toBe('pending');
        expect(button.label).toBe('Arming…');
        expect(button.action).toBe('stop');
    });

    it('exiting transition shows pending Stopping state', () => {
        const button = deriveTimedExportButton('exiting', {});
        expect(button.variant).toBe('pending');
        expect(button.label).toBe('Stopping…');
        expect(button.action).toBe('stop');
    });

    it('HR318-blocked window uses the blocked variant with a state label', () => {
        const button = deriveTimedExportButton('blocked_by_pause', {});
        expect(button.variant).toBe('blocked');
        expect(button.label).toBe('Timed Export — Blocked');
        expect(button.action).toBe('stop');
        expect(button.ariaLabel).toContain('Blocked by Pause Discharge');
    });

    it('machine error shows the error variant instead of active', () => {
        const button = deriveTimedExportButton('error', {});
        expect(button.variant).toBe('error');
        expect(button.label).toBe('Timed Export — Error');
        expect(button.action).toBe('stop');
        expect(button.ariaLabel).toContain('Error');
    });

    it('physically-armed readback without a configured window still offers Stop', () => {
        // The stale/invalid armed state (HR27=0 + enable, no slots): Stop
        // must stay reachable — never a disabled Arm.
        const button = deriveTimedExportButton('off', {
            hasConfiguredSlot: false,
            physicallyArmed: true,
        });
        expect(button.label).toBe('Stop Timed Export');
        expect(button.variant).toBe('active');
        expect(button.action).toBe('stop');
        expect(button.disabled).toBe(false);
    });

    it('an in-flight API request overrides with Applying… pending', () => {
        const button = deriveTimedExportButton('off', { hasConfiguredSlot: true, applying: true });
        expect(button.variant).toBe('pending');
        expect(button.label).toBe('Applying…');
    });
});
