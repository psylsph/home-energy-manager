import { useEffect, useRef, useState } from 'react';
import { HeadingWithHelp } from './HelpTip';
import { apiPost } from '../lib/api';
import { useInverterStore } from '../store/useInverterStore';

/** HR 29 command values (GivTCP's Off, Start and Charge Only). */
const CALIBRATION_STOP = 0;
const CALIBRATION_START = 1;
const CALIBRATION_CHARGE_ONLY = 3;

const STAGE_LABELS: Record<number, string> = {
  0: 'Off',
  1: 'Discharging…',
  2: 'Setting Lower Limit…',
  3: 'Charging…',
  4: 'Setting Upper Limit…',
  5: 'Balancing…',
  6: 'Setting Full Capacity…',
  7: 'Finished',
};

/** Longest the buttons wait for the inverter's reading to follow a command. */
const SETTLE_MS = 15_000;

type Feedback = 'sent' | 'error';

/**
 * Control page: the inverter's own battery calibration (HR 29, issue #477).
 * The normal way to calibrate; offered on single-phase LV models. HEM only
 * starts, stops or limits it to charging and shows the stage the inverter
 * reports.
 */
export default function BuiltInBatteryCalibration() {
  const snapshot = useInverterStore((s) => s.snapshot);
  const supported = snapshot?.supports_battery_calibration ?? false;
  const stage = snapshot?.battery_calibration_stage ?? 0;
  const [saving, setSaving] = useState(false);
  const [feedback, setFeedback] = useState<{ control: number; result: Feedback } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const settleTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  /** The stage when a command went out: the buttons stay locked until the inverter's reading moves. */
  const [sentAt, setSentAt] = useState<number | null>(null);
  useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current);
      if (settleTimer.current) clearTimeout(settleTimer.current);
    },
    [],
  );

  // Not offered on three-phase, HV or batteryless models.
  if (!supported) return null;

  const isActive = stage > 0 && stage < 7;
  // Anything non-zero, even a finished or unrecognised stage, can be cleared with Stop.
  const canStop = stage !== 0;
  // The reading only follows a command on the next poll; a second press in
  // between would write HR 29 again (Start then Charge Only, say).
  const awaitingReading = sentAt !== null && stage === sentAt;

  const send = async (control: number, question: string) => {
    if (!confirm(question)) return;
    setSaving(true);
    setFeedback(null);
    setError(null);
    let result: Feedback = 'sent';
    try {
      await apiPost('/api/control/calibration', { stage: control });
      setSentAt(stage);
      if (settleTimer.current) clearTimeout(settleTimer.current);
      settleTimer.current = setTimeout(() => setSentAt(null), SETTLE_MS);
    } catch (e) {
      result = 'error';
      setError(e instanceof Error ? e.message : 'The request failed');
    }
    setSaving(false);
    setFeedback({ control, result });
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(() => setFeedback(null), 3000);
  };

  const label = (control: number, idle: string) => {
    if (feedback?.control === control) return feedback.result === 'sent' ? '✓ Sent' : '✗ Error';
    return idle;
  };

  const buttonClass =
    'flex-1 py-1.5 bg-amber-500/20 text-text-primary rounded-lg text-xs font-medium hover:bg-amber-500/30 transition disabled:opacity-40 border border-amber-500/30';

  return (
    <div className="space-y-3">
      <HeadingWithHelp
        as="h2"
        className="text-text-primary font-semibold text-lg"
        title="Battery Calibration"
        help="Runs the inverter's own calibration routine, which brings a drifting battery SOC back into line. HEM starts or stops it and shows the stage the inverter reports; it can't restore settings the routine changes. Charge Only skips the discharge and just charges the battery to full."
      />

      <div className="bg-amber-900/20 border border-amber-700/30 rounded-xl p-3 space-y-3">
        <p className="text-text-primary text-xs font-medium">⚠️  Warning</p>
        <p className="text-text-secondary text-xs">
          Discharges, charges and balances the battery over several hours. Stop ends it early.
        </p>

        <div className="flex flex-wrap items-center gap-3 pt-3 border-t border-amber-700/20">
          <button
            type="button"
            onClick={() =>
              void send(
                CALIBRATION_START,
                '⚠️  BATTERY CALIBRATION\n\nThe inverter will run its own calibration. The battery will discharge, charge and balance over several hours. You can stop it with Stop Calibration.\n\nContinue?',
              )
            }
            disabled={saving || isActive || awaitingReading}
            className={buttonClass}
          >
            {label(CALIBRATION_START, 'Start Calibration')}
          </button>
          <button
            type="button"
            onClick={() =>
              void send(
                CALIBRATION_CHARGE_ONLY,
                'Charge the battery to full without discharging it first?\n\nThe inverter runs the charging part of its calibration only.',
              )
            }
            disabled={saving || isActive || awaitingReading}
            className={buttonClass}
          >
            {label(CALIBRATION_CHARGE_ONLY, 'Charge Only')}
          </button>
          <button
            type="button"
            onClick={() => void send(CALIBRATION_STOP, 'Stop the battery calibration?')}
            disabled={saving || !canStop || awaitingReading}
            className={buttonClass}
          >
            {label(CALIBRATION_STOP, 'Stop Calibration')}
          </button>
          <span className="text-text-secondary text-xs shrink-0">
            Stage:{' '}
            <span className={`font-mono ${isActive ? 'text-amber-400' : 'text-text-primary'}`}>
              {STAGE_LABELS[stage] || `Unknown (${stage})`}
            </span>
          </span>
        </div>
        {error && <p className="text-red-400 text-xs">{error}</p>}
      </div>
    </div>
  );
}
