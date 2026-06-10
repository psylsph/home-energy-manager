import { create } from 'zustand';
import type { InverterSnapshot, ConnectionState, HistoryRange, ScheduleSlot } from '../lib/types';

type ThemeMode = 'dark' | 'light';

interface InverterState {
  snapshot: InverterSnapshot | null;
  connectionState: ConnectionState;
  connectedHost: string | null;
  developerMode: boolean;
  themeMode: ThemeMode;
  /** Panels hidden from the bottom navigation. */
  hiddenPanels: string[];
  /** Shared time range used by Power and History charts. */
  chartRange: HistoryRange;
  /** Discharge slots configured locally in Eco mode, not yet written to the inverter. */
  pendingDischargeSlots: Record<number, ScheduleSlot>;
  setSnapshot: (snapshot: InverterSnapshot) => void;
  clearSnapshot: () => void;
  setConnection: (state: ConnectionState, host?: string) => void;
  setDeveloperMode: (enabled: boolean) => void;
  setThemeMode: (mode: ThemeMode) => void;
  setChartRange: (range: HistoryRange) => void;
  setPendingDischargeSlots: (slots: Record<number, ScheduleSlot>) => void;
  clearPendingDischargeSlots: () => void;
  setHiddenPanels: (panels: string[]) => void;
}

function loadDeveloperMode(): boolean {
  try {
    return localStorage.getItem('devMode') === 'true';
  } catch {
    return false;
  }
}

function loadChartRange(): HistoryRange {
  try {
    const stored = localStorage.getItem('chartRange');
    switch (stored) {
      case '1h':
      case '6h':
      case '12h':
      case '24h':
      case 'today':
      case '7d':
      case '30d':
      case 'month':
      case '6m':
      case '1y':
        return stored;
      default:
        return '24h';
    }
  } catch {
    return '24h';
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

function loadPendingDischargeSlots(): Record<number, ScheduleSlot> {
  try {
    const stored = localStorage.getItem('pendingDischargeSlots');
    if (stored) return JSON.parse(stored);
  } catch { /* ignore */ }
  return {};
}

function savePendingDischargeSlots(slots: Record<number, ScheduleSlot>) {
  try {
    localStorage.setItem('pendingDischargeSlots', JSON.stringify(slots));
  } catch { /* ignore */ }
}

export const useInverterStore = create<InverterState>((set) => ({
  snapshot: null,
  connectionState: 'disconnected',
  connectedHost: null,
  developerMode: loadDeveloperMode(),
  themeMode: loadThemeMode(),
  hiddenPanels: [],
  chartRange: loadChartRange(),
  pendingDischargeSlots: loadPendingDischargeSlots(),
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
  setChartRange: (range) => {
    try {
      localStorage.setItem('chartRange', range);
    } catch { /* ignore */ }
    set({ chartRange: range });
  },
  setPendingDischargeSlots: (slots) => {
    savePendingDischargeSlots(slots);
    set({ pendingDischargeSlots: slots });
  },
  clearPendingDischargeSlots: () => {
    savePendingDischargeSlots({});
    set({ pendingDischargeSlots: {} });
  },
  setHiddenPanels: (panels) => set({ hiddenPanels: panels }),
}));
