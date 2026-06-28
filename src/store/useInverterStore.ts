import { create } from 'zustand';
import type { InverterSnapshot, ConnectionState, HistoryRange, ScheduleSlot } from '../lib/types';
import type { GridLineWeight } from '../lib/historyRangeConfig';

type ThemeMode = 'dark' | 'light';

interface InverterState {
  snapshot: InverterSnapshot | null;
  connectionState: ConnectionState;
  connectedHost: string | null;
  developerMode: boolean;
  themeMode: ThemeMode;
  /**
   * Read-only mode hides the Control and Settings tabs from the bottom
   * navigation. Set initially by visiting the app with a `?RO` URL
   * parameter (intended for sharing a household dashboard link with
   * non-admin users — see issue #114). Persisted to localStorage so
   * the flag sticks in the same browser across reloads.
   */
  readOnly: boolean;

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
  /**
   * Grid line weight for the recharts `CartesianGrid` on every live history
   * chart (Power, History, Battery tab, Solar tab). `'standard'` matches the
   * original 2-px dashed look; `'subtle'` drops to a hairline that sits
   * behind the data series (issue #111). Persisted to localStorage so the
   * user's choice survives reloads.
   */
  gridLineWeight: GridLineWeight;
  /** Discharge slots configured locally in Eco mode, not yet written to the inverter. */
  pendingDischargeSlots: Record<number, ScheduleSlot>;
  /** EV Charger host — non-empty when configured in Settings. */
  evcHost: string;
  /** EV Charger active power (watts), updated by EVC poll loop. */
  evcPower: number;
  /**
   * Raw EV Charger charging-state string from HR 0 (`"Unknown"`, `"Idle"`,
   * `"Connected"`, `"Starting"`, `"Charging"`, …). Used to render the
   * "Idle" label on the Status page when the EVC reports state=1 but
   * isn't actively delivering power (issue #139). Empty string when no
   * snapshot has arrived yet.
   */
  evcChargingState: string;
  /** EV Charger charging state (true = actively delivering power). */
  evcCharging: boolean;
  /** EV Charger Modbus connection/data status. */
  evcConnected: boolean;
  /**
   * True once we've received at least one valid EVC snapshot since the
   * page loaded. Lets the UI distinguish "charger was here, now offline"
   * (rendered as 'Disconnected') from "we've never successfully reached the
   * configured host" (rendered as 'Not Found' — issue #138). Resets when
   * the user changes the EVC host in Settings, since the new host is a
   * fresh attempt.
   */
  evcEverConnected: boolean;
  /** Epoch millis when the current connection was established (null when disconnected). */
  connectedSince: number | null;
  /** Consecutive connection failures since last successful connect. */
  connectFailures: number;
  /**
   * Whether to show short status words under orbit nodes (Generating,
   * Importing, Charging, etc.). Default: on — the words carry the
   * direction signal that used to live in a `+`/`-` prefix on the
   * value, so a non-technical user reading "−839W + Discharging"
   * doesn't have to reconcile two contradictory signs. The toggle in
   * Settings remains for users who prefer the bare value.
   */
  showFlowStatusWords: boolean;
  /**
   * Noise floor in watts for the energy flow diagram. Flows below this
   * value are treated as zero — no animated line, no arrow, displayed
   * value rounds to "0W". Default: 20W.
   */
  visualNoiseThreshold: number;
  setSnapshot: (snapshot: InverterSnapshot) => void;
  clearSnapshot: () => void;
  setConnection: (state: ConnectionState, host?: string, connectedSince?: number | null) => void;
  setDeveloperMode: (enabled: boolean) => void;
  setThemeMode: (mode: ThemeMode) => void;
  setReadOnly: (enabled: boolean) => void;

