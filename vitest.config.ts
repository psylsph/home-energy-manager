import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

// Vitest configuration for component / hook unit tests.
//
// Kept as a dedicated file (rather than a `test` key on vite.config.ts) so the
// production build config stays minimal — only the Vite + Tailwind plugins ship
// to end users — and the jsdom environment + test globs never leak into `vite
// build`. Run with `npm test` (`vitest run`) or `npm run test:watch`.
export default defineConfig({
  plugins: [react()],
  // Mirror the `define` from vite.config.ts so components that reference the
  // build-injected `__APP_VERSION__` global (e.g. <App/>'s header) can render
  // under Vitest — without this the bare identifier resolves to an undeclared
  // global and throws a ReferenceError during render.
  define: {
    __APP_VERSION__: JSON.stringify('test'),
  },
  test: {
    environment: 'jsdom',
    // 2-core CI runners (windows-latest) can starve workers past the 5s
    // default while the full suite runs in parallel — even pure-logic lib
    // tests have timed out there. 15s keeps a hung test failing while
    // giving heavyweight jsdom/Recharts page tests (History, Logs) the
    // headroom they need under load.
    testTimeout: 15_000,
    // Global setup file. Replaces jsdom 29's stub `localStorage` /
    // `sessionStorage` (which lack the actual storage methods) with working
    // in-memory implementations, so persistence tests can run without
    // `localStorage.X is not a function` errors. See tests/setup.ts for the
    // full rationale.
    setupFiles: ['./tests/setup.ts'],
    include: ['tests/**/*.test.{ts,tsx}'],
    exclude: ['node_modules', 'dist', 'e2e', 'src-tauri'],
  },
});
