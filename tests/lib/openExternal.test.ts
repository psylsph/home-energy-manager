import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

// ---------------------------------------------------------------------------
// openExternal() opens a URL via the Tauri opener plugin when running inside
// the Tauri desktop shell, falling back to window.open otherwise (and also
// when the plugin import/call fails). We mock the opener plugin so we can
// assert on openUrl and control whether it resolves or rejects. The mock is
// hoisted above the import of the module under test.
// ---------------------------------------------------------------------------

const openUrl = vi.fn<(target: string) => Promise<void>>();
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl }));

import { openExternal } from '../../src/lib/openExternal';

/** Type-safe handle for the Tauri marker injected onto window. */
const tauriWindow = window as unknown as { __TAURI_INTERNALS__?: unknown };

/** Install/Remove the __TAURI_INTERNALS__ marker that gates the Tauri branch. */
function setTauriShell(present: boolean): void {
  if (present) tauriWindow.__TAURI_INTERNALS__ = {};
  else delete tauriWindow.__TAURI_INTERNALS__;
}

describe('openExternal', () => {
  let windowOpenSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    // jsdom's window.open is a no-op stub; replace it with a spy so we can
    // assert it was (or was not) called and with what arguments.
    windowOpenSpy = vi.spyOn(window, 'open').mockImplementation(vi.fn());
    openUrl.mockReset();
    // Headless by default — individual Tauri-path tests opt in.
    setTauriShell(false);
  });

  afterEach(() => {
    windowOpenSpy.mockRestore();
    setTauriShell(false);
  });

  describe('in headless/browser mode (no Tauri shell)', () => {
    it('calls window.open with the URL, _blank target, and noopener,noreferrer features', async () => {
      await openExternal('https://example.com');

      expect(windowOpenSpy).toHaveBeenCalledOnce();
      expect(windowOpenSpy).toHaveBeenCalledWith(
        'https://example.com',
        '_blank',
        'noopener,noreferrer',
      );
    });

    it('passes different URLs through verbatim', async () => {
      await openExternal('https://octopus.energy/dashboard');
      await openExternal('mailto:test@example.com');

      expect(windowOpenSpy).toHaveBeenCalledTimes(2);
      expect(windowOpenSpy).toHaveBeenNthCalledWith(
        1,
        'https://octopus.energy/dashboard',
        '_blank',
        'noopener,noreferrer',
      );
      expect(windowOpenSpy).toHaveBeenNthCalledWith(
        2,
        'mailto:test@example.com',
        '_blank',
        'noopener,noreferrer',
      );
    });

    it('does not invoke the Tauri opener plugin', async () => {
      await openExternal('https://example.com');

      expect(openUrl).not.toHaveBeenCalled();
    });

    it('resolves to undefined', async () => {
      await expect(openExternal('https://example.com')).resolves.toBeUndefined();
    });
  });

  describe('inside the Tauri desktop shell', () => {
    it('uses the Tauri opener plugin instead of window.open when it succeeds', async () => {
      setTauriShell(true);
      openUrl.mockResolvedValue(undefined);

      await openExternal('https://example.com');

      expect(openUrl).toHaveBeenCalledOnce();
      expect(openUrl).toHaveBeenCalledWith('https://example.com');
      // window.open must not run when the plugin path succeeds.
      expect(windowOpenSpy).not.toHaveBeenCalled();
    });

    it('falls back to window.open when the plugin call rejects', async () => {
      setTauriShell(true);
      // A rejection inside the try block exercises the same catch branch as a
      // failed dynamic import — both fall through to window.open.
      openUrl.mockRejectedValue(new Error('plugin unavailable'));

      await openExternal('https://example.com');

      expect(openUrl).toHaveBeenCalledOnce();
      expect(windowOpenSpy).toHaveBeenCalledOnce();
      expect(windowOpenSpy).toHaveBeenCalledWith(
        'https://example.com',
        '_blank',
        'noopener,noreferrer',
      );
    });

    it('still resolves to undefined after falling back to window.open', async () => {
      setTauriShell(true);
      openUrl.mockRejectedValue(new Error('boom'));

      await expect(openExternal('https://example.com')).resolves.toBeUndefined();
    });

    it('treats an empty-string Tauri marker value as present', async () => {
      // `in` checks property existence, not truthiness — an empty object/value
      // still routes through the plugin branch.
      tauriWindow.__TAURI_INTERNALS__ = '';
      openUrl.mockResolvedValue(undefined);

      await openExternal('https://example.com');

      expect(openUrl).toHaveBeenCalledOnce();
      expect(windowOpenSpy).not.toHaveBeenCalled();
    });
  });

  describe('routing depends on the presence of __TAURI_INTERNALS__', () => {
    it('routes through window.open when the marker is absent', async () => {
      setTauriShell(false);
      await openExternal('https://example.com');

      expect(openUrl).not.toHaveBeenCalled();
      expect(windowOpenSpy).toHaveBeenCalledOnce();
    });

    it('routes through the plugin when the marker is present', async () => {
      setTauriShell(true);
      openUrl.mockResolvedValue(undefined);
      await openExternal('https://example.com');

      expect(openUrl).toHaveBeenCalledOnce();
      expect(windowOpenSpy).not.toHaveBeenCalled();
    });

    it('re-evaluates the marker on every call (toggling between paths)', async () => {
      // Start in Tauri mode.
      setTauriShell(true);
      openUrl.mockResolvedValue(undefined);
      await openExternal('https://a.test');
      expect(openUrl).toHaveBeenCalledOnce();
      expect(windowOpenSpy).not.toHaveBeenCalled();

      // Drop into headless mode for the next call.
      setTauriShell(false);
      await openExternal('https://b.test');
      expect(openUrl).toHaveBeenCalledOnce(); // still just the first call
      expect(windowOpenSpy).toHaveBeenCalledOnce();
    });
  });
});
