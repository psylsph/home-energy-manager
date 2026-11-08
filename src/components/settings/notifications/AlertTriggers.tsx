import Toggle from '../../Toggle';
import { useInverterStore } from '../../../store/useInverterStore';
import { SettingsGroup, SettingsRow } from '../SettingsSection';
import {
  FIELD_LABEL,
  FULL_CHARGE_DAYS_MAX,
  FULL_CHARGE_DAYS_MIN,
  fullChargeDaysValid,
  TEXT_INPUT,
  type AlertsConfig,
  type AlertsFormProps,
} from './alertsConfig';

type BooleanField = {
  [K in keyof AlertsConfig]: AlertsConfig[K] extends boolean ? K : never;
}[keyof AlertsConfig];

const TRIGGERS: { key: BooleanField; label: string; hint?: string; help?: string }[] = [
  { key: 'grid_offline_enabled', label: 'Grid Offline' },
  { key: 'inverter_trip_enabled', label: 'Inverter Trip' },
  { key: 'battery_over_temp_enabled', label: 'Inverter Battery Warning' },
  {
    key: 'solar_clipping_enabled',
    label: 'Solar Clipping',
    hint: "Alert when generation sustains above your inverter's rated limit",
  },
  {
    key: 'battery_connection_lost_enabled',
    label: 'Battery Connection Lost',
    hint: 'Notify when a battery stops responding or its DC breaker trips (inverter voltage collapses while the battery stays online), and when it reconnects',
  },
  {
    key: 'cell_spread_alert_enabled',
    label: 'Battery Cell Spread Widening',
    hint: 'A daily check for a growing gap between cells near full charge',
    help: "Checked once a day: notify when the gap between a battery's highest and lowest cell near full charge keeps growing, an early sign of a weakening cell",
  },
  {
    key: 'soc_anomaly_alert_enabled',
    label: 'Battery SOC Jumps or Sticks',
    hint: "Notify when a battery's state of charge jumps in one step or stays put while it charges or discharges (the BMS SOC bug)",
  },
  {
    key: 'full_charge_alert_enabled',
    label: 'Battery Not Fully Charged',
    hint: "Notify when the battery hasn't reached 100% for a while",
    help: 'Checked once a day. The cells balance and the SOC reading re-anchors at full charge, so a battery held below 100% (by the charge cap, or by short winter days) slowly drifts.',
  },
  {
    key: 'car_charging_alert_enabled',
    label: 'Car Charging Started or Finished',
    hint: 'Notify when the car starts and stops charging, with the energy and time',
    help: "Needs a Car Charging Threshold: the car is the home load above it, or a GivEnergy charger's own power where HEM reads one. A session starts after three minutes of charging and ends after ten without, so a pause between slots doesn't send a second message.",
  },
  {
    key: 'connection_lost_enabled',
    label: 'Connection Lost',
    hint: 'Notify when contact with the inverter is dropped and restored',
  },
];

/** Which conditions alert, plus the cooldown between alerts. */
export default function AlertTriggers({ config, update }: AlertsFormProps) {
  // The car alert has nothing to go on without a threshold (issue #454).
  const carThreshold = useInverterStore((s) => s.carCharging.forecast_exclude_load_above_kw);
  return (
    <SettingsGroup title="Alert Triggers & Cooldown">
      {TRIGGERS.map(({ key, label, hint, help }) => (
        <div key={key} className="contents">
          <SettingsRow label={label} hint={hint} help={help}>
            <Toggle checked={config[key]} ariaLabel={label} onChange={(v) => update({ [key]: v })} />
          </SettingsRow>
          {key === 'full_charge_alert_enabled'
            && (config.full_charge_alert_enabled || !fullChargeDaysValid(config.full_charge_alert_days)) && (
            <label className="flex items-center justify-between gap-3">
              <span className={FIELD_LABEL}>Days Without a Full Charge (7–90)</span>
              <input
                type="number" step="1" min={FULL_CHARGE_DAYS_MIN} max={FULL_CHARGE_DAYS_MAX}
                value={config.full_charge_alert_days}
                onChange={(e) => update({ full_charge_alert_days: Number(e.target.value) })}
                className={`${TEXT_INPUT} w-28 text-left`}
              />
            </label>
          )}
          {key === 'car_charging_alert_enabled' && carThreshold == null && (
            <p className="text-amber-300 text-xs font-sans -mt-1">
              Needs a Car Charging Threshold: set one in Settings → EV Charger.
            </p>
          )}
          {key === 'solar_clipping_enabled' && config.solar_clipping_enabled && (
            <label className="flex items-center justify-between gap-3">
              <span className={FIELD_LABEL}>
                Clipping Ceiling (W) — Rated AC Output, 0 = Off
              </span>
              <input
                type="number" step="100" min="0" max="100000"
                value={config.solar_clipping_ceiling_w}
                onChange={(e) => update({ solar_clipping_ceiling_w: Number(e.target.value) })}
                className={`${TEXT_INPUT} w-28 text-left`}
              />
            </label>
          )}
        </div>
      ))}
      <div className="flex flex-col gap-1">
        <span className="text-text-primary text-sm font-sans font-medium">Cooldown Timer</span>
        <label className="flex items-center justify-between gap-2">
          <span className={FIELD_LABEL}>Minutes Between All Alerts</span>
          <input
            type="number" min={1} max={1440}
            value={config.cooldown_minutes}
            onChange={(e) => update({ cooldown_minutes: Number(e.target.value) })}
            className={`${TEXT_INPUT} w-28 text-left`}
          />
        </label>
      </div>
    </SettingsGroup>
  );
}
