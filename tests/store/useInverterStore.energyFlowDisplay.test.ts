import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { useInverterStore } from '../../src/store/useInverterStore';

/**
 * Status page energy-flow display preference (issue #422): Orbit by default,
 * House on request, saved per device in localStorage.
 */
describe('useInverterStore — energyFlowDisplay (issue #422)', () => {
  beforeEach(() => {
    localStorage.removeItem('energyFlowDisplay');
    useInverterStore.setState({ energyFlowDisplay: 'orbit' });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    localStorage.removeItem('energyFlowDisplay');
    useInverterStore.setState({ energyFlowDisplay: 'orbit' });
  });

  it('switches to the house and back, saving each choice', () => {
    useInverterStore.getState().setEnergyFlowDisplay('house');
    expect(useInverterStore.getState().energyFlowDisplay).toBe('house');
    expect(localStorage.getItem('energyFlowDisplay')).toBe('house');

    useInverterStore.getState().setEnergyFlowDisplay('orbit');
    expect(useInverterStore.getState().energyFlowDisplay).toBe('orbit');
    expect(localStorage.getItem('energyFlowDisplay')).toBe('orbit');
  });

  it('ignores values that are not a known display', () => {
    useInverterStore.getState().setEnergyFlowDisplay('house');
    // An untyped caller (JSON, a URL param) must not be able to store junk.
    useInverterStore.getState().setEnergyFlowDisplay('radial' as never);
    expect(useInverterStore.getState().energyFlowDisplay).toBe('house');
    expect(localStorage.getItem('energyFlowDisplay')).toBe('house');
  });

  it('still switches when localStorage refuses the write', () => {
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('quota');
    });
    useInverterStore.getState().setEnergyFlowDisplay('house');
    expect(useInverterStore.getState().energyFlowDisplay).toBe('house');
  });

  describe('initial load', () => {
    const load = async () => {
      vi.resetModules();
      const mod = await import('../../src/store/useInverterStore');
      return mod.useInverterStore.getState().energyFlowDisplay;
    };

    it('defaults to the orbit when nothing is saved', async () => {
      expect(await load()).toBe('orbit');
    });

    it('restores a saved house choice', async () => {
      localStorage.setItem('energyFlowDisplay', 'house');
      expect(await load()).toBe('house');
    });

    it('falls back to the orbit for an unknown saved value', async () => {
      localStorage.setItem('energyFlowDisplay', 'radial');
      expect(await load()).toBe('orbit');
    });

    it('falls back to the orbit when localStorage cannot be read', async () => {
      vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
        throw new Error('denied');
      });
      expect(await load()).toBe('orbit');
    });
  });
});

/**
 * Show Today Statistics (issue #422, round 2): shown with the orbit, hidden
 * with the house view until the toggle is saved; per device in localStorage.
 */
describe('useInverterStore — showTodayStats', () => {
  beforeEach(() => {
    localStorage.removeItem('showTodayStats');
    useInverterStore.setState({ showTodayStats: true });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    localStorage.removeItem('showTodayStats');
    useInverterStore.setState({ showTodayStats: true });
  });

  it('hides and shows the tiles, saving each choice', () => {
    useInverterStore.getState().setShowTodayStats(false);
    expect(useInverterStore.getState().showTodayStats).toBe(false);
    expect(localStorage.getItem('showTodayStats')).toBe('false');
    useInverterStore.getState().setShowTodayStats(true);
    expect(useInverterStore.getState().showTodayStats).toBe(true);
    expect(localStorage.getItem('showTodayStats')).toBe('true');
  });

  it('still applies when localStorage refuses the write', () => {
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('quota');
    });
    useInverterStore.getState().setShowTodayStats(false);
    expect(useInverterStore.getState().showTodayStats).toBe(false);
  });

  describe('initial load', () => {
    const load = async () => {
      vi.resetModules();
      const mod = await import('../../src/store/useInverterStore');
      return mod.useInverterStore.getState().showTodayStats;
    };

    it('shows the tiles on the orbit when nothing is saved', async () => {
      expect(await load()).toBe(true);
    });

    it('hides them on the house view, which has to fit a phone', async () => {
      localStorage.setItem('energyFlowDisplay', 'house');
      expect(await load()).toBe(false);
    });

    it('restores a saved hide', async () => {
      localStorage.setItem('showTodayStats', 'false');
      expect(await load()).toBe(false);
    });

    it('keeps the tiles for any other saved value', async () => {
      // Not a real choice: the view's default applies.
      localStorage.setItem('showTodayStats', 'nope');
      expect(await load()).toBe(true);
    });

    it('keeps the tiles when localStorage cannot be read', async () => {
      vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
        throw new Error('denied');
      });
      expect(await load()).toBe(true);
    });
  });

  describe('switching views, before the toggle is saved', () => {
    beforeEach(() => {
      localStorage.removeItem('energyFlowDisplay');
      useInverterStore.setState({ energyFlowDisplay: 'orbit', showTodayStats: true });
    });

    afterEach(() => {
      localStorage.removeItem('energyFlowDisplay');
      localStorage.removeItem('showTodayStats');
    });

    it('starts the house without the tiles', () => {
      useInverterStore.getState().setEnergyFlowDisplay('house');
      expect(useInverterStore.getState().showTodayStats).toBe(false);
    });

    it('shows them again when the orbit comes back', () => {
      useInverterStore.getState().setEnergyFlowDisplay('house');
      useInverterStore.getState().setEnergyFlowDisplay('orbit');
      expect(useInverterStore.getState().showTodayStats).toBe(true);
    });

    it('keeps a saved show on the house', () => {
      localStorage.setItem('showTodayStats', 'true');
      useInverterStore.getState().setEnergyFlowDisplay('house');
      expect(useInverterStore.getState().showTodayStats).toBe(true);
    });

    it('keeps a saved hide on the orbit', () => {
      localStorage.setItem('showTodayStats', 'false');
      useInverterStore.getState().setEnergyFlowDisplay('house');
      useInverterStore.getState().setEnergyFlowDisplay('orbit');
      expect(useInverterStore.getState().showTodayStats).toBe(false);
    });
  });
});
