import { beforeEach, describe, expect, it, vi } from 'vitest';

// Issue #476: Show Battery Runtime moved from each browser's localStorage to
// settings.json. The old per-browser value no longer turns it on, and the
// obsolete key is cleared, as the old readOnly flag was.
describe('battery runtime toggle at startup', () => {
  beforeEach(() => {
    localStorage.clear();
    vi.resetModules();
  });

  it('starts off whatever the old localStorage key says, and removes it', async () => {
    localStorage.setItem('showBatteryRuntime', 'true');
    const { useInverterStore } = await import('../../src/store/useInverterStore');
    expect(useInverterStore.getState().showBatteryRuntime).toBe(false);
    expect(localStorage.getItem('showBatteryRuntime')).toBeNull();
  });

  it('never writes the old key back', async () => {
    const { useInverterStore } = await import('../../src/store/useInverterStore');
    useInverterStore.getState().setShowBatteryRuntime(true);
    expect(useInverterStore.getState().showBatteryRuntime).toBe(true);
    expect(localStorage.getItem('showBatteryRuntime')).toBeNull();
  });
});
