import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { renderHook, cleanup, act } from '@testing-library/react';
import { useInverterStore } from '../../src/store/useInverterStore';

// Mock the api module
vi.mock('../../src/lib/api', () => ({
  getWsUrl: () => 'ws://127.0.0.1:7337/ws',
  apiGet: vi.fn(),
}));

// Mock WebSocket
class MockWebSocket {
  url: string;
  onopen: ((event: any) => void) | null = null;
  onmessage: ((event: any) => void) | null = null;
  onclose: ((event: any) => void) | null = null;
  onerror: ((event: any) => void) | null = null;
  readyState: number = 0;
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;

  constructor(url: string) {
    this.url = url;
    // Store reference for test control
    mockWsInstances.push(this);
    // Simulate async open
    setTimeout(() => {
      this.readyState = 1;
      this.onopen?.({});
    }, 0);
  }

  close(code?: number, reason?: string) {
    this.readyState = 3;
    this.onclose?.({ code: code ?? 1000, reason: reason ?? '' });
  }

  send(data: string) {}
}

const mockWsInstances: MockWebSocket[] = [];
vi.stubGlobal('WebSocket', MockWebSocket);

// Mock setTimeout/clearTimeout for controlled timing
vi.useFakeTimers();

const { useWebSocket } = await import('../../src/hooks/useWebSocket');

function resetStore() {
  useInverterStore.setState({
    snapshot: null,
    connectionState: 'disconnected',
    connectedHost: null,
    connectedSince: null,
    connectFailures: 0,
    evcHost: '',
    evcPower: 0,
    evcChargingState: '',
    evcCharging: false,
    evcConnected: false,
    evcEverConnected: false,
  });
}

describe('useWebSocket', () => {
  beforeEach(() => {
    resetStore();
    mockWsInstances.length = 0;
    vi.useFakeTimers();
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it('connects to WebSocket on mount', () => {
    renderHook(() => useWebSocket());
    expect(mockWsInstances.length).toBe(1);
    expect(mockWsInstances[0].url).toBe('ws://127.0.0.1:7337/ws');
  });

  it('updates snapshot on snapshot message', () => {
    renderHook(() => useWebSocket());

    // Simulate WebSocket message with snapshot data
    const ws = mockWsInstances[0];
    ws.onmessage?.({
      data: JSON.stringify({
        type: 'snapshot',
        soc: 75,
        solar_power: 2000,
        battery_power: 500,
        grid_power: -300,
        home_power: 1200,
      }),
    });

    const state = useInverterStore.getState();
    expect(state.snapshot).not.toBeNull();
    expect(state.snapshot!.soc).toBe(75);
    expect(state.snapshot!.solar_power).toBe(2000);
  });

  it('updates connection state on connection message', () => {
    renderHook(() => useWebSocket());

    const ws = mockWsInstances[0];
    ws.onmessage?.({
      data: JSON.stringify({
        type: 'connection',
        state: 'connected',
        host: '192.168.1.10',
        connected_since_epoch_ms: 1000,
      }),
    });

    const state = useInverterStore.getState();
    expect(state.connectionState).toBe('connected');
    expect(state.connectedHost).toBe('192.168.1.10');
    expect(state.connectFailures).toBe(0);
  });

  it('clears snapshot on non-connected connection message', () => {
    useInverterStore.setState({ snapshot: { soc: 50 } as any });
    renderHook(() => useWebSocket());

    const ws = mockWsInstances[0];
    ws.onmessage?.({
      data: JSON.stringify({
        type: 'connection',
        state: 'disconnected',
      }),
    });

    expect(useInverterStore.getState().snapshot).toBeNull();
  });

  it('updates EVC data on evc message', () => {
    renderHook(() => useWebSocket());

    const ws = mockWsInstances[0];
    ws.onmessage?.({
      data: JSON.stringify({
        type: 'evc',
        charging_state: 'Charging',
        connection_status: 'Connected',
        active_power: 3500,
      }),
    });

    const state = useInverterStore.getState();
    expect(state.evcPower).toBe(3500);
    expect(state.evcCharging).toBe(true);
    expect(state.evcConnected).toBe(true);
    expect(state.evcChargingState).toBe('Charging');
  });

  it('sets evcEverConnected on evc_connected message', () => {
    renderHook(() => useWebSocket());

    const ws = mockWsInstances[0];
    ws.onmessage?.({
      data: JSON.stringify({
        type: 'evc_connected',
      }),
    });

    expect(useInverterStore.getState().evcEverConnected).toBe(true);
    expect(useInverterStore.getState().evcConnected).toBe(true);
  });

  it('clears EVC data on evc_disconnected message', () => {
    useInverterStore.setState({
      evcPower: 3500,
      evcCharging: true,
      evcConnected: true,
    });
    renderHook(() => useWebSocket());

    const ws = mockWsInstances[0];
    ws.onmessage?.({
      data: JSON.stringify({
        type: 'evc_disconnected',
      }),
    });

    const state = useInverterStore.getState();
    expect(state.evcPower).toBe(0);
    expect(state.evcCharging).toBe(false);
    expect(state.evcConnected).toBe(false);
  });

  it('reconnects after close with non-1000 code', () => {
    renderHook(() => useWebSocket());

    const ws = mockWsInstances[0];
    ws.onclose?.({ code: 1001, reason: 'going away' });

    // Should schedule reconnect after 3s
    expect(mockWsInstances.length).toBe(1); // no new WS yet

    // Advance time by 3s to trigger reconnect
    vi.advanceTimersByTime(3000);
    expect(mockWsInstances.length).toBe(2);
  });

  it('does not reconnect after close with code 1000', () => {
    renderHook(() => useWebSocket());

    const ws = mockWsInstances[0];
    ws.onclose?.({ code: 1000, reason: 'Normal closure' });

    // Advance time — no reconnect should happen
    vi.advanceTimersByTime(5000);
    expect(mockWsInstances.length).toBe(1);
  });

  it('handles JSON parse errors gracefully', () => {
    renderHook(() => useWebSocket());

    const ws = mockWsInstances[0];
    // This should not throw
    ws.onmessage?.({ data: 'invalid json' });

    // Store should be unchanged
    expect(useInverterStore.getState().snapshot).toBeNull();
  });
});
