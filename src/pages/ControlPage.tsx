import { useState, useCallback, useEffect } from 'react';
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
        className="w-full flex flex-col items-center gap-2 p-4 bg-bg-surface rounded-xl border border-transparent hover:border-battery/40 hover:bg-bg-elevated transition disabled:opacity-50"
      >
        <span className="text-2xl">{icon}</span>
        <span className="text-text-primary text-sm font-medium">{label}</span>
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
        {Array.from({ length: 60 }, (_, i) => i).filter((m) => m % 5 === 0).map((m) => (
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
}: {
  slotIndex: number;
  slot: ScheduleSlot;
  onSave: (index: number, slot: ScheduleSlot, path: string) => void;
  showTargetSoc: boolean;
}) {
  const [local, setLocal] = useState<ScheduleSlot>({ ...slot });
  const [saving, setSaving] = useState(false);
  const [feedback, setFeedback] = useState<'saved' | 'error' | null>(null);

  const apiPath = showTargetSoc ? '/api/control/charge-slot' : '/api/control/discharge-slot';

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
          <div className="flex items-center gap-6">
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
                min={0}
                max={100}
                step={5}
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
          <div className="text-xs bg-blue-900/40 text-blue-200 px-3 py-2 rounded-lg">
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
                step={5}
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
            <div className="text-xs bg-yellow-900/30 text-yellow-300 px-3 py-2 rounded-lg">
              Winter mode charges the battery using grid power when solar is insufficient.
              Your existing charge schedule will be restored when the battery warms up.
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
    </section>
  );
}

