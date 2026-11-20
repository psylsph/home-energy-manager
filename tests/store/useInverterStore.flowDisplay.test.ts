import { beforeEach, describe, expect, it, vi } from 'vitest';

// Issue #498: Show Node Status Words and Home Excludes Car Charging moved
// from each browser's localStorage, which forgot them whenever site data
// went, to settings.json. The store starts from the defaults until App loads
// the settings, and clears the obsolete keys.
describe('energy flow display switches at startup', () => {
  beforeEach(() => {
    localStorage.clear();
    vi.resetModules();
  });

  it('starts from the defaults whatever the old keys say, and removes them', async () => {
    localStorage.setItem('showFlowStatusWords', 'false');
    localStorage.setItem('homeExcludesCar', 'true');
    const { useInverterStore } = await import('../../src/store/useInverterStore');
    expect(useInverterStore.getState().showFlowStatusWords).toBe(true);
    expect(useInverterStore.getState().homeExcludesCar).toBe(false);
    expect(localStorage.getItem('showFlowStatusWords')).toBeNull();
    expect(localStorage.getItem('homeExcludesCar')).toBeNull();
  });

  it('never writes the old keys back', async () => {
    const { useInverterStore } = await import('../../src/store/useInverterStore');
    useInverterStore.getState().setShowFlowStatusWords(false);
    useInverterStore.getState().setHomeExcludesCar(true);
    expect(useInverterStore.getState().showFlowStatusWords).toBe(false);
    expect(useInverterStore.getState().homeExcludesCar).toBe(true);
    expect(localStorage.getItem('showFlowStatusWords')).toBeNull();
    expect(localStorage.getItem('homeExcludesCar')).toBeNull();
  });
});
