import { useEffect, useRef, useCallback } from 'react';
import { getWsUrl } from '../lib/api';
import { useInverterStore } from '../store/useInverterStore';
import type { InverterSnapshot, ConnectionState } from '../lib/types';

export function useWebSocket() {
  const wsRef = useRef<WebSocket | null>(null);
  const reconnectTimeout = useRef<number>(0);
  const { setSnapshot, setConnection } = useInverterStore();

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
          const { type, ...snapshot } = data;
          setSnapshot(snapshot as InverterSnapshot);
        } else if (data.type === 'connection') {
          setConnection(data.state as ConnectionState, data.host);
        }
      } catch (e) {
        console.error('WebSocket parse error:', e);
      }
    };

    ws.onclose = () => {
      console.log('WebSocket closed, reconnecting in 3s...');
      reconnectTimeout.current = window.setTimeout(connect, 3000);
    };

    ws.onerror = (err) => {
      console.error('WebSocket error:', err);
      ws.close();
    };
  }, [setSnapshot, setConnection]);

  useEffect(() => {
    connect();
    return () => {
      clearTimeout(reconnectTimeout.current);
      wsRef.current?.close();
    };
  }, [connect]);
}
