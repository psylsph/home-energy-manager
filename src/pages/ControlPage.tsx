import { useState, useCallback, useEffect, useRef } from 'react';
import { useInverterStore } from '../store/useInverterStore';
import { apiPost, apiGet } from '../lib/api';
import type { ScheduleSlot } from '../lib/types';

type BatteryMode = 'unknown' | 'eco' | 'eco_paused' | 'timed_demand' | 'timed_export' | 'export_paused';

const ECO_MODES: { key: BatteryMode; label: string; tooltip: string }[] = [
  { key: 'eco', label: 'Eco', tooltip: 'Automatic — charges from solar, discharges to cover home demand' },
  { key: 'eco_paused', label: 'Eco Paused', tooltip: 'Battery stops discharging (SOC reserve set to 100%). Still charges from solar.' },
];

const TIMED_MODES: { key: BatteryMode; label: string; tooltip: string }[] = [
  { key: 'timed_demand', label: 'Timed Discharge', tooltip: 'Discharges battery during scheduled times to power your home' },
  { key: 'export_paused', label: 'Paused', tooltip: 'Pauses scheduled discharge. Schedule is kept for next time.' },
];

type ModeCategory = 'eco' | 'timed';

function modeToCategory(mode: BatteryMode): ModeCategory {
  return mode === 'eco' || mode === 'eco_paused' ? 'eco' : 'timed';
}

interface ActionState {
  loading: boolean;
  success: boolean;
  error: string | null;
}

function useAction() {
  const [state, setState] = useState<ActionState>({
    loading: false,
    success: false,
    error: null,
  });

  const execute = useCallback(
    async (path: string, body?: unknown) => {
      setState({ loading: true, success: false, error: null });
      try {
        await apiPost(path, body);
        setState({ loading: false, success: true, error: null });
        setTimeout(() => setState((s) => (s.success ? { ...s, success: false } : s)), 2000);
      } catch (e) {
        setState({ loading: false, success: false, error: (e as Error).message });
        setTimeout(() => setState((s) => (s.error ? { ...s, error: null } : s)), 3000);
      }
    },
    [],
  );

  return { ...state, execute };
}

function ActionButton({
  label,
  icon,
  path,
  body,
}: {
  label: string;
  icon: string;
  path: string;
  body?: unknown;
}) {
  const { loading, success, error, execute } = useAction();

  return (
    <div className="relative">
      <button
        onClick={() => execute(path, body)}
        disabled={loading}
        className="w-full flex flex-col items-center gap-1 sm:gap-2 p-2 sm:p-4 bg-bg-surface rounded-xl border border-transparent hover:border-battery/40 hover:bg-bg-elevated transition disabled:opacity-50"
      >
        <span className="text-xl sm:text-2xl">{icon}</span>
        <span className="text-text-primary text-xs sm:text-sm font-medium leading-tight text-center">{label}</span>
      </button>
      {loading && (
        <div className="absolute inset-0 flex items-center justify-center bg-bg-surface/80 rounded-xl">
          <div className="w-5 h-5 border-2 border-battery border-t-transparent rounded-full animate-spin" />
        </div>
      )}
      {success && (
        <div className="absolute inset-0 flex items-center justify-center bg-bg-surface/80 rounded-xl">
          <span className="text-green-400 text-xl">&#10003;</span>
        </div>
      )}
      {error && (
        <div className="absolute bottom-0 left-0 right-0 bg-red-900/80 text-red-200 text-xs text-center py-1 rounded-b-xl">
          {error}
        </div>
      )}
    </div>
  );
}

function TimePicker({
  hour,
  minute,
  onChange,
}: {
  hour: number;
  minute: number;
  onChange: (h: number, m: number) => void;
}) {
  return (
    <div className="flex items-center gap-1">
      <select
        value={hour}
        onChange={(e) => onChange(Number(e.target.value), minute)}
        className="bg-bg-elevated text-text-primary font-mono text-sm rounded-lg px-2 py-1.5 border border-transparent focus:border-battery outline-none"
      >
        {Array.from({ length: 24 }, (_, i) => (
          <option key={i} value={i}>
            {String(i).padStart(2, '0')}
          </option>
        ))}
      </select>
      <span className="text-text-secondary">:</span>
      <select
        value={minute}
        onChange={(e) => onChange(hour, Number(e.target.value))}
        className="bg-bg-elevated text-text-primary font-mono text-sm rounded-lg px-2 py-1.5 border border-transparent focus:border-battery outline-none"
      >
        {Array.from({ length: 60 }, (_, i) => i).map((m) => (
          <option key={m} value={m}>
            {String(m).padStart(2, '0')}
          </option>
        ))}
      </select>
    </div>
  );
}

function ScheduleSlotEditor({
  slotIndex,
  slot,
  onSave,
  showTargetSoc,
  apiPath = '/api/control/discharge-slot',
}: {
  slotIndex: number;
  slot: ScheduleSlot;
  onSave: (index: number, slot: ScheduleSlot, path: string) => void;
  showTargetSoc: boolean;
  apiPath?: string;
}) {
  const [local, setLocal] = useState<ScheduleSlot>({ ...slot });
  const [saving, setSaving] = useState(false);
  const [feedback, setFeedback] = useState<'saved' | 'error' | null>(null);

  const handleSave = async () => {
    setSaving(true);
    setFeedback(null);
    try {
      await onSave(slotIndex, local, apiPath);
      setFeedback('saved');
    } catch {
      setFeedback('error');
    }
    setSaving(false);
    setTimeout(() => setFeedback(null), 2000);
  };

  return (
    <div className="bg-bg-surface rounded-xl p-3 space-y-2">
      <div className="flex items-center justify-between">
        <span className="text-text-primary text-sm font-medium">Slot {slotIndex + 1}</span>
        <button
          onClick={() => setLocal((l) => ({ ...l, enabled: !l.enabled }))}
          className={`relative w-9 h-4 rounded-full transition ${local.enabled ? 'bg-battery' : 'bg-bg-elevated'
            }`}
        >
          <span
            className={`absolute top-0.5 w-3 h-3 rounded-full bg-white transition ${local.enabled ? 'left-5' : 'left-0.5'
              }`}
          />
        </button>
      </div>

      {local.enabled && (
        <>
          <div className="flex flex-col sm:flex-row items-center gap-4 sm:gap-6">
            <div className="flex items-center gap-1.5">
              <span className="text-text-secondary text-sm shrink-0">Start</span>
              <TimePicker
                hour={local.start_hour}
                minute={local.start_minute}
                onChange={(h, m) => setLocal((l) => ({ ...l, start_hour: h, start_minute: m }))}
              />
            </div>
            <div className="flex items-center gap-1.5">
              <span className="text-text-secondary text-sm shrink-0">End</span>
              <TimePicker
                hour={local.end_hour}
                minute={local.end_minute}
                onChange={(h, m) => setLocal((l) => ({ ...l, end_hour: h, end_minute: m }))}
              />
            </div>
          </div>

          {showTargetSoc && (
            <div className="space-y-1">
              <div className="flex items-center justify-between">
                <span className="text-text-secondary text-sm">Target SOC</span>
                <span className="font-mono text-text-primary text-sm">{local.target_soc}%</span>
              </div>
              <input
                type="range"
                min={4}
                max={100}
                step={1}
                value={local.target_soc}
                onChange={(e) => setLocal((l) => ({ ...l, target_soc: Number(e.target.value) }))}
                className="w-full"
              />
            </div>
          )}
        </>
      )}

      <button
        onClick={handleSave}
        disabled={saving}
        className="w-full py-2 bg-battery/20 text-battery rounded-lg text-sm font-medium hover:bg-battery/30 transition disabled:opacity-50"
      >
        {saving ? 'Saving...' : feedback === 'saved' ? '✓ Saved' : feedback === 'error' ? '✗ Error' : 'Save'}
      </button>
    </div>
  );
}

