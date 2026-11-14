/**
 * Whether the address asks for read-only mode (`?RO`, issues #114, #471):
 * in the query before the hash, or in the hash route's own query
 * (`#/forecast?RO`). Pure, so it's testable without a window.
 */
export function addressIsReadOnly(search: string, hash: string): boolean {
  const has = (query: string) => {
    try {
      return new URLSearchParams(query).has('RO');
    } catch {
      return false;
    }
  };
  const hashQuery = hash.includes('?') ? hash.slice(hash.indexOf('?')) : '';
  return has(search) || has(hashQuery);
}
