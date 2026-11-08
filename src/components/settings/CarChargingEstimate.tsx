import { useState } from 'react';
import { apiPost } from '../../lib/api';
import { DEFAULT_CHARGER_AMPS } from '../../lib/carCharging';
import { parseLoadAboveKw } from '../../lib/forecast';
import { parseChargerCurrentA } from '../../lib/settingsForm';
import type { PollSettings } from '../../lib/types';
import { SettingsGroup } from './SettingsSection';
import { useInverterStore } from '../../store/useInverterStore';
import { useSettingsSave, type Flash } from './useSettingsSave';

const INPUT =
  'bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono w-28 border border-transparent focus:outline-none focus:border-accent';

/**
 * Settings → EV Charger → Car Charging Estimate (issue #454, built on #400): in a house where only
 * the car pushes home load past a known level, any minute above the
 * threshold is the car charging, drawing the charger's current at the grid
 * voltage. History, the Power page, the forecast and the reports take the
 * car out of Home using this. Empty threshold turns it off. Text fields, not
 * number ones: a number input reports a mistyped entry ("7,4") as empty,
 * which would read as "off" instead of an error.
 */
export default function CarChargingEstimate({
  flash,
  initial,
}: {
  flash: Flash;
  initial: Partial<PollSettings>;
}) {
  const [threshold, setThreshold] = useState(() =>
    initial.forecast_exclude_load_above_kw == null ? '' : String(initial.forecast_exclude_load_above_kw),
  );
  const [amps, setAmps] = useState(() => String(initial.car_charger_current_a ?? DEFAULT_CHARGER_AMPS));
  const [thresholdError, setThresholdError] = useState<string | null>(null);
  const [ampsError, setAmpsError] = useState<string | null>(null);
  const { busy: saving, run } = useSettingsSave(flash);

  const handleSave = () => {
    const kw = parseLoadAboveKw(threshold);
    const current = parseChargerCurrentA(amps);
    setThresholdError(kw.ok ? null : kw.error);
    setAmpsError(current.ok ? null : current.error);
    if (!kw.ok || !current.ok) return;
    void run(async () => {
      await apiPost('/api/settings', {
        forecast_exclude_load_above_kw: kw.kw,
        car_charger_current_a: current.amps,
      });
      // The Status diagrams read these from the store (no refresh needed).
      useInverterStore.getState().setCarCharging({
        forecast_exclude_load_above_kw: kw.kw,
        car_charger_current_a: current.amps,
      });
      return 'Car charging saved';
    }, 'Failed to save car charging');
  };

  return (
    <SettingsGroup
      title="Car Charging Estimate"
      description="Without a charger HEM reads, estimate the car from home load."
      help="Any minute where home load is above the threshold is treated as the car charging, drawing the charger current at the grid voltage (32 A is about 7.4 kW). History, the Power page, the Planner's consumption forecast and the reports then show it as Car Charging instead of Home. It only suits a house where nothing but the car goes over the threshold. A GivEnergy EV charger that HEM reads (above) uses its own measured power instead. The estimate is worked out when the data is read, so changing either value re-works past history too. Leave the threshold empty to turn this off."
    >
      <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
        <div className="flex flex-col gap-1.5">
          <label htmlFor="car-charging-threshold" className="text-text-primary text-sm font-sans font-medium">
            Car Charging Threshold
          </label>
          <div className="flex items-center gap-2">
            <input
              id="car-charging-threshold"
              type="text"
              inputMode="decimal"
              placeholder="Off"
              value={threshold}
              onChange={(e) => setThreshold(e.target.value)}
              className={INPUT}
              data-testid="car-charging-threshold-input"
            />
            <span className="text-text-secondary text-xs font-sans">kW</span>
          </div>
          {thresholdError && (
            <span data-testid="car-charging-threshold-error" className="text-xs text-red-400">
              {thresholdError}
            </span>
          )}
          <span className="text-text-secondary text-xs font-sans">Home load above this is the car. Empty is off.</span>
        </div>
        <div className="flex flex-col gap-1.5">
          <label htmlFor="car-charging-current" className="text-text-primary text-sm font-sans font-medium">
            Charger Current
          </label>
          <div className="flex items-center gap-2">
            <input
              id="car-charging-current"
              type="text"
              inputMode="numeric"
              value={amps}
              onChange={(e) => setAmps(e.target.value)}
              className={INPUT}
              data-testid="car-charging-current-input"
            />
            <span className="text-text-secondary text-xs font-sans">A</span>
          </div>
          {ampsError && (
            <span data-testid="car-charging-current-error" className="text-xs text-red-400">
              {ampsError}
            </span>
          )}
          <span className="text-text-secondary text-xs font-sans">Single-phase chargers; 32 A is a 7 kW unit.</span>
        </div>
      </div>

      <button
        onClick={handleSave}
        className="bg-accent text-on-accent font-sans font-semibold text-sm px-5 py-2 rounded-lg hover:opacity-90 transition-opacity disabled:opacity-30 self-start"
        disabled={saving}
        data-testid="car-charging-save"
      >
        {saving ? 'Saving…' : 'Save Car Charging'}
      </button>
    </SettingsGroup>
  );
}
