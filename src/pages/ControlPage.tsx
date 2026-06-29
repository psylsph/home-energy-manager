import { useState, useCallback, useEffect, useRef } from 'react';
import { useInverterStore } from '../store/useInverterStore';
import { useAction } from '../hooks/useAction';
import { apiPost, apiGet } from '../lib/api';
import { deviceSupportsEps, deviceSupportsTimedDischarge } from '../lib/deviceCapabilities';
import type { ScheduleSlot } from '../lib/types';

/**
 * Front-end charging-mode dropdown values. Maps 1:1 to the backend
 * `AgileScope` enum but adds `'standard'` (which the backend models as
 * `AgileScope::Off` plus `cosy_enabled = false`) and `'cosy'` (which
 * the backend models as `cosy_enabled = true` regardless of scope).
 */
type ChargeMode = 'standard' | 'cosy' | 'agile' | 'agile_charge' | 'agile_discharge';

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
  masterArmed = true,
  /**
   * Visual treatment:
   *   - 'normal'        — fully editable, default.
   *   - 'agile_readonly' — greyed-out (existing `notArmed` opacity-60)
   *                        PLUS an explicit "Controlled by manual timer"
   *                        label so the user understands why the slot is
   *                        locked when an Agile sub-mode is active.
   * The schedule itself remains editable in `agile_readonly` mode so
   * the user can pre-configure their manual schedule; the grey +
   * label just signals "Agile has taken over the slot, so changes
   * here won't affect the inverter while Agile is on".
   */
  mode = 'normal',
}: {
  slotIndex: number;
  slot: ScheduleSlot;
  onSave: (index: number, slot: ScheduleSlot, path: string) => void;
  showTargetSoc: boolean;
  apiPath?: string;
  /** Whether the schedule's master enable flag (e.g. enable_charge / HR 96)
   *  is ON. A slot can hold configured times in its registers while the
   *  master flag is OFF — the inverter ignores the window, but the slot is
   *  still "configured". Defaults to `true` so callsites that don't pass it
   *  (currently the discharge schedule, whose Eco/Timed arming model is
   *  different) keep the original always-armed rendering. */
  masterArmed?: boolean;
  mode?: 'normal' | 'agile_readonly';
}) {
  const [local, setLocal] = useState<ScheduleSlot>({ ...slot });
  const [saving, setSaving] = useState(false);
  const [feedback, setFeedback] = useState<'saved' | 'error' | null>(null);
  // `dirty` is true when `local` differs from the last-saved (or just-loaded)
  // slot. We deep-compare the six scalar fields the user can change.
  // `baseline` lives in state (not a ref) so it's safe to consume in the
  // same render that derives `dirty` from it.
  const [baseline, setBaseline] = useState<ScheduleSlot>(slot);
  const isDirty = (s: ScheduleSlot) =>
    s.start_hour !== baseline.start_hour ||
    s.start_minute !== baseline.start_minute ||
    s.end_hour !== baseline.end_hour ||
    s.end_minute !== baseline.end_minute ||
    s.enabled !== baseline.enabled ||
    s.target_soc !== baseline.target_soc;
  // Track the slot prop reference so we can re-baseline when the parent
  // hands us a fresh slot (e.g. after a re-fetch of the snapshot, or after
  // a sibling save) — but only if the user isn't in the middle of an
  // edit, otherwise their typing would be wiped out. This is React's
  // documented "derive state from props" pattern: setState during render
  // schedules a re-render, never an infinite loop, because the next render
  // sees the same `slot` and skips the update.
  const [prevSlot, setPrevSlot] = useState(slot);
  if (slot !== prevSlot) {
    setPrevSlot(slot);
    if (!isDirty(local)) {
      setBaseline(slot);
    }
  }

  // A slot is "configured but not armed" when it holds times (enabled) but
  // the schedule's master enable flag is off. The slot times live in their
  // own registers (e.g. HR 94/95) independent of the master flag (HR 96),
  // so this state is real and common — leftover/factory windows show up
  // here. We keep the slot visible (issue #41: never hide a configured slot
  // just because the master flag is off) and dim it.
  const notArmed = local.enabled && !masterArmed;

  const handleSave = async () => {
    setSaving(true);
    setFeedback(null);
    try {
      await onSave(slotIndex, local, apiPath);
      setBaseline({ ...local });
      setFeedback('saved');
    } catch {
      setFeedback('error');
    }
    setSaving(false);
    setTimeout(() => setFeedback(null), 2000);
  };

  return (
    <div
      className={`bg-bg-surface rounded-xl p-3 space-y-2 transition ${notArmed ? 'opacity-60' : ''
        }`}
    >
      {mode === 'agile_readonly' && (
        // Explain the dim styling. This label appears above the slot
        // body, distinct from the existing "configured but not armed"
        // opacity-60 rendering, so the user can tell why their slot
        // looks dim while an Agile sub-mode is active.
        <div className="text-[11px] text-text-secondary/80 italic border-l-2 border-battery/40 pl-2">
          Controlled by manual timer — not changed by Agile.
        </div>
      )}
      <div className="flex items-center justify-between">
        <span className="text-text-primary text-sm font-medium">Slot {slotIndex + 1}</span>
        <button
          onClick={() => setLocal((l) => ({ ...l, enabled: !l.enabled }))}
          aria-pressed={local.enabled}
          aria-label={`Slot ${slotIndex + 1} ${local.enabled ? 'enabled' : 'disabled'}`}
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
          setTargetSoc(Math.max(4, res.data.config.target_soc));
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
function CosyChargingSection({ mode, cosyActive, onModeChange }: { mode: ChargeMode; cosyActive: boolean; onModeChange: (m: ChargeMode) => void }) {
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

  const handleModeChange = async (newMode: ChargeMode) => {
    // Don't switch until slots have loaded
    if (!loaded || slots.length === 0) return;
    const cosyEnabled = newMode === 'cosy';
    // Translate the new 5-value scope into the backend's wire format.
    // The backend accepts `{ scope }` (new) or `{ enabled }` (legacy).
    // We always send scope so the front-end's intent is explicit.
    const scopeValue: 'off' | 'full' | 'charge_only' | 'discharge_only' =
      newMode === 'standard' ? 'off' :
      newMode === 'agile' ? 'full' :
      newMode === 'agile_charge' ? 'charge_only' :
      'discharge_only';
    onModeChange(newMode);
    setSaving(true);
    try {
      await Promise.all([
        apiPost('/api/cosy', { enabled: cosyEnabled, slots }),
        apiPost('/api/agile', { scope: scopeValue }),
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
          <select
          value={mode}
          onChange={(e) => handleModeChange(e.target.value as ChargeMode)}
          disabled={saving}
          className="bg-bg-elevated text-text-primary font-mono text-sm rounded-lg px-3 py-1.5 border border-transparent focus:border-battery outline-none cursor-pointer"
        >
          <option value="standard">Standard</option>
          <option value="cosy">Cosy</option>
          <optgroup label="Agile">
            <option value="agile">Agile (full)</option>
            <option value="agile_charge">Agile — Charge only</option>
            <option value="agile_discharge">Agile — Discharge only</option>
          </optgroup>
        </select>
          <button
            onClick={async () => {
              setSaving(true);
              try {
                const scopeValue: 'off' | 'full' | 'charge_only' | 'discharge_only' =
                  mode === 'standard' ? 'off' :
                  mode === 'agile' ? 'full' :
                  mode === 'agile_charge' ? 'charge_only' :
                  'discharge_only';
                await Promise.all([
                  apiPost('/api/cosy', { enabled: mode === 'cosy', slots }),
                  apiPost('/api/agile', { scope: scopeValue }),
                ]);
                setSaveFeedback('saved');
              } catch {
                setSaveFeedback('error');
              }
              setSaving(false);
              setTimeout(() => setSaveFeedback(null), 2000);
            }}
            disabled={saving}
            className="text-sm font-medium px-4 py-1.5 rounded-lg bg-battery/20 text-battery hover:bg-battery/30 transition disabled:opacity-50"
          >
            {saveFeedback === 'saved' ? '✓' : saveFeedback === 'error' ? '!' : saving ? '...' : 'Apply'}
          </button>
      </div>
      </div>

      {(mode === 'cosy' || mode === 'agile' || mode === 'agile_charge' || mode === 'agile_discharge') && (
        <div className="rounded-xl border border-yellow-500/30 bg-yellow-500/10 p-2.5 space-y-1">
          <div className="flex items-center gap-1.5">
            <span className="text-[10px] font-bold text-text-primary uppercase tracking-wide">Beta</span>
            <span className="text-[11px] text-text-primary font-medium">App must be kept running</span>
          </div>
          <p className="text-[11px] text-text-secondary leading-relaxed">
            {mode === 'cosy'
              ? 'Cosy mode schedules force-charging based on time slots you define. The app must stay running for slot entry and exit to work — if you close it mid-slot, the inverter stays in force-charge mode until you reopen the app or stop it manually.'
              : mode === 'agile'
                ? 'Agile mode automatically charges and discharges based on live Octopus prices. The app must stay running for price checks and switching to work — if you close it, the inverter stays in whatever mode it was last set to.'
                : mode === 'agile_charge'
                  ? 'Agile — Charge only drives the cheap-side charge slot from prices, while your Discharge Schedule and Timed Discharge keep full control of the discharge side. The app must stay running for price checks to keep the charge slot current.'
                  : 'Agile — Discharge only drives the expensive-side discharge slot from prices, while your Charge Schedule keeps full control of the charge side. The app must stay running for price checks to keep the discharge slot current.'
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

      {(mode === 'agile' || mode === 'agile_charge' || mode === 'agile_discharge') && (
        <AgileControls
          scope={
            mode === 'agile_charge'
              ? 'charge_only'
              : mode === 'agile_discharge'
                ? 'discharge_only'
                : 'full'
          }
        />
      )}
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

/** Agile Octopus controls — shown when any Agile charging mode is selected. */
function AgileControls({ scope }: { scope: 'full' | 'charge_only' | 'discharge_only' }) {
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
    // Enforce a 5p minimum gap between charge and discharge thresholds.
    // An inverted or overlapping pair (e.g. charge at 30p, discharge
    // at 25p) means the inverter would never charge — the price is
    // always >= the discharge threshold so the slot would always be
    // discharge-shaped, and vice versa. Clamp on save rather than
    // rejecting so the user can still adjust one slider at a time.
    const MIN_GAP = 5;
    let safeDischarge = dischargeThreshold;
    if (safeDischarge - chargeThreshold < MIN_GAP) {
      if (chargeThreshold > dischargeThreshold) {
        // User dragged the charge slider above discharge: bump discharge up.
        safeDischarge = chargeThreshold + MIN_GAP;
      } else {
        // User dragged discharge down too close: bump discharge up.
        safeDischarge = chargeThreshold + MIN_GAP;
      }
      setDischargeThreshold(safeDischarge);
    }
    setSaving(true);
    setSaveFeedback(null);
    try {
      await apiPost('/api/agile', {
        enabled: true,
        region,
        charge_threshold: chargeThreshold,
        discharge_threshold: safeDischarge,
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

      {/* Charge threshold — hidden in Discharge Only mode (user's charge
          schedule owns charging). Always rendered in Full and Charge
          Only modes. The hidden threshold's value is preserved in
          state so flipping back to Full restores it. */}
      {scope !== 'discharge_only' && (
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
          Charge the battery from the grid during any contiguous run of cheap half-hour slots at or below this price. Eco mode.
        </p>
      </div>
      )}
      {scope === 'discharge_only' && (
        // Hint that the threshold is set but hidden because this mode
        // ignores it. Prevents the user thinking their setting has been
        // wiped when they switch modes.
        <div className="text-[11px] text-text-secondary/80 italic px-1">
          Charge threshold: {chargeThreshold}p/kWh (set, hidden because Discharge Only mode ignores charging).
        </div>
      )}

      {/* Discharge threshold — hidden in Charge Only mode (user's
          discharge schedule owns discharging). Always rendered in Full
          and Discharge Only modes. */}
      {scope !== 'charge_only' && (
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
          Discharge the battery to the grid at full power during any contiguous run of expensive half-hour slots at or above this price. Export mode.
        </p>
      </div>
      )}
      {scope === 'charge_only' && (
        <div className="text-[11px] text-text-secondary/80 italic px-1">
          Discharge threshold: {dischargeThreshold}p/kWh (set, hidden because Charge Only mode ignores discharging).
        </div>
      )}

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

/** Battery calibration section — shown when battery BMS firmware indicates Gen1/Gen2 battery. */
function BatteryCalibrationSection() {
  const { snapshot } = useInverterStore();
  const supported = snapshot?.supports_battery_calibration ?? false;
  const stage = snapshot?.battery_calibration_stage ?? 0;
  const [saving, setSaving] = useState(false);
  const [feedback, setFeedback] = useState<'saved' | 'error' | null>(null);

  // Hide when battery auto-calibrates (Gen3+ BMS firmware >= 3000) or no battery present.
  if (!supported) return null;

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
    <div className="space-y-3">
      <h3 className="text-text-primary font-medium text-base">Battery Calibration</h3>

      <div className="bg-amber-900/20 border border-amber-700/30 rounded-xl p-3 space-y-3">
        <p className="text-text-primary text-xs font-medium">⚠️  WARNING</p>
        <p className="text-text-secondary text-xs">
          Calibration cycles the battery through: discharge → calibrate lower
          limit → charge → balance → calibrate upper limit. Once started, the
          process cannot be cancelled — it must run to completion.
          This can take several hours. Only use if you understand the risks.
        </p>
        <p className="text-amber-300/70 text-xs">
          ℹ️  This control is for Gen1/Gen2 batteries only (BMS firmware &lt; 3000).
          Gen3+ batteries auto-calibrate via BMS OCV and do not need manual calibration.
        </p>

        <div className="flex items-center gap-3 pt-3 border-t border-amber-700/20">
          <button
            onClick={handleStartCalibration}
            disabled={saving || isActive}
            className="flex-1 py-1.5 bg-amber-500/20 text-text-primary rounded-lg text-xs font-medium hover:bg-amber-500/30 transition disabled:opacity-40 border border-amber-500/30"
          >
            {saving ? 'Sending...' : feedback === 'saved' ? '✓ Sent' : feedback === 'error' ? '✗ Error' : 'Start Calibration'}
          </button>
          <span className="text-text-secondary text-xs shrink-0">
            Stage: <span className={`font-mono ${isActive ? 'text-amber-400' : 'text-text-primary'}`}>
              {stageLabels[stage] || `Unknown (${stage})`}
            </span>
          </span>
        </div>
      </div>
    </div>
  );
}

/** Reboot inverter button — developer mode only. */
function RebootInverterSection() {
  const [saving, setSaving] = useState(false);
  const [feedback, setFeedback] = useState<'saved' | 'error' | null>(null);

  const handleReboot = async () => {
    if (!confirm('⚠️  REBOOT INVERTER\n\nThis will restart the inverter immediately. The connection will drop for 1-2 minutes.\n\nContinue?')) return;
    setSaving(true);
    setFeedback(null);
    try {
      await apiPost('/api/control/reboot');
      setFeedback('saved');
    } catch {
      setFeedback('error');
    }
    setSaving(false);
    setTimeout(() => setFeedback(null), 3000);
  };

  return (
    <div className="space-y-3">
      <h3 className="text-text-primary font-medium text-base">Inverter Reboot</h3>
      <div className="bg-red-900/20 border border-red-700/30 rounded-xl p-3 space-y-2">
        <p className="text-text-primary text-xs font-medium">⚠️  DANGER</p>
        <p className="text-text-secondary text-xs">
          This immediately reboots the inverter. The connection will drop and
          the inverter will be offline for 1-2 minutes while it restarts.
        </p>
        <button
          onClick={handleReboot}
          disabled={saving}
          className="w-full py-2 bg-red-500/20 text-text-primary rounded-lg text-xs font-medium hover:bg-red-500/30 transition disabled:opacity-40 border border-red-500/30"
        >
          {saving ? 'Sending...' : feedback === 'saved' ? '✓ Sent' : feedback === 'error' ? '✗ Error' : 'Reboot Inverter'}
        </button>
      </div>
    </div>
  );
}

// Type for the load limiter config from the API.
interface LoadLimiterConfig {
  enabled: boolean;
  threshold_w: number;
  trigger_delay_minutes: number;
  start_hour: number;
  start_minute: number;
  end_hour: number;
  end_minute: number;
}

/**
 * Load Discharge Limiter — developer mode only.
 *
 * Monitors home power consumption. When it exceeds a user-defined threshold
 * for a sustained period within an activation window, pauses battery
 * discharge (Eco Paused). When the load drops below the threshold for the
 * same period, restores Eco mode. Only operates when the battery is in Eco
 * mode and no other automated feature (auto-winter, Cosy, Agile) is active.
 */
function LoadLimiterSection() {
  const { snapshot } = useInverterStore();
  const [enabled, setEnabled] = useState(false);
  const [thresholdW, setThresholdW] = useState(3000);
  const [triggerDelay, setTriggerDelay] = useState(5);
  const [startHour, setStartHour] = useState(0);
  const [startMinute, setStartMinute] = useState(0);
  const [endHour, setEndHour] = useState(0);
  const [endMinute, setEndMinute] = useState(0);
  const [saving, setSaving] = useState(false);
  const [saveFeedback, setSaveFeedback] = useState<'saved' | 'error' | null>(null);

  useEffect(() => {
    (async () => {
      try {
        const res = await apiGet<{ ok: boolean; data: { config: LoadLimiterConfig } }>('/api/load-limiter');
        if (res.ok) {
          const cfg = res.data.config;
          setEnabled(cfg.enabled);
          setThresholdW(cfg.threshold_w);
          setTriggerDelay(cfg.trigger_delay_minutes);
          setStartHour(cfg.start_hour);
          setStartMinute(cfg.start_minute);
          setEndHour(cfg.end_hour);
          setEndMinute(cfg.end_minute);
        }
      } catch { /* use defaults */ }
    })();
  }, []);

  const handleSave = async () => {
    setSaving(true);
    setSaveFeedback(null);
    try {
      await apiPost('/api/load-limiter', {
        enabled,
        threshold_w: thresholdW,
        trigger_delay_minutes: triggerDelay,
        start_hour: startHour,
        start_minute: startMinute,
        end_hour: endHour,
        end_minute: endMinute,
      });
      setSaveFeedback('saved');
    } catch {
      setSaveFeedback('error');
    }
    setSaving(false);
    setTimeout(() => setSaveFeedback(null), 2000);
  };

  // The limiter operates in Eco and EcoPaused. EcoPaused is the mode the
  // limiter itself sets (HR 110 = 100%) when it pauses discharge, so the
  // GUI must keep showing the live state once the mode has flipped —
  // issue #158: previously gated on `mode === 'eco'`, the moment the
  // limiter did its job the GUI flipped back to "Idle" even though
  // `load_limiter_active` was still true.
  const mode = snapshot?.battery_mode;
  const isLimiterOperating = mode === 'eco' || mode === 'eco_paused';
  const homePower = snapshot?.home_power ?? 0;
  const loadLimiterActive = snapshot?.load_limiter_active ?? false;

  // Derive human-readable state. The primary signal is `load_limiter_active`:
  // while the limiter is in its Paused/PausedFromRestart state, the GUI
  // reflects that. The `enabled && isLimiterOperating` gate is only
  // consulted for the "Monitoring…" state, which means the limiter is
  // armed and watching for the next threshold crossing.
  let stateLabel = 'Idle';
  if (loadLimiterActive) {
    const belowThreshold = homePower <= thresholdW;
    stateLabel = belowThreshold ? 'Recovering…' : 'Paused';
  } else if (enabled && isLimiterOperating && homePower > thresholdW && homePower !== 0) {
    stateLabel = 'Monitoring…';
  }

  return (
    <div className="space-y-3">
      <h2 className="text-text-primary font-semibold text-lg">Load Discharge Limiter</h2>

      {/* Only warn when the mode is something the limiter can't run in
          (Timed, Export). When the limiter is the reason we're in EcoPaused,
          hide the banner — "load limiter only operates in Eco (current:
          eco paused)" is misleading while the limiter is actively holding
          the battery in pause. */}
      {snapshot != null && mode !== 'eco' && mode !== 'eco_paused' && (
        <div className="rounded-xl border border-yellow-500/30 bg-yellow-500/10 p-2.5 text-xs text-text-primary">
          Load limiter only operates in <strong>Eco</strong> mode (current:{' '}
          {snapshot.battery_mode.replace('_', ' ')})
        </div>
      )}

      {/* Master toggle */}
      <div className="bg-bg-surface rounded-xl p-4 space-y-4">
        <div className="flex items-center justify-between">
          <div>
            <span className="text-text-primary text-sm font-medium">Enable</span>
            <p className="text-text-secondary text-xs mt-0.5">
              Automatically pause battery discharge when home load is high
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
            {/* Status indicator */}
            {snapshot != null && (
              <div className="flex items-center justify-between text-xs">
                <span className="text-text-secondary">State</span>
                <span className={`font-mono font-medium ${loadLimiterActive ? 'text-amber-400' : 'text-battery'}`}>
                  {stateLabel}
                </span>
              </div>
            )}
            {snapshot != null && (
              <div className="flex items-center justify-between text-xs">
                <span className="text-text-secondary">Home Power</span>
                <span className="font-mono text-text-primary">
                  {homePower >= 1000 ? (homePower / 1000).toFixed(1) : homePower}{' '}
                  {homePower >= 1000 ? 'kW' : 'W'}
                  <span className="text-text-secondary/50 ml-1">/ {thresholdW >= 1000 ? (thresholdW / 1000).toFixed(1) : thresholdW}{thresholdW >= 1000 ? 'kW' : 'W'}</span>
                </span>
              </div>
            )}

            {/* Load threshold */}
            <div className="space-y-1">
              <div className="flex items-center justify-between">
                <span className="text-text-secondary text-sm">Load Threshold</span>
                <span className="font-mono text-text-primary text-sm">
                  {thresholdW >= 1000 ? (thresholdW / 1000).toFixed(1) : thresholdW}
                  {thresholdW >= 1000 ? ' kW' : ' W'}
                </span>
              </div>
              <input
                type="range"
                min={500}
                max={15000}
                step={100}
                value={thresholdW}
                onChange={(e) => setThresholdW(Number(e.target.value))}
                className="w-full"
              />
              <p className="text-text-secondary text-xs">
                Pause discharge when home power stays above this for the trigger delay
              </p>
            </div>

            {/* Trigger delay */}
            <div className="space-y-1">
              <div className="flex items-center justify-between">
                <span className="text-text-secondary text-sm">Trigger Delay</span>
                <span className="font-mono text-text-primary text-sm">{triggerDelay} min</span>
              </div>
              <input
                type="range"
                min={1}
                max={60}
                step={1}
                value={triggerDelay}
                onChange={(e) => setTriggerDelay(Number(e.target.value))}
                className="w-full"
              />
              <p className="text-text-secondary text-xs">
                How long the load must stay over/under the threshold before acting
              </p>
            </div>

            {/* Activation window */}
            <div className="space-y-1.5">
              <span className="text-text-secondary text-sm">Activation Window</span>
              <p className="text-text-secondary text-xs">
                00:00 to 00:00 means always active
              </p>
              <div className="flex flex-col sm:flex-row items-center gap-4 sm:gap-6">
                <div className="flex items-center gap-1.5">
                  <span className="text-text-secondary text-sm shrink-0">Start</span>
                  <TimePicker
                    hour={startHour}
                    minute={startMinute}
                    onChange={(h, m) => { setStartHour(h); setStartMinute(m); }}
                  />
                </div>
                <div className="flex items-center gap-1.5">
                  <span className="text-text-secondary text-sm shrink-0">End</span>
                  <TimePicker
                    hour={endHour}
                    minute={endMinute}
                    onChange={(h, m) => { setEndHour(h); setEndMinute(m); }}
                  />
                </div>
              </div>
            </div>

            {/* App-must-stay-running note */}
            <div className="text-xs bg-blue-900/30 text-text-primary px-3 py-2 rounded-lg">
              <strong>Note:</strong> This is implemented locally within this app. It monitors
              home power via Modbus and pauses battery discharge when the load is high.
              The app must stay running for this to work.
            </div>
          </>
        )}

        <button
          onClick={handleSave}
          disabled={saving}
          className="w-full py-2 bg-battery/20 text-battery rounded-lg text-sm font-medium hover:bg-battery/30 transition disabled:opacity-50"
        >
          {saving ? 'Saving...' : saveFeedback === 'saved' ? '✓ Saved' : saveFeedback === 'error' ? '✗ Error' : 'Save'}
        </button>
      </div>
    </div>
  );
}


export default function ControlPage() {
  const { snapshot, developerMode, connectionState, connectedHost } = useInverterStore();
  const [ecoSaving, setEcoSaving] = useState(false);
  const [timedChargeSaving, setTimedChargeSaving] = useState(false);
  const [timedExportSaving, setTimedExportSaving] = useState(false);
  const [timedDischargeSaving, setTimedDischargeSaving] = useState(false);
  const [timedDischargeOverride, setTimedDischargeOverride] = useState<boolean | null>(null);

  // ---- Connection gate ----
  // When the inverter isn't currently connected, controls would be a lie:
  // the schedule/slot editors bind to a stale snapshot, the Quick Actions
  // and Save buttons POST against a backend that can't write to the dongle,
  // and the sliders present values the user can't trust. Render a
  // reconnecting screen at the top of the JSX instead — the existing
  // Rules of Hooks tests forbid an early `return` here because the rest
  // of the component declares many `useState` / `useEffect` calls below.
  //
  // We gate on `connectionState !== 'connected'` rather than `snapshot == null`
  // because we *do* often still have a stale snapshot during a reconnect
  // cycle — the poll loop clears `latest_snapshot` on disconnect, but the
  // backend's last good broadcast lingers on the WS until the next
  // Connection { state: Reconnecting } frame. Showing controls against that
  // stale data was the source of the "really confusing" report.
  const [manualReconnecting, setManualReconnecting] = useState(false);
  const handleManualReconnect = useCallback(async () => {
    setManualReconnecting(true);
    try {
      await fetch('/api/reconnect', { method: 'POST' });
    } catch { /* swallow — the poll loop's own back-off retries anyway */ }
    // Reset after a few seconds in case the request doesn't trigger a state change.
    setTimeout(() => setManualReconnecting(false), 5000);
  }, []);
  const isConnected = connectionState === 'connected';

  // Battery limits: local draft state while dragging, otherwise from snapshot
  const [draftReserve, setDraftReserve] = useState<number | null>(null);
  const [draftCharge, setDraftCharge] = useState<number | null>(null);
  const [draftDischarge, setDraftDischarge] = useState<number | null>(null);
  const [draftActivePower, setDraftActivePower] = useState<number | null>(null);
  // ChargeMode is defined at module scope so it can be referenced by
  // the `CosyChargingSection` component declared earlier in this file.
  const snapshotCosyEnabled = snapshot?.cosy_enabled ?? false;
  const snapshotAgileScope = snapshot?.agile_scope ?? 'off';
  const [localChargeOverride, setLocalChargeOverride] = useState<ChargeMode | null>(null);
  // Use local override if user has interacted; otherwise derive from the
  // backend snapshot. Cosy wins if both are enabled (the two modes are
  // mutually exclusive in practice but this gives a deterministic
  // display order).
  const chargeMode: ChargeMode = localChargeOverride ?? (
    snapshotCosyEnabled
      ? 'cosy'
      : snapshotAgileScope === 'charge_only'
        ? 'agile_charge'
        : snapshotAgileScope === 'discharge_only'
          ? 'agile_discharge'
          : snapshotAgileScope === 'full'
            ? 'agile'
            : 'standard'
  );
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
        // The backend now returns an explicit `scope` field plus the
        // legacy `enabled` boolean. Prefer the explicit scope; fall
        // back to `enabled` for backends that haven't been upgraded.
        const res = await apiGet<{
          ok: boolean;
          enabled?: boolean;
          scope?: 'off' | 'full' | 'charge_only' | 'discharge_only';
        }>('/api/agile');
        if (res.ok) {
          if (res.scope === 'charge_only') {
            setLocalChargeOverride('agile_charge');
          } else if (res.scope === 'discharge_only') {
            setLocalChargeOverride('agile_discharge');
          } else if (res.scope === 'full' || res.enabled) {
            setLocalChargeOverride('agile');
          }
        }
      } catch { /* use defaults */ }
    })();
  }, []);

  const currentMode = snapshot?.battery_mode ?? 'eco';
  const cosyActive = snapshot?.cosy_active ?? false;
  const ecoEnabled = snapshot?.battery_power_mode != null
    ? snapshot.battery_power_mode === 1
    : currentMode === 'eco' || currentMode === 'eco_paused' || currentMode === 'timed_demand';
  const timedChargeEnabled = snapshot?.enable_charge ?? false;
  const timedExportEnabled = snapshot?.enable_discharge ?? false;
  const snapshotTimedDischargeEnabled = snapshot?.battery_pause_mode === 2;
  const timedDischargeEnabled = timedDischargeOverride ?? snapshotTimedDischargeEnabled;

  // Show draft while dragging; once snapshot confirms the saved value, use snapshot.
  // Default to null (no data) until the first snapshot arrives to avoid showing
  // misleading 100% values that then jump to real values.
  const reserveSoc = (draftReserve != null && snapshot?.battery_reserve !== draftReserve)
    ? Math.max(4, Math.min(100, draftReserve))
    : Math.max(4, Math.min(100, snapshot?.battery_reserve ?? 4));
  const isAcCoupled = snapshot?.device_type_code === '3001' || snapshot?.device_type_code === '3002';

  // Whether this inverter exposes the Emergency Power Supply enable register
  // at HR 317 — see lib/deviceCapabilities.ts. DC hybrids and pure
  // three-phase models have no AC output stage, so the firmware silently
  // drops HR 317 writes. The backend rejects /api/control/eps with HTTP
  // 400 in that case; we hide the toggle here so the user never sees a
  // control that cannot work.
  const supportsEps = deviceSupportsEps(snapshot);

  // Whether this inverter exposes the battery pause registers (HR318-320)
  // that drive the portal-style Timed Discharge feature. Same AC-config
  // block as EPS (HR317) — see lib/deviceCapabilities.ts. On DC hybrids and
  // every other non-AC model the registers don't exist, so both the Quick
  // Action button and the dedicated schedule section are hidden and the
  // backend refuses the write with HTTP 400.
  const supportsTimedDischarge = deviceSupportsTimedDischarge(snapshot);

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
      || snapshot.device_type_code.startsWith('70')
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
  // Duration (in minutes) for Force Charge and Force Discharge quick actions.
  // Backend clamps to 1..=1439; 1440 means "until stopped" (writes a
  // full-day slot) and is the upper bound of the slider for symmetry with
  // the existing 1..=1440 cooldown clamp on alerts. Persisted to
  // localStorage so the choice survives page reloads.
  const [forceDurationMinutes, setForceDurationMinutes] = useState<number>(() => {
    if (typeof window === 'undefined') return 30;
    const raw = window.localStorage.getItem('forceDurationMinutes');
    const parsed = raw == null ? NaN : Number.parseInt(raw, 10);
    return Number.isFinite(parsed) && parsed >= 1 && parsed <= 1440 ? parsed : 30;
  });
  const [forceDurationSaving, setForceDurationSaving] = useState(false);

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

  const baseDischargeSlots: ScheduleSlot[] =
    snapshot?.discharge_slots?.length != null && snapshot.discharge_slots.length >= maxDischargeSlots
      ? snapshot.discharge_slots.slice(0, maxDischargeSlots)
      : Array.from({ length: maxDischargeSlots }, () => ({
        enabled: false, start_hour: 16, start_minute: 0, end_hour: 19, end_minute: 0, target_soc: 4,
      } as ScheduleSlot));

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

  const dischargeSlots: ScheduleSlot[] = baseDischargeSlots;

  const pauseSlot = snapshot?.battery_pause_slot;
  const timedDischargeSlot: ScheduleSlot = snapshotTimedDischargeEnabled && pauseSlot?.enabled
    ? {
        enabled: true,
        // HR319/320 stores the inverse pause window. Display the demand
        // window the user asked for: pause end → pause start.
        start_hour: pauseSlot.end_hour,
        start_minute: pauseSlot.end_minute,
        end_hour: pauseSlot.start_hour,
        end_minute: pauseSlot.start_minute,
        target_soc: 100,
      }
    : {
        enabled: timedDischargeEnabled,
        start_hour: 3,
        start_minute: 0,
        end_hour: 4,
        end_minute: 0,
        target_soc: 100,
      };

  useEffect(() => {
    if (timedDischargeOverride == null) return;
    const timeout = setTimeout(() => setTimedDischargeOverride(null), 10_000);
    return () => clearTimeout(timeout);
  }, [timedDischargeOverride]);

  const handleEcoToggle = async () => {
    const enabled = !ecoEnabled;
    setEcoSaving(true);
    try {
      await apiPost('/api/control/eco', { enabled });
    } finally {
      setEcoSaving(false);
    }
  };

  const handleTimedChargeToggle = async () => {
    const enabled = !timedChargeEnabled;
    setTimedChargeSaving(true);
    try {
      await apiPost('/api/control/timed-charge', { enabled });
    } finally {
      setTimedChargeSaving(false);
    }
  };

  const handleTimedExportToggle = async () => {
    const enabled = !timedExportEnabled;
    setTimedExportSaving(true);
    try {
      await apiPost('/api/control/timed-export', { enabled });
    } finally {
      setTimedExportSaving(false);
    }
  };

  const handleTimedDischargeToggle = async () => {
    const enabled = !timedDischargeEnabled;
    setTimedDischargeSaving(true);
    setTimedDischargeOverride(enabled);
    try {
      await apiPost('/api/control/timed-discharge', {
        enabled,
        start_hour: timedDischargeSlot.start_hour,
        start_minute: timedDischargeSlot.start_minute,
        end_hour: timedDischargeSlot.end_hour,
        end_minute: timedDischargeSlot.end_minute,
      });
    } catch {
      setTimedDischargeOverride(null);
    } finally {
      setTimedDischargeSaving(false);
    }
  };

  const handleSlotSave = async (index: number, slot: ScheduleSlot, path: string) => {
    if (path === '/api/control/timed-discharge') {
      setTimedDischargeOverride(slot.enabled);
      await apiPost(path, {
        enabled: slot.enabled,
        start_hour: slot.start_hour,
        start_minute: slot.start_minute,
        end_hour: slot.end_hour,
        end_minute: slot.end_minute,
      });
      return;
    }

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
    } catch (e: unknown) { console.warn("Slot save failed:", e); }
    setReserveSaving(false);
  };

  const handleChargeRateSave = async () => {
    if (chargeRate == null) return;
    setChargeRateSaving(true);
    try {
      await apiPost('/api/control/charge-rate', { limit: Math.round(chargeRate / rateDisplayMultiplier) });
    } catch (e: unknown) { console.warn("Slot save failed:", e); }
    setChargeRateSaving(false);
  };

  const handleDischargeRateSave = async () => {
    if (dischargeRate == null) return;
    setDischargeRateSaving(true);
    try {
      await apiPost('/api/control/discharge-rate', { limit: Math.round(dischargeRate / rateDisplayMultiplier) });
    } catch (e: unknown) { console.warn("Slot save failed:", e); }
    setDischargeRateSaving(false);
  };

  const handleActivePowerSave = async () => {
    if (activePowerRate == null) return;
    setActivePowerSaving(true);
    try {
      await apiPost('/api/control/active-power-rate', { rate: activePowerRate });
    } catch (e: unknown) { console.warn("Slot save failed:", e); }
    setActivePowerSaving(false);
  };

  // Connection gate: when not connected, short-circuit the entire JSX so
  // none of the controls (and their stale-snapshot bindings) leak through.
  // Rendered at the top of the return so the rest of the component's
  // hooks remain unconditional below (React Rules of Hooks).
  if (!isConnected) {
    return (
      <div className="flex flex-col items-center justify-center min-h-[60vh] gap-4">
        <div className="w-10 h-10 border-4 border-flow-active border-t-transparent rounded-full animate-spin" />
        <p className="text-text-secondary text-sm font-sans">
          {connectionState === 'reconnecting'
            ? 'Connection lost — reconnecting…'
            : connectionState === 'disconnected'
              ? 'Disconnected — will retry automatically'
              : 'Waiting for data'}
        </p>
        {connectedHost && (
          <p className="text-text-secondary/60 text-xs font-sans">
            Host: {connectedHost.replace(/:.*$/, '')}
          </p>
        )}
        <button
          onClick={handleManualReconnect}
          disabled={manualReconnecting}
          className="px-4 py-1.5 text-xs font-semibold rounded-lg bg-bg-surface hover:bg-white/10 border border-white/10 transition-colors disabled:opacity-50"
        >
          {manualReconnecting ? 'Reconnecting…' : 'Retry now'}
        </button>
        <p className="text-text-secondary/60 text-xs font-sans text-center max-w-xs">
          Controls are disabled while the inverter is unreachable. They will
          reappear automatically once the connection is restored.
        </p>
      </div>
    );
  }

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
                    // Stop Charge: restore the inverter to its pre-force-charge
                    // state via the dedicated endpoint. The backend snapshots
                    // the relevant registers on Force Charge start and replays
                    // them here (HR_ENABLE_CHARGE, HR_CHARGE_TARGET_SOC, the
                    // charge slot, and three-phase force/AC-charge flags).
                    await apiPost('/api/control/force-charge/stop');
                    setLocalForceChargeOverride(false);
                  } else {
                    // Clamp 1440 → 1439 to match the backend's 1..=1439 slot clamp.
                    // 1440 on the slider represents "full day" which the backend
                    // expresses as a 00:00→23:59 slot — the Quick Action still
                    // works in that case because the slot remains non-zero.
                    const minutes = Math.min(forceDurationMinutes, 1439);
                    await apiPost('/api/control/force-charge', { minutes });
                    setLocalForceChargeOverride(true);
                  }
                } catch (e: unknown) { console.warn("Slot save failed:", e); }
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
                    // Stop Discharge: restore the inverter to its pre-force-discharge
                    // state via the dedicated endpoint. The backend snapshots the
                    // relevant registers (HR_ENABLE_DISCHARGE, the discharge slots,
                    // three-phase force flags) on Force Discharge start and replays
                    // them here.
                    await apiPost('/api/control/force-discharge/stop');
                    setLocalForceDischargeOverride(false);
                  } else {
                    // Mirror the force-charge path: pass the duration slider
                    // value so the discharge slot is `now → now+minutes`
                    // instead of the encoder's default 00:00–23:59. Clamp
                    // 1440 → 1439 to match the backend's 1..=1439 slot clamp.
                    const minutes = Math.min(forceDurationMinutes, 1439);
                    await apiPost('/api/control/force-discharge', { minutes });
                    setLocalForceDischargeOverride(true);
                  }
                } catch (e: unknown) { console.warn("Slot save failed:", e); }
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


      {/* Section 2: Independent battery mechanisms */}
      <section className="space-y-3">
        <div className="flex items-center gap-3">
          <h2 className="text-text-primary font-semibold text-lg">Battery Mode</h2>
          {cosyActive && (
            <span className="text-xs text-battery font-semibold bg-battery/10 px-2 py-0.5 rounded-full flex items-center gap-1">
              <span className="inline-block w-1.5 h-1.5 bg-battery rounded-full animate-pulse" />
              Cosy Charging
            </span>
          )}
        </div>
        <div className="grid grid-cols-1 md:grid-cols-4 gap-2">
          <button
            type="button"
            onClick={handleEcoToggle}
            disabled={ecoSaving}
            aria-pressed={ecoEnabled}
            className={`px-3 py-3 rounded-lg border text-xs font-medium transition flex flex-col items-start gap-1 ${ecoEnabled
                ? 'bg-battery/20 border-battery text-battery'
                : 'bg-bg-surface border-transparent hover:border-battery/40 text-text-secondary'
              } disabled:opacity-50`}
          >
            <span className="flex items-center justify-center gap-2 w-full text-sm">
              {ecoSaving && <span className="inline-block w-3 h-3 border-2 border-current border-t-transparent rounded-full animate-spin" />}
              <b>Eco</b>
            </span>
            <span className="text-[11px] text-text-secondary">Battery Covers Home Demand</span>
          </button>
          <button
            type="button"
            onClick={handleTimedChargeToggle}
            disabled={timedChargeSaving}
            aria-pressed={timedChargeEnabled}
            className={`px-3 py-3 rounded-lg border text-xs font-medium transition flex flex-col items-start gap-1 ${timedChargeEnabled
                ? 'bg-battery/20 border-battery text-battery'
                : 'bg-bg-surface border-transparent hover:border-battery/40 text-text-secondary'
              } disabled:opacity-50`}
          >
            <span className="flex items-center justify-center gap-2 w-full text-sm">
              {timedChargeSaving && <span className="inline-block w-3 h-3 border-2 border-current border-t-transparent rounded-full animate-spin" />}
              <b>Timed Charge</b>
            </span>
            <span className="text-[11px] text-text-secondary">Performs Charge During Specified Time(s)</span>
          </button>
          {supportsTimedDischarge && (
            <button
              type="button"
              onClick={handleTimedDischargeToggle}
              disabled={timedDischargeSaving}
              aria-pressed={timedDischargeEnabled}
              className={`px-3 py-3 rounded-lg border text-xs font-medium transition flex flex-col items-start gap-1 ${timedDischargeEnabled
                  ? 'bg-battery/20 border-battery text-battery'
                  : 'bg-bg-surface border-transparent hover:border-battery/40 text-text-secondary'
                } disabled:opacity-50`}
            >
              <span className="flex items-center justify-center gap-2 w-full text-sm">
                {timedDischargeSaving && <span className="inline-block w-3 h-3 border-2 border-current border-t-transparent rounded-full animate-spin" />}
                <b>Timed Discharge</b>
              </span>
              <span className="text-[11px] text-text-secondary">Only Allow Discharge During Specified Time</span>
            </button>
          )}
          <button
            type="button"
            onClick={handleTimedExportToggle}
            disabled={timedExportSaving}
            aria-pressed={timedExportEnabled}
            className={`px-3 py-3 rounded-lg border text-xs font-medium transition flex flex-col items-start gap-1 ${timedExportEnabled
                ? 'bg-battery/20 border-battery text-battery'
                : 'bg-bg-surface border-transparent hover:border-battery/40 text-text-secondary'
              } disabled:opacity-50`}
          >
            <span className="flex items-center justify-center gap-2 w-full text-sm">
              {timedExportSaving && <span className="inline-block w-3 h-3 border-2 border-current border-t-transparent rounded-full animate-spin" />}
              <b>Timed Export</b>
            </span>
            <span className="text-[11px] text-text-secondary">Forces Battery Export During Specified Time(s)</span>
          </button>

        </div>
      </section>

      {/* Section 3: Charging Mode */}
      <CosyChargingSection mode={chargeMode} cosyActive={cosyActive} onModeChange={setChargeMode} />

      {/*
        Visibility matrix for the three schedule sections below. The
        `chargeMode` is the user-facing charging-mode dropdown value;
        `agileOwnsCharge` and `agileOwnsDischarge` are derived from
        the active scope. Sections that Agile has taken over are
        hidden; the rest are shown, with `mode="agile_readonly"` on
        `ScheduleSlotEditor` when the schedule coexists with an
        Agile sub-mode (rendering them dimmed + labelled).

        - Standard mode: all three sections visible, fully editable.
        - Cosy mode: all three sections visible (Cosy owns the
          charge-side mechanism; the discharge schedules are
          independent inverter mechanisms per the existing
          `cosy_discharge_slots` design).
        - Agile (full): all three sections hidden (Agile owns both).
        - Agile — Charge only: Charge Schedule hidden; Timed
          Discharge and Discharge Schedule visible + greyed.
        - Agile — Discharge only: Timed Discharge and Discharge
          Schedule hidden; Charge Schedule visible + greyed.
      */}
      {(() => {
        const agileOwnsCharge = chargeMode === 'agile' || chargeMode === 'agile_charge';
        const agileOwnsDischarge = chargeMode === 'agile' || chargeMode === 'agile_discharge';
        const scheduleModeForSlots: 'normal' | 'agile_readonly' =
          chargeMode === 'agile_charge' || chargeMode === 'agile_discharge'
            ? 'agile_readonly'
            : 'normal';
        return (
          <>
      {/* Section 4: Charge Schedule */}
      {!cosyEnabled && !agileOwnsCharge && schedulesUnsupported && (
        <section className="space-y-3">
          <h2 className="text-text-primary font-semibold">Charge/Discharge Schedules</h2>
          <div className="rounded-xl border border-yellow-500/30 bg-yellow-500/10 p-3 text-sm text-text-primary">
            <div className="font-semibold mb-1">Schedules are hidden for this inverter model</div>
            This three-phase/HV inverter uses a different schedule register map.
            Reading real-time data is supported,GivEnergy Cloud
            editing is disabled until those registers are implemented safely.
          </div>
        </section>
      )}

      {!cosyEnabled && !agileOwnsCharge && !schedulesUnsupported && <section className="space-y-3">
        <h2 className="text-text-primary font-semibold text-lg">Charge Schedule</h2>
        <p className="text-text-secondary/60 text-xs">Please Allow upto 10 Seconds for Changes to Save</p>
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
                        This app uses the canonical Modbus register layout from the{' '}
                        <code>givenergy-modbus</code> reference library, which labels
                        discharge slots in the opposite order to the GivEnergy cloud UI:
                        our <strong>Slot 1</strong> is the cloud&apos;s <strong>Slot 2</strong>{' '}
                        and vice versa. The underlying schedule data is identical — only the labels differ.
                    </div>
                  )}
                  {isLegacyGen3Fw && (
                    <div className="rounded-xl border border-yellow-500/30 bg-yellow-500/10 p-3 text-xs text-text-primary">
                      <div className="font-semibold mb-1">
                        Older Gen3 firmware detected (ARM FW {snapshot?.firmware_version})
                      </div>
                      Slot 2 and beyond (and per-slot target SOCs) come from extended
                      registers that your inverter firmware does not fully
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
                masterArmed={snapshot?.enable_charge === true}
                mode={scheduleModeForSlots}
              />
            </>
          ))}
        </div>
      </section>}

      {/* Section 5: Timed Discharge — portal-style pause-discharge inverse window.
          Visible in Cosy mode too: Cosy only owns the force-charge side
          (timed-charge window), and the pause-discharge window is an
          independent inverter mechanism, so the user can layer them
          (e.g. "force-charge 02:00–05:00, and only allow discharge
          16:00–19:00"). Hidden in Agile (full) and Agile — Discharge
          Only because those modes drive discharge from prices. Visible
          in Agile — Charge Only because charge-only doesn't touch the
          discharge side. Hidden on devices without the HR 300-359
          block (DC hybrids, three-phase, Gateway, EMS, PV inverter)
          since the pause registers don't exist there — see
          supportsTimedDischarge / lib/deviceCapabilities.ts. */}
      {!agileOwnsDischarge && !schedulesUnsupported && supportsTimedDischarge && (
        <section className="space-y-3">
          <h2 className="text-text-primary font-semibold text-lg">Timed Discharge</h2>
          <p className="text-text-secondary/60 text-xs">Please Allow upto 10 Seconds for Changes to Save</p>
          <ScheduleSlotEditor
            key={`timed-discharge-${timedDischargeSlot.enabled}-${timedDischargeSlot.start_hour}:${timedDischargeSlot.start_minute}-${timedDischargeSlot.end_hour}:${timedDischargeSlot.end_minute}`}
            slotIndex={0}
            slot={timedDischargeSlot}
            onSave={handleSlotSave}
            showTargetSoc={false}
            apiPath="/api/control/timed-discharge"
            masterArmed={timedDischargeEnabled}
            mode={scheduleModeForSlots}
          />
        </section>
      )}

      {/* Section 6: Timed Export / DC Discharge Schedule — visible in Cosy
          mode too: Cosy only owns the force-charge side, and
          enable_discharge (the schedule that drives Timed Export) is an
          independent inverter mechanism. The user can configure Timed Export
          windows while Cosy handles charging. Hidden in Agile (full) and
          Agile — Discharge Only because those modes drive discharge from
          prices. Visible in Agile — Charge Only because charge-only
          doesn't touch the discharge side. */}
      {!agileOwnsDischarge && !schedulesUnsupported && (
        <section className="space-y-3">
          <h2 className="text-text-primary font-semibold text-lg">Discharge Schedule</h2>
          <p className="text-text-secondary/60 text-xs">Please Allow upto 10 Seconds for Changes to Save</p>
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
                        This app uses the canonical Modbus register layout from the{' '}
                        <code>givenergy-modbus</code> reference library, which labels
                        discharge slots in the opposite order to the GivEnergy cloud UI:
                        our <strong>Slot 1</strong> is the cloud&apos;s <strong>Slot 2</strong>{' '}
                        and vice versa. The underlying schedule data is identical — only the labels differ.
                      </div>
                    )}
                    {isLegacyGen3Fw && (
                      <div className="rounded-xl border border-yellow-500/30 bg-yellow-500/10 p-3 text-xs text-text-primary">
                        <div className="font-semibold mb-1">
                          Older Gen3 firmware detected (ARM FW {snapshot?.firmware_version})
                        </div>
                        Slot 2 and beyond come from extended registers that
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
                  // Only show target SOC slider on models that support
                  // extended schedule slots (Gen3+ hybrid, three-phase,
                  // AIO, HV Gen3, Gen4). On AC-coupled/Gen1/Gen2 there
                  // is no register to write a per-slot or global discharge
                  // target SOC — the slider would silently do nothing.
                  showTargetSoc={maxDischargeSlots > 2}
                  apiPath="/api/control/discharge-slot"
                  mode={scheduleModeForSlots}
                />
              </>
            ))}
          </div>
        </section>
      )}
          </>
        );
      })()}

      {/* Section 6: Battery and Power Controls */}
      <section className="space-y-3">
        <h2 className="text-text-primary font-semibold text-lg">Battery and Power Controls</h2>
        <div className="bg-bg-surface rounded-xl p-4 space-y-5">
          {/* EPS (Emergency Power Supply) — AC-coupled / AC-three-phase / All-in-One */}
          {supportsEps && (
            <div className="space-y-1 pt-2 border-t border-bg-elevated">
              <div className="flex items-center justify-between">
                <div>
                  <span className="text-text-primary text-sm font-medium">Emergency Power Supply (EPS)</span>
                  <p className="text-text-secondary text-xs mt-0.5">
                    Enable Backup Power During Grid Outages
                  </p>
                </div>
                <button
                  onClick={async () => {
                    try {
                      await apiPost('/api/control/eps', { enabled: !snapshot?.ac_eps_enabled });
                    } catch (e: unknown) { console.warn("EPS toggle failed:", e); }
                  }}
                  className={`relative w-10 h-5 rounded-full transition ${snapshot?.ac_eps_enabled ? 'bg-battery' : 'bg-bg-elevated'}`}
                >
                  <span
                    className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition ${snapshot?.ac_eps_enabled ? 'left-5.5' : 'left-0.5'}`}
                  />
                </button>
              </div>
            </div>
          )}
          {/* Quick Action Duration */}
          <div className="space-y-1">
            <div className="flex items-center justify-between">
              <div>
                <span className="text-text-secondary text-sm">Quick Action Duration</span>
                <p className="text-text-secondary text-xs mt-0.5">
                  How long the Quick Action force-charge / force-discharge slot
                  should run. Applies to both Quick Action buttons.
                </p>
              </div>
              <span className="font-mono text-text-primary text-sm">
                {forceDurationMinutes >= 1440
                  ? '24h'
                  : forceDurationMinutes >= 60
                    ? `${Math.floor(forceDurationMinutes / 60)}h ${forceDurationMinutes % 60 ? `${forceDurationMinutes % 60}m` : ''}`.trim()
                    : `${forceDurationMinutes}m`}
              </span>
            </div>
            <div className="flex items-center gap-3">
              <input
                type="range"
                min={1}
                max={1440}
                step={1}
                value={forceDurationMinutes}
                onChange={(e) => setForceDurationMinutes(Math.max(1, Math.min(1440, Number(e.target.value))))}
                className="flex-1"
              />
              <button
                onClick={async () => {
                  setForceDurationSaving(true);
                  try {
                    // Persist so the choice survives page reloads.
                    localStorage.setItem('forceDurationMinutes', String(forceDurationMinutes));
                  } finally {
                    setForceDurationSaving(false);
                  }
                }}
                disabled={forceDurationSaving}
                className="px-3 py-1.5 bg-battery/20 text-battery rounded-lg text-xs font-medium hover:bg-battery/30 transition disabled:opacity-50"
              >
                {forceDurationSaving ? '...' : 'Save'}
              </button>
            </div>
          </div>
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
              <span className="font-mono text-text-primary text-sm whitespace-nowrap">{activePowerRate ?? '—'}%{activePowerKw != null && activePowerWatts != null && activePowerWatts > 0 ? `(${activePowerKw})` : ''}</span>
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
        {/* Load Discharge Limiter — always visible when battery is in Eco mode */}
        <LoadLimiterSection />
        {/* Auto Winter Mode */}
        <AutoWinterSection />
        {/* Developer Controls (dev mode only) */}
        {developerMode && (
          <section className="space-y-4 border-t border-bg-elevated pt-4 mt-4">
            <div className="flex items-center gap-2">
              <h2 className="text-text-primary font-semibold text-lg">Developer Controls</h2>
              <span className="text-xs bg-amber-500/20 text-text-primary px-2 py-0.5 rounded-full font-medium">DEV</span>
            </div>
            <BatteryCalibrationSection />
            <RebootInverterSection />
          </section>
        )}
      </section>
    </div>
  );
}
