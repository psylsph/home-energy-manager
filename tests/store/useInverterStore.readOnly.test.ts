import { beforeEach, describe, expect, it, vi } from 'vitest';

describe('read-only session persistence', () => {
  beforeEach(() => {
    localStorage.clear();
    sessionStorage.clear();
    vi.resetModules();
  });

  it('ignores and removes the obsolete permanent localStorage flag on startup', async () => {
    localStorage.setItem('readOnly', 'true');

    const { useInverterStore } = await import('../../src/store/useInverterStore');

    expect(useInverterStore.getState().readOnly).toBe(false);
    expect(localStorage.getItem('readOnly')).toBeNull();
  });

  it('restores read-only mode from sessionStorage on startup', async () => {
    sessionStorage.setItem('readOnly', 'true');

    const { useInverterStore } = await import('../../src/store/useInverterStore');

    expect(useInverterStore.getState().readOnly).toBe(true);
  });
});