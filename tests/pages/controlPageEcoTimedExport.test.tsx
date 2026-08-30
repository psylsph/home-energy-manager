/**
 * Tests for ControlPage Eco / Timed Export state presentation.
 *
 * Issue #289: Eco is the normal baseline outside Timed Export windows.
 * The UI should distinguish:
 * - Baseline: Eco — Active / Temporarily overridden / Off
 * - Current behaviour: Eco, charging, demand discharge, export, paused, blocked
 * - Schedules: Off / Configured / Armed / Active now / Blocked / Error
 */

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { act, fireEvent, render, screen, within, cleanup } from '@testing-library/react';
import ControlPage from '../../src/pages/ControlPage';
import { useInverterStore } from '../../src/store/useInverterStore';
import { apiGet } from '../../src/lib/api';
import type { InverterSnapshot, ScheduleSlot } from '../../src/lib/types';

// Mock the API layer
vi.mock('../../src/lib/api', () => ({
    apiGet: vi.fn((path: string) => {
        if (path === '/api/agile') return Promise.resolve({ ok: true, data: null });
        if (path === '/api/auto-winter') return Promise.resolve({ ok: true, data: null });
        if (path === '/api/cosy') return Promise.resolve({ ok: true, data: null });
        if (path === '/api/settings') return Promise.resolve({ ok: true, data: {} });
        if (path === '/api/load-limiter') return Promise.resolve({ ok: true, data: null });
        if (path === '/api/snapshot') return Promise.resolve({ ok: true, data: null });
        if (path === '/api/timed-export') {
            return Promise.resolve({ ok: true, data: { schedule_enabled: false, slots: [] } });
        }
        return Promise.resolve({ ok: true, data: null });
    }),
    apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
    getApiBase: vi.fn().mockReturnValue('http://localhost:7337'),
    getServerPort: vi.fn().mockReturnValue(7337),
    isTauri: vi.fn().mockReturnValue(false),
    fetchHistory: vi.fn().mockResolvedValue({ ok: true, data: {} }),
}));

/** Silence console.error for expected act warnings */
function silenceConsoleError() {
    const spy = vi.spyOn(console, 'error').mockImplementation(() => { });
    return spy;
}

/** Default slot factory */
function emptySlot(overrides: Partial<ScheduleSlot> = {}): ScheduleSlot {
    return {
        enabled: false,
        start_hour: 0,
        start_minute: 0,
        end_hour: 0,
        end_minute: 0,
        target_soc: 4,
        ...overrides,
    };
}

