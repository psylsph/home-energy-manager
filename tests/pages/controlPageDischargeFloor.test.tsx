import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, within, act } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Discharge Schedule Minimum SOC section (developer mode only). Its load
// failures and "Save waits for the load" behaviour are covered in
// controlPageSaveBeforeLoad.test.tsx. This file covers the developer-mode
// gating in both directions and the save feedback, including the failure path.
// (Plan 2026-10-17-002, gaps 2 and 3.)
// ---------------------------------------------------------------------------

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn((path: string) =>
    Promise.resolve(
      path === '/api/discharge-floor'
        ? { ok: true, data: { config: { enabled: false, floor_soc: 40 } } }
        : { ok: true, data: {} },
    ),
  ),
  apiPost: vi.fn(),
  getApiBase: () => 'http://localhost:7337',
  getServerPort: () => 7337,
  fetchHistory: vi.fn().mockResolvedValue({}),
  isTauri: false,
}));

import ControlPage from '../../src/pages/ControlPage';
import { useInverterStore } from '../../src/store/useInverterStore';
import { apiPost } from '../../src/lib/api';
import { makeSnapshot } from '../fixtures/snapshot';

const HEADING = 'Discharge Schedule Minimum SOC';
const SAVE = /^(Save|Saving\.\.\.|\.\.\.|✓ Saved|✗ Error)$/;

beforeEach(() => {
  vi.spyOn(console, 'error').mockImplementation(() => {});
  vi.spyOn(console, 'warn').mockImplementation(() => {});
  vi.stubGlobal(
    'matchMedia',
    vi.fn().mockImplementation((query: string) => ({
      matches: false, media: query, onchange: null, addListener: vi.fn(), removeListener: vi.fn(),
      addEventListener: vi.fn(), removeEventListener: vi.fn(), dispatchEvent: vi.fn(),
    })),
  );
  vi.spyOn(globalThis, 'fetch').mockResolvedValue({ ok: true, json: async () => ({ results: [] }) } as Response);
  window.localStorage.clear();
  vi.mocked(apiPost).mockReset();
  useInverterStore.setState({
    snapshot: makeSnapshot(),
    developerMode: true,
    connectionState: 'connected',
    connectedHost: '192.168.1.36:8899',
    batteryModePending: null,
    batteryModeError: null,
  });
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  cleanup();
  useInverterStore.setState({ snapshot: null, connectionState: 'disconnected', developerMode: false });
});

const section = () => screen.getByRole('heading', { name: HEADING }).parentElement as HTMLElement;
const saveButton = () => within(section()).getByRole('button', { name: SAVE });

async function renderLoaded() {
  render(<ControlPage />);
  await waitFor(() => expect(saveButton()).toBeEnabled());
}

describe('<ControlPage/> — Discharge Schedule Minimum SOC', () => {
  it('is shown in developer mode', async () => {
    await renderLoaded();
    expect(screen.getByRole('heading', { name: HEADING })).toBeInTheDocument();
    expect(screen.getByLabelText('Hold Minimum SOC during schedule')).toBeInTheDocument();
  });

  it('is hidden outside developer mode', async () => {
    useInverterStore.setState({ developerMode: false });
    render(<ControlPage />);
    await screen.findByText('Quick Actions');
    expect(screen.queryByRole('heading', { name: HEADING })).toBeNull();
  });

  it('shows the loaded values in the form', async () => {
    await renderLoaded();
    expect((screen.getByLabelText('Hold Minimum SOC during schedule') as HTMLInputElement).checked).toBe(false);
    expect((screen.getByLabelText(/Floor:/) as HTMLInputElement).value).toBe('40');
  });

  it('confirms a successful save, then returns the button to Save', async () => {
    vi.mocked(apiPost).mockResolvedValue({ ok: true });
    await renderLoaded();
    fireEvent.click(screen.getByLabelText('Hold Minimum SOC during schedule'));
    vi.useFakeTimers();

    fireEvent.click(saveButton());
    await act(async () => {});

    expect(apiPost).toHaveBeenCalledWith('/api/discharge-floor', { enabled: true, floor_soc: 40 });
    expect(saveButton().textContent).toBe('✓ Saved');
    act(() => {
      vi.advanceTimersByTime(2000);
    });
    expect(saveButton().textContent).toBe('Save');
  });

  it('reports a failed save, keeps the edits, then returns the button to Save', async () => {
    vi.mocked(apiPost).mockRejectedValue(new Error('inverter busy'));
    await renderLoaded();
    fireEvent.click(screen.getByLabelText('Hold Minimum SOC during schedule'));
    fireEvent.change(screen.getByLabelText(/Floor:/), { target: { value: '65' } });
    vi.useFakeTimers();

    fireEvent.click(saveButton());
    await act(async () => {});

    expect(saveButton().textContent).toBe('✗ Error');
    expect(saveButton()).toBeEnabled();
    // The user's edits are still on screen so they can simply try again.
    expect((screen.getByLabelText('Hold Minimum SOC during schedule') as HTMLInputElement).checked).toBe(true);
    expect((screen.getByLabelText(/Floor:/) as HTMLInputElement).value).toBe('65');

    act(() => {
      vi.advanceTimersByTime(2000);
    });
    expect(saveButton().textContent).toBe('Save');
    // And a retry posts the same edits.
    vi.mocked(apiPost).mockResolvedValue({ ok: true });
    fireEvent.click(saveButton());
    await act(async () => {});
    expect(apiPost).toHaveBeenLastCalledWith('/api/discharge-floor', { enabled: true, floor_soc: 65 });
  });
});
