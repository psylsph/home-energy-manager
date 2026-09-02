import { useInverterStore } from '../store/useInverterStore';
import { openExternal } from '../lib/openExternal';
import { isUpdateAvailable, normaliseVersionKey } from '../lib/updateCheck';

/**
 * Dismissible "a new version is available" banner, rendered once below the
 * app header (see `Layout` in `App.tsx`). It compares the installed version
 * (`current_version`) against the cached latest release and shows itself
 * only when the latest is strictly newer AND the user hasn't already
 * dismissed the banner for that specific release.
 *
 * The dismissal is recorded per-version (see `dismissUpdateVersion`), so
 * hiding the banner for v0.70.3 does not suppress it again when v0.70.4
 * ships. The backend's `/api/latest-version` provides the data; this
 * component recomputes the comparison locally so the per-version dismissal
 * logic stays self-contained and unit-testable.
 *
 * Nothing happens automatically — the link opens the GitHub release page in
 * the system browser (via `openExternal`, which uses the Tauri opener plugin
 * in the desktop shell) where the user can download the new build. If the
 * payload carries no URL, the fallback is the `releases/latest` redirect —
 * always the newest published release, matching what the backend's version
 * check fetches.
 */
export default function UpdateBanner() {
  const info = useInverterStore((s) => s.latestVersionInfo);
  const dismissed = useInverterStore((s) => s.dismissedUpdateVersion);
  const dismiss = useInverterStore((s) => s.dismissUpdateVersion);

  if (!info || info.disabled) return null;
  const latest = info.latest_version;
  if (!latest) return null;
  // Recompute locally rather than trusting the payload flag, so the
  // dismissal comparison and the "show" decision use the same logic.
  if (!isUpdateAvailable(info.current_version, latest)) return null;
  // Already dismissed for this exact release — stay quiet until a newer one.
  if (dismissed === normaliseVersionKey(latest)) return null;

  return (
    <div className="bg-amber-950/80 border-b border-amber-500/40 px-4 py-2 text-amber-100 text-sm">
      <div className="max-w-4xl mx-auto flex items-center gap-2">
        <span aria-hidden="true">⬆️</span>
        <strong>
          A new version is available — v{latest}
        </strong>
        <span className="text-amber-100/85 hidden sm:inline">
          (you're running v{info.current_version})
        </span>
        <button
          type="button"
          onClick={() => void openExternal(info.release_url ?? 'https://github.com/psylsph/home-energy-manager/releases/latest')}
          className="ml-auto shrink-0 rounded-full bg-amber-500/30 hover:bg-amber-500/50 border border-amber-500/30 px-3 py-1 text-xs font-semibold transition-colors"
        >
          View release
        </button>
        <button
          type="button"
          onClick={() => dismiss(normaliseVersionKey(latest))}
          aria-label="Dismiss update notice"
          title="Dismiss — I'll check the Settings page later"
          className="shrink-0 rounded-full hover:bg-amber-500/30 px-2 py-1 text-amber-100/80 transition-colors"
        >
          ✕
        </button>
      </div>
    </div>
  );
}