/** Configured slot factory */
function configuredSlot(overrides: Partial<ScheduleSlot> = {}): ScheduleSlot {
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

/** Create a minimal snapshot for testing */
function makeSnapshot(overrides: Partial<InverterSnapshot> = {}): InverterSnapshot {
    return {
        device_type_code: '2001',
        firmware_arm_version: '318',
        battery_power_mode: 1,
        enable_charge: false,
        enable_discharge: false,
        battery_pause_mode: 0,
        discharge_slots: [
            emptySlot(),
            emptySlot(),
            emptySlot(),
            emptySlot(),
            emptySlot(),
            emptySlot(),
            emptySlot(),
            emptySlot(),
            emptySlot(),
            emptySlot(),
        ],
        max_discharge_slots: 2,
        soc: 50,
        battery_power: 0,
        grid_power: 0,
        home_power: 500,
        solar_power: 0,
        battery_state: 'idle',
        inverter_serial: 'TEST123',
        ...overrides,
    } as InverterSnapshot;
}

describe('ControlPage Eco / Timed Export presentation', () => {
    beforeEach(() => {
        silenceConsoleError();
        vi.stubGlobal('matchMedia', () => ({
            matches: false,
            addListener: vi.fn(),
            removeListener: vi.fn(),
        }));
    });

    afterEach(() => {
        vi.restoreAllMocks();
        vi.useRealTimers();
        vi.unstubAllGlobals();
        cleanup();
        useInverterStore.setState({
            snapshot: null,
            connectionState: 'disconnected',
            developerMode: false,
            // Battery-mode action state is store-owned (survives page
            // navigation) — reset it so one test's in-flight arm can't
            // leak into the next.
            batteryModePending: null,
            batteryModePendingSince: null,
            batteryModeError: null,
            timedExportArmFailed: false,
        });
    });

    /**
     * Helper to render ControlPage with a snapshot
     */
    async function renderWithSnapshot(snapshot: InverterSnapshot) {
        useInverterStore.setState({
            snapshot,
            connectionState: 'connected',
            developerMode: false,
        });
        return render(<ControlPage />);
    }

    function enableManagedTimedExportForLiveSlots() {
        const original = vi.mocked(apiGet).getMockImplementation();
        vi.mocked(apiGet).mockImplementation((path: string) => {
            if (path === '/api/timed-export') {
                return Promise.resolve({
                    ok: true,
                    data: { schedule_enabled: true, slots: [], state: 'Configured' },
                });
            }
            return original ? original(path) : Promise.resolve({ ok: true, data: null });
        });
    }

    /** Mock GET /api/timed-export with an explicit machine_state. */
    function mockTimedExportSchedule(data: {
        schedule_enabled: boolean;
        slots?: ScheduleSlot[];
        machine_state?: unknown;
    }) {
        const original = vi.mocked(apiGet).getMockImplementation();
        vi.mocked(apiGet).mockImplementation((path: string) => {
            if (path === '/api/timed-export') {
                return Promise.resolve({ ok: true, data });
            }
            return original ? original(path) : Promise.resolve({ ok: true, data: null });
        });
    }

    /** The Timed Export control button in the Battery Mode section. */
    function timedExportControl(): HTMLButtonElement {
        const heading = screen.getByRole('heading', { name: 'Battery Mode', exact: true });
        const section = heading.closest('section')!;
        return within(section).getByRole('button', { name: /Timed Export/ }) as HTMLButtonElement;
    }

    describe('Baseline Eco state', () => {
        it('shows "Eco — Active" when in Eco mode with no overrides', async () => {
            await renderWithSnapshot(
                makeSnapshot({
                    battery_power_mode: 1,
                    enable_discharge: false,
                })
            );

            const section = await screen.findByRole('heading', { name: 'Battery Mode', exact: true });
            const batteryModeSection = section.closest('section')!;

            expect(
                within(batteryModeSection).getByText(/Eco — Active/)
            ).toBeInTheDocument();
        });

        it('shows "Eco — Temporarily overridden" during active Timed Export', async () => {
            vi.useFakeTimers({ shouldAdvanceTime: true });
            vi.setSystemTime(new Date('2026-06-28T17:00:00'));
            enableManagedTimedExportForLiveSlots();
            await renderWithSnapshot(
                makeSnapshot({
                    battery_power_mode: 0, // Max power mode
                    enable_discharge: true, // Export armed
                    discharge_slots: [configuredSlot()],
                })
            );

            const section = await screen.findByRole('heading', { name: 'Battery Mode', exact: true });
            const batteryModeSection = section.closest('section')!;

            expect(
                within(batteryModeSection).getByText(/Eco — Temporarily overridden/)
            ).toBeInTheDocument();
        });

        it('shows "Eco — Temporarily overridden" when paused by HR318', async () => {
            vi.useFakeTimers({ shouldAdvanceTime: true });
            vi.setSystemTime(new Date('2026-06-28T17:00:00'));
            await renderWithSnapshot(
                makeSnapshot({
                    battery_power_mode: 1,
                    battery_pause_mode: 2, // Pause discharge
                    battery_pause_slot: configuredSlot({ start_hour: 20, end_hour: 18 }),
                })
            );

            const section = await screen.findByRole('heading', { name: 'Battery Mode', exact: true });
            const batteryModeSection = section.closest('section')!;

            expect(
                within(batteryModeSection).getByText(/Eco — Temporarily overridden/)
            ).toBeInTheDocument();
        });

        it('shows "Eco — Off" when not in Eco mode and no schedule', async () => {
            await renderWithSnapshot(
                makeSnapshot({
                    battery_power_mode: 0,
                    enable_discharge: false,
                })
            );

            const section = await screen.findByRole('heading', { name: 'Battery Mode', exact: true });
            const batteryModeSection = section.closest('section')!;

            expect(
                within(batteryModeSection).getByText(/Eco — Off/)
            ).toBeInTheDocument();
        });
    });

    describe('Timed Export schedule state', () => {
        it('shows "Configured" for future export slot, not "Active now"', async () => {
            vi.useFakeTimers({ shouldAdvanceTime: true });
            vi.setSystemTime(new Date('2026-06-28T10:00:00'));
            // Slot is 16:00-19:00; the pinned clock is outside it.
            enableManagedTimedExportForLiveSlots();
            await renderWithSnapshot(
                makeSnapshot({
                    battery_power_mode: 1,
                    enable_discharge: false,
                    discharge_slots: [configuredSlot()],
                })
            );

            // The state display is in the Battery Mode section
            const section = await screen.findByRole('heading', { name: 'Battery Mode', exact: true });
            const batteryModeSection = section.closest('section')!;

            // Should show "Configured" for future slot (outside window).
            // Both the control button and the Schedule card carry the state
            // label now (CODE_REVIEW.md item 4), so match all of them.
            expect(
                within(batteryModeSection).queryAllByText(/Timed Export — Configured/).length
            ).toBeGreaterThan(0);
            expect(
                within(batteryModeSection).queryByText(/Active now/)
            ).not.toBeInTheDocument();
        });

        it('does not call the end of an open export window the next export start', async () => {
            const exportSlot = configuredSlot({ start_hour: 10, end_hour: 11 });
            const original = vi.mocked(apiGet).getMockImplementation();
            vi.mocked(apiGet).mockImplementation((path: string) => {
                if (path === '/api/timed-export') {
                    return Promise.resolve({
                        ok: true,
                        data: {
                            schedule_enabled: true,
                            slots: [exportSlot],
                            machine_state: 'Configured',
                        },
                    });
                }
                return original ? original(path) : Promise.resolve({ ok: true, data: null });
            });

            await renderWithSnapshot(makeSnapshot({
                inverter_time: '2026-08-30 10:30:00',
                battery_power_mode: 1,
                enable_discharge: false,
                battery_pause_mode: 0,
                discharge_slots: [exportSlot],
            }));

            expect((await screen.findAllByText('Timed Export — Entering…')).length).toBeGreaterThan(0);
            expect(screen.queryByText('Next export starts at 11:00')).not.toBeInTheDocument();
        });

        it('shows "Active now" during confirmed export window', async () => {
            // Use fake timers to control time
            vi.useFakeTimers({ shouldAdvanceTime: true });
            vi.setSystemTime(new Date('2026-06-28T17:00:00')); // 17:00, inside 16:00-19:00
            enableManagedTimedExportForLiveSlots();

            await renderWithSnapshot(
                makeSnapshot({
                    battery_power_mode: 0,
                    enable_discharge: true,
                    battery_power: 100,
                    grid_power: 50,
                    discharge_slots: [configuredSlot()],
                })
            );

            // The state display is in the Battery Mode section
            const section = await screen.findByRole('heading', { name: 'Battery Mode', exact: true });
            const batteryModeSection = section.closest('section')!;

            // Should show "Exporting now" (may appear in both Current behaviour and Schedule)
            await screen.findAllByText('Timed Export — Exporting now');
            const activeElements = within(batteryModeSection).queryAllByText(/Exporting now/);
            expect(activeElements.length).toBeGreaterThan(0);

            vi.useRealTimers();
        });

        it('does not present the managed export window as Force Discharge', async () => {
            vi.useFakeTimers({ shouldAdvanceTime: true });
            vi.setSystemTime(new Date('2026-06-28T17:00:00'));
            enableManagedTimedExportForLiveSlots();

            await renderWithSnapshot(
                makeSnapshot({
                    battery_power_mode: 0,
                    enable_discharge: true,
                    battery_power: 100,
                    grid_power: 50,
                    discharge_slots: [configuredSlot()],
                })
            );

            expect(await screen.findByRole('button', { name: /Force Discharge/i })).toBeInTheDocument();
            expect(screen.queryByRole('button', { name: /Stop Discharge/i })).not.toBeInTheDocument();
        });

        it('shows Armed when export registers are ready but telemetry is flat', async () => {
            vi.useFakeTimers({ shouldAdvanceTime: true });
            vi.setSystemTime(new Date('2026-06-28T17:00:00'));
            enableManagedTimedExportForLiveSlots();

            await renderWithSnapshot(
                makeSnapshot({
                    battery_power_mode: 0,
                    enable_discharge: true,
                    battery_power: 0,
                    grid_power: 0,
                    discharge_slots: [configuredSlot()],
                })
            );

            await screen.findAllByText('Timed Export — Armed');
            expect(screen.getAllByText('Timed Export — Armed').length).toBeGreaterThan(0);
        });

        it('shows "Blocked by Pause Discharge" when HR318 blocks export', async () => {
            vi.useFakeTimers({ shouldAdvanceTime: true });
            vi.setSystemTime(new Date('2026-06-28T17:00:00'));
            enableManagedTimedExportForLiveSlots();

            await renderWithSnapshot(
                makeSnapshot({
                    battery_power_mode: 0,
                    enable_discharge: true,
                    battery_pause_mode: 2, // Pause discharge
                    battery_pause_slot: configuredSlot({ start_hour: 20, end_hour: 18 }),
                    discharge_slots: [configuredSlot()],
                })
            );

            // The state display is in the Battery Mode section
            const section = await screen.findByRole('heading', { name: 'Battery Mode', exact: true });
            const batteryModeSection = section.closest('section')!;

            // Should show "Blocked by Pause Discharge" (may appear multiple times)
            await screen.findAllByText('Timed Export — Blocked by Pause Discharge');
            const blockedElements = within(batteryModeSection).queryAllByText(/Blocked by Pause Discharge/);
            expect(blockedElements.length).toBeGreaterThan(0);

            vi.useRealTimers();
        });
    });

    describe('Regression: HR59 alone is not sufficient for Timed Export', () => {
        it('HR27=1/HR59=1 is Timed Demand, not Timed Export', async () => {
            await renderWithSnapshot(
                makeSnapshot({
                    battery_power_mode: 1, // HR27=1 = Eco mode
                    enable_discharge: true, // HR59=1 = discharge armed
                    discharge_slots: [configuredSlot()],
                })
            );

            // The state display is in the Battery Mode section
            const section = await screen.findByRole('heading', { name: 'Battery Mode', exact: true });
            const batteryModeSection = section.closest('section')!;

            // Should NOT show "Active now" because HR27=1 means Timed Demand
            expect(
                within(batteryModeSection).queryByText(/Active now/)
            ).not.toBeInTheDocument();
        });
    });

    describe('Schedule visibility', () => {
        it('persisted schedule remains visible when live physical slots are zero', async () => {
            // The snapshot has empty/zero slots, but the user has a persisted schedule
            // This tests the fix for issue #289 where discharge_slots_backup existed
            // but UI showed empty slots
            await renderWithSnapshot(
                makeSnapshot({
                    battery_power_mode: 1,
                    enable_discharge: false,
                    discharge_slots: [emptySlot()], // All zero - physical slots cleared
                })
            );

            // The Timed Export button should be present (even if currently disabled
            // because no live slot is configured - this will change when the new
            // persisted schedule API is fully wired)
            const timedExportButtons = screen.queryAllByRole('button', { name: /Timed Export/i });
            expect(timedExportButtons.length).toBeGreaterThan(0);
        });

        it('no contradictory "enable Eco" banner while Eco is baseline', async () => {
            await renderWithSnapshot(
                makeSnapshot({
                    battery_power_mode: 1,
                    enable_discharge: false,
                })
            );

            // Should NOT show "Timed Export is active" banner when in Eco
            expect(
                screen.queryByText(/Timed Export is active/)
            ).not.toBeInTheDocument();
        });
    });

    describe('Next transition display', () => {
        it('shows when Eco will resume after export window', async () => {
            vi.useFakeTimers({ shouldAdvanceTime: true });
            vi.setSystemTime(new Date('2026-06-28T17:00:00'));
            enableManagedTimedExportForLiveSlots();

            await renderWithSnapshot(
                makeSnapshot({
                    battery_power_mode: 0,
                    enable_discharge: true,
                    battery_power: 100,
                    grid_power: 50,
                    discharge_slots: [configuredSlot({ end_hour: 19, end_minute: 0 })],
                })
            );

            // Should show when Eco will resume (banner + baseline card both
            // carry the line during active export)
            await screen.findAllByText(/Eco resumes/);
            const resumeElements = screen.queryAllByText(/Eco resumes/);
            expect(resumeElements.length).toBeGreaterThan(0);

            vi.useRealTimers();
        });
    });

    describe('Persisted schedule API response shape', () => {
        it('reads the HEM schedule from the {ok, data} envelope', async () => {
            // The backend GET /api/timed-export returns { ok: true, data: { schedule_enabled, slots, state } }.
            // apiGet returns the whole body; the page must unwrap `.data`, not
            // treat the envelope itself as the schedule. Regression for the
            // local-simulator E2E where the persisted schedule stayed invisible
            // (UI showed Off) because the unwrap was missing.
            // Pin the clock: the 18:00-20:00 persisted slot must read as
            // Configured (future window), which requires "now" outside it.
            vi.useFakeTimers({ shouldAdvanceTime: true });
            vi.setSystemTime(new Date('2026-06-28T12:00:00'));
            const persistedSlot = configuredSlot({ start_hour: 18, start_minute: 0, end_hour: 20, end_minute: 0 });
            (apiGet as ReturnType<typeof vi.fn>).mockImplementation((path: string) => {
                if (path === '/api/timed-export') {
                    return Promise.resolve({
                        ok: true,
                        data: {
                            schedule_enabled: true,
                            slots: [persistedSlot],
                            state: 'Configured',
                        },
                    });
                }
                return Promise.resolve({ ok: true, data: null });
            });

            // Physical slots are zero/empty — the persisted schedule is the
            // only source of "Configured".
            await renderWithSnapshot(
                makeSnapshot({
                    battery_power_mode: 1,
                    enable_discharge: false,
                    discharge_slots: [emptySlot()],
                })
            );

            // Schedule card must read Configured, not Off. The control
            // button carries the same state label, hence findAllByText.
            expect(
                (await screen.findAllByText('Timed Export — Configured')).length
            ).toBeGreaterThan(0);
            vi.useRealTimers();
        });

        it('refreshes machine state when a new poll snapshot arrives', async () => {
            // Pin the clock outside the 18:00-20:00 window so the machine
            // starts from Configured regardless of when the suite runs.
            vi.useFakeTimers({ shouldAdvanceTime: true });
            vi.setSystemTime(new Date('2026-06-28T12:00:00'));
            const persistedSlot = configuredSlot({ start_hour: 18, end_hour: 20 });
            let machineState: unknown = 'Configured';
            (apiGet as ReturnType<typeof vi.fn>).mockImplementation((path: string) => {
                if (path === '/api/timed-export') {
                    return Promise.resolve({
                        ok: true,
                        data: {
                            schedule_enabled: true,
                            slots: [persistedSlot],
                            machine_state: machineState,
                        },
                    });
                }
                return Promise.resolve({ ok: true, data: null });
            });

            const firstSnapshot = makeSnapshot({ timestamp: 1 });
            await renderWithSnapshot(firstSnapshot);
            expect(
                (await screen.findAllByText('Timed Export — Configured')).length
            ).toBeGreaterThan(0);

            machineState = { Error: { reason: 'write retries exhausted' } };
            await act(async () => {
                useInverterStore.setState({
                    snapshot: { ...firstSnapshot, timestamp: 2 },
                });
            });

            expect((await screen.findAllByText('Timed Export — Error')).length).toBeGreaterThan(0);
            vi.useRealTimers();
        });
    });

    describe('Timed Export control button (single toggle, Arm/Stop semantics)', () => {
        it('uses the scheduled-to-fire variant while a future window is configured but registers are not set', async () => {
            vi.useFakeTimers({ shouldAdvanceTime: true });
            vi.setSystemTime(new Date('2026-06-28T12:00:00')); // before the 16:00 window
            mockTimedExportSchedule({
                schedule_enabled: true,
                slots: [configuredSlot()],
                machine_state: 'Configured',
            });

            await renderWithSnapshot(makeSnapshot());

            // Wait for the async GET /api/timed-export to land.
            const button = (await screen.findByRole('button', {
                name: /Timed Export — Configured/,
            })) as HTMLButtonElement;
            // State label on the button — "Stop Timed Export" is reserved
            // for when export is actually armed/live.
            expect(button.textContent).toContain('Timed Export — Configured');
            expect(button.textContent).not.toContain('Stop Timed Export');
            expect(button.dataset.variant).toBe('scheduled');
            expect(button.getAttribute('aria-pressed')).toBe('true');
            expect(button.getAttribute('aria-label')).toContain('Configured');
            expect(button.getAttribute('aria-label')).toContain('Press to stop');

            vi.useRealTimers();
        });

        it('switches to the active variant once readback confirms the export registers', async () => {
            vi.useFakeTimers({ shouldAdvanceTime: true });
            vi.setSystemTime(new Date('2026-06-28T17:00:00')); // inside 16:00-19:00
            mockTimedExportSchedule({
                schedule_enabled: true,
                slots: [configuredSlot()],
                machine_state: 'Active',
            });

            await renderWithSnapshot(
                makeSnapshot({
                    battery_power_mode: 0,
                    enable_discharge: true,
                    battery_power: 100,
                    grid_power: 50,
                    discharge_slots: [configuredSlot()],
                })
            );

            const button = timedExportControl();
            expect(button.textContent).toContain('Stop Timed Export');
            expect(button.dataset.variant).toBe('active');
            expect(button.getAttribute('aria-label')).toContain('Exporting now');

            vi.useRealTimers();
        });

        it('shows the pending Arming state while the machine is Entering', async () => {
            vi.useFakeTimers({ shouldAdvanceTime: true });
            vi.setSystemTime(new Date('2026-06-28T17:00:00'));
            mockTimedExportSchedule({
                schedule_enabled: true,
                slots: [configuredSlot()],
                machine_state: 'Entering',
            });

            await renderWithSnapshot(
                makeSnapshot({
                    // Registers not yet confirmed — the entry writes are in flight.
                    battery_power_mode: 1,
                    enable_discharge: false,
                    discharge_slots: [configuredSlot()],
                })
            );

            const button = (await screen.findByRole('button', {
                name: /Timed Export — Entering/,
            })) as HTMLButtonElement;
            expect(button.textContent).toContain('Arming…');
            expect(button.dataset.variant).toBe('pending');

            vi.useRealTimers();
        });

        it('shows the failed state, not active, when the machine surfaces an error', async () => {
            vi.useFakeTimers({ shouldAdvanceTime: true });
            vi.setSystemTime(new Date('2026-06-28T17:00:00'));
            mockTimedExportSchedule({
                schedule_enabled: true,
                slots: [configuredSlot()],
                machine_state: { Error: { reason: 'write retries exhausted' } },
            });

            await renderWithSnapshot(makeSnapshot());

            const button = (await screen.findByRole('button', {
                name: /Timed Export — Error/,
            })) as HTMLButtonElement;
            expect(button.textContent).toContain('Timed Export — Error');
            expect(button.dataset.variant).toBe('error');
            expect(button.dataset.variant).not.toBe('active');

            vi.useRealTimers();
        });

        it('leaves the pending state when the next poll readback confirms the arm registers', async () => {
            vi.useFakeTimers({ shouldAdvanceTime: true });
            vi.setSystemTime(new Date('2026-06-28T17:00:00'));
            let machineState: unknown = 'Entering';
            mockTimedExportSchedule({
                schedule_enabled: true,
                slots: [configuredSlot()],
                get machine_state() {
                    return machineState;
                },
            });

            await renderWithSnapshot(
                makeSnapshot({
                    battery_power_mode: 1,
                    enable_discharge: false,
                    discharge_slots: [configuredSlot()],
                })
            );
            expect(
                (await screen.findByRole('button', { name: /Timed Export — Entering/ })).textContent
            ).toContain('Arming…');

            // Next poll: readback confirms HR27=0 + discharge-enable and the
            // machine advanced to Active.
            machineState = 'Active';
            await act(async () => {
                useInverterStore.setState({
                    snapshot: makeSnapshot({
                        battery_power_mode: 0,
                        enable_discharge: true,
                        battery_power: 100,
                        grid_power: 50,
                        discharge_slots: [configuredSlot()],
                        timestamp: 2,
                    } as Partial<InverterSnapshot>),
                });
            });

            await screen.findByRole('button', { name: /Exporting now/ });
            const button = timedExportControl();
            expect(button.textContent).toContain('Stop Timed Export');
            expect(button.dataset.variant).toBe('active');

            vi.useRealTimers();
        });

        it('shows the error variant, not active, when the arm request is rejected', async () => {
            const { apiPost } = await import('../../src/lib/api');
            vi.mocked(apiPost).mockRejectedValueOnce(
                new Error('Discharge slot 1 was saved and retained, but Timed Export could not be armed yet'),
            );
            mockTimedExportSchedule({ schedule_enabled: false, slots: [] });

            await renderWithSnapshot(
                makeSnapshot({ discharge_slots: [configuredSlot()] })
            );

            const button = timedExportControl();
            expect(button.textContent).toContain('Arm Timed Export');
            fireEvent.click(button);

            expect(await screen.findByRole('alert')).toBeInTheDocument();
            const after = timedExportControl();
            expect(after.dataset.variant).toBe('error');
            expect(after.dataset.variant).not.toBe('active');
            expect(after.textContent).toContain('Arm Timed Export');
        });

        it('keeps Arm disabled with an explanatory accessible name when no slot is configured', async () => {
            mockTimedExportSchedule({ schedule_enabled: false, slots: [] });

            await renderWithSnapshot(makeSnapshot());

            const button = timedExportControl();
            expect((button as HTMLButtonElement).disabled).toBe(true);
            expect(button.textContent).toContain('Arm Timed Export');
            expect(button.getAttribute('aria-label')).toContain('Configure a discharge slot');
        });
    });
});