function AutoWinterSection() {
  const { snapshot } = useInverterStore();
  const [enabled, setEnabled] = useState(false);
  const [coldThreshold, setColdThreshold] = useState(8);
  const [recoveryThreshold, setRecoveryThreshold] = useState(12);
  const [targetSoc, setTargetSoc] = useState(80);
  const [debounce, setDebounce] = useState(10);
  const [saving, setSaving] = useState(false);
  const [saveFeedback, setSaveFeedback] = useState<'saved' | 'error' | null>(null);

  useEffect(() => {
    (async () => {
      try {
        const res = await apiGet<{ ok: boolean; data: { config: { enabled: boolean; cold_threshold: number; recovery_threshold: number; target_soc: number; debounce_readings: number } } }>('/api/auto-winter');
        if (res.ok) {
          setEnabled(res.data.config.enabled);
          setColdThreshold(Math.round(res.data.config.cold_threshold));
          setRecoveryThreshold(Math.round(res.data.config.recovery_threshold));
          setTargetSoc(res.data.config.target_soc);
          setDebounce(res.data.config.debounce_readings);
        }
      } catch { /* use defaults */ }
    })();
  }, []);

  const handleSave = async () => {
    setSaving(true);
    setSaveFeedback(null);
    try {
      await apiPost('/api/auto-winter', {
        enabled,
        cold_threshold: coldThreshold,
        recovery_threshold: recoveryThreshold,
        target_soc: targetSoc,
        debounce_readings: debounce,
      });
      setSaveFeedback('saved');
    } catch {
      setSaveFeedback('error');
    }
    setSaving(false);
    setTimeout(() => setSaveFeedback(null), 2000);
  };

  const winterActive = snapshot?.auto_winter_active;
  const batteryTemp = snapshot?.battery_temperature;

  return (
    <section className="space-y-3">
      <h2 className="text-text-primary font-semibold text-lg">Auto Winter Mode</h2>
      <div className="bg-bg-surface rounded-xl p-4 space-y-4">
        {winterActive && (
          <div className="text-xs bg-blue-900/40 text-text-primary px-3 py-2 rounded-lg">
            Winter mode active — battery is being charged to {snapshot?.target_soc ?? 80}%
            {batteryTemp != null && ` (${batteryTemp.toFixed(1)}°C)`}
          </div>
        )}

        {/* Master toggle */}
        <div className="flex items-center justify-between">
          <div>
            <span className="text-text-primary text-sm font-medium">Enable</span>
            <p className="text-text-secondary text-xs mt-0.5">
              Automatically charge battery when cold to warm cells
            </p>
          </div>
          <button
            onClick={() => setEnabled(!enabled)}
            className={`relative w-10 h-5 rounded-full transition ${enabled ? 'bg-battery' : 'bg-bg-elevated'}`}
          >
            <span
              className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition ${enabled ? 'left-5.5' : 'left-0.5'}`}
            />
          </button>
        </div>

        {enabled && (
          <>
            {/* Cold Threshold */}
            <div className="space-y-1">
              <div className="flex items-center justify-between">
                <span className="text-text-secondary text-sm">Cold Threshold</span>
                <span className="font-mono text-text-primary text-sm">{coldThreshold}°C</span>
              </div>
              <input
                type="range"
                min={0}
                max={20}
                step={1}
                value={coldThreshold}
                onChange={(e) => setColdThreshold(Number(e.target.value))}
                className="w-full"
              />
              <p className="text-text-secondary text-xs">
                Activate winter mode when battery drops below this temperature
              </p>
            </div>

            {/* Recovery Threshold */}
            <div className="space-y-1">
              <div className="flex items-center justify-between">
                <span className="text-text-secondary text-sm">Recovery Threshold</span>
                <span className="font-mono text-text-primary text-sm">{recoveryThreshold}°C</span>
              </div>
              <input
                type="range"
                min={1}
                max={25}
                step={1}
                value={recoveryThreshold}
                onChange={(e) => setRecoveryThreshold(Number(e.target.value))}
                className="w-full"
              />
              <p className="text-text-secondary text-xs">
                Disable winter mode when battery warms above this temperature
              </p>
            </div>

            {/* Target SOC */}
            <div className="space-y-1">
              <div className="flex items-center justify-between">
                <span className="text-text-secondary text-sm">Target SOC</span>
                <span className="font-mono text-text-primary text-sm">{targetSoc}%</span>
              </div>
              <input
                type="range"
                min={4}
                max={100}
                step={1}
                value={targetSoc}
                onChange={(e) => setTargetSoc(Number(e.target.value))}
                className="w-full"
              />
            </div>

            {/* Debounce */}
            <div className="space-y-1">
              <div className="flex items-center justify-between">
                <span className="text-text-secondary text-sm">Debounce readings</span>
                <span className="font-mono text-text-primary text-sm">{debounce}</span>
              </div>
              <input
                type="range"
                min={1}
                max={30}
                step={1}
                value={debounce}
                onChange={(e) => setDebounce(Number(e.target.value))}
                className="w-full"
              />
              <p className="text-text-secondary text-xs">
                Number of consecutive readings before switching (~{debounce * 60}s at 60s interval)
              </p>
            </div>

            {/* Warning */}
            <div className="text-xs bg-yellow-900/30 text-text-primary px-3 py-2 rounded-lg">
              Winter mode charges the battery using grid power when solar is insufficient.
              Your existing charge schedule will be restored when the battery warms up.
            </div>
          </>
        )}

        {/* App vs cloud note — always visible, even when disabled */}
        <div className="text-xs bg-blue-900/30 text-text-primary px-3 py-2 rounded-lg">
          <strong>Note:</strong> This is implemented locally within this app — it monitors
          battery temperature via Modbus and forces charging when the battery gets cold.
          It does not use GivEnergy's cloud-based winter mode. The app must stay running
          for it to work.
        </div>

        <button
          onClick={handleSave}
          disabled={saving}
          className="w-full py-2 bg-battery/20 text-battery rounded-lg text-sm font-medium hover:bg-battery/30 transition disabled:opacity-50"
        >
          {saving ? 'Saving...' : saveFeedback === 'saved' ? '✓ Saved' : saveFeedback === 'error' ? '✗ Error' : 'Save'}
        </button>
      </div>
    </section>
  );
}

/** Charging mode section — select between Standard, Cosy, or Agile charging. */
function CosyChargingSection({ mode, cosyActive, onModeChange }: { mode: 'standard' | 'cosy' | 'agile'; cosyActive: boolean; onModeChange: (m: 'standard' | 'cosy' | 'agile') => void }) {
  const [slots, setSlots] = useState<
    { enabled: boolean; start_hour: number; start_minute: number; end_hour: number; end_minute: number; target_soc: number }[]
  >([]);
  const [saving, setSaving] = useState(false);
  const [saveFeedback, setSaveFeedback] = useState<'saved' | 'error' | null>(null);
  const [loaded, setLoaded] = useState(false);

  const enabled = mode === 'cosy';

  useEffect(() => {
    (async () => {
      try {
        const res = await apiGet<{ ok: boolean; enabled: boolean; slots: typeof slots }>('/api/cosy');
        if (res.ok) {
          if (res.enabled !== (mode === 'cosy')) {
            onModeChange(res.enabled ? 'cosy' : 'standard');
          }
          const initial = res.slots.length === 3
            ? res.slots
            : Array.from({ length: 3 }, () => ({
                enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100,
              }));
          setSlots(initial);
        }
      } catch { /* use defaults */ }
      setLoaded(true);
    })();
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const handleModeChange = async (newMode: 'standard' | 'cosy' | 'agile') => {
    // Don't switch until slots have loaded
    if (!loaded || slots.length === 0) return;
    const cosyEnabled = newMode === 'cosy';
    const agileEnabled = newMode === 'agile';
    onModeChange(newMode);
    setSaving(true);
    try {
      await Promise.all([
        apiPost('/api/cosy', { enabled: cosyEnabled, slots }),
        apiPost('/api/agile', { enabled: agileEnabled }),
      ]);
      setSaveFeedback('saved');
    } catch {
      setSaveFeedback('error');
    }
    setSaving(false);
    setTimeout(() => setSaveFeedback(null), 2000);
  };

  const saveSlots = async () => {
    setSaving(true);
    setSaveFeedback(null);
    try {
      await apiPost('/api/cosy', { enabled, slots });
      setSaveFeedback('saved');
    } catch {
      setSaveFeedback('error');
    }
    setSaving(false);
    setTimeout(() => setSaveFeedback(null), 2000);
  };

  return (
    <section className="space-y-3 border-t border-bg-elevated pt-4">
      <div className="flex items-center justify-between">
        <h2 className="text-text-primary font-semibold text-lg">Charging Mode</h2>
        <div className="flex items-center gap-2">
          <button
            onClick={async () => {
              setSaving(true);
              try {
                await Promise.all([
                  apiPost('/api/cosy', { enabled: mode === 'cosy', slots }),
                  apiPost('/api/agile', { enabled: mode === 'agile' }),
                ]);
                setSaveFeedback('saved');
              } catch {
                setSaveFeedback('error');
              }
              setSaving(false);
              setTimeout(() => setSaveFeedback(null), 2000);
            }}
            disabled={saving}
            className="text-xs font-medium px-2.5 py-1 rounded-lg bg-battery/20 text-battery hover:bg-battery/30 transition disabled:opacity-50"
          >
            {saveFeedback === 'saved' ? '✓' : saveFeedback === 'error' ? '!' : saving ? '...' : 'Apply'}
          </button>
          <select
          value={mode}
          onChange={(e) => handleModeChange(e.target.value as 'standard' | 'cosy' | 'agile')}
          disabled={saving}
          className="bg-bg-elevated text-text-primary font-mono text-sm rounded-lg px-3 py-1.5 border border-transparent focus:border-battery outline-none cursor-pointer"
        >
          <option value="standard">Standard</option>
          <option value="cosy">Cosy</option>
          <option value="agile">Agile</option>
        </select>
      </div>
      </div>

      {(mode === 'cosy' || mode === 'agile') && (
        <div className="rounded-xl border border-yellow-500/30 bg-yellow-500/10 p-2.5 space-y-1">
          <div className="flex items-center gap-1.5">
            <span className="text-[10px] font-bold text-text-primary uppercase tracking-wide">Beta</span>
            <span className="text-[11px] text-text-primary font-medium">App must be kept running</span>
          </div>
          <p className="text-[11px] text-text-secondary leading-relaxed">
            {mode === 'cosy'
              ? 'Cosy mode schedules force-charging based on time slots you define. The app must stay running for slot entry and exit to work — if you close it mid-slot, the inverter stays in force-charge mode until you reopen the app or stop it manually.'
              : 'Agile mode automatically charges and discharges based on live Octopus prices. The app must stay running for price checks and switching to work — if you close it, the inverter stays in whatever mode it was last set to.'
            }
          </p>
        </div>
      )}

      {mode === 'cosy' && (
        <p className="text-text-secondary/60 text-xs mt-3">
          Force-charges the battery from the grid during these windows. The inverter is locked to Cosy mode while enabled.
        </p>
      )}

      {mode === 'cosy' && (
        <div className="space-y-4">
          {slots.map((slot, i) => {
            const now = new Date();
            const nowMins = now.getHours() * 60 + now.getMinutes();
            const startMins = slot.start_hour * 60 + slot.start_minute;
            const endMins = slot.end_hour * 60 + slot.end_minute;
            const crossesMidnight = endMins <= startMins;
            const slotActive = slot.enabled && cosyActive && (
              crossesMidnight
                ? (nowMins >= startMins || nowMins < endMins)
                : (nowMins >= startMins && nowMins < endMins)
            );
            return (
            <div key={i} className={`rounded-xl p-3 space-y-2 border ${slotActive ? 'bg-battery/10 border-battery/30' : 'bg-bg-surface border-transparent'}`}>
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-2">
                  <span className="text-text-primary text-sm font-medium">Slot {i + 1}</span>
                  {slotActive && (
                    <span className="flex items-center gap-1 text-xs text-battery font-semibold">
                      <span className="inline-block w-1.5 h-1.5 rounded-full bg-battery animate-pulse" />
                      Charging
                    </span>
                  )}
                </div>
                <button
                  onClick={() => {
                    const next = [...slots];
                    next[i] = { ...next[i], enabled: !next[i].enabled };
                    setSlots(next);
                  }}
                  className={`relative w-9 h-4 rounded-full transition ${slot.enabled ? 'bg-battery' : 'bg-bg-elevated'}`}
                >
                  <span className={`absolute top-0.5 w-3 h-3 rounded-full bg-white transition ${slot.enabled ? 'left-5' : 'left-0.5'}`} />
                </button>
              </div>
              {slot.enabled && (
                <div className="space-y-2">
                  <div className="flex flex-col sm:flex-row items-center gap-4 sm:gap-6">
                    <div className="flex items-center gap-1.5">
                      <span className="text-text-secondary text-sm">Start</span>
                      <TimePicker
                        hour={slot.start_hour}
                        minute={slot.start_minute}
                        onChange={(h, m) => {
                          const next = [...slots];
                          next[i] = { ...next[i], start_hour: h, start_minute: m };
                          setSlots(next);
                        }}
                      />
                    </div>
                    <div className="flex items-center gap-1.5">
                      <span className="text-text-secondary text-sm">End</span>
                      <TimePicker
                        hour={slot.end_hour}
                        minute={slot.end_minute}
                        onChange={(h, m) => {
                          const next = [...slots];
                          next[i] = { ...next[i], end_hour: h, end_minute: m };
                          setSlots(next);
                        }}
                      />
                    </div>
                  </div>
                  <div className="flex items-center gap-2">
                    <span className="text-text-secondary text-xs w-20 shrink-0">Target SOC</span>
                    <input
                      type="range"
                      min={4}
                      max={100}
                      step={1}
                      value={slot.target_soc}
                      onChange={(e) => {
                        const next = [...slots];
                        next[i] = { ...next[i], target_soc: Number(e.target.value) };
                        setSlots(next);
                      }}
                      className="flex-1"
                    />
                    <span className="font-mono text-text-primary text-xs w-8 text-right">{slot.target_soc}%</span>
                  </div>
                </div>
              )}
            </div>
          );
          })}
          <button
            onClick={saveSlots}
            disabled={saving}
            className="w-full py-2 bg-battery/20 text-battery rounded-lg text-sm font-medium hover:bg-battery/30 transition disabled:opacity-50"
          >
            {saving ? 'Saving...' : saveFeedback === 'saved' ? '✓ Saved' : saveFeedback === 'error' ? '✗ Error' : 'Save slots'}
          </button>
        </div>
      )}

      {mode === 'agile' && <AgileControls />}
    </section>
  );
}

// Map UK postcode area (first 1-2 letters) to Octopus GSP group letter.
// Based on Distribution Network Operator boundaries and tariff regions.
const POSTCODE_AREA_TO_GSP: Record<string, string> = {
  // GSP A — Eastern England
  CB: 'A', CO: 'A', IP: 'A', NR: 'A', PE: 'A', AL: 'A', CM: 'A', EN: 'A', SG: 'A', SS: 'A', WD: 'A',
  // GSP B — East Midlands
  CV: 'B', DE: 'B', DN: 'B', LE: 'B', LN: 'B', NG: 'B', NN: 'B',
  // GSP C — London
  BR: 'C', CR: 'C', DA: 'C', E: 'C', EC: 'C', HA: 'C', IG: 'C', KT: 'C', N: 'C', NW: 'C',
  RM: 'C', SE: 'C', SM: 'C', SW: 'C', TW: 'C', UB: 'C', W: 'C', WC: 'C',
  // GSP D — North Wales & Merseyside
  CH: 'D',  L: 'D', LL: 'D', WA: 'D',
  // GSP E — West Midlands
  B: 'E', DY: 'E', ST: 'E', TF: 'E', WS: 'E', WV: 'E', WR: 'E',
  // GSP F — North East England
  DH: 'F', DL: 'F', NE: 'F', SR: 'F', TS: 'F',
  // GSP G — North West England
  BB: 'G', BL: 'G', CA: 'G', FY: 'G', LA: 'G', M: 'G', OL: 'G', PR: 'G', SK: 'G', WN: 'G',
  // GSP H — Southern England
  BN: 'H', GU: 'H', PO: 'H', RH: 'H', SO: 'H',
  // GSP J — South East England
  CT: 'J', ME: 'J', TN: 'J',
  // GSP K — South Wales
  CF: 'K', LD: 'K', NP: 'K', SA: 'K',
  // GSP L — South West England
  BA: 'L', BS: 'L', DT: 'L', EX: 'L', PL: 'L', TA: 'L', TQ: 'L', TR: 'L', SN: 'L', SP: 'L', GL: 'L',
  // GSP M — Yorkshire
  BD: 'M', HD: 'M', HG: 'M', HX: 'M', HU: 'M', LS: 'M', WF: 'M', YO: 'M',
  // GSP N — South & Central Scotland
  DD: 'N', DG: 'N', EH: 'N', FK: 'N', G: 'N', KA: 'N', KY: 'N', ML: 'N', TD: 'N',
  // GSP P — North Scotland
  AB: 'P', HS: 'P', IV: 'P', KW: 'P', ZE: 'P',
};

/** Summary bar shown below the price forecast grid. */
function PriceSummary({ prices, decisionForPrice }: {
  prices: { validFrom: Date; validTo: Date; pence: number }[];
  decisionForPrice: (p: number) => 'charge' | 'discharge' | 'nothing';
}) {
  const { snapshot } = useInverterStore();
  const [importTariff, setImportTariff] = useState(0.285);
  const [exportTariff, setExportTariff] = useState(0.15);

  // Load tariff config from backend
  useEffect(() => {
    (async () => {
      try {
        const res = await apiGet<{ ok: boolean; data: { import_tariff: number; export_tariff: number; import_tariff_config: unknown } }>('/api/settings');
        if (res.ok) {
          setImportTariff(res.data.import_tariff);
          setExportTariff(res.data.export_tariff);
        }
      } catch { /* use defaults */ }
    })();
  }, []);

  const now = new Date();
  const upcoming = prices.filter(s => s.validTo > now);
  const chargeSlots = upcoming.filter(s => decisionForPrice(s.pence) === 'charge');
  const dischargeSlots = upcoming.filter(s => decisionForPrice(s.pence) === 'discharge');
  const idleSlots = upcoming.filter(s => decisionForPrice(s.pence) === 'nothing');

  const avgChargePrice = chargeSlots.length
    ? chargeSlots.reduce((a, s) => a + s.pence, 0) / chargeSlots.length
    : 0;
  const avgDischargePrice = dischargeSlots.length
    ? dischargeSlots.reduce((a, s) => a + s.pence, 0) / dischargeSlots.length
    : 0;
  const minPrice = upcoming.length ? Math.min(...upcoming.map(s => s.pence)) : 0;
  const maxPrice = upcoming.length ? Math.max(...upcoming.map(s => s.pence)) : 0;

  // Rough daily saving estimate
  // Assume each 30-min slot can charge/discharge at 1/48 of daily battery throughput.
  // Typical: one full charge cycle per day at battery capacity.
  const battKwh = snapshot?.battery_capacity_kwh ?? 5;
  const chargeSaving = chargeSlots.length
    ? (chargeSlots.length / 48) * battKwh * Math.max(0, importTariff - avgChargePrice / 100)
    : 0;
  const dischargeSaving = dischargeSlots.length
    ? (dischargeSlots.length / 48) * battKwh * Math.max(0, avgDischargePrice / 100 - (exportTariff))
    : 0;
  const totalSaving = chargeSaving + dischargeSaving;

  return (
    <div className="bg-bg-surface rounded-lg p-2.5 space-y-1.5">
      <div className="flex items-center gap-3 text-xs text-text-secondary flex-wrap">
        <span className="flex items-center gap-1">
          <span className="inline-block w-2 h-2 rounded-full bg-battery" />
          {chargeSlots.length} charge
        </span>
        <span className="flex items-center gap-1">
          <span className="inline-block w-2 h-2 rounded-full bg-orange-500" />
          {dischargeSlots.length} discharge
        </span>
        <span className="flex items-center gap-1">
          <span className="inline-block w-2 h-2 rounded-full bg-text-secondary/30" />
          {idleSlots.length} hold
        </span>
        <span className="text-text-secondary/50">·</span>
        <span>Min {minPrice.toFixed(1)}p</span>
        <span className="text-text-secondary/50">·</span>
        <span>Max {maxPrice.toFixed(1)}p</span>
      </div>
      {chargeSlots.length > 0 && (
        <div className="text-xs text-text-secondary">
          Avg charge price: <span className="font-mono text-battery">{avgChargePrice.toFixed(1)}p</span>
          {avgChargePrice < importTariff * 100 && (
            <> — saves <span className="font-mono text-battery">{(importTariff * 100 - avgChargePrice).toFixed(1)}p</span>/kWh vs standard rate</>
          )}
        </div>
      )}
      {dischargeSlots.length > 0 && (
        <div className="text-xs text-text-secondary">
          Avg discharge price: <span className="font-mono text-orange-400">{avgDischargePrice.toFixed(1)}p</span>
          {avgDischargePrice > exportTariff * 100 && (
            <> — earns <span className="font-mono text-orange-400">{(avgDischargePrice - exportTariff * 100).toFixed(1)}p</span>/kWh vs standard export</>
          )}
        </div>
      )}
      {totalSaving > 0.01 && (
        <div className="text-xs font-medium">
          Estimated daily saving:{' '}
          <span className="font-mono text-battery">£{totalSaving.toFixed(2)}</span>
          {battKwh > 0 && (
            <span className="text-text-secondary"> ({(battKwh).toFixed(1)}kWh battery, {(importTariff * 100).toFixed(1)}p import)</span>
          )}
        </div>
      )}
    </div>
  );
}

/** Agile Octopus controls — shown when Agile charging mode is selected. */
function AgileControls() {
  const [chargeThreshold, setChargeThreshold] = useState(10);
  const [dischargeThreshold, setDischargeThreshold] = useState(30);
  const [region, setRegion] = useState('A');
  const [postcode, setPostcode] = useState('');
  const [postcodeLookup, setPostcodeLookup] = useState<'idle' | 'loading' | 'found' | 'not_found' | 'error'>('idle');

  const regions = [
    { code: 'A', label: 'Eastern England' },
    { code: 'B', label: 'East Midlands' },
    { code: 'C', label: 'London' },
    { code: 'D', label: 'North Wales & Merseyside' },
    { code: 'E', label: 'West Midlands' },
    { code: 'F', label: 'North East England' },
    { code: 'G', label: 'North West England' },
    { code: 'H', label: 'Southern England' },
    { code: 'J', label: 'South East England' },
    { code: 'K', label: 'South Wales' },
    { code: 'L', label: 'South West England' },
    { code: 'M', label: 'Yorkshire' },
    { code: 'N', label: 'South & Central Scotland' },
    { code: 'P', label: 'North Scotland' },
  ];

  const lookUpPostcode = useCallback(async (pc: string) => {
    const clean = pc.replace(/\s+/g, '').toUpperCase();
    if (clean.length < 3) return;
    setPostcodeLookup('loading');
    try {
      const res = await fetch(`https://api.postcodes.io/postcodes/${encodeURIComponent(clean)}`);
      if (!res.ok) {
        setPostcodeLookup('not_found');
        return;
      }
      const json = await res.json();
      if (json.status !== 200) {
        setPostcodeLookup('not_found');
        return;
      }
      // Extract postcode area (first 1-2 alpha characters)
      const match = clean.match(/^([A-Z]{1,2})/);
      if (!match) {
        setPostcodeLookup('not_found');
        return;
      }
      const area = match[1];
      const gsp = POSTCODE_AREA_TO_GSP[area];
      if (gsp) {
        setRegion(gsp);
        setPostcodeLookup('found');
      } else {
        setPostcodeLookup('not_found');
      }
    } catch {
      setPostcodeLookup('error');
    }
  }, []);

  const debouncedPostcode = useRef<ReturnType<typeof setTimeout> | null>(null);
  const handlePostcodeChange = (value: string) => {
    setPostcode(value);
    if (debouncedPostcode.current) clearTimeout(debouncedPostcode.current);
    if (value.replace(/\s+/g, '').length >= 3) {
      debouncedPostcode.current = setTimeout(() => lookUpPostcode(value), 500);
    } else {
      setPostcodeLookup('idle');
    }
  };

  // Half-hourly price data from Octopus API
  interface PriceSlot {
    validFrom: Date;
    validTo: Date;
    pence: number;
  }

  // Cache prices keyed by date (YYYY-MM-DD) so the display survives API handovers
  const pricesCache = useRef<Record<string, PriceSlot[]>>({});

  const [displaySlots, setDisplaySlots] = useState<PriceSlot[]>([]);
  const [pricesLoading, setPricesLoading] = useState(false);
  const [pricesError, setPricesError] = useState<string | null>(null);

  // Rolling window: return up to 48 upcoming slots from cache
  const computeRollingWindow = useCallback(() => {
    const now = new Date();
    const allCached = Object.values(pricesCache.current)
      .flat()
      .sort((a, b) => a.validFrom.getTime() - b.validFrom.getTime());
    const startIdx = allCached.findIndex(s => s.validTo.getTime() > now.getTime());
    return startIdx >= 0 ? allCached.slice(startIdx, startIdx + 48) : [];
  }, []);

  // Fetch prices from Octopus API (covering today onwards), cache by date so
  // the display survives the API's next-day handover, and re-fetch every 5
  // minutes to pick up newly published prices. The fetch logic lives inside
  // the effect (rather than a useCallback) so the react-hooks linter doesn't
  // trace the synchronous setState calls as cascading-render triggers.
  useEffect(() => {
    let cancelled = false;

    const load = async () => {
      setPricesLoading(true);
      setPricesError(null);
      try {
        const now = new Date();
        const todayStr = now.toISOString().slice(0, 10); // YYYY-MM-DD

        const baseUrl = `https://api.octopus.energy/v1/products/AGILE-24-10-01/electricity-tariffs/E-1R-AGILE-24-10-01-${region}/standard-unit-rates/`;
        const url = `${baseUrl}?period_from=${todayStr}T00:00:00Z&page_size=96`;
        const res = await fetch(url);
        if (!res.ok) {
          if (!cancelled) setPricesError(`API returned ${res.status}`);
          return;
        }
        const json = await res.json();
        const slots: PriceSlot[] = (json.results || []).map((r: { valid_from: string; valid_to: string; value_inc_vat: number }) => ({
          validFrom: new Date(r.valid_from),
          validTo: new Date(r.valid_to),
          pence: r.value_inc_vat,
        }));

        // Update cache by date
        const newCache = { ...pricesCache.current };
        for (const slot of slots) {
          const key = slot.validFrom.toISOString().slice(0, 10);
          if (!newCache[key]) newCache[key] = [];
          // Deduplicate by slot start time
          if (!newCache[key].some(s => s.validFrom.getTime() === slot.validFrom.getTime())) {
            newCache[key].push(slot);
          }
        }
        // Prune cache: keep only today, yesterday (just fetched), and tomorrow
        const yesterday = new Date(now.getTime() - 86400000);
        const keepKeys = [
          yesterday.toISOString().slice(0, 10),
          todayStr,
          new Date(now.getTime() + 86400000).toISOString().slice(0, 10),
        ];
        for (const key of Object.keys(newCache)) {
          if (!keepKeys.includes(key)) delete newCache[key];
        }
        pricesCache.current = newCache;

        if (!cancelled) setDisplaySlots(computeRollingWindow());
      } catch (e) {
        if (!cancelled) setPricesError((e as Error).message);
      } finally {
        if (!cancelled) setPricesLoading(false);
      }
    };

    load();
    const interval = setInterval(load, 5 * 60 * 1000); // every 5 minutes
    return () => { cancelled = true; clearInterval(interval); };
  }, [region, computeRollingWindow]);

  // Recompute rolling window every 30 seconds (for the "now" indicator)
  useEffect(() => {
    const tick = setInterval(() => setDisplaySlots(computeRollingWindow()), 30000);
    return () => clearInterval(tick);
  }, [computeRollingWindow]);

  const [saving, setSaving] = useState(false);
  const [saveFeedback, setSaveFeedback] = useState<'saved' | 'error' | null>(null);

  // Load config from backend on mount
  useEffect(() => {
    (async () => {
      try {
        const res = await apiGet<{ ok: boolean; enabled: boolean; region: string; charge_threshold: number; discharge_threshold: number }>('/api/agile');
        if (res.ok) {
          setRegion(res.region);
          setChargeThreshold(res.charge_threshold);
          setDischargeThreshold(res.discharge_threshold);
        }
      } catch { /* use defaults */ }
    })();
  }, []);

  // Determine charge/discharge decision for a given price
  const decisionForPrice = (pence: number): 'charge' | 'discharge' | 'nothing' => {
    if (pence <= chargeThreshold) return 'charge';
    if (pence >= dischargeThreshold) return 'discharge';
    return 'nothing';
  };

  const saveConfig = async () => {
    setSaving(true);
    setSaveFeedback(null);
    try {
      await apiPost('/api/agile', {
        enabled: true,
        region,
        charge_threshold: chargeThreshold,
        discharge_threshold: dischargeThreshold,
      });
      setSaveFeedback('saved');
    } catch {
      setSaveFeedback('error');
    }
    setSaving(false);
    setTimeout(() => setSaveFeedback(null), 2000);
  };

  return (
    <div className="space-y-4 mt-3">
      <p className="text-text-secondary/60 text-xs">
        Automatically charges when Agile prices are low and discharges when prices are high.
        Prices are fetched from the public Octopus Energy API — no account needed.
      </p>

      {/* Postcode lookup */}
      <div className="space-y-1.5">
        <div className="flex items-center justify-between">
          <span className="text-text-secondary text-sm">Postcode</span>
          {postcodeLookup === 'found' && (
            <span className="text-xs text-battery">Region set to {region}</span>
          )}
          {postcodeLookup === 'loading' && (
            <span className="text-xs text-text-secondary flex items-center gap-1">
              <span className="inline-block w-3 h-3 border-2 border-current border-t-transparent rounded-full animate-spin" />
              Looking up…
            </span>
          )}
          {postcodeLookup === 'not_found' && (
            <span className="text-xs text-amber-400">Could not determine region</span>
          )}
          {postcodeLookup === 'error' && (
            <span className="text-xs text-red-400">Lookup failed</span>
          )}
        </div>
        <div className="flex gap-2">
          <input
            type="text"
            placeholder="e.g. SW1A 1AA"
            value={postcode}
            onChange={(e) => handlePostcodeChange(e.target.value)}
            className="flex-1 bg-bg-elevated text-text-primary font-mono text-sm rounded-lg px-3 py-2 border border-transparent focus:border-battery outline-none"
          />
        </div>
        <p className="text-text-secondary text-xs">
          Enter your postcode to auto-detect your Octopus region. Powered by postcodes.io.
        </p>
      </div>

      {/* Region selector */}
      <div className="space-y-1.5">
        <div className="flex items-center justify-between">
          <span className="text-text-secondary text-sm">Region</span>
          <span className="font-mono text-text-primary text-xs">{region}</span>
        </div>
        <select
          value={region}
          onChange={(e) => setRegion(e.target.value)}
          className="w-full bg-bg-elevated text-text-primary font-mono text-sm rounded-lg px-3 py-2 border border-transparent focus:border-battery outline-none cursor-pointer"
        >
          {regions.map((r) => (
            <option key={r.code} value={r.code}>{r.label}</option>
          ))}
        </select>
      </div>

      {/* Charge threshold */}
      <div className="space-y-1">
        <div className="flex items-center justify-between">
          <span className="text-text-secondary text-sm">Charge when below</span>
          <span className="font-mono text-text-primary text-sm">{chargeThreshold}p/kWh</span>
        </div>
        <input
          type="range"
          min={0}
          max={50}
          step={0.5}
          value={chargeThreshold}
          onChange={(e) => setChargeThreshold(Number(e.target.value))}
          className="w-full"
        />
        <p className="text-text-secondary text-xs">
          Force-charge the battery from the grid when the current half-hour price is at or below this threshold.
        </p>
      </div>

      {/* Discharge threshold */}
      <div className="space-y-1">
        <div className="flex items-center justify-between">
          <span className="text-text-secondary text-sm">Discharge when above</span>
          <span className="font-mono text-text-primary text-sm">{dischargeThreshold}p/kWh</span>
        </div>
        <input
          type="range"
          min={5}
          max={100}
          step={0.5}
          value={dischargeThreshold}
          onChange={(e) => setDischargeThreshold(Number(e.target.value))}
          className="w-full"
        />
        <p className="text-text-secondary text-xs">
          Discharge the battery to power the home when the current half-hour price is at or above this threshold.
        </p>
      </div>

      {/* Price forecast — 12 columns × 4 rows, no scroll */}
      <div className="space-y-2">
        <div className="flex items-center justify-between">
          <span className="text-text-secondary text-sm">Price Forecast</span>
          {pricesLoading && (
            <span className="text-xs text-text-secondary flex items-center gap-1">
              <span className="inline-block w-3 h-3 border-2 border-current border-t-transparent rounded-full animate-spin" />
              Loading…
            </span>
          )}
          {pricesError && (
            <span className="text-xs text-red-400">{pricesError}</span>
          )}
        </div>

        {displaySlots.length === 0 && !pricesLoading && (
          <p className="text-text-secondary text-xs py-2">No price data available.</p>
        )}

        {displaySlots.length > 0 && (
          <>
            <div className="bg-bg-surface rounded-lg p-2 space-y-0.5">
              {/* 4 rows × 12 columns = up to 48 half-hour slots */}
              {Array.from({ length: 4 }, (_, row) => (
                <div key={row} className="grid grid-cols-12 gap-0.5">
                  {Array.from({ length: 12 }, (_, col) => {
                    const idx = row * 12 + col;
                    const slot = displaySlots[idx];
                    if (!slot) return <div key={col} />;
                    const decision = decisionForPrice(slot.pence);
                    const now = new Date();
                    const isPast = slot.validTo <= now;
                    const isNow = slot.validFrom <= now && slot.validTo > now;
                    const mins = String(slot.validFrom.getMinutes()).padStart(2, '0');
                    let barColor = 'bg-bg-elevated';
                    if (!isPast) {
                      if (decision === 'charge') barColor = 'bg-battery';
                      else if (decision === 'discharge') barColor = 'bg-orange-500';
                      else barColor = 'bg-text-secondary/30';
                    }
                    return (
                      <div
                        key={col}
                        className={`flex flex-col items-center rounded-sm py-0.5 ${barColor} ${isNow ? 'border-2 border-red-500 animate-pulse' : ''} ${isPast ? 'opacity-30' : ''}`}
                        title={`${slot.validFrom.toLocaleTimeString()} - ${slot.validTo.toLocaleTimeString()}: ${slot.pence.toFixed(1)}p — ${isPast ? 'past' : decision}`}
                      >
                        <span className="text-[9px] leading-none font-mono">{slot.pence.toFixed(1)}</span>
                        <span className="text-[7px] leading-none text-text-secondary mt-px">:{mins}</span>
                      </div>
                    );
                  })}
                </div>
              ))}
            </div>

            {/* Summary bar */}
            <PriceSummary prices={displaySlots} decisionForPrice={decisionForPrice} />
          </>
        )}

        {/* Legend */}
        <div className="flex gap-3 text-[10px] text-text-secondary">
          <span className="flex items-center gap-1">
            <span className="inline-block w-2.5 h-2.5 rounded-sm bg-battery" />
            Charge
          </span>
          <span className="flex items-center gap-1">
            <span className="inline-block w-2.5 h-2.5 rounded-sm bg-text-secondary/30" />
            Hold
          </span>
          <span className="flex items-center gap-1">
            <span className="inline-block w-2.5 h-2.5 rounded-sm bg-orange-500" />
            Discharge
          </span>
          <span className="flex items-center gap-1">
            <span className="inline-block w-2.5 h-2.5 rounded-sm opacity-30 bg-bg-elevated" />
            Past
          </span>
        </div>
      </div>

      <button
        onClick={saveConfig}
        disabled={saving}
        className="w-full py-2 bg-battery/20 text-battery rounded-lg text-sm font-medium hover:bg-battery/30 transition disabled:opacity-50"
      >
        {saving ? 'Saving...' : saveFeedback === 'saved' ? '✓ Saved' : saveFeedback === 'error' ? '✗ Error' : 'Save'}
      </button>
    </div>
  );
}