/** Cosy charging section — only shown in developer mode + Eco mode. */
function CosyChargingSection({ enabled, onToggle }: { enabled: boolean; onToggle: (v: boolean) => void }) {
  const [slots, setSlots] = useState<
    { enabled: boolean; start_hour: number; start_minute: number; end_hour: number; end_minute: number; target_soc: number }[]
  >([]);
  const [saving, setSaving] = useState(false);
  const [saveFeedback, setSaveFeedback] = useState<'saved' | 'error' | null>(null);

  useEffect(() => {
    (async () => {
      try {
        const res = await apiGet<{ ok: boolean; enabled: boolean; slots: typeof slots }>('/api/cosy');
        if (res.ok) {
          onToggle(res.enabled);
          setSlots(res.slots.length === 3 ? res.slots : Array.from({ length: 3 }, () => ({
            enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100,
          })));
        }
      } catch { /* use defaults */ }
    })();
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const toggleCosy = async () => {
    const newEnabled = !enabled;
    onToggle(newEnabled);
    setSaving(true);
    try {
      await apiPost('/api/cosy', { enabled: newEnabled, slots });
      setSaveFeedback('saved');
    } catch {
      setSaveFeedback('error');
    }
    setSaving(false);
    setTimeout(() => setSaveFeedback(null), 2000);
  };

  const save = async () => {
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
        <h2 className="text-text-primary font-semibold text-lg">Cosy Charging</h2>
        <button
          onClick={toggleCosy}
          disabled={saving}
          className={`relative w-10 h-5 rounded-full transition ${enabled ? 'bg-battery' : 'bg-bg-surface'}`}
        >
          <span className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition ${enabled ? 'left-5.5' : 'left-0.5'}`} />
        </button>
      </div>
      <p className="text-text-secondary/60 text-xs">
        Inverter stays in Eco mode. Charge slots are stored locally — the app
        sends ForceCharge commands during these windows.
      </p>

      {enabled && (
        <div className="space-y-4">
          {slots.map((slot, i) => (
            <div key={i} className="bg-bg-surface rounded-xl p-3 space-y-2">
              <div className="flex items-center justify-between">
                <span className="text-text-primary text-sm font-medium">Slot {i + 1}</span>
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
                  <div className="flex items-center gap-6">
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
                      step={5}
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
          ))}
          <button
            onClick={save}
            disabled={saving}
            className="w-full py-2 bg-battery/20 text-battery rounded-lg text-sm font-medium hover:bg-battery/30 transition disabled:opacity-50"
          >
            {saving ? 'Saving...' : saveFeedback === 'saved' ? '✓ Saved' : saveFeedback === 'error' ? '✗ Error' : 'Save slots'}
          </button>
        </div>
      )}
    </section>
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
        <span className="text-xs bg-amber-500/20 text-amber-400 px-2 py-0.5 rounded-full font-medium">DEV</span>
      </div>

      <div className="bg-amber-900/20 border border-amber-700/30 rounded-xl p-3 space-y-2">
        <p className="text-amber-300 text-xs font-medium">⚠️  WARNING</p>
        <p className="text-amber-200/70 text-xs">
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
            className="w-full py-2 bg-amber-500/20 text-amber-400 rounded-lg text-xs font-medium hover:bg-amber-500/30 transition disabled:opacity-40 border border-amber-500/30"
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
        <p className="text-red-300 text-xs font-medium">⚠️  DANGER</p>
        <p className="text-red-200/70 text-xs">
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
          className="w-full py-2 bg-red-500/20 text-red-400 rounded-lg text-xs font-medium hover:bg-red-500/30 transition disabled:opacity-40 border border-red-500/30"
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
  const [cosyEnabled, setCosyEnabled] = useState(false);

  // Show draft while dragging; once snapshot confirms the saved value, use snapshot
  const reserveSoc = (draftReserve != null && snapshot?.battery_reserve !== draftReserve) ? draftReserve : (snapshot?.battery_reserve ?? 4);
  const chargeRate = (draftCharge != null && snapshot?.charge_rate !== draftCharge) ? draftCharge : (snapshot?.charge_rate ?? 50);
  const dischargeRate = (draftDischarge != null && snapshot?.discharge_rate !== draftDischarge) ? draftDischarge : (snapshot?.discharge_rate ?? 50);

  const [reserveSaving, setReserveSaving] = useState(false);
  const [chargeRateSaving, setChargeRateSaving] = useState(false);
  const [dischargeRateSaving, setDischargeRateSaving] = useState(false);

  // Default slots if snapshot doesn't have them
  // Only 2 charge slots are supported by the inverter registers
  const chargeSlots: ScheduleSlot[] =
    snapshot?.charge_slots?.length != null && snapshot.charge_slots.length >= 2
      ? snapshot.charge_slots.slice(0, 2)
      : [
        { enabled: false, start_hour: 0, start_minute: 0, end_hour: 6, end_minute: 0, target_soc: 100 },
        { enabled: false, start_hour: 0, start_minute: 0, end_hour: 6, end_minute: 0, target_soc: 100 },
      ];

  const dischargeSlots: ScheduleSlot[] =
    snapshot?.discharge_slots?.length === 2
      ? snapshot.discharge_slots
      : [
        { enabled: false, start_hour: 16, start_minute: 0, end_hour: 19, end_minute: 0, target_soc: 0 },
        { enabled: false, start_hour: 16, start_minute: 0, end_hour: 19, end_minute: 0, target_soc: 0 },
      ];

  const currentMode = snapshot?.battery_mode ?? 'eco';
  const [requestedMode, setRequestedMode] = useState<BatteryMode | null>(null);

  // Clear requested mode after 30s timeout (safety net for unconfirmed writes).
  // The inverter confirming the change is handled by deriving effectiveMode below.
  useEffect(() => {
    if (!requestedMode) return;
    const timeout = setTimeout(() => setRequestedMode(null), 30_000);
    return () => clearTimeout(timeout);
  }, [requestedMode]);

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
    setChargeRateSaving(true);
    try {
      await apiPost('/api/control/charge-rate', { limit: chargeRate });
    } catch { /* handled silently */ }
    setChargeRateSaving(false);
  };

  const handleDischargeRateSave = async () => {
    setDischargeRateSaving(true);
    try {
      await apiPost('/api/control/discharge-rate', { limit: dischargeRate });
    } catch { /* handled silently */ }
    setDischargeRateSaving(false);
  };

  return (
    <div className="flex flex-col gap-6 max-w-2xl mx-auto px-4 py-6">
      {/* Section 1: Quick Actions */}
      <section className="space-y-3">
        <h2 className="text-text-primary font-semibold text-lg">Quick Actions</h2>
        <div className="grid grid-cols-2 sm:grid-cols-4 gap-3">
          <ActionButton
            label="Force Charge"
            icon="☀️"
            path="/api/control/force-charge"
            body={{ minutes: 30 }}
          />
          <ActionButton
            label="Force Discharge"
            icon="⚡"
            path="/api/control/force-discharge"
            body={{ minutes: 30 }}
          />
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
                    : 'bg-bg-surface border-transparent hover:border-battery/40 hover:bg-bg-elevated text-text-secondary'
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


      {/* Section 3: Charge Schedule */}
      {!cosyEnabled && <section className="space-y-3">
        <h2 className="text-text-primary font-semibold text-lg">Charge Schedule</h2>
        <div className="space-y-3">
          {chargeSlots.map((slot, i) => (
            <ScheduleSlotEditor
              key={`charge-${i}-${slot.enabled}-${slot.start_hour}:${slot.start_minute}-${slot.end_hour}:${slot.end_minute}-${slot.target_soc}`}
              slotIndex={i}
              slot={slot}
              onSave={handleSlotSave}
              showTargetSoc
            />
          ))}
        </div>
      </section>}

      {/* Section 4: Discharge Schedule */}
      {modeToCategory(effectiveMode) === 'timed' && (
        <section className="space-y-3">
          <h2 className="text-text-primary font-semibold text-lg">Discharge Schedule</h2>
          <div className="space-y-3">
            {dischargeSlots.map((slot, i) => (
              <ScheduleSlotEditor
                key={`discharge-${i}-${slot.enabled}-${slot.start_hour}:${slot.start_minute}-${slot.end_hour}:${slot.end_minute}`}
                slotIndex={i}
                slot={slot}
                onSave={handleSlotSave}
                showTargetSoc={false}
              />
            ))}
          </div>
        </section>
      )}

      {/* Section 5: Cosy Charging (dev mode only, Eco only) */}
      {developerMode && modeToCategory(effectiveMode) === 'eco' && <CosyChargingSection enabled={cosyEnabled} onToggle={setCosyEnabled} />}

      {/* Section 5: Auto Winter Mode */}
      <AutoWinterSection />
      {/* Section 6: Battery Limits */}
      <section className="space-y-3">
        <h2 className="text-text-primary font-semibold text-lg">Battery Limits</h2>
        <div className="bg-bg-surface rounded-xl p-4 space-y-5">
          {/* Reserve SOC */}
          <div className="space-y-2">
            <div className="flex items-center justify-between">
              <span className="text-text-secondary text-sm">Reserve SOC</span>
              <span className="font-mono text-text-primary text-sm">{reserveSoc}%</span>
            </div>
            <div className="flex items-center gap-3">
              <input
                type="range"
                min={0}
                max={100}
                step={1}
                value={reserveSoc}
                onChange={(e) => setDraftReserve(Number(e.target.value))}
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

          {/* Charge Rate */}
          <div className="space-y-1">
            <div className="flex items-center justify-between">
              <span className="text-text-secondary text-sm">Charge Rate</span>
              <span className="font-mono text-text-primary text-sm">{chargeRate}%</span>
            </div>
            <div className="flex items-center gap-3">
              <input
                type="range"
                min={0}
                max={50}
                step={5}
                value={chargeRate}
                onChange={(e) => setDraftCharge(Number(e.target.value))}
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

          {/* Discharge Rate */}
          <div className="space-y-1">
            <div className="flex items-center justify-between">
              <span className="text-text-secondary text-sm">Discharge Rate</span>
              <span className="font-mono text-text-primary text-sm">{dischargeRate}%</span>
            </div>
            <div className="flex items-center gap-3">
              <input
                type="range"
                min={0}
                max={50}
                step={5}
                value={dischargeRate}
                onChange={(e) => setDraftDischarge(Number(e.target.value))}
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
        </div>
        {/* Battery Calibration (dev mode only) */}
        {developerMode && <BatteryCalibrationSection />}
      </section>
    </div>
  );
}
