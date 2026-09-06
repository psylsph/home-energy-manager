import * as fs from 'fs';
import * as path from 'path';
import { fileURLToPath } from 'url';

/** Return Cargo's platform-specific executable filename for the backend. */
export function backendExecutableName(platform: NodeJS.Platform = process.platform): string {
  return platform === 'win32' ? 'givenergy-local.exe' : 'givenergy-local';
}

/**
 * Build the ordered candidate list for the givenergy-simulator release
 * binary, rooted at the e2e/ directory containing this helper. Split out
 * from simulatorBinaryPath() so the path arithmetic is unit-testable
 * without touching the real filesystem.
 *
 * The simulator checkout has lived in two locations: a sibling of this
 * repo under the same parent (~/repos/givenergy-simulator — where the
 * build instructions point) and, formerly, a sibling of the repos
 * directory (~/givenergy-simulator). Accept both, plus a
 * GIVENERGY_SIMULATOR_BIN override, so the local E2E suite works from
 * all of them. First existing candidate wins.
 */
export function simulatorBinaryCandidates(
  e2eDir: string,
  env: NodeJS.ProcessEnv = process.env,
  platform: NodeJS.Platform = process.platform,
): string[] {
  const executable = platform === 'win32' ? 'sim-api.exe' : 'sim-api';
  // Resolve using the target platform rather than the host running the test.
  // Besides making the helper honestly unit-testable, this matters when a
  // Windows path is supplied by a cross-platform launcher or CI fixture.
  const platformPath = platform === 'win32' ? path.win32 : path.posix;
  const repoRoot = platformPath.resolve(e2eDir, '..');
  return [
    env.GIVENERGY_SIMULATOR_BIN,
    platformPath.resolve(repoRoot, '..', 'givenergy-simulator', 'target', 'release', executable),
    platformPath.resolve(repoRoot, '..', '..', 'givenergy-simulator', 'target', 'release', executable),
  ].filter((p): p is string => typeof p === 'string' && p.length > 0);
}

/**
 * Resolve the givenergy-simulator release binary. Returns undefined when
 * no candidate exists — callers guard on the value, not existence.
 */
export function simulatorBinaryPath(): string | undefined {
  const e2eDir = path.dirname(fileURLToPath(import.meta.url));
  return simulatorBinaryCandidates(e2eDir).find((p) => fs.existsSync(p));
}
