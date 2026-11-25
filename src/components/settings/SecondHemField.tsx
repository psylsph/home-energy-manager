import { useState } from 'react';
import { apiPost } from '../../lib/api';
import { secondUnitStatus } from '../../lib/secondUnit';
import { useInverterStore } from '../../store/useInverterStore';
import HelpTip from '../HelpTip';
import { TEXT_INPUT } from './styles';
import { useSettingsSave, type Flash } from './useSettingsSave';

/**
 * Settings → Inverter Connection → Second HEM Address (issue #456).
 *
 * An optional address of another HEM that reads a second inverter on the same
 * supply. HEM only ever asks it for its snapshot; it never sends it anything.
 * Saved on its own, so changing it never touches or reconnects the inverter.
 */
export default function SecondHemField({
  initialUrl,
  flash,
}: {
  initialUrl: string;
  flash: Flash;
}) {
  const [url, setUrl] = useState(initialUrl);
  const [savedUrl, setSavedUrl] = useState(initialUrl);
  const info = useInverterStore((s) => s.snapshot?.second_unit);
  const { busy, run } = useSettingsSave(flash);
  const status = savedUrl ? secondUnitStatus(info) : '';

  const save = () =>
    run(async () => {
      const next = url.trim();
      await apiPost('/api/settings', { second_hem_url: next });
      setUrl(next);
      setSavedUrl(next);
      return next ? 'Second HEM saved' : 'Second HEM removed';
    }, 'Failed to save the second HEM address');

  return (
    <div className="flex flex-col gap-1">
      <label htmlFor="second-hem-url" className="text-text-secondary text-xs font-sans">
        Second HEM Address <span className="opacity-50">(optional)</span>
      </label>
      <p className="text-text-secondary text-xs font-sans">
        Another HEM reading your other inverter, so Home shows the whole house.{' '}
        <HelpTip label="Second HEM Address">
          Use this when two inverters share one supply and neither sees the whole house, for example
          an All-in-One behind a Gateway next to an AC-coupled inverter that has the solar. Run a
          second HEM on the other inverter and enter its address here, such as
          http://192.168.1.50:7338. This HEM only reads its snapshot and never sends it anything.
          Home, Solar and Grid power and the day totals then cover both inverters; the battery,
          schedules and every control stay on this one.
        </HelpTip>
      </p>
      <div className="flex gap-2">
        <input
          id="second-hem-url"
          type="text"
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder="e.g. http://second-pc:7338"
          className={`${TEXT_INPUT} flex-1 min-w-0`}
        />
        <button
          onClick={() => void save()}
          disabled={busy || url.trim() === savedUrl}
          className="bg-bg-elevated text-text-primary font-sans text-sm px-5 py-2 rounded-lg hover:bg-bg-base transition-colors disabled:opacity-40"
        >
          {busy ? 'Saving…' : 'Save'}
        </button>
      </div>
      {status && <p className="text-text-secondary text-xs font-sans">{status}</p>}
    </div>
  );
}
