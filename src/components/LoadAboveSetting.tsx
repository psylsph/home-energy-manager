import { useState } from 'react';
import { apiPost } from '../lib/api';
import { useDeferredSave } from '../hooks/useDeferredSave';
import { parseLoadAboveKw } from '../lib/forecast';

interface Props {
  /** The saved threshold in kW, or null when off. Read once: remount (key)
   *  to resync, and not after this field's own saves, so an edit in
   *  progress keeps its text and focus. */
  savedKw: number | null;
  disabled?: boolean;
  /** Called after a save so the page can refetch the forecast. */
  onSaved: () => Promise<void> | void;
}

/**
 * "Leave out load above" (issue #400): home load above this is treated as EV
 * charging and left out of the consumption prediction. Empty turns it off.
 * A text field, not a number one: a number input reports a mistyped entry
 * ("3,5") as empty, which would read as "off" instead of an error.
 */
export default function LoadAboveSetting({ savedKw, disabled, onSaved }: Props) {
  const [input, setInput] = useState(savedKw == null ? '' : String(savedKw));
  const [error, setError] = useState<string | null>(null);

  const save = async () => {
    const parsed = parseLoadAboveKw(input);
    if (!parsed.ok) {
      setError(parsed.error);
      return;
    }
    setError(null);
    try {
      await apiPost('/api/settings', { forecast_exclude_load_above_kw: parsed.kw });
      await onSaved();
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Save failed');
    }
  };
  const saver = useDeferredSave(save);

  return (
    <div className="pt-3 flex flex-col gap-1">
      <div className="flex items-center gap-3 flex-wrap">
        <label
          htmlFor="forecast-load-above-input"
          className="text-text-primary text-xs font-sans font-medium"
        >
          Leave out load above
        </label>
        <input
          id="forecast-load-above-input"
          type="text"
          inputMode="decimal"
          placeholder="Off"
          value={input}
          onChange={(e) => {
            setInput(e.target.value);
            const parsed = parseLoadAboveKw(e.target.value);
            // An emptied field turns the setting off only on blur or Enter,
            // so clearing it to type a new value doesn't save "off" first.
            saver.edited(parsed.ok && parsed.kw !== null);
          }}
          onBlur={saver.flush}
          onKeyDown={(e) => {
            if (e.key === 'Enter') saver.flush();
          }}
          disabled={disabled}
          className="bg-bg-surface text-text-primary rounded-lg px-3 py-1.5 text-sm font-mono w-20 border border-transparent focus:outline-none focus:border-accent disabled:opacity-50"
          data-testid="forecast-load-above-input"
        />
        <span className="text-text-secondary text-xs font-sans">kW</span>
        {error && (
          <span data-testid="forecast-load-above-error" className="text-xs text-red-400">
            {error}
          </span>
        )}
      </div>
      <p className="text-[11px] text-text-secondary font-sans">
        Treats home load above this as EV charging and leaves it out of the consumption
        prediction, so an evening the car charged doesn't make the planner charge more every
        night. Leave it empty to keep all load. While HEM is connected to a GivEnergy EV
        charger it takes out the charger's own power instead.
      </p>
    </div>
  );
}
