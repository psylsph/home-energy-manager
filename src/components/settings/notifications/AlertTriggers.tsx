import Toggle from '../../Toggle';
import { SettingsGroup, SettingsRow } from '../SettingsSection';
import { FIELD_LABEL, TEXT_INPUT, type AlertsConfig, type AlertsFormProps } from './alertsConfig';

type BooleanField = {
  [K in keyof AlertsConfig]: AlertsConfig[K] extends boolean ? K : never;
}[keyof AlertsConfig];

const TRIGGERS: { key: BooleanField; label: string; hint?: string }[] = [
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
    hint: "Checked once a day: notify when the gap between a battery's highest and lowest cell near full charge keeps growing, an early sign of a weakening cell",
  },
  {
    key: 'soc_anomaly_alert_enabled',
    label: 'Battery SOC Jumps or Sticks',
    hint: "Notify when a battery's state of charge jumps in one step or stays put while it charges or discharges (the BMS SOC bug)",
  },
  {
    key: 'connection_lost_enabled',
    label: 'Connection Lost',
    hint: 'Notify when contact with the inverter is dropped and restored',
  },
];

/** Which conditions alert, plus the cooldown between alerts. */
export default function AlertTriggers({ config, update }: AlertsFormProps) {
  return (
    <SettingsGroup title="Alert Triggers & Cooldown">
      {TRIGGERS.map(({ key, label, hint }) => (
        <div key={key} className="contents">
          <SettingsRow label={label} hint={hint}>
            <Toggle checked={config[key]} ariaLabel={label} onChange={(v) => update({ [key]: v })} />
          </SettingsRow>
          {key === 'solar_clipping_enabled' && config.solar_clipping_enabled && (
            <label className="flex items-center justify-between gap-3">
              <span className={FIELD_LABEL}>
                Clipping Ceiling (W) — rated AC output, 0 = off
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
