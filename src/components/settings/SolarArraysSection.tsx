import { useState } from 'react';
import { apiPost } from '../../lib/api';
import { cleanCtArrays, positiveOrZero, type CtArrayRow } from '../../lib/settingsForm';
import type { PollSettings } from '../../lib/types';
import { SettingsSection } from './SettingsSection';
import { useSettingsSave, type Flash } from './useSettingsSave';

const METER_ADDRESSES = [1, 2, 3, 4, 5, 6, 7, 8];

const kwpText = (kw: number | null | undefined) => (kw != null && kw > 0 ? String(kw) : '');

function KwpInput({ n, value, onChange, placeholder }: { n: 1 | 2; value: string; onChange: (v: string) => void; placeholder: string }) {
  return (
    <div className="flex flex-col gap-1.5">
      <label htmlFor={`pv${n}-rated-kw`} className="text-text-primary text-sm font-sans font-medium">
        PV{n} Rated Capacity (kWp)
      </label>
      <div className="flex items-center gap-2">
        <input
          id={`pv${n}-rated-kw`}
          type="number"
          min={0}
          max={100}
          step={0.01}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={placeholder}
          className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono w-28 border border-transparent focus:outline-none focus:border-accent"
          data-testid={`pv${n}-rated-kw-input`}
        />
        <span className="text-text-secondary text-xs font-sans">kWp</span>
      </div>
      <span className="text-text-secondary text-xs font-sans">
        Leave blank to hide PV{n} from the % display.
      </span>
    </div>
  );
}

/**
 * Settings → Solar Arrays (issue #110): rated kWp per array, so the Solar
 * page can show output as a % of maximum. Hybrids rate PV1/PV2; AC-coupled
 * systems label the CT clamp on their separate solar inverter.
 */
