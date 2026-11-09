import { useState } from 'react';
import {
  PART_LABELS,
  followableParts,
  joinParts,
  partStatus,
  updatedText,
  type AppliedParts,
  type PartName,
  type TariffPreview,
  type TariffSource,
} from '../../lib/tariffSource';
import { SettingsGroup } from './SettingsSection';

const SEGMENT =
  'px-4 py-2 text-sm font-sans font-medium transition-colors disabled:opacity-40 disabled:cursor-not-allowed';
const BUTTON =
  'bg-bg-elevated text-text-primary font-sans font-semibold text-sm px-4 py-2 rounded-lg hover:opacity-90 disabled:opacity-40 disabled:cursor-not-allowed transition-opacity';
const PRIMARY =
  'bg-accent text-on-accent font-sans font-semibold text-sm px-4 py-2 rounded-lg hover:opacity-90 disabled:opacity-40 disabled:cursor-not-allowed transition-opacity';

/**
 * Manual | From Octopus (issue #455). Following Octopus fills what the
 * supplier's rates can express as fixed daily windows and keeps it current;
 * any part that can't be (Agile, say) stays manual, with the reason shown.
 * Choosing Octopus over hand-entered numbers asks first.
 */
export default function TariffSourceControl({
  source,
  preview,
  applied,
  replaced,
  busy,
  error,
  onSwitch,
}: {
  source: TariffSource;
  preview: TariffPreview | null;
  applied: AppliedParts;
  /** Parts holding hand-entered values that switching to Octopus would overwrite. */
  replaced: PartName[];
  busy: boolean;
  error: string | null;
  onSwitch: (next: TariffSource) => void | Promise<void>;
}) {
  const [confirming, setConfirming] = useState(false);

  const choose = (next: TariffSource) => {
    if (next === source) return;
    if (next === 'octopus' && replaced.length > 0) {
      setConfirming(true);
      return;
    }
    setConfirming(false);
    void onSwitch(next);
  };

  const followable = followableParts(preview);
  const updated = updatedText(applied.at);

  return (
    <SettingsGroup
      title="Tariff Source"
      description="Type your rates in, or follow the rates Octopus charges."
      help="From Octopus fills in the windows and rates your Octopus agreement charges, plus the standing charge, and keeps them up to date after each sync. It only does this for tariffs that repeat the same windows every day. A tariff that changes every half hour, such as Agile, stays manual."
    >
      <div role="group" aria-label="Tariff Source" className="inline-flex self-start rounded-lg overflow-hidden border border-white/10">
        {(
          [
            ['manual', 'Manual'],
            ['octopus', 'From Octopus'],
          ] as const
        ).map(([value, label]) => (
          <button
            key={value}
            type="button"
            aria-pressed={source === value}
            disabled={busy}
            onClick={() => choose(value)}
            className={`${SEGMENT} ${
              source === value ? 'bg-accent text-on-accent' : 'bg-bg-elevated text-text-primary hover:opacity-90'
            }`}
          >
            {label}
          </button>
        ))}
      </div>

      {source === 'manual' && followable.length > 0 && !confirming && (
        <div className="flex flex-wrap items-center gap-3">
          <span className="text-text-secondary text-xs font-sans">
            Octopus rates are available for {joinParts(followable)}.
          </span>
          <button type="button" className={BUTTON} disabled={busy} onClick={() => choose('octopus')}>
            Use My Octopus Rates
          </button>
        </div>
      )}

      {confirming && (
        <div role="alert" className="border border-white/10 rounded-xl p-3 flex flex-col gap-2">
          <p className="text-text-primary text-sm font-sans">
            This replaces your {joinParts(replaced)} {replaced.length === 1 ? 'rate' : 'rates'} with what
            Octopus charges and keeps them up to date. You can switch back to Manual at any time.
          </p>
          <div className="flex gap-3">
            <button
              type="button"
              className={PRIMARY}
              disabled={busy}
              onClick={() => {
                setConfirming(false);
                void onSwitch('octopus');
              }}
            >
              Use Octopus Rates
            </button>
            <button type="button" className={BUTTON} onClick={() => setConfirming(false)}>
              Cancel
            </button>
          </div>
        </div>
      )}

      {source === 'octopus' && (
        <ul className="flex flex-col gap-1 text-xs font-sans text-text-secondary">
          {(['import', 'export', 'gas'] as const)
            .filter((p) => p !== 'gas' || preview?.gas_enabled || applied.gas)
            .map((p) => (
              <li key={p} aria-label={`${PART_LABELS[p]} Source`}>
                {partStatus(p, preview, applied)}
              </li>
            ))}
          {updated && <li>{updated}</li>}
        </ul>
      )}

      {error && (
        <p role="alert" className="text-red-400 text-sm font-sans">
          {error}
        </p>
      )}
    </SettingsGroup>
  );
}
