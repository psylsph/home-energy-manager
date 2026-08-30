/**
 * The battery-mode action banners (Applying… progress, confirmation timeout
 * error, Timed Export arm failure) must survive navigating away from the
 * Control tab and back. The backend write batch keeps draining paced writes
 * for ~35 s after the click — unmounting the page must not discard the
 * progress/warning state or restart its confirmation watchdog from zero.
 */

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { act, fireEvent, render, screen, cleanup } from '@testing-library/react';
import ControlPage from '../../src/pages/ControlPage';
import { useInverterStore } from '../../src/store/useInverterStore';
import { apiGet, apiPost } from '../../src/lib/api';
import type { InverterSnapshot, ScheduleSlot } from '../../src/lib/types';

vi.mock('../../src/lib/api', () => ({
    apiGet: vi.fn(async (path: string) => {
        if (path === '/api/agile') return { ok: true, enabled: false };
        if (path === '/api/cosy') return { ok: true, enabled: false, slots: [] };
        if (path === '/api/settings') return { ok: true, data: {} };
        if (path === '/api/timed-export') {
            return { ok: true, data: { schedule_enabled: false, slots: [] } };
        }
        return { ok: true, data: null };
    }),
    apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
    getApiBase: () => 'http://localhost:7337',
    getServerPort: () => 7337,
    fetchHistory: vi.fn().mockResolvedValue({}),
    isTauri: false,
}));

function emptySlot(): ScheduleSlot {
    return {
        enabled: false,
        start_hour: 0,
        start_minute: 0,
        end_hour: 0,
        end_minute: 0,
        target_soc: 100,
    };
}

function makeSnapshot(overrides: Partial<InverterSnapshot> = {}): InverterSnapshot {
    return {
        device_type_code: '2001',
        battery_power_mode: 1,
        enable_charge: false,
        enable_discharge: false,
        battery_pause_mode: 0,
        discharge_slots: [emptySlot(), emptySlot()],
        soc: 50,
        battery_state: 'idle',
        inverter_serial: 'TEST123',
        ...overrides,
    } as InverterSnapshot;
}

describe('ControlPage battery-mode banners survive tab navigation', () => {
    beforeEach(() => {
        vi.spyOn(console, 'error').mockImplementation(() => {});
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
            batteryModePending: null,
            batteryModePendingSince: null,
            batteryModeError: null,
            timedExportArmFailed: false,
        });
    });

    it('keeps the Applying banner across unmount/remount while writes drain', async () => {
        // The POST never resolves: the write batch is still being paced out
        // by the poll loop when the user switches tabs.
        vi.mocked(apiPost).mockImplementationOnce(() => new Promise(() => {}));

        async function renderControl() {
            useInverterStore.setState({
                snapshot: makeSnapshot(),
                connectionState: 'connected',
            });
            const view = render(<ControlPage />);
            // Wait for the async page-load fetches to settle. The Battery
            // Mode heading is the stable "page ready" signal — while the
            // action is in flight the mode buttons read "Applying…", so the
            // button name is not a reliable anchor on remount.
            await screen.findByRole('heading', { name: 'Battery Mode', exact: true });
            return view;
        }

        const first = await renderControl();
        fireEvent.click(screen.getByRole('button', { name: /Timed Charge/ }));
        expect(await screen.findByText('Applying changes to inverter…')).toBeInTheDocument();

        // Navigate away (unmount) and back (fresh ControlPage, same store).
        first.unmount();
        await renderControl();

        expect(screen.getByText('Applying changes to inverter…')).toBeInTheDocument();
        expect(screen.getByText('Applying…')).toBeInTheDocument();
    });

    it('still confirms from a snapshot that arrives while the page is remounted', async () => {
        vi.mocked(apiPost).mockImplementationOnce(() => new Promise(() => {}));

        async function renderControl() {
            useInverterStore.setState({
                snapshot: makeSnapshot(),
                connectionState: 'connected',
            });
            const view = render(<ControlPage />);
            await screen.findByRole('heading', { name: 'Battery Mode', exact: true });
            return view;
        }

        await renderControl();
        fireEvent.click(screen.getByRole('button', { name: /Timed Charge/ }));
        expect(await screen.findByText('Applying changes to inverter…')).toBeInTheDocument();

        cleanup();

        await renderControl();
        // The banner survived the navigation — the action is still in flight.
        expect(screen.getByText('Applying changes to inverter…')).toBeInTheDocument();

        // The poll loop confirms HR96 while the remounted page is visible.
        act(() => {
            useInverterStore.setState({
                snapshot: makeSnapshot({ enable_charge: true }),
            });
        });

        expect(await screen.findByText('Timed Charge')).toBeInTheDocument();
        expect(screen.queryByText('Applying changes to inverter…')).not.toBeInTheDocument();
    });

    it('fires the confirmation-timeout error based on elapsed time, not time on page', async () => {
        vi.useFakeTimers({ shouldAdvanceTime: true });
        vi.mocked(apiPost).mockImplementation(() => new Promise(() => {}));

        async function renderControl() {
            useInverterStore.setState({
                snapshot: makeSnapshot(),
                connectionState: 'connected',
            });
            const view = render(<ControlPage />);
            await screen.findByRole('heading', { name: 'Battery Mode', exact: true });
            return view;
        }

        await renderControl();
        fireEvent.click(screen.getByRole('button', { name: /Timed Charge/ }));
        expect(await screen.findByText('Applying changes to inverter…')).toBeInTheDocument();

        // 60 s in: navigate away, advance another 40 s (past the 90 s
        // confirmation deadline), then come back. The watchdog must account
        // for the time spent away rather than restarting from zero.
        act(() => { vi.advanceTimersByTime(60_000); });
        cleanup();
        act(() => { vi.advanceTimersByTime(40_000); });
        await renderControl();

        expect(
            await screen.findByText('Timed Charge did not confirm the change. Please try again.'),
        ).toBeInTheDocument();
        expect(screen.queryByText('Applying changes to inverter…')).not.toBeInTheDocument();
    });

    it('keeps the Timed Export arm-failure state across navigation', async () => {
        vi.mocked(apiPost).mockRejectedValueOnce(
            new Error('Discharge slot 1 was saved and retained, but Timed Export could not be armed yet'),
        );

        async function renderControl() {
            useInverterStore.setState({
                snapshot: makeSnapshot({ discharge_slots: [{ ...emptySlot(), enabled: true, start_hour: 16, end_hour: 19 }] }),
                connectionState: 'connected',
            });
            const view = render(<ControlPage />);
            await screen.findByRole('button', { name: /Timed Export/ });
            return view;
        }

        await renderControl();
        fireEvent.click(screen.getByRole('button', { name: /Timed Export/ }));
        expect(await screen.findByRole('alert')).toBeInTheDocument();

        cleanup();
        await renderControl();

        const alert = await screen.findByRole('alert');
        expect(alert.textContent).toContain('could not be armed yet');
        const exportButton = screen.getByRole('button', { name: /Timed Export/ }) as HTMLButtonElement;
        expect(exportButton.dataset.variant).toBe('error');
        void apiGet;
    });
});
