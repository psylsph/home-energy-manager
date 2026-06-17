/**
 * Open a URL in the system's default external browser.
 *
 * Uses the Tauri opener plugin when running inside the Tauri desktop shell.
 * Falls back to window.open when running in headless/browser mode.
 */
export async function openExternal(url: string): Promise<void> {
  if ('__TAURI_INTERNALS__' in window) {
    try {
      const { openUrl } = await import('@tauri-apps/plugin-opener');
      await openUrl(url);
      return;
    } catch {
      // Plugin not available — fall through to window.open
    }
  }
  window.open(url, '_blank', 'noopener,noreferrer');
}
