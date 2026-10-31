import { openExternal } from '../../../lib/openExternal';
import { SettingsGroup } from '../SettingsSection';
import { FIELD_LABEL, LINK, TEXT_INPUT, type AlertsFormProps } from './alertsConfig';

export default function NtfySettings({
  config,
  update,
  generatedTopic,
  effectiveTopic,
  onCopyTopic,
}: AlertsFormProps & {
  /** `hem-<serial>`, or '' with no serial known. */
  generatedTopic: string;
  /** The topic that will be saved: the user's own or the generated one. */
  effectiveTopic: string;
  onCopyTopic: () => void;
}) {
  return (
    <SettingsGroup title="ntfy Push Notifications">
      <p className="text-text-secondary/70 text-xs font-sans">
        Free push notifications via&nbsp;
        <button onClick={() => openExternal('https://ntfy.sh')} className={LINK}>ntfy.sh</button>.
        Install the app on your phone and subscribe to the topic below.
        A topic is auto-generated from your inverter serial (unique to you) — edit it only if you want a custom one.
      </p>
      <label className="flex flex-col gap-1">
        <span className={FIELD_LABEL}>Topic (Subscribe to This in the ntfy App)</span>
        <div className="flex items-center gap-2">
          <input
            type="text"
            placeholder={generatedTopic || 'hem-your-inverter-serial'}
            value={config.ntfy_topic}
            onChange={(e) => update({ ntfy_topic: e.target.value })}
            className={`flex-1 ${TEXT_INPUT}`}
          />
          <button
            onClick={onCopyTopic}
            disabled={!effectiveTopic}
            className="shrink-0 bg-accent text-on-accent text-xs font-sans font-semibold px-3 py-2 rounded-lg hover:opacity-90 disabled:opacity-40 transition-opacity"
          >
            Copy
          </button>
        </div>
        {!config.ntfy_topic && generatedTopic && (
          <span className="text-text-secondary/60 text-xs font-sans">
            Using generated topic: <code className="font-mono">{generatedTopic}</code>
          </span>
        )}
        {!config.ntfy_topic && !generatedTopic && (
          <span className="text-text-secondary/50 text-xs font-sans italic">
            Connect to an inverter to auto-generate a topic, or enter one manually.
          </span>
        )}
      </label>
      <label className="flex flex-col gap-1">
        <span className={FIELD_LABEL}>Server (optional, default: ntfy.sh)</span>
        <input
          type="text" placeholder="https://ntfy.sh"
          value={config.ntfy_server}
          onChange={(e) => update({ ntfy_server: e.target.value })}
          className={TEXT_INPUT}
        />
      </label>
    </SettingsGroup>
  );
}
