/** The notification settings, as GET/POST /api/alerts carry them. */
export interface AlertsConfig {
  enabled: boolean;
  telegram_bot_token: string;
  telegram_chat_id: string;
  cooldown_minutes: number;
  batt_temp_min: number;
  batt_temp_max: number;
  inverter_temp_min: number;
  inverter_temp_max: number;
  soc_min: number;
  soc_max: number;
  grid_offline_enabled: boolean;
  inverter_trip_enabled: boolean;
  battery_over_temp_enabled: boolean;
  battery_connection_lost_enabled: boolean;
  cell_spread_alert_enabled: boolean;
  soc_anomaly_alert_enabled: boolean;
  connection_lost_enabled: boolean;
  solar_clipping_enabled: boolean;
  solar_clipping_ceiling_w: number;
  ntfy_topic: string;
  ntfy_server: string;
  pushover_app_token: string;
  pushover_user_key: string;
}

/**
 * Defaults the form starts from. Older backends may omit fields added later,
 * so a loaded config is merged over these: every input stays controlled and
 * nothing is sent back as `undefined`.
 */
export const DEFAULT_ALERTS_CONFIG: AlertsConfig = {
  enabled: false, telegram_bot_token: '', telegram_chat_id: '',
  cooldown_minutes: 30,
  batt_temp_min: 0, batt_temp_max: 0,
  inverter_temp_min: 8, inverter_temp_max: 60,
  soc_min: 4, soc_max: 100,
  grid_offline_enabled: false, inverter_trip_enabled: false, battery_over_temp_enabled: false,
  battery_connection_lost_enabled: true,
  cell_spread_alert_enabled: true,
  soc_anomaly_alert_enabled: true,
  connection_lost_enabled: false,
  solar_clipping_enabled: false, solar_clipping_ceiling_w: 0,
  ntfy_topic: '',
  ntfy_server: 'https://ntfy.sh',
  pushover_app_token: '',
  pushover_user_key: '',
};

/** Props every notification sub-form takes: the config and a patcher. */
export interface AlertsFormProps {
  config: AlertsConfig;
  update: (patch: Partial<AlertsConfig>) => void;
}

export { TEXT_INPUT } from '../styles';
export const FIELD_LABEL = 'text-text-secondary text-xs font-sans';
export const LINK = 'text-accent underline hover:opacity-80 inline';

/**
 * The ntfy topic to save: the user's own, or `hem-<serial>` when they left it
 * blank, so ntfy works without making them invent one. `generated` is the
 * serial-based default ('' with no serial known).
 */
export function ntfyTopic(config: Pick<AlertsConfig, 'ntfy_topic'>, serial: string): { generated: string; effective: string } {
  const generated = serial ? `hem-${serial}` : '';
  return { generated, effective: config.ntfy_topic || generated };
}

/** True when at least one provider has what it needs to send. */
export function hasChannel(config: AlertsConfig, effectiveNtfyTopic: string): boolean {
  return (
    (Boolean(config.telegram_bot_token) && Boolean(config.telegram_chat_id)) ||
    Boolean(effectiveNtfyTopic) ||
    (Boolean(config.pushover_app_token) && Boolean(config.pushover_user_key))
  );
}
