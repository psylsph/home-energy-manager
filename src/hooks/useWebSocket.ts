import { useEffect, useRef, useCallback } from 'react';
import { getWsUrl, apiGet } from '../lib/api';
import { useInverterStore } from '../store/useInverterStore';
import type { InverterSnapshot, ConnectionState } from '../lib/types';

export interface EvcSnapshot {
  charging_state: string;
  connection_status: string;
  active_power: number;
  current_l1: number;
  current_l2: number;
  current_l3: number;
  voltage_l1: number;
  voltage_l2: number;
  voltage_l3: number;
  meter_energy_kwh: number;
  session_energy_kwh: number;
  session_duration_secs: number;
  charge_limit_a: number;
  serial_number: string;
}

export function useWebSocket() {
  const wsRef = useRef<WebSocket | null>(null);
  const reconnectTimeout = useRef<number>(0);
  const connectRef = useRef<() => void>(() => {});
  const { setSnapshot, clearSnapshot, setConnection, setEvcData } = useInverterStore();

  // Fetch initial connection state from REST API (in case WS messages
  // were missed before the page loaded).
  const fetchInitialStatus = useCallback(async () => {
    try {
      const res = await apiGet<{
        ok: boolean;
        connection: ConnectionState;
        host: string;
        connected_since_epoch_ms: number | null;
        connect_failures: number;
      }>('/api/status');
      if (res.ok) {
        setConnection(res.connection, res.host, res.connected_since_epoch_ms ?? undefined);
        // Update connect failures separately (setConnection only resets on connect).
        useInverterStore.setState({ connectFailures: res.connect_failures });
      }
    } catch {
      // Backend not reachable — stay disconnected
    }
  }, [setConnection]);

  // Fetch initial EVC reachability (issue #138). The broadcast channel
  // doesn't replay past `EvcConnected` / `Evc` frames, so a WS client
  // that subscribes after the EVC has already connected (e.g. user
  // opens the page minutes after app launch) would otherwise never see
  // the latch fire. We ask the backend directly for the cached snapshot
  // and seed the store accordingly.
  const fetchInitialEvcStatus = useCallback(async () => {
    try {
      const res = await apiGet<{
        ok: boolean;
        evc_host: string;
        evc_port: number;
        reachable: boolean;
        snapshot: {
          charging_state: string;
          connection_status: string;
          active_power: number;
          current_l1: number;
          current_l2: number;
          current_l3: number;
          voltage_l1: number;
          voltage_l2: number;
          voltage_l3: number;
          meter_energy_kwh: number;
          session_energy_kwh: number;
          session_duration_secs: number;
          charge_limit_a: number;
          serial_number: string;
        } | null;
      }>('/api/evc/status');
      if (!res.ok) return;
      if (res.reachable && res.snapshot) {
        const snap = res.snapshot;
        const charging = snap.charging_state === 'Charging' || snap.active_power > 0;
        // Decoupled from the cable state: a reachable snapshot proves the
        // host is on the network, so the network flag is true regardless of
        // whether a cable is plugged in. The cable state (HR 2) is carried
        // separately so the diagram can render it under the kW value.
        const cableConnected = snap.connection_status === 'Connected';
        // Carry the raw charging_state string through so the diagram can
        // render the EVC's own "Idle" label when state=1 (issue #139).
        setEvcData(snap.active_power, charging, true, snap.charging_state, cableConnected, snap.session_energy_kwh);
      } else if (res.evc_host) {
        // EVC is configured but the backend has never seen a snapshot
        // since startup. Latch `evcEverConnected` based on the live WS
        // path going forward; for now, leave evcConnected=false so the
        // diagram reads "Not Found" (issue #138).
      }
    } catch {
      // Backend not reachable — leave defaults
    }
  }, [setEvcData]);

  // Export the fetch functions so the StatusPage can trigger re-fetches.
  useEffect(() => {
    (window as unknown as Record<string, unknown>).__fetchInitialStatus = fetchInitialStatus;
    (window as unknown as Record<string, unknown>).__fetchInitialEvcStatus = fetchInitialEvcStatus;
  }, [fetchInitialStatus, fetchInitialEvcStatus]);

  const connect = useCallback(() => {
    const url = getWsUrl();
    const ws = new WebSocket(url);
    wsRef.current = ws;

    ws.onopen = () => {
      console.log('WebSocket connected');
    };

    ws.onmessage = (event) => {
      try {
        const data = JSON.parse(event.data);
        if (data.type === 'snapshot') {
          // Destructure to separate the discriminator from the payload
          const snapshot: InverterSnapshot = (() => {
            const { type: _, ...rest } = data;
            void _;
            return rest as InverterSnapshot;
          })();
          setSnapshot(snapshot);
        } else if (data.type === 'connection') {
          setConnection(
            data.state as ConnectionState,
            data.host,
            data.connected_since_epoch_ms ?? undefined,
          );
          // Update connect failures
          if (data.state === 'connected') {
            useInverterStore.setState({ connectFailures: 0 });
          }
          // Clear stale snapshot when connection drops so the UI shows
          // "waiting for data" instead of frozen old values.
          if (data.state !== 'connected') {
            clearSnapshot();
          }
        } else if (data.type === 'evc') {
          const evc = data as EvcSnapshot;
          const charging = evc.charging_state === 'Charging' || evc.active_power > 0;
          // Decoupled: an arriving `evc` frame proves the host is reachable
          // over the network, so the network flag is true regardless of the
          // physical cable. The cable state (HR 2 `connection_status`) is
          // carried separately as `evcCableConnected` so the diagram can show
          // it under the kW value independently of the operational-status
          // word (Charging / Idle / Connected / Disconnected / Not Found).
          const cableConnected = evc.connection_status === 'Connected';
          // Carry the raw charging_state string through so the diagram
          // can render the EVC's own "Idle" label when state=1
          // (issue #139).
          setEvcData(evc.active_power, charging, true, evc.charging_state, cableConnected, evc.session_energy_kwh);
        } else if (data.type === 'evc_connected') {
          // Backend just established the TCP/Modbus connection to the
          // configured EVC host (issue #138). Latch `evcEverConnected`
          // immediately so the UI drops out of the misleading "Not
          // Found" state. Also flip `evcConnected=true` for the brief
          // window between TCP success and the first register read so
          // the label reads "Connected" rather than flickering through
          // "Disconnected". A subsequent `EvcDisconnected` will correct
          // the flag if the first read fails.
          useInverterStore.getState().markEvcConnectedReached();
        } else if (data.type === 'evc_disconnected') {
          setEvcData(0, false, false);
        }
      } catch (e) {
        console.error('WebSocket parse error:', e);
      }
    };

    ws.onclose = (event) => {
      if (event.code === 1000) {
        // Code 1000 = Normal Closure (sent by cleanup `ws.close(1000, ...)`).
        // The component intentionally closed this connection — don't reconnect.
        return;
      }
      console.log('WebSocket closed, reconnecting in 3s...');
      reconnectTimeout.current = window.setTimeout(() => connectRef.current(), 3000);
    };

    ws.onerror = (err) => {
      console.error('WebSocket error:', err);
      ws.close();
    };
  }, [setSnapshot, clearSnapshot, setConnection, setEvcData]);

  // Keep ref in sync so the reconnect closure always calls the latest connect
  useEffect(() => {
    connectRef.current = connect;
  }, [connect]);

  useEffect(() => {
    fetchInitialStatus();
    fetchInitialEvcStatus();
    connect();
    return () => {
      clearTimeout(reconnectTimeout.current);
      wsRef.current?.close(1000, 'Unmount');
    };
  }, [connect, fetchInitialStatus, fetchInitialEvcStatus]);
}
