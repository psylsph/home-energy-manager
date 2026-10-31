import { useEffect, useState } from 'react';
import { openExternal } from '../../../lib/openExternal';
import { SettingsGroup } from '../SettingsSection';
import { FIELD_LABEL, LINK, TEXT_INPUT, type AlertsFormProps } from './alertsConfig';

function StoreQr({ url, alt }: { url: string; alt: string }) {
  const [dataUrl, setDataUrl] = useState('');
  useEffect(() => {
    let cancelled = false;
    import('qrcode').then((qr) => {
      qr.toString(url, { type: 'svg', width: 120, margin: 1, color: { dark: '#4ade80', light: '#1a1a2e' } })
        .then((svg) => { if (!cancelled) setDataUrl('data:image/svg+xml;utf8,' + encodeURIComponent(svg)); });
    }).catch(() => {});
    return () => { cancelled = true; };
  }, [url]);
  if (!dataUrl) return <div className="w-[120px] h-[120px] bg-bg-elevated rounded-lg animate-pulse" />;
  return <img src={dataUrl} alt={alt} className="w-[120px] h-[120px] rounded-lg" />;
}

export default function TelegramSettings({ config, update }: AlertsFormProps) {
  return (
    <SettingsGroup title="Telegram">
      <p className={FIELD_LABEL}>
        <strong className="text-green-400">Recommended</strong> — Send alerts when critical conditions are detected. Create a bot via{' '}
        <button onClick={() => openExternal('https://t.me/botfather')} className={LINK}>@BotFather</button> on Telegram, get your bot token, then send /start to your bot and get your chat ID from{' '}
        <button onClick={() => openExternal('https://t.me/userinfobot')} className={LINK}>@userinfobot</button>.
        Once configured, send <code>/status</code>, <code>/today</code>, <code>/battery</code>, or <code>/help</code> in the chat to ask your inverter a question. The bot only replies to this chat id.
      </p>
      <label className="flex flex-col gap-1">
        <span className={FIELD_LABEL}>Bot Token (from @BotFather)</span>
        <input
          type="password" placeholder="123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11"
          value={config.telegram_bot_token}
          onChange={(e) => update({ telegram_bot_token: e.target.value })}
          className={TEXT_INPUT}
        />
      </label>
      <label className="flex flex-col gap-1">
        <span className={FIELD_LABEL}>Chat ID (from @userinfobot)</span>
        <input
          type="text" placeholder="123456789"
          value={config.telegram_chat_id}
          onChange={(e) => update({ telegram_chat_id: e.target.value })}
          className={TEXT_INPUT}
        />
      </label>
      <div className="flex justify-center gap-4 mt-1">
        <div className="flex flex-col items-center gap-1">
          <StoreQr url="https://play.google.com/store/apps/details?id=org.telegram.messenger" alt="Telegram Android" />
          <span className="text-text-secondary text-[10px] font-sans">Android</span>
        </div>
        <div className="flex flex-col items-center gap-1">
          <StoreQr url="https://apps.apple.com/app/telegram/id686449807" alt="Telegram iOS" />
          <span className="text-text-secondary text-[10px] font-sans">iOS</span>
        </div>
      </div>
    </SettingsGroup>
  );
}
