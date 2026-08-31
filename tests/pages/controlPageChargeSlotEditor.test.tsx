/**
 * Tests for ScheduleSlotEditor prop-sync inside ControlPage's Charge
 * Schedule section.
 *
 * The editor keeps a local working copy of the slot, initialised from the
 * snapshot at mount time. When a later poll delivers a *decoded* slot
 * (e.g. the seeded registers become an enabled 06:00-10:00 window), the
 * editor must re-sync its local copy — otherwise the card renders the
 * mount-time defaults forever (disabled, no Target SOC slider) and the
 * user has to reload the page to see the inverter's schedule.
 */

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { render, screen, within, cleanup } from '@testing-library/react';
import ControlPage from '../../src/pages/ControlPage';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot, ScheduleSlot } from '../../src/lib/types';

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

function makeSnapshot(overrides: Partial<InverterSnapshot> = {}): InverterSnapshot {
    return {
        device_type_code: '2001',
        firmware_arm_version: '318',
        battery_power_mode: 1,
        enable_charge: true,
        enable_discharge: false,
        battery_pause_mode: 0,
        charge_slots: [
            emptySlot(),
            emptySlot(),
        ],
        discharge_slots: [
            emptySlot(),
            emptySlot(),
        ],
        max_charge_slots: 2,
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

describe('ControlPage Charge Schedule editor prop sync', () => {
    beforeEach(() => {
        vi.spyOn(console, 'error').mockImplementation(() => { });
        vi.stubGlobal('matchMedia', () => ({
            matches: false,
            addListener: vi.fn(),
            removeListener: vi.fn(),
        }));
    });

    afterEach(() => {
        vi.restoreAllMocks();
        vi.unstubAllGlobals();
        cleanup();
        useInverterStore.setState({
            snapshot: null,
            connectionState: 'disconnected',
            developerMode: false,
            batteryModePending: null,
            batteryModePendingSince: null,
            batteryModeError: null,
            timedExportArmFailed: false,
        });
    });

    it('reveals the Target SOC slider when a poll decodes the seeded slot', async () => {
        // Mount while charge slot 1 is still zeroed (the pre-seed state).
        useInverterStore.setState({
            snapshot: makeSnapshot(),
            connectionState: 'connected',
            developerMode: false,
        });
        render(<ControlPage />);

        const heading = await screen.findByRole('heading', { name: 'Charge Schedule', exact: true });
        const chargeSection = heading.closest('section')!;

        // No Target SOC slider while every slot is disabled.
        expect(within(chargeSection).queryByText('Target SOC')).toBeNull();

        // A later poll decodes the seeded window (enabled slot).
        useInverterStore.setState({
            snapshot: makeSnapshot({
                charge_slots: [
                    emptySlot({ enabled: true, start_hour: 6, start_minute: 0, end_hour: 10, end_minute: 0, target_soc: 100 }),
                    emptySlot(),
                ],
            }),
        });

        // The editor must re-sync from the decoded slot: Target SOC slider
        // visible (local.enabled true), not stuck at the mount-time default.
        const slider = await within(chargeSection).findByRole('slider', {}, { timeout: 5_000 });
        expect(slider).toBeVisible();
        expect(within(chargeSection).getByText('Target SOC')).toBeDefined();
    });
});
