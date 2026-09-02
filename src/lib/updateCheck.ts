/**
 * Pure helpers for the "new version available" banner. Kept separate from the
 * component so the version-comparison logic is unit-testable without React.
 *
 * The backend already computes `update_available` in the `/api/latest-version`
 * payload, but the frontend needs to recompute it locally too: the dismissal
 * check ("did the user already hide *this* version?") compares versions, and
 * we want the banner logic robust even if a future payload omits the flag.
 */

/**
 * How often the mounted app re-fetches `/api/latest-version` to refresh the
 * banner (issue #296). An instance left running unattended for days used to
 * freeze its banner at whatever release was current at page load, pointing
 * "View release" at a stale release page. One hour keeps an unattended
 * instance within ~an hour of the real latest release while staying far
 * under the backend's GitHub rate-limit budget: each poke triggers at most
 * one on-demand fetch (`ON_DEMAND_MIN_INTERVAL` = 60s) against GitHub's
 * unauthenticated ceiling of 60 requests/hour/IP, on top of the backend's
 * own 6-hour loop.
 */
export const UPDATE_REFRESH_INTERVAL_MS = 60 * 60 * 1000;

/**
 * Parse the leading `major.minor.patch` triple out of a version string.
 *
 * Accepts an optional leading `v`/`V` and ignores anything after the third
 * numeric component (prerelease suffixes like `-rc.1`, build metadata). Used
 * both for comparison and for normalising the tag stored in the dismissal
 * record so `v0.70.3` and `0.70.3` are treated as the same version.
 *
 * Returns `null` for strings that don't begin with at least `N.N.N`.
 */
export function parseVersion(input: string): [number, number, number] | null {
  const cleaned = input.trim().replace(/^[vV]/, '');
  const match = cleaned.match(/^(\d+)\.(\d+)\.(\d+)/);
  if (!match) return null;
  const major = Number(match[1]);
  const minor = Number(match[2]);
  const patch = Number(match[3]);
  if (!Number.isFinite(major) || !Number.isFinite(minor) || !Number.isFinite(patch)) {
    return null;
  }
  return [major, minor, patch];
}

/**
 * True when `latest` is a strictly newer release than `current`. Returns
 * `false` if either string fails to parse — a malformed value must never
 * trigger a spurious banner.
 */
export function isUpdateAvailable(current: string, latest: string): boolean {
  const a = parseVersion(latest);
  const b = parseVersion(current);
  if (!a || !b) return false;
  if (a[0] !== b[0]) return a[0] > b[0];
  if (a[1] !== b[1]) return a[1] > b[1];
  return a[2] > b[2];
}

/**
 * Normalise a version string for use as the dismissal key. Strips a leading
 * `v` and any prerelease/build suffix so `v0.70.3-rc.1` and `0.70.3` collapse
 * to the same key `0.70.3`. Returns the trimmed base (`"0.70.3"`) or the
 * raw input lower-cased if it doesn't parse (defensive — still a usable key).
 */
export function normaliseVersionKey(input: string): string {
  const parsed = parseVersion(input);
  if (parsed) return parsed.join('.');
  return input.trim().replace(/^[vV]/, '').toLowerCase();
}
