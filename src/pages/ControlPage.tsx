import { useState, useCallback } from 'react';
import { useInverterStore } from '../store/useInverterStore';
import { apiPost } from '../lib/api';
import type { ScheduleSlot } from '../lib/types';

type BatteryMode = 'eco' | 'eco_paused' | 'timed_demand' | 'timed_export' | 'export_paused';

const BATTERY_MODES: { key: BatteryMode; label: string }[] = [
  { key: 'eco', label: 'Eco' },
  { key: 'eco_paused', label: 'Eco Paused' },
  { key: 'timed_demand', label: 'Timed Demand' },
  { key: 'timed_export', label: 'Timed Export' },
  { key: 'export_paused', label: 'Export Paused' },
];

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
        {Array.from({ length: 60 }, (_, i) => i).filter((m) => m % 15 === 0).map((m) => (
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
    <div className="bg-bg-elevated rounded-xl p-4 space-y-3">
      <div className="flex items-center justify-between">
        <span className="text-text-primary font-medium">Slot {slotIndex + 1}</span>
        <button
          onClick={() => setLocal((l) => ({ ...l, enabled: !l.enabled }))}
          className={`relative w-10 h-5 rounded-full transition ${
            local.enabled ? 'bg-battery' : 'bg-bg-surface'
          }`}
        >
          <span
            className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition ${
              local.enabled ? 'left-5.5' : 'left-0.5'
            }`}
          />
        </button>
      </div>

      {local.enabled && (
        <>
          <div className="grid grid-cols-2 gap-3">
            <div className="flex items-center gap-3">
              <span className="text-text-secondary text-sm w-12">Start</span>
              <TimePicker
                hour={local.start_hour}
                minute={local.start_minute}
                onChange={(h, m) => setLocal((l) => ({ ...l, start_hour: h, start_minute: m }))}
              />
            </div>
            <div className="flex items-center gap-3">
              <span className="text-text-secondary text-sm w-12">End</span>
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

export default function ControlPage() {
  const { snapshot } = useInverterStore();
  const modeAction = useAction();

  // Battery limits local state
  const [reserveSoc, setReserveSoc] = useState<number>(snapshot?.battery_reserve ?? 4);
  const [chargeRate, setChargeRate] = useState<number>(snapshot?.charge_rate ?? 100);
  const [dischargeRate, setDischargeRate] = useState<number>(snapshot?.discharge_rate ?? 100);

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
        <h2 className="text-text-primary font-semibold text-lg">Battery Mode</h2>
        <div className="grid grid-cols-2 sm:grid-cols-5 gap-2">
          {BATTERY_MODES.map(({ key, label }) => {
            const isActive = currentMode === key;
            return (
              <button
                key={key}
                onClick={() => modeAction.execute('/api/control/mode', { mode: key })}
                disabled={modeAction.loading}
                className={`px-3 py-3 rounded-lg border text-xs font-medium transition w-full ${
                  isActive
                    ? 'bg-battery/20 border-battery text-battery'
                    : 'bg-bg-surface border-transparent hover:border-battery/40 hover:bg-bg-elevated text-text-secondary'
                } disabled:opacity-50`}
              >
                {label}
              </button>
            );
          })}
        </div>
        {modeAction.error && (
          <p className="text-red-400 text-sm">{modeAction.error}</p>
        )}
      </section>


      {/* Section 3: Charge Schedule */}
      <section className="space-y-3">
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
      </section>

      {/* Section 4: Discharge Schedule */}
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

      {/* Section 5: Battery Limits */}
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
                step={5}
                value={reserveSoc}
                onChange={(e) => setReserveSoc(Number(e.target.value))}
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
                max={100}
                step={5}
                value={chargeRate}
                onChange={(e) => setChargeRate(Number(e.target.value))}
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
                max={100}
                step={5}
                value={dischargeRate}
                onChange={(e) => setDischargeRate(Number(e.target.value))}
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
      </section>
    </div>
  );
}