export default function SolarArraysSection({
  id,
  flash,
  initial,
}: {
  id?: string;
  flash: Flash;
  initial: Partial<PollSettings>;
}) {
  const [pv1, setPv1] = useState(() => kwpText(initial.pv1_rated_kw));
  const [pv2, setPv2] = useState(() => kwpText(initial.pv2_rated_kw));
  const [ctArrays, setCtArrays] = useState<CtArrayRow[]>(() =>
    (initial.solar_arrays ?? []).map((a) => ({
      meter_address: a.meter_address,
      name: a.name ?? '',
      rated_kw: kwpText(a.rated_kw),
    })),
  );
  const { busy: saving, run } = useSettingsSave(flash);

  const updateRow = (idx: number, patch: Partial<CtArrayRow>) =>
    setCtArrays((rows) => rows.map((r, i) => (i === idx ? { ...r, ...patch } : r)));

  const handleSave = () =>
    run(async () => {
      const cleaned = cleanCtArrays(ctArrays);
      await apiPost('/api/settings', {
        pv1_rated_kw: positiveOrZero(pv1),
        pv2_rated_kw: positiveOrZero(pv2),
        solar_arrays: cleaned,
      });
      // Show what was saved, so a second Save sends the same thing.
      setCtArrays(cleaned.map((a) => ({ ...a, rated_kw: kwpText(a.rated_kw) })));
      return 'Solar arrays saved';
    }, 'Failed to save solar arrays');

  return (
    <SettingsSection
      id={id}
      title="Solar Arrays"
      description="Each array's peak capacity, so the Solar page can show output as a percentage."
      help="Enter each solar array's rated peak capacity (kWp) so the Solar page can show output as a percentage of maximum. For hybrid / DC-coupled inverters, enter the kWp of each PV string below. For AC-coupled systems whose panels feed a separate inverter, label the CT clamp that measures that inverter (meter addresses 0x01–0x08 appear on the Meters page) and enter its array's kWp."
    >
      <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
        <KwpInput n={1} value={pv1} onChange={setPv1} placeholder="e.g. 6" />
        <KwpInput n={2} value={pv2} onChange={setPv2} placeholder="e.g. 4.2" />
      </div>

      <div className="flex flex-col gap-2">
        <div className="flex items-center justify-between">
          <span className="text-text-primary text-sm font-sans font-medium">External CT Meter Arrays</span>
          <button
            type="button"
            onClick={() => {
              // Default a new row to the lowest unused address.
              const used = new Set(ctArrays.map((a) => a.meter_address));
              const next = METER_ADDRESSES.find((n) => !used.has(n)) ?? 1;
              setCtArrays((rows) => [...rows, { meter_address: next, name: '', rated_kw: '' }]);
            }}
            className="bg-bg-elevated text-text-primary text-xs font-sans px-3 py-1.5 rounded-lg hover:bg-bg-elevated/80 transition-colors"
            data-testid="solar-array-add"
          >
            + Add Array
          </button>
        </div>
        {ctArrays.length === 0 ? (
          <p className="text-text-secondary/70 text-xs font-sans italic">
            No external CT meter arrays configured. Add one if a separate solar inverter is wired via a CT clamp.
          </p>
        ) : (
          <div className="flex flex-col gap-2">
            {ctArrays.map((row, idx) => (
              <div
                key={idx}
                className="grid grid-cols-1 sm:grid-cols-[auto,1fr,1fr,auto] gap-2 items-end bg-bg-elevated rounded-lg p-3"
                data-testid="solar-array-row"
              >
                <div className="flex flex-col gap-1">
                  <label className="text-text-secondary text-[10px] font-sans uppercase tracking-wide">Meter</label>
                  <select
                    value={row.meter_address}
                    onChange={(e) => updateRow(idx, { meter_address: Number(e.target.value) })}
                    className="bg-bg-base text-text-primary rounded-lg px-2 py-2 text-sm font-mono border border-transparent focus:outline-none focus:border-accent"
                    data-testid="solar-array-address"
                  >
                    {METER_ADDRESSES.map((n) => (
                      <option key={n} value={n}>0x{n.toString(16).padStart(2, '0')}</option>
                    ))}
                  </select>
                </div>
                <div className="flex flex-col gap-1">
                  <label className="text-text-secondary text-[10px] font-sans uppercase tracking-wide">Name</label>
                  <input
                    type="text"
                    value={row.name}
                    onChange={(e) => updateRow(idx, { name: e.target.value })}
                    placeholder="e.g. East roof"
                    className="bg-bg-base text-text-primary rounded-lg px-3 py-2 text-sm font-sans border border-transparent focus:outline-none focus:border-accent"
                    data-testid="solar-array-name"
                  />
                </div>
                <div className="flex flex-col gap-1">
                  <label className="text-text-secondary text-[10px] font-sans uppercase tracking-wide">kWp</label>
                  <input
                    type="number"
                    min={0}
                    max={100}
                    step={0.01}
                    value={row.rated_kw}
                    onChange={(e) => updateRow(idx, { rated_kw: e.target.value })}
                    placeholder="e.g. 6"
                    className="bg-bg-base text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-transparent focus:outline-none focus:border-accent"
                    data-testid="solar-array-kwp"
                  />
                </div>
                <button
                  type="button"
                  onClick={() => setCtArrays((rows) => rows.filter((_, i) => i !== idx))}
                  className="text-text-secondary hover:text-red-400 text-xs font-sans px-2 py-2 rounded-lg hover:bg-bg-base transition-colors"
                  aria-label="Remove Array"
                  data-testid="solar-array-remove"
                >
                  ✕
                </button>
              </div>
            ))}
          </div>
        )}
      </div>

      <button
        onClick={() => void handleSave()}
        className="bg-accent text-on-accent font-sans font-semibold text-sm px-5 py-2 rounded-lg hover:opacity-90 transition-opacity disabled:opacity-30 self-start"
        disabled={saving}
        data-testid="solar-arrays-save"
      >
        {saving ? 'Saving…' : 'Save Solar Arrays'}
      </button>
    </SettingsSection>
  );
}
