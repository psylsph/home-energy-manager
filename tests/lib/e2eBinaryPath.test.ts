import { describe, expect, it } from 'vitest';
import { backendExecutableName, simulatorBinaryCandidates } from '../../e2e/binary-path.js';

describe('backendExecutableName', () => {
  it('uses Cargo\'s .exe suffix on Windows', () => {
    expect(backendExecutableName('win32')).toBe('givenergy-local.exe');
  });

  it('uses the extensionless binary name on Unix platforms', () => {
    expect(backendExecutableName('linux')).toBe('givenergy-local');
    expect(backendExecutableName('darwin')).toBe('givenergy-local');
  });
});

describe('simulatorBinaryCandidates', () => {
  // Regression: the shared resolver (462b2af) climbed two levels out of
  // e2e/ — landing on the *parent of the repos directory* — so both
  // candidates missed ~/repos/givenergy-simulator and every local spec
  // failed with "Simulator not found" on this checkout layout.
  it('looks beside the repo first, then in home', () => {
    expect(
      simulatorBinaryCandidates('/x/repos/home-energy-manager/e2e', {}, 'linux'),
    ).toEqual([
      '/x/repos/givenergy-simulator/target/release/sim-api',
      '/x/givenergy-simulator/target/release/sim-api',
    ]);
  });

  it('uses the .exe suffix on Windows', () => {
    expect(
      simulatorBinaryCandidates('C:\\x\\r\\e2e', {}, 'win32'),
    ).toEqual([
      'C:\\x\\givenergy-simulator\\target\\release\\sim-api.exe',
      'C:\\givenergy-simulator\\target\\release\\sim-api.exe',
    ]);
  });

  it('uses POSIX path rules for macOS', () => {
    expect(
      simulatorBinaryCandidates('/Users/stuart/repos/home-energy-manager/e2e', {}, 'darwin'),
    ).toEqual([
      '/Users/stuart/repos/givenergy-simulator/target/release/sim-api',
      '/Users/stuart/givenergy-simulator/target/release/sim-api',
    ]);
  });

  it('puts the GIVENERGY_SIMULATOR_BIN override first and drops it when unset', () => {
    expect(
      simulatorBinaryCandidates('/x/repos/home-energy-manager/e2e', { GIVENERGY_SIMULATOR_BIN: '/custom/sim' }, 'linux')[0],
    ).toBe('/custom/sim');
    expect(
      simulatorBinaryCandidates('/x/repos/home-energy-manager/e2e', {}, 'linux')[0],
    ).not.toBe('/custom/sim');
  });
});
