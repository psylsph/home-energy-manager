import { openExternal } from '../../../lib/openExternal';
import { SettingsGroup } from '../SettingsSection';
import { FIELD_LABEL, LINK, TEXT_INPUT, type AlertsFormProps } from './alertsConfig';

export default function PushoverSettings({ config, update }: AlertsFormProps) {
  return (
    <SettingsGroup
      title="Pushover"
      help="Paid-once-per-platform push notifications. Create an application at pushover.net/apps/build to get your App API Token, then copy your User Key from your Pushover account settings."
    >
      <p className="text-text-secondary/70 text-xs font-sans">
        Push via{' '}
        <button onClick={() => openExternal('https://pushover.net')} className={LINK}>Pushover</button>; create an
        app at{' '}
        <button onClick={() => openExternal('https://pushover.net/apps/build')} className={LINK}>pushover.net/apps/build</button>.
      </p>
      <label className="flex flex-col gap-1">
        <span className={FIELD_LABEL}>App API Token (from pushover.net/apps/build)</span>
        <input
          type="password" placeholder="Your App API Token"
          value={config.pushover_app_token}
          onChange={(e) => update({ pushover_app_token: e.target.value })}
          className={TEXT_INPUT}
        />
      </label>
      <label className="flex flex-col gap-1">
        <span className={FIELD_LABEL}>User Key (from Your Pushover Account)</span>
        <input
          type="password" placeholder="Your User Key"
          value={config.pushover_user_key}
          onChange={(e) => update({ pushover_user_key: e.target.value })}
          className={TEXT_INPUT}
        />
      </label>
    </SettingsGroup>
  );
}