/** Battery calibration section — developer mode only. */
function BatteryCalibrationSection() {
  const { snapshot } = useInverterStore();
  const stage = snapshot?.battery_calibration_stage ?? 0;
  const [saving, setSaving] = useState(false);
  const [feedback, setFeedback] = useState<'saved' | 'error' | null>(null);

  const handleStartCalibration = async () => {
    if (!confirm('⚠️  BATTERY CALIBRATION\n\nThis will cycle the battery through: discharge → calibrate → charge → balance → set capacity.\n\nThe balancing phase ensures all cells are equalized. The full cycle can take several hours.\n\nOnly proceed if you understand the risks. Continue?')) return;
    setSaving(true);
    setFeedback(null);
    try {
      await apiPost('/api/control/calibration', { stage: 1 });
      setFeedback('saved');
    } catch {
      setFeedback('error');
    }
    setSaving(false);
    setTimeout(() => setFeedback(null), 3000);
  };

  const stageLabels: Record<number, string> = {
    0: 'Off',
    1: 'Discharging…',
    2: 'Setting lower limit…',
    3: 'Charging…',
    4: 'Setting upper limit…',
    5: 'Balancing…',
    6: 'Setting full capacity…',
    7: 'Finished',
  };

  const isActive = stage > 0 && stage < 7;

  return (
    <section className="space-y-3 border-t border-bg-elevated pt-4">
      <div className="flex items-center gap-2">
        <h2 className="text-text-primary font-semibold text-lg">Battery Calibration</h2>
        <span className="text-xs bg-amber-500/20 text-text-primary px-2 py-0.5 rounded-full font-medium">DEV</span>
      </div>

      <div className="bg-amber-900/20 border border-amber-700/30 rounded-xl p-3 space-y-2">
        <p className="text-text-primary text-xs font-medium">⚠️  WARNING</p>
        <p className="text-text-secondary text-xs">
          Calibration cycles the battery through: discharge → calibrate lower
          limit → charge → balance → calibrate upper limit. Once started, the
          process cannot be cancelled — it must run to completion.
          This can take several hours. Only use if you understand the risks.
        </p>
      </div>

      <div className="bg-bg-surface rounded-xl p-3 space-y-2">
        <div className="flex items-center justify-between">
          <span className="text-text-secondary text-sm">Current Stage</span>
          <span className={`font-mono text-sm ${isActive ? 'text-amber-400' : 'text-text-primary'}`}>
            {stageLabels[stage] || `Unknown (${stage})`}
          </span>
        </div>

        <div className="flex gap-2 pt-1">
          <button
            onClick={handleStartCalibration}
            disabled={saving || isActive}
            className="w-full py-2 bg-amber-500/20 text-text-primary rounded-lg text-xs font-medium hover:bg-amber-500/30 transition disabled:opacity-40 border border-amber-500/30"
          >
            Start Calibration
          </button>
        </div>

        {feedback && (
          <p className={`text-xs ${feedback === 'saved' ? 'text-battery' : 'text-red-400'}`}>
            {feedback === 'saved' ? 'Command sent' : 'Error sending command'}
          </p>
        )}
      </div>

      {/* Reboot Inverter */}
      <div className="bg-red-900/20 border border-red-700/30 rounded-xl p-3 space-y-2">
        <p className="text-text-primary text-xs font-medium">⚠️  DANGER</p>
        <p className="text-text-secondary text-xs">
          This immediately reboots the inverter. The connection will drop and
          the inverter will be offline for 1-2 minutes while it restarts.
        </p>
        <button
          onClick={async () => {
            if (!confirm('⚠️  REBOOT INVERTER\n\nThis will restart the inverter immediately. The connection will drop for 1-2 minutes.\n\nContinue?')) return;
            setSaving(true);
            try {
              await apiPost('/api/control/reboot');
              setFeedback('saved');
            } catch {
              setFeedback('error');
            }
            setSaving(false);
            setTimeout(() => setFeedback(null), 3000);
          }}
          disabled={saving}
          className="w-full py-2 bg-red-500/20 text-text-primary rounded-lg text-xs font-medium hover:bg-red-500/30 transition disabled:opacity-40 border border-red-500/30"
        >
          Reboot Inverter
        </button>
      </div>
    </section>
  );
}

