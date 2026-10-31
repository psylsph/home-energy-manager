import { SettingsGroup } from '../SettingsSection';
import { FIELD_LABEL, TEXT_INPUT, type AlertsConfig, type AlertsFormProps } from './alertsConfig';

type NumericField = {
  [K in keyof AlertsConfig]: AlertsConfig[K] extends number ? K : never;
}[keyof AlertsConfig];

const TEMPERATURE_FIELDS: { key: NumericField; label: string; step: string; min: string; max: string }[] = [
  { key: 'batt_temp_min', label: 'Battery Temp Below °C', step: '0.5', min: '-40', max: '50' },
  { key: 'batt_temp_max', label: 'Battery Temp Above °C', step: '0.5', min: '0', max: '80' },
  { key: 'inverter_temp_min', label: 'Inverter Temp Below °C', step: '0.5', min: '-40', max: '100' },
  { key: 'inverter_temp_max', label: 'Inverter Temp Above °C', step: '0.5', min: '0', max: '120' },
];

const SOC_FIELDS: { key: NumericField; label: string }[] = [
  { key: 'soc_min', label: 'SOC Below %' },
  { key: 'soc_max', label: 'SOC Above %' },
];

/**
 * Temperature and SOC alert thresholds. A Gateway reports no temperature
 * telemetry, so it gets the SOC thresholds only.
 */
export default function AlertThresholds({
  config,
  update,
  isGateway,
}: AlertsFormProps & { isGateway: boolean }) {
  const field = (key: NumericField, label: string, attrs: { step?: string; min: string; max: string }) => (
    <label key={key} className="flex flex-col gap-1">
      <span className={FIELD_LABEL}>{label}</span>
      <input
        type="number" {...attrs}
        value={config[key]}
        onChange={(e) => update({ [key]: Number(e.target.value) })}
        className={`${TEXT_INPUT} w-full`}
      />
    </label>
  );
  return (
    <SettingsGroup
      title={isGateway ? 'SOC' : 'Temperature & SOC'}
      description={
        isGateway
          ? 'Gateway does not expose battery or inverter temperature telemetry, so temperature alerts are hidden.'
          : 'Battery and inverter temperature alerts only work with devices that report temperature telemetry.'
      }
    >
      <div className="grid grid-cols-2 sm:grid-cols-4 gap-3">
        {!isGateway && TEMPERATURE_FIELDS.map(({ key, label, ...attrs }) => field(key, label, attrs))}
        {SOC_FIELDS.map(({ key, label }) => field(key, label, { min: '0', max: '100' }))}
      </div>
    </SettingsGroup>
  );
}
