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
  /** Whether the trend charts on the Battery/Solar tabs are shown. */
  panelGraphsEnabled: boolean;
  /** Time scale for the trend charts on the Battery/Solar tabs. */
  panelGraphsScale: 'today' | '24h';
  /** Lock chart Y-axis to inverter's rated power. */
  panelGraphsYLock: boolean;
  /** Highest Y-axis ceiling seen this session when lock is enabled (0 = unset). */
  panelGraphsYLockMax: number;
  /** Discharge slots configured locally in Eco mode, not yet written to the inverter. */
  pendingDischargeSlots: Record<number, ScheduleSlot>;
  /** EV Charger host — non-empty when configured in Settings. */
  evcHost: string;
  /** EV Charger active power (watts), updated by EVC poll loop. */
  evcPower: number;
  /** EV Charger charging state (true = actively delivering power). */
  evcCharging: boolean;
  /** EV Charger Modbus connection/data status. */
  evcConnected: boolean;
  setSnapshot: (snapshot: InverterSnapshot) => void;
  clearSnapshot: () => void;
  setConnection: (state: ConnectionState, host?: string) => void;
  setDeveloperMode: (enabled: boolean) => void;
  setThemeMode: (mode: ThemeMode) => void;

  setChartRange: (range: HistoryRange) => void;
  setPanelGraphsEnabled: (enabled: boolean) => void;
  setPanelGraphsScale: (scale: 'today' | '24h') => void;
  setPanelGraphsYLock: (enabled: boolean) => void;
  setPanelGraphsYLockMax: (max: number) => void;
  setPendingDischargeSlots: (slots: Record<number, ScheduleSlot>) => void;
  clearPendingDischargeSlots: () => void;
  setHiddenPanels: (panels: string[]) => void;
  setEvcHost: (host: string) => void;
  setEvcData: (power: number, charging: boolean, connected?: boolean) => void;
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

function loadPanelGraphsEnabled(): boolean {
  try {
    const stored = localStorage.getItem('panelGraphsEnabled');
    // Default to showing graphs when the key is absent.
    return stored === null ? true : stored === 'true';
  } catch {
    return true;
  }
}

function loadPanelGraphsScale(): 'today' | '24h' {
  try {
    return localStorage.getItem('panelGraphsScale') === '24h' ? '24h' : 'today';
  } catch {
    return 'today';
  }
}

function loadPanelGraphsYLock(): boolean {
  try {
    const stored = localStorage.getItem('panelGraphsYLock');
    // Default to locked (true) when the key is absent.
    return stored === null ? true : stored === 'true';
  } catch {
    return true;
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
  panelGraphsEnabled: loadPanelGraphsEnabled(),
  panelGraphsScale: loadPanelGraphsScale(),
  panelGraphsYLock: loadPanelGraphsYLock(),
  panelGraphsYLockMax: 0,
  pendingDischargeSlots: loadPendingDischargeSlots(),
  evcHost: '',
  evcPower: 0,
  evcCharging: false,
  evcConnected: false,
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
  setPanelGraphsEnabled: (enabled) => {
    try {
      localStorage.setItem('panelGraphsEnabled', String(enabled));
    } catch { /* ignore */ }
    set({ panelGraphsEnabled: enabled });
  },
  setPanelGraphsScale: (scale) => {
    try {
      localStorage.setItem('panelGraphsScale', scale);
    } catch { /* ignore */ }
    set({ panelGraphsScale: scale });
  },
  setPanelGraphsYLock: (enabled) => {
    try {
      localStorage.setItem('panelGraphsYLock', String(enabled));
    } catch { /* ignore */ }
    set({ panelGraphsYLock: enabled, panelGraphsYLockMax: 0 });
  },
  setPanelGraphsYLockMax: (max) => set({ panelGraphsYLockMax: max }),
  setPendingDischargeSlots: (slots) => {
    savePendingDischargeSlots(slots);
    set({ pendingDischargeSlots: slots });
  },
  clearPendingDischargeSlots: () => {
    savePendingDischargeSlots({});
    set({ pendingDischargeSlots: {} });
  },
  setHiddenPanels: (panels) => set({ hiddenPanels: panels }),
  setEvcHost: (host) => set({ evcHost: host }),
  setEvcData: (power, charging, connected = true) => set({ evcPower: power, evcCharging: charging, evcConnected: connected }),
}));