export default function ControlPage() {
  const { snapshot, developerMode } = useInverterStore();
  const modeAction = useAction();

  // Battery limits: local draft state while dragging, otherwise from snapshot
  const [draftReserve, setDraftReserve] = useState<number | null>(null);
  const [draftCharge, setDraftCharge] = useState<number | null>(null);
  const [draftDischarge, setDraftDischarge] = useState<number | null>(null);
  const [draftActivePower, setDraftActivePower] = useState<number | null>(null);
  type ChargeMode = 'standard' | 'cosy' | 'agile';
  const snapshotCosyEnabled = snapshot?.cosy_enabled ?? false;
  const [localChargeOverride, setLocalChargeOverride] = useState<ChargeMode | null>(null);
  // Use local override if user has interacted; otherwise derive from snapshot.
  // Snapshot only knows cosy_enabled (boolean), so 'agile' can only come from a local override.
  const chargeMode: ChargeMode = localChargeOverride ?? (snapshotCosyEnabled ? 'cosy' : 'standard');
  const cosyEnabled = chargeMode === 'cosy';
  const setChargeMode = (m: ChargeMode) => {
    setLocalChargeOverride(m);
    // Keep the backend cosy flag in sync — only 'cosy' mode has it enabled.
    const newCosyEnabled = m === 'cosy';
    // Fire-and-forget: update cosy_enabled via the API so the next poll
    // reflects it, but don't await (the local override takes immediate effect).
    fetch('/api/cosy', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ enabled: newCosyEnabled }),
    }).catch(() => {});
  };

  // On mount, load agile config from backend to seed the local override.
  useEffect(() => {
    (async () => {
      try {
        const res = await apiGet<{ ok: boolean; enabled: boolean }>('/api/agile');
        if (res.ok && res.enabled) {
          setLocalChargeOverride('agile');
        }
      } catch { /* use defaults */ }
    })();
  }, []);

  const currentMode = snapshot?.battery_mode ?? 'eco';
  const cosyActive = snapshot?.cosy_active ?? false;

  // Show draft while dragging; once snapshot confirms the saved value, use snapshot.
  // Default to null (no data) until the first snapshot arrives to avoid showing
  // misleading 100% values that then jump to real values.
  const reserveSoc = (draftReserve != null && snapshot?.battery_reserve !== draftReserve)
    ? Math.max(4, Math.min(100, draftReserve))
    : Math.max(4, Math.min(100, snapshot?.battery_reserve ?? 4));
  const isAcCoupled = snapshot?.device_type_code === '3001' || snapshot?.device_type_code === '3002';

  // ARM firmware version as integer (e.g. 318, 352, 449). Used for firmware-gating
  // the extended schedule block on Gen3 hybrids.
  const armFwNum = snapshot?.firmware_version != null && snapshot.firmware_version !== ''
    ? parseInt(snapshot.firmware_version, 10)
    : NaN;
  const isHybrid = snapshot?.device_type_code != null && snapshot.device_type_code.startsWith('20');

  // Slot ordering mismatch warning — see issue #41.
  // Our app uses the canonical register naming from the givenergy-modbus
  // reference library: Slot 1 = HR 94-95 (charge) / HR 56-57 (discharge),
  // Slot 2 = HR 31-32 (charge) / HR 44-45 (discharge). GE Cloud's UI appears
  // to label them the other way around (Cloud Slot 1 = HR 31-32 / 44-45).
  // Pure labelling difference — underlying schedule data is identical.
  // Only relevant for devices with 2+ slots: Gen1 hybrids and AC-coupled only
  // have one slot, so there's no Slot 1 vs Slot 2 ambiguity to warn about.
  const maxSlotsForWarning = snapshot?.max_charge_slots ?? 0;
  const showSlotOrderingWarning = isHybrid && maxSlotsForWarning >= 2;

  // Gen3 hybrid firmware gating — see givenergy-modbus reference library:
  // the extended schedule block (HR 240-299, used for per-slot target SOCs
  // and slots 3-10) is only supported on ARM firmware > 302. On older firmware
  // the dongle returns stale or garbage values, which can show up as phantom
  // schedules or wrong target SOCs. See issue #41.
  const isLegacyGen3Fw = isHybrid && !Number.isNaN(armFwNum) && armFwNum > 0 && armFwNum <= 302;
  const isThreePhaseLimitModel = snapshot?.device_type_code != null
    && (snapshot.device_type_code.startsWith('40')
      || snapshot.device_type_code.startsWith('41')
      || snapshot.device_type_code.startsWith('60')
      || snapshot.device_type_code.startsWith('81')
      || snapshot.device_type_code.startsWith('82'));
  // Three-phase-bank models use HR1113-1121 for charge/discharge schedules;
  // the backend now selects that register map automatically.
  const schedulesUnsupported = false;
  const usesDirectPowerLimit = isAcCoupled || isThreePhaseLimitModel;
  // DC-coupled hybrid registers HR111/112 are 0-50 and are displayed as 0-100%.
  // AC-coupled HR313/314 and three-phase HR1110/1108 are already 1-100%, so display directly.
  const rateRegisterMax = usesDirectPowerLimit ? 100 : 50;
  const rateDisplayMultiplier = usesDirectPowerLimit ? 1 : 2;
  const rateDisplayMin = usesDirectPowerLimit ? 1 : 0;
  const snapshotChargeRate = snapshot?.charge_rate != null
    ? Math.max(0, Math.min(rateRegisterMax, snapshot.charge_rate))
    : undefined;
  const snapshotDischargeRate = snapshot?.discharge_rate != null
    ? Math.max(0, Math.min(rateRegisterMax, snapshot.discharge_rate))
    : undefined;
  const chargeRate = (draftCharge != null && (snapshotChargeRate == null || snapshotChargeRate * rateDisplayMultiplier !== draftCharge))
    ? Math.max(rateDisplayMin, Math.min(100, draftCharge))
    : snapshotChargeRate != null ? snapshotChargeRate * rateDisplayMultiplier : undefined;
  const dischargeRate = (draftDischarge != null && (snapshotDischargeRate == null || snapshotDischargeRate * rateDisplayMultiplier !== draftDischarge))
    ? Math.max(rateDisplayMin, Math.min(100, draftDischarge))
    : snapshotDischargeRate != null ? snapshotDischargeRate * rateDisplayMultiplier : undefined;
  const activePowerRate = (draftActivePower != null && snapshot?.active_power_rate !== draftActivePower) ? draftActivePower : snapshot?.active_power_rate;
  const activePowerWatts = activePowerRate != null && snapshot?.max_ac_power_w
    ? Math.round(activePowerRate / 100 * snapshot.max_ac_power_w)
    : null;
  const activePowerKw = activePowerWatts != null
    ? `${Number((activePowerWatts / 1000).toFixed(1))}kW`
    : null;

  // DC hybrid HR111/112 are 0-50, using the GivTCP formula:
  // display_rate / 200 × battery_capacity. AC-coupled HR313/314 and three-phase
  // HR1110/1108 are direct 1-100% percentages of inverter battery power rating.
  const maxBatteryPowerW = snapshot?.max_battery_power_w ?? 0;
  const batteryCapacityW = (snapshot?.battery_capacity_kwh ?? 0) * 1000;
  const chargeWatts = chargeRate != null
    ? usesDirectPowerLimit
      ? Math.round(chargeRate / 100 * maxBatteryPowerW)
      : Math.min(Math.round(chargeRate / 200 * batteryCapacityW), maxBatteryPowerW)
    : null;
  const dischargeWatts = dischargeRate != null
    ? usesDirectPowerLimit
      ? Math.round(dischargeRate / 100 * maxBatteryPowerW)
      : Math.min(Math.round(dischargeRate / 200 * batteryCapacityW), maxBatteryPowerW)
    : null;

  // Derive force charge/discharge state from snapshot registers, with
  // local override so the toggle feels instant (doesn't wait for next poll).
  // Force charge is active when enable_charge (master schedule flag) is set
  // AND the current time falls within an active charge slot window. On
  // three-phase, enable_charge maps to HR 1123 (dedicated force-charge flag).
  // On single-phase/AC, it maps to HR 96 (schedule enable). In both cases,
  // the window gate is the definitive "charging now" signal.
  // For three-phase: enable_charge alone suffices (HR 1123 is the force-charge flag).
  const inChargeWindow = (snapshot?.charge_slots ?? []).some(slot => {
    if (!slot.enabled) return false;
    const now = new Date();
    const curMin = now.getHours() * 60 + now.getMinutes();
    const startMin = slot.start_hour * 60 + slot.start_minute;
    const endMin = slot.end_hour * 60 + slot.end_minute;
    return startMin < endMin
      ? curMin >= startMin && curMin < endMin
      : curMin >= startMin || curMin < endMin;
  });
  // Force charge is active only when the master charge-enable flag is set
  // AND the current time falls within an active charge slot window. Outside
  // the window the inverter is idle (eco or discharging), not force-charging.
  // This prevents the button staying highlighted when a charge schedule is
  // configured but no slot is active — enable_charge (HR 96 / HR 1123) is a
  // sticky schedule-enable flag, not an instantaneous "charging now" signal.
  const snapshotForceCharge = (snapshot?.enable_charge ?? false) && inChargeWindow;
  const [localForceChargeOverride, setLocalForceChargeOverride] = useState<boolean | null>(null);
  const forceChargeActive = localForceChargeOverride ?? snapshotForceCharge;
  const [forceChargeLoading, setForceChargeLoading] = useState(false);
  // Force discharge is active only when the schedule is enabled AND the
  // current time falls within an active discharge slot window. Outside the
  // window the inverter is idle (eco), not force-discharging. This prevents
  // the button staying highlighted during Timed Demand/Export when no slot
  // is active.
  const inDischargeWindow = (snapshot?.discharge_slots ?? []).some(slot => {
    if (!slot.enabled) return false;
    const now = new Date();
    const curMin = now.getHours() * 60 + now.getMinutes();
    const startMin = slot.start_hour * 60 + slot.start_minute;
    const endMin = slot.end_hour * 60 + slot.end_minute;
    return startMin < endMin
      ? curMin >= startMin && curMin < endMin
      : curMin >= startMin || curMin < endMin; // overnight slot
  });
  const snapshotForceDischarge = (snapshot?.enable_discharge ?? false) && inDischargeWindow;
  const [localForceDischargeOverride, setLocalForceDischargeOverride] = useState<boolean | null>(null);
  const forceDischargeActive = localForceDischargeOverride ?? snapshotForceDischarge;
  const [forceDischargeLoading, setForceDischargeLoading] = useState(false);
  const [reserveSaving, setReserveSaving] = useState(false);
  const [chargeRateSaving, setChargeRateSaving] = useState(false);
  const [dischargeRateSaving, setDischargeRateSaving] = useState(false);
  const [activePowerSaving, setActivePowerSaving] = useState(false);

  // Limit slots shown to what the inverter model supports
  // (e.g. AC Coupled only has 1 charge slot; Gen3 has 10).
  const maxChargeSlots = snapshot?.max_charge_slots ?? 2;
  const maxDischargeSlots = snapshot?.max_discharge_slots ?? 2;

  const chargeSlots: ScheduleSlot[] =
    snapshot?.charge_slots?.length != null && snapshot.charge_slots.length >= maxChargeSlots
      ? snapshot.charge_slots.slice(0, maxChargeSlots)
      : Array.from({ length: maxChargeSlots }, () => ({
        enabled: false, start_hour: 0, start_minute: 0, end_hour: 6, end_minute: 0, target_soc: 100,
      } as ScheduleSlot));

  const dischargeSlots: ScheduleSlot[] =
    snapshot?.discharge_slots?.length != null && snapshot.discharge_slots.length >= maxDischargeSlots
      ? snapshot.discharge_slots.slice(0, maxDischargeSlots)
      : Array.from({ length: maxDischargeSlots }, () => ({
        enabled: false, start_hour: 16, start_minute: 0, end_hour: 19, end_minute: 0, target_soc: 4,
      } as ScheduleSlot));

  const [requestedMode, setRequestedMode] = useState<BatteryMode | null>(null);

  // Clear requested mode after 30s timeout (safety net for unconfirmed writes).
  // The inverter confirming the change is handled by deriving effectiveMode below.
  useEffect(() => {
    if (!requestedMode) return;
    const timeout = setTimeout(() => setRequestedMode(null), 30_000);
    return () => clearTimeout(timeout);
  }, [requestedMode]);

  // Force-discharge local override auto-clear after 10s. Prevents stale
  // overrides from previous interactions or failed writes from sticking.
  useEffect(() => {
    if (localForceDischargeOverride == null) return;
    const timeout = setTimeout(() => setLocalForceDischargeOverride(null), 10_000);
    return () => clearTimeout(timeout);
  }, [localForceDischargeOverride]);

  // Force-charge local override auto-clear after 10s (same pattern).
  useEffect(() => {
    if (localForceChargeOverride == null) return;
    const timeout = setTimeout(() => setLocalForceChargeOverride(null), 10_000);
    return () => clearTimeout(timeout);
  }, [localForceChargeOverride]);

  // Use requested mode unless the inverter has already caught up
  const effectiveMode = (requestedMode && requestedMode !== currentMode)
    ? requestedMode
    : currentMode;

  const handleModeChange = async (mode: BatteryMode) => {
    setRequestedMode(mode);
    try {
      await modeAction.execute('/api/control/mode', { mode });
    } catch {
      setRequestedMode(null);
    }
  };

  const handleSlotSave = async (index: number, slot: ScheduleSlot, path: string) => {
    // API expects 1-based slot number
    await apiPost(path, {
      slot: index + 1,
      enabled: slot.enabled,
      start_hour: slot.start_hour,
      start_minute: slot.start_minute,
      end_hour: slot.end_hour,
      end_minute: slot.end_minute,
      target_soc: slot.target_soc,
    });
  };

  const handleReserveSave = async () => {
    setReserveSaving(true);
    try {
      await apiPost('/api/control/reserve', { soc: reserveSoc });
    } catch { /* handled silently */ }
    setReserveSaving(false);
  };

  const handleChargeRateSave = async () => {
    if (chargeRate == null) return;
    setChargeRateSaving(true);
    try {
      await apiPost('/api/control/charge-rate', { limit: Math.round(chargeRate / rateDisplayMultiplier) });
    } catch { /* handled silently */ }
    setChargeRateSaving(false);
  };

  const handleDischargeRateSave = async () => {
    if (dischargeRate == null) return;
    setDischargeRateSaving(true);
    try {
      await apiPost('/api/control/discharge-rate', { limit: Math.round(dischargeRate / rateDisplayMultiplier) });
    } catch { /* handled silently */ }
    setDischargeRateSaving(false);
  };

  const handleActivePowerSave = async () => {
    if (activePowerRate == null) return;
    setActivePowerSaving(true);
    try {
      await apiPost('/api/control/active-power-rate', { rate: activePowerRate });
    } catch { /* handled silently */ }
    setActivePowerSaving(false);
  };

  return (
    <div className="flex flex-col gap-6 max-w-2xl mx-auto px-4 py-6">
      {/* Section 1: Quick Actions */}
      <section className="space-y-3">
        <h2 className="text-text-primary font-semibold text-lg">Quick Actions</h2>
        <div className="grid grid-cols-4 gap-2 sm:gap-3">
          <div className="relative">
            <button
              onClick={async () => {
                setForceChargeLoading(true);
                try {
                  if (forceChargeActive) {
                    await apiPost('/api/control/pause');
                    setLocalForceChargeOverride(false);
                  } else {
                    await apiPost('/api/control/force-charge', { minutes: 30 });
                    setLocalForceChargeOverride(true);
                  }
                } catch { /* handled silently */ }
                setForceChargeLoading(false);
              }}
              disabled={forceChargeLoading}
              className={`w-full flex flex-col items-center gap-1 sm:gap-2 p-2 sm:p-4 rounded-xl border transition disabled:opacity-50 ${forceChargeActive
                ? 'bg-green-900/30 border-green-500/40 hover:bg-green-900/50'
                : 'bg-bg-surface border-transparent hover:border-battery/40 hover:bg-bg-elevated'
              }`}
            >
              <span className="text-xl sm:text-2xl">{forceChargeActive ? '⏹' : '☀️'}</span>
              <span className="text-text-primary text-xs sm:text-sm font-medium leading-tight text-center">
                {forceChargeActive ? 'Stop Charge' : 'Force Charge'}
              </span>
            </button>
            {forceChargeLoading && (
              <div className="absolute inset-0 flex items-center justify-center bg-bg-surface/80 rounded-xl">
                <div className="w-5 h-5 border-2 border-battery border-t-transparent rounded-full animate-spin" />
              </div>
            )}
          </div>
          <div className="relative">
            <button
              onClick={async () => {
                setForceDischargeLoading(true);
                try {
                  if (forceDischargeActive) {
                    await apiPost('/api/control/pause');
                    setLocalForceDischargeOverride(false);
                  } else {
                    await apiPost('/api/control/force-discharge');
                    setLocalForceDischargeOverride(true);
                  }
                } catch { /* handled silently */ }
                setForceDischargeLoading(false);
              }}
              disabled={forceDischargeLoading}
              className={`w-full flex flex-col items-center gap-1 sm:gap-2 p-2 sm:p-4 rounded-xl border transition disabled:opacity-50 ${forceDischargeActive
                ? 'bg-green-900/30 border-green-500/40 hover:bg-green-900/50'
                : 'bg-bg-surface border-transparent hover:border-battery/40 hover:bg-bg-elevated'
              }`}
            >
              <span className="text-xl sm:text-2xl">{forceDischargeActive ? '⏹' : '⚡'}</span>
              <span className="text-text-primary text-xs sm:text-sm font-medium leading-tight text-center">
                {forceDischargeActive ? 'Stop Discharge' : 'Force Discharge'}
              </span>
            </button>
            {forceDischargeLoading && (
              <div className="absolute inset-0 flex items-center justify-center bg-bg-surface/80 rounded-xl">
                <div className="w-5 h-5 border-2 border-battery border-t-transparent rounded-full animate-spin" />
              </div>
            )}
          </div>
          <ActionButton
            label="Pause Battery"
            icon="⏸️"
            path="/api/control/pause"
            body={{ minutes: 30 }}
          />
          <ActionButton
            label="Sync Clock"
            icon="🕐"
            path="/api/control/sync-clock"
          />
        </div>
      </section>


      {/* Section 2: Battery Mode */}
      <section className="space-y-3">
        <div className="flex items-center gap-3">
          <h2 className="text-text-primary font-semibold text-lg">Battery Mode</h2>
          {cosyActive && (
            <span className="text-xs text-battery font-semibold bg-battery/10 px-2 py-0.5 rounded-full flex items-center gap-1">
              <span className="inline-block w-1.5 h-1.5 bg-battery rounded-full animate-pulse" />
              Cosy Charging
            </span>
          )}
          <div className="flex rounded-lg border border-bg-elevated overflow-hidden">
            {([
              { key: 'eco' as ModeCategory, label: 'Eco' },
              { key: 'timed' as ModeCategory, label: 'Timed' },
            ] as const).map(({ key, label }) => (
              <button
                key={key}
                onClick={() => {
                  if (key === 'eco') handleModeChange('eco');
                  else handleModeChange('timed_demand');
                }}
                className={`px-4 py-1.5 text-xs font-medium transition flex items-center gap-1.5 ${modeToCategory(effectiveMode) === key
                    ? 'bg-battery/20 text-battery'
                    : 'text-text-secondary hover:bg-bg-surface'
                  }`}
              >
                {modeAction.loading && modeToCategory(requestedMode ?? currentMode) === key && (
                  <span className="inline-block w-3 h-3 border-2 border-current border-t-transparent rounded-full animate-spin" />
                )}
                {label}
              </button>
            ))}
          </div>
        </div>

        {/* Sub-mode buttons */}
        <div className="grid grid-cols-2 gap-2">
          {(modeToCategory(effectiveMode) === 'eco' ? ECO_MODES : TIMED_MODES).map(({ key, label, tooltip }) => {
            // timed_export from inverter maps to timed_demand button
            const displayMode = effectiveMode === 'timed_export' ? 'timed_demand' : effectiveMode;
            const isActive = displayMode === key;
            return (
              <button
                key={key}
                title={tooltip}
                onClick={() => handleModeChange(key)}
                disabled={modeAction.loading}
                className={`px-3 py-3 rounded-lg border text-xs font-medium transition w-full flex items-center justify-center gap-2 ${isActive
                    ? 'bg-battery/20 border-battery text-battery'
                    : 'bg-bg-surface border-transparent hover:border-battery/40 hover:bg-bg-bg-elevated text-text-secondary'
                  } disabled:opacity-50`}
              >
                {modeAction.loading && requestedMode === key && (
                  <span className="inline-block w-3 h-3 border-2 border-current border-t-transparent rounded-full animate-spin" />
                )}
                {label}
              </button>
            );
          })}
        </div>
        {modeAction.loading && (
          <p className="text-battery text-sm flex items-center gap-1.5">
            <span className="inline-block w-3 h-3 border-2 border-current border-t-transparent rounded-full animate-spin" />
            Sending command…
          </p>
        )}
        {requestedMode && !modeAction.loading && (
          <p className="text-amber-400 text-sm flex items-center gap-1.5">
            <span className="inline-block w-3 h-3 border-2 border-current border-t-transparent rounded-full animate-spin" />
            Settings are being applied — this may take up to 30 seconds
          </p>
        )}
        {modeAction.error && (
          <p className="text-red-400 text-sm">{modeAction.error}</p>
        )}
      </section>

      {/* Section 5: Charging Mode */}
      <CosyChargingSection mode={chargeMode} cosyActive={cosyActive} onModeChange={setChargeMode} />

      {/* Section 3: Charge Schedule */}
      {!cosyEnabled && chargeMode !== 'agile' && schedulesUnsupported && (
        <section className="space-y-3">
          <h2 className="text-text-primary font-semibold">Charge/Discharge Schedules</h2>
          <div className="rounded-xl border border-yellow-500/30 bg-yellow-500/10 p-3 text-sm text-text-primary">
            <div className="font-semibold mb-1">Schedules are hidden for this inverter model</div>
            This three-phase/HV inverter uses a different schedule register map
            (HR 1113-1121). Reading real-time data is supported,GivEnergy Cloud
            editing is disabled until those registers are implemented safely.
          </div>
        </section>
      )}

      {!cosyEnabled && chargeMode !== 'agile' && !schedulesUnsupported && <section className="space-y-3">
        <h2 className="text-text-primary font-semibold text-lg">Charge Schedule</h2>
        <p className="text-text-secondary/60 text-xs">Please Allow upto 30 Seconds for Changes to Save</p>
        <div className="space-y-3">
          {chargeSlots.map((slot, i) => (
            <>
              {i === 1 && (showSlotOrderingWarning || isLegacyGen3Fw) && (
                <div key="slot-warn-charge" className="space-y-2">
                  {showSlotOrderingWarning && (
                    <div className="rounded-xl border border-yellow-500/30 bg-yellow-500/10 p-3 text-xs text-text-primary">
                      <div className="font-semibold mb-1">
                        Slot labels differ from the GivEnergy cloud
                      </div>
                      Our app uses the canonical Modbus register layout from the{' '}
                      <code>givenergy-modbus</code> reference library, which labels
                      charge slots in the opposite order to the GivEnergy cloud UI:
                      our <strong>Slot 1</strong> is the cloud&apos;s <strong>Slot 2</strong>{' '}
                      (registers HR 94-95) and vice versa (registers HR 31-32). The
                      underlying schedule data is identical — only the labels differ.
                      If your schedule appears in a different slot than you expected,
                      this is why.
                    </div>
                  )}
                  {isLegacyGen3Fw && (
                    <div className="rounded-xl border border-yellow-500/30 bg-yellow-500/10 p-3 text-xs text-text-primary">
                      <div className="font-semibold mb-1">
                        Older Gen3 firmware detected (ARM FW {snapshot?.firmware_version})
                      </div>
                      Slot 2 and beyond (and per-slot target SOCs) come from extended
                      registers (HR 240-299) that your inverter firmware does not fully
                      support. Values shown here may be stale or incorrect. GivEnergy&apos;s
                      own cloud UI generally hides these slots on this firmware. Updating
                      your inverter firmware to version 303 or later (if available) will
                      resolve this.
                    </div>
                  )}
                </div>
              )}
              <ScheduleSlotEditor
                key={`charge-${i}-${slot.enabled}-${slot.start_hour}:${slot.start_minute}-${slot.end_hour}:${slot.end_minute}-${slot.target_soc}`}
                slotIndex={i}
                slot={slot}
                onSave={handleSlotSave}
                showTargetSoc
                apiPath="/api/control/charge-slot"
              />
            </>
          ))}
        </div>
      </section>}

      {/* Section 4: Discharge Schedule — only editable in timed mode */}
      {!cosyEnabled && chargeMode !== 'agile' && !schedulesUnsupported && modeToCategory(effectiveMode) === 'timed' && (
        <section className="space-y-3">
          <h2 className="text-text-primary font-semibold text-lg">Discharge Schedule</h2>
          <p className="text-text-secondary/60 text-xs">Please Allow upto 30 Seconds for Changes to Save</p>
          <div className="space-y-3">
            {dischargeSlots.map((slot, i) => (
              <>
                {i === 1 && (showSlotOrderingWarning || isLegacyGen3Fw) && (
                  <div key="slot-warn-discharge" className="space-y-2">
                    {showSlotOrderingWarning && (
                      <div className="rounded-xl border border-yellow-500/30 bg-yellow-500/10 p-3 text-xs text-text-primary">
                        <div className="font-semibold mb-1">
                          Slot labels differ from the GivEnergy cloud
                        </div>
                        Our app uses the canonical Modbus register layout from the{' '}
                        <code>givenergy-modbus</code> reference library, which labels
                        discharge slots in the opposite order to the GivEnergy cloud UI:
                        our <strong>Slot 1</strong> is the cloud&apos;s <strong>Slot 2</strong>{' '}
                        (registers HR 56-57) and vice versa (registers HR 44-45). The
                        underlying schedule data is identical — only the labels differ.
                      </div>
                    )}
                    {isLegacyGen3Fw && (
                      <div className="rounded-xl border border-yellow-500/30 bg-yellow-500/10 p-3 text-xs text-text-primary">
                        <div className="font-semibold mb-1">
                          Older Gen3 firmware detected (ARM FW {snapshot?.firmware_version})
                        </div>
                        Slot 2 and beyond come from extended registers (HR 240-299) that
                        your inverter firmware does not fully support. Values shown here
                        may be stale or incorrect.
                      </div>
                    )}
                  </div>
                )}
                <ScheduleSlotEditor
                  key={`discharge-${i}-${slot.enabled}-${slot.start_hour}:${slot.start_minute}-${slot.end_hour}:${slot.end_minute}-${slot.target_soc}`}
                  slotIndex={i}
                  slot={slot}
                  onSave={handleSlotSave}
                  showTargetSoc={true}
                  apiPath="/api/control/discharge-slot"
                />
              </>
            ))}
          </div>
        </section>
      )}

      {/* Section 5: Auto Winter Mode */}
      <AutoWinterSection />
      {/* Section 6: Battery & Power Limits */}
      <section className="space-y-3">
        <h2 className="text-text-primary font-semibold text-lg">Battery & Power Limits</h2>
        <div className="bg-bg-surface rounded-xl p-4 space-y-5">
          {/* Reserve SOC */}
          <div className="space-y-2">
            <div className="flex items-center justify-between">
              <span className="text-text-secondary text-sm">Minimum SOC</span>
              <span className="font-mono text-text-primary text-sm">{reserveSoc}%</span>
            </div>
            <div className="flex items-center gap-3">
              <input
                type="range"
                min={4}
                max={100}
                step={1}
                value={reserveSoc}
                onChange={(e) => setDraftReserve(Math.max(4, Number(e.target.value)))}
                className="flex-1"
              />
              <button
                onClick={handleReserveSave}
                disabled={reserveSaving}
                className="px-3 py-1.5 bg-battery/20 text-battery rounded-lg text-xs font-medium hover:bg-battery/30 transition disabled:opacity-50"
              >
                {reserveSaving ? '...' : 'Save'}
              </button>
            </div>
          </div>

          {/* Charge Power Limit */}
          <div className="space-y-1">
            <div className="flex items-center justify-between">
              <span className="text-text-secondary text-sm">{isThreePhaseLimitModel ? 'Three-phase Charge Power Limit' : isAcCoupled ? 'AC Charge Power Limit' : 'Battery Charge Power Limit'}</span>
              <span className="font-mono text-text-primary text-sm">{chargeRate ?? '—'}%{chargeWatts != null && chargeWatts > 0 ? ` (${(chargeWatts / 1000).toFixed(1)} kW)` : ''}</span>
            </div>
            <div className="flex items-center gap-3">
              <input
                type="range"
                min={rateDisplayMin}
                max={100}
                step={1}
                value={chargeRate ?? 100}
                onChange={(e) => setDraftCharge(Math.max(rateDisplayMin, Math.min(100, Number(e.target.value))))}
                className="flex-1"
              />
              <button
                onClick={handleChargeRateSave}
                disabled={chargeRateSaving}
                className="px-3 py-1.5 bg-battery/20 text-battery rounded-lg text-xs font-medium hover:bg-battery/30 transition disabled:opacity-50"
              >
                {chargeRateSaving ? '...' : 'Save'}
              </button>
            </div>
          </div>

          {/* Discharge Power Limit */}
          <div className="space-y-1">
            <div className="flex items-center justify-between">
              <span className="text-text-secondary text-sm">{isThreePhaseLimitModel ? 'Three-phase Discharge Power Limit' : isAcCoupled ? 'AC Discharge Power Limit' : 'Battery Discharge Power Limit'}</span>
              <span className="font-mono text-text-primary text-sm">{dischargeRate ?? '—'}%{dischargeWatts != null && dischargeWatts > 0 ? ` (${(dischargeWatts / 1000).toFixed(1)} kW)` : ''}</span>
            </div>
            <div className="flex items-center gap-3">
              <input
                type="range"
                min={rateDisplayMin}
                max={100}
                step={1}
                value={dischargeRate ?? 100}
                onChange={(e) => setDraftDischarge(Math.max(rateDisplayMin, Math.min(100, Number(e.target.value))))}
                className="flex-1"
              />
              <button
                onClick={handleDischargeRateSave}
                disabled={dischargeRateSaving}
                className="px-3 py-1.5 bg-battery/20 text-battery rounded-lg text-xs font-medium hover:bg-battery/30 transition disabled:opacity-50"
              >
                {dischargeRateSaving ? '...' : 'Save'}
              </button>
            </div>
          </div>

          {/* Inverter Active Power Limit */}
          <div className="space-y-1">
            <div className="flex items-center justify-between">
              <span className="text-text-secondary text-sm">Inverter Active Power Limit</span>
              <span className="font-mono text-text-primary text-sm">{activePowerRate ?? '—'}%{activePowerKw != null && activePowerWatts != null && activePowerWatts > 0 ? `(${activePowerKw})` : ''}</span>
            </div>
            <div className="flex items-center gap-3">
              <input
                type="range"
                min={0}
                max={100}
                step={1}
                value={activePowerRate ?? 100}
                onChange={(e) => setDraftActivePower(Number(e.target.value))}
                className="flex-1"
              />
              <button
                onClick={handleActivePowerSave}
                disabled={activePowerSaving}
                className="px-3 py-1.5 bg-battery/20 text-battery rounded-lg text-xs font-medium hover:bg-battery/30 transition disabled:opacity-50"
              >
                {activePowerSaving ? '...' : 'Save'}
              </button>
            </div>
          </div>
        </div>
        {/* Battery Calibration (dev mode only) */}
        {developerMode && <BatteryCalibrationSection />}
      </section>
    </div>
  );
}
