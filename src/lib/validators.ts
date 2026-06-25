// Small input validators shared by the Settings page.
// Kept here (rather than inline in a component) so they're easy to
// unit-test and reuse if the same rules are needed elsewhere later.

/**
 * Returns true if `value` is a syntactically valid IPv4 address:
 * four decimal octets in the range 0–255, separated by dots.
 *
 * Empty strings and whitespace-only strings return false — callers
 * that want "blank means disabled" must check `value === ''`
 * before calling this.
 */
export function isValidIpv4Host(value: string): boolean {
  if (value.trim() === '') return false;
  const parts = value.split('.');
  if (parts.length !== 4) return false;
  for (const part of parts) {
    // Reject leading zeros (e.g. "01") and anything that isn't
    // purely digits — keeps the format strict as requested by
    // users entering hostnames/typos like "10.1.71".
    if (!/^(0|[1-9]\d*)$/.test(part)) return false;
    const n = Number(part);
    if (!Number.isInteger(n) || n < 0 || n > 255) return false;
  }
  return true;
}