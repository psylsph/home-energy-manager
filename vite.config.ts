import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import { readFileSync } from 'node:fs'

interface PackageManifest {
  version?: string
}

const packageManifest = JSON.parse(
  readFileSync(new URL('./package.json', import.meta.url), 'utf8'),
) as PackageManifest

export function resolveAppVersion(
  npmPackageVersion: string | undefined,
  packageVersion: string | undefined,
): string {
  return npmPackageVersion || packageVersion || '0.0.0'
}

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss()],
  define: {
    __APP_VERSION__: JSON.stringify(
      resolveAppVersion(process.env.npm_package_version, packageManifest.version),
    ),
  },
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    watch: {
      ignored: ['**/src-tauri/target/**', '**/src-tauri/target/doc/**'],
    },
  },
})