  setChartRange: (range: HistoryRange) => void;
  setPanelGraphsEnabled: (enabled: boolean) => void;
  setPanelGraphsScale: (scale: 'today' | '24h') => void;
  setPanelGraphsYLock: (enabled: boolean) => void;
  setPanelGraphsYLockMax: (max: number) => void;
  setGridLineWeight: (weight: GridLineWeight) => void;
  setPendingDischargeSlots: (slots: Record<number, ScheduleSlot>) => void;
  clearPendingDischargeSlots: () => void;
  setHiddenPanels: (panels: string[]) => void;
  setEvcHost: (host: string) => void;
  setEvcData: (
    power: number,
    charging: boolean,
    connected?: boolean,
    chargingState?: string,
  ) => void;
  /**
   * Mark the EVC as "we just successfully reached the host" without
   * touching power / charging (we don't have register data yet — the
   * next `Evc` frame will set those). Called from the WS handler when
   * the backend broadcasts `EvcConnected` after a successful TCP/Modbus
   * handshake (issue #138). The first register read may still fail, in
   * which case a subsequent `EvcDisconnected` will reset `evcConnected`
   * — the latch stays so the label reads "Disconnected" rather than
   * regressing to "Not Found".
   */
  markEvcConnectedReached: () => void;
  /**
   * Reset the EVC session state (called when the user saves a new host or
   * disables the charger in Settings). Clears the cached snapshot and the
   * "ever connected" flag so the UI goes back to "Not Found" until the new
   * host actually responds.
   */
  resetEvc: () => void;
  setShowFlowStatusWords: (enabled: boolean) => void;
  setVisualNoiseThreshold: (threshold: number) => void;
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

function loadReadOnly(): boolean {
  try {
    return localStorage.getItem('readOnly') === 'true';
  } catch {
    return false;
  }
}

function saveReadOnly(enabled: boolean) {
  try {
    localStorage.setItem('readOnly', String(enabled));
  } catch { /* ignore */ }
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

function loadShowFlowStatusWords(): boolean {
  try {
    // Default to ON: the status words (Generating / Importing / Charging /
    // Discharging / Exporting / Idle) under each orbit node carry the
    // direction signal that used to live in a `-`/`+` prefix on the value.
    // Showing the words by default makes the diagram self-explanatory for
    // non-technical users without them having to find a Settings toggle.
    // The toggle remains in Settings so users who prefer the bare value
    // can still turn it off.
    const stored = localStorage.getItem('showFlowStatusWords');
    return stored === null ? true : stored === 'true';
  } catch {
    return true;
  }
}

function loadVisualNoiseThreshold(): number {
  try {
    const stored = localStorage.getItem('visualNoiseThreshold');
    if (stored !== null) {
      const n = Number(stored);
      if (Number.isFinite(n) && n >= 0) return n;
    }
  } catch { /* ignore */ }
  return 20;
}

function loadGridLineWeight(): GridLineWeight {
  try {
    const stored = localStorage.getItem('gridLineWeight');
    // Reject anything that isn't one of the two known presets — defends
    // against a typo or a future preset name being written by an older
    // build (issue #111). Default to 'standard' so existing users see
    // the original look after upgrade.
    return stored === 'subtle' ? 'subtle' : 'standard';
  } catch { /* ignore */ }
  return 'standard';
}

function saveGridLineWeight(weight: GridLineWeight) {
  try {
    localStorage.setItem('gridLineWeight', weight);
  } catch { /* ignore */ }
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
  connectedSince: null,
  connectFailures: 0,
  developerMode: loadDeveloperMode(),
  themeMode: loadThemeMode(),
  readOnly: loadReadOnly(),

  hiddenPanels: [],
  chartRange: loadChartRange(),
  panelGraphsEnabled: loadPanelGraphsEnabled(),
  panelGraphsScale: loadPanelGraphsScale(),
  panelGraphsYLock: loadPanelGraphsYLock(),
  panelGraphsYLockMax: 0,
  showFlowStatusWords: loadShowFlowStatusWords(),
  visualNoiseThreshold: loadVisualNoiseThreshold(),
  gridLineWeight: loadGridLineWeight(),
  pendingDischargeSlots: loadPendingDischargeSlots(),
  evcHost: '',
  evcPower: 0,
  evcChargingState: '',
  evcCharging: false,
  evcConnected: false,
  evcEverConnected: false,
  setSnapshot: (snapshot) => set({ snapshot }),
  clearSnapshot: () => set({ snapshot: null }),
  setConnection: (state, host, connectedSince) =>
    set((prev) => ({
      connectionState: state,
      connectedHost: host ?? null,
      connectedSince: state === 'connected' ? (connectedSince ?? Date.now()) : null,
      connectFailures: state === 'connected' ? 0 : prev.connectFailures,
    })),
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
  setReadOnly: (enabled) => {
    saveReadOnly(enabled);
    set({ readOnly: enabled });
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
  setShowFlowStatusWords: (enabled) => {
    try {
      localStorage.setItem('showFlowStatusWords', String(enabled));
    } catch { /* ignore */ }
    set({ showFlowStatusWords: enabled });
  },
  setVisualNoiseThreshold: (threshold) => {
    try {
      localStorage.setItem('visualNoiseThreshold', String(threshold));
    } catch { /* ignore */ }
    set({ visualNoiseThreshold: threshold });
  },
  setGridLineWeight: (weight) => {
    // Defensive: the setter takes a `GridLineWeight`, so the type system
    // already prevents unknown values. Belt-and-braces guard against a
    // future caller passing a string through an untyped boundary.
    if (weight !== 'standard' && weight !== 'subtle') return;
    saveGridLineWeight(weight);
    set({ gridLineWeight: weight });
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
  setEvcHost: (host) => set({ evcHost: host }),
  setEvcData: (power, charging, connected = true, chargingState = '') =>
    set((prev) => ({
      evcPower: power,
      evcChargingState: chargingState,
      evcCharging: charging,
      evcConnected: connected,
      // Latch: once we've ever seen a live EVC snapshot, stay latched.
      // SettingsPage calls `resetEvc()` when the user saves a new host so
      // the flag clears cleanly at that point.
      evcEverConnected: prev.evcEverConnected || connected,
    })),
  markEvcConnectedReached: () =>
    set({
      evcConnected: true,
      evcEverConnected: true,
    }),
  resetEvc: () =>
    set({
      evcPower: 0,
      evcChargingState: '',
      evcCharging: false,
      evcConnected: false,
      evcEverConnected: false,
    }),
}));
