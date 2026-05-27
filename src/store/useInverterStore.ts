import { create } from 'zustand';
import type { InverterSnapshot, ConnectionState } from '../lib/types';

interface InverterState {
  snapshot: InverterSnapshot | null;
  connectionState: ConnectionState;
  connectedHost: string | null;
  setSnapshot: (snapshot: InverterSnapshot) => void;
  setConnection: (state: ConnectionState, host?: string) => void;
}

export const useInverterStore = create<InverterState>((set) => ({
  snapshot: null,
  connectionState: 'disconnected',
  connectedHost: null,
  setSnapshot: (snapshot) => set({ snapshot }),
  setConnection: (state, host) => set({ connectionState: state, connectedHost: host ?? null }),
}));
