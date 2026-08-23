import * as fs from 'fs';
import * as path from 'path';
import { fileURLToPath } from 'url';

/** Return Cargo's platform-specific executable filename for the backend. */
export function backendExecutableName(platform: NodeJS.Platform = process.platform): string {
  return platform === 'win32' ? 'givenergy-local.exe' : 'givenergy-local';
}

/**
 * Resolve the givenergy-simulator release binary.
 *
 * The simulator checkout has lived in two locations: a sibling of this
 * repo (~/givenergy-simulator) and, currently, ~/repos/givenergy-simulator
 * (which is where the build instructions point). Accept either, plus a
 * GIVENERGY_SIMULATOR_BIN override, so the local E2E suite works from all
 * of them. First existing candidate wins; returns undefined when the
 * simulator isn't built yet.
 */
export function simulatorBinaryPath(): string | undefined {
  const e2eDir = path.dirname(fileURLToPath(import.meta.url));
  const executable = process.platform === 'win32' ? 'sim-api.exe' : 'sim-api';
  const repoRoot = path.resolve(e2eDir, '..', '..');
  const candidates = [
    process.env.GIVENERGY_SIMULATOR_BIN,
    path.resolve(repoRoot, 'repos', 'givenergy-simulator', 'target', 'release', executable),
    path.resolve(repoRoot, '..', 'givenergy-simulator', 'target', 'release', executable),
  ].filter((p): p is string => typeof p === 'string' && p.length > 0);
  return candidates.find((p) => fs.existsSync(p));
}
