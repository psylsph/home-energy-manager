/**
 * When the battery reaches its reserve (issue #472), with one to three dots
 * for how sure the forecast is. Off unless Show Battery Runtime is on. The
 * house view puts it in its Battery Level card; the orbit, which has no such
 * card, puts it under the diagram behind a battery icon.
 */
import { useBatteryRuntime } from '../hooks/useBatteryRuntime';
import { batteryRuntimeLine } from '../lib/batteryRuntime';
import { useNow } from '../hooks/useNow';
import HelpTip from './HelpTip';

/** How often the row re-checks its answer's age without a new one. */
const AGE_CHECK_MS = 60_000;

/**
 * `now` pins the clock (tests); otherwise the row keeps its own, so a kept
 * answer still goes once it's stale while the backend is unreachable.
 */
export default function BatteryRuntimeRow({ now, view }: { now?: Date; view: 'house' | 'orbit' }) {
  const tick = useNow(AGE_CHECK_MS);
  const line = batteryRuntimeLine(useBatteryRuntime(), now ?? new Date(tick));
  if (!line) return null;
  const confidence = line.confidence
    ? `${line.confidence[0]!.toUpperCase()}${line.confidence.slice(1)} confidence`
    : null;
  const orbit = view === 'orbit';
  return (
    <span
      // Wraps rather than truncating: the time is the point, so on a phone
      // the dots and the ⓘ go to the next line instead of cutting it off.
      className={`flex flex-wrap items-center gap-x-1.5 gap-y-0.5 min-w-0 leading-tight text-text-secondary ${
        orbit ? 'justify-center px-2 pb-1.5 text-xs' : 'text-[11px]'
      }`}
      data-testid={`${view}-battery-runtime`}
      title={line.title || undefined}
    >
      {orbit && (
        <>
          {/* An icon rather than a label, so the line keeps one font. */}
          <span className="sr-only">Battery: </span>
          <svg
            width="15" height="9" viewBox="0 0 15 9" aria-hidden="true" className="shrink-0"
            data-testid="orbit-runtime-icon"
          >
            <rect x="0.75" y="0.75" width="12" height="7.5" rx="1.5" fill="none" stroke="currentColor" strokeWidth="1.5" />
            <rect x="13.5" y="3" width="1.5" height="3" rx="0.5" fill="currentColor" />
          </svg>
        </>
      )}
      {/* One 16px line height, the ⓘ's size, so text, dots and ⓘ share a centre. */}
      <span className={`min-w-0${orbit ? ' leading-4' : ''}`} data-testid={`${view}-runtime-text`}>{line.text}</span>
      {line.dots != null && (
        <span className="flex gap-0.5 shrink-0" role="img" aria-label={confidence ?? undefined} data-testid={`${view}-runtime-dots`}>
          {[1, 2, 3].map((n) => (
            <span
              key={n}
              data-testid={`${view}-runtime-dot`}
              data-lit={n <= line.dots!}
              className="h-1.5 w-1.5 rounded-full"
              style={{ backgroundColor: n <= line.dots! ? 'var(--color-text-secondary)' : 'var(--color-bg-elevated)' }}
            />
          ))}
        </span>
      )}
      {/* The dots need explaining, and phones have no hover title. */}
      {line.help && <HelpTip label="Battery Runtime" className="shrink-0">{line.help}</HelpTip>}
    </span>
  );
}
