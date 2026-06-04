import { create } from 'zustand';
import type { InverterSnapshot, ConnectionState } from '../lib/types';

type ThemeMode = 'dark' | 'light';

interface InverterState {
  snapshot: InverterSnapshot | null;
  connectionState: ConnectionState;
  connectedHost: string | null;
  developerMode: boolean;
  themeMode: ThemeMode;
  setSnapshot: (snapshot: InverterSnapshot) => void;
  clearSnapshot: () => void;
  setConnection: (state: ConnectionState, host?: string) => void;
  setDeveloperMode: (enabled: boolean) => void;
  setThemeMode: (mode: ThemeMode) => void;
}

function loadDeveloperMode(): boolean {
  try {
    return localStorage.getItem('devMode') === 'true';
  } catch {
    return false;
  }
}

function loadThemeMode(): ThemeMode {
  try {
    const stored = localStorage.getItem('themeMode');
    return stored === 'light' ? 'light' : 'dark';
  } catch {
    return 'dark';
  }
}

export const useInverterStore = create<InverterState>((set) => ({
  snapshot: null,
  connectionState: 'disconnected',
  connectedHost: null,
  developerMode: loadDeveloperMode(),
  themeMode: loadThemeMode(),
  setSnapshot: (snapshot) => set({ snapshot }),
  clearSnapshot: () => set({ snapshot: null }),
  setConnection: (state, host) => set({ connectionState: state, connectedHost: host ?? null }),
  setDeveloperMode: (enabled) => {
    try {
      localStorage.setItem('devMode', String(enabled));
    } catch { /* ignore */ }
    set({ developerMode: enabled });
  },
  setThemeMode: (mode) => {
    try {
      localStorage.setItem('themeMode', mode);
    } catch { /* ignore */ }
    set({ themeMode: mode });
  },
}));
