import { describe, it, expect, vi, afterEach } from 'vitest';
import { renderHook, act, cleanup } from '@testing-library/react';
import { useSettingsSave, useSettingToggle } from '../../src/components/settings/useSettingsSave';

afterEach(cleanup);

/** A promise the test settles by hand, to look at state mid-save. */
function deferred<T = void>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

describe('useSettingsSave', () => {
  it('is busy while the action runs and flashes the text it returns', async () => {
    const flash = vi.fn();
    const { result } = renderHook(() => useSettingsSave(flash));
    const gate = deferred<string>();

    let done!: Promise<void>;
    act(() => { done = result.current.run(() => gate.promise, 'Failed'); });
    expect(result.current.busy).toBe(true);

    await act(async () => { gate.resolve('Saved'); await done; });
    expect(result.current.busy).toBe(false);
    expect(flash).toHaveBeenCalledWith('Saved', true);
  });

  it('flashes nothing when the action returns nothing', async () => {
    const flash = vi.fn();
    const { result } = renderHook(() => useSettingsSave(flash));
    await act(() => result.current.run(async () => undefined, 'Failed'));
    expect(flash).not.toHaveBeenCalled();
  });

  it("flashes the error's message, and stops being busy", async () => {
    const flash = vi.fn();
    const { result } = renderHook(() => useSettingsSave(flash));
    await act(() => result.current.run(async () => { throw new Error('port in use'); }, 'Failed'));
    expect(flash).toHaveBeenCalledWith('port in use', false);
    expect(result.current.busy).toBe(false);
  });

  it('falls back to the given text for an error without a message', async () => {
    const flash = vi.fn();
    const { result } = renderHook(() => useSettingsSave(flash));
    await act(() => result.current.run(async () => { throw null; }, 'Failed to save'));
    await act(() => result.current.run(async () => { throw new Error(''); }, 'Failed to save'));
    expect(flash.mock.calls).toEqual([['Failed to save', false], ['Failed to save', false]]);
  });

  it('lets the caller word the failure', async () => {
    const flash = vi.fn();
    const { result } = renderHook(() => useSettingsSave(flash));
    await act(() =>
      result.current.run(async () => { throw new Error('detail'); }, () => 'Failed to save panel visibility'),
    );
    expect(flash).toHaveBeenCalledWith('Failed to save panel visibility', false);
  });
});

describe('useSettingsSave — sticky toasts', () => {
  it('passes `sticky` through to both the success and the failure toast', async () => {
    const flash = vi.fn();
    const { result } = renderHook(() => useSettingsSave(flash));
    await act(() => result.current.run(async () => 'Key generated', 'Failed', { sticky: true }));
    await act(() => result.current.run(async () => { throw new Error('no key'); }, 'Failed', { sticky: true }));
    expect(flash.mock.calls).toEqual([
      ['Key generated', true, { sticky: true }],
      ['no key', false, { sticky: true }],
    ]);
  });

  it('leaves ordinary toasts to clear themselves', async () => {
    const flash = vi.fn();
    const { result } = renderHook(() => useSettingsSave(flash));
    await act(() => result.current.run(async () => 'Saved', 'Failed'));
    expect(flash.mock.calls).toEqual([['Saved', true]]);
  });
});

describe('useSettingToggle', () => {
  const TEXT = { on: 'On now', off: 'Off now', fail: 'Failed to switch' };

  it('moves at once, saves, and confirms', async () => {
    const flash = vi.fn();
    const gate = deferred();
    const persist = vi.fn(() => gate.promise);
    const { result } = renderHook(() => useSettingToggle(false, persist, flash, TEXT));

    let done!: Promise<void>;
    act(() => { done = result.current[1](true); });
    expect(result.current[0]).toBe(true);
    expect(persist).toHaveBeenCalledWith(true);

    await act(async () => { gate.resolve(); await done; });
    expect(result.current[0]).toBe(true);
    expect(flash).toHaveBeenCalledWith('On now', true);
  });

  it('confirms switching off with the off text', async () => {
    const flash = vi.fn();
    const { result } = renderHook(() => useSettingToggle(true, async () => {}, flash, TEXT));
    await act(() => result.current[1](false));
    expect(result.current[0]).toBe(false);
    expect(flash).toHaveBeenCalledWith('Off now', true);
  });

  it('moves back and says why when the save fails', async () => {
    const flash = vi.fn();
    const { result } = renderHook(() =>
      useSettingToggle(false, async () => { throw new Error('settings locked'); }, flash, TEXT),
    );
    await act(() => result.current[1](true));
    expect(result.current[0]).toBe(false);
    expect(flash).toHaveBeenCalledWith('Failed to switch: settings locked', false);
  });

  it('reports a non-Error rejection as text', async () => {
    const flash = vi.fn();
    const { result } = renderHook(() =>
      useSettingToggle(true, () => Promise.reject('offline'), flash, TEXT),
    );
    await act(() => result.current[1](false));
    expect(result.current[0]).toBe(true);
    expect(flash).toHaveBeenCalledWith('Failed to switch: offline', false);
  });
});
