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
