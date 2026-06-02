import { useEffect, useRef, useCallback } from 'react';
import { getWsUrl, apiGet } from '../lib/api';
import { useInverterStore } from '../store/useInverterStore';
import type { InverterSnapshot, ConnectionState } from '../lib/types';

export function useWebSocket() {
  const wsRef = useRef<WebSocket | null>(null);
  const reconnectTimeout = useRef<number>(0);
  const connectRef = useRef<() => void>(() => {});
  const { setSnapshot, clearSnapshot, setConnection } = useInverterStore();

  // Fetch initial connection state from REST API (in case WS messages
  // were missed before the page loaded).
  const fetchInitialStatus = useCallback(async () => {
    try {
      const res = await apiGet<{ ok: boolean; connection: ConnectionState; host: string }>('/api/status');
      if (res.ok) {
        setConnection(res.connection, res.host);
      }
    } catch {
      // Backend not reachable — stay disconnected
    }
  }, [setConnection]);

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
          setConnection(data.state as ConnectionState, data.host);
          // Clear stale snapshot when connection drops so the UI shows
          // "waiting for data" instead of frozen old values.
          if (data.state !== 'connected') {
            clearSnapshot();
          }
        }
      } catch (e) {
        console.error('WebSocket parse error:', e);
      }
    };

    ws.onclose = () => {
      console.log('WebSocket closed, reconnecting in 3s...');
      reconnectTimeout.current = window.setTimeout(() => connectRef.current(), 3000);
    };

    ws.onerror = (err) => {
      console.error('WebSocket error:', err);
      ws.close();
    };
  }, [setSnapshot, clearSnapshot, setConnection]);

  // Keep ref in sync so the reconnect closure always calls the latest connect
  useEffect(() => {
    connectRef.current = connect;
  }, [connect]);

  useEffect(() => {
    fetchInitialStatus();
    connect();
    return () => {
      clearTimeout(reconnectTimeout.current);
      wsRef.current?.close();
    };
  }, [connect, fetchInitialStatus]);
}
