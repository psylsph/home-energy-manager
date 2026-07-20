import { describe, expect, it } from 'vitest';
import { backendExecutableName } from '../../e2e/binary-path.js';

describe('backendExecutableName', () => {
  it('uses Cargo\'s .exe suffix on Windows', () => {
    expect(backendExecutableName('win32')).toBe('givenergy-local.exe');
  });

  it('uses the extensionless binary name on Unix platforms', () => {
    expect(backendExecutableName('linux')).toBe('givenergy-local');
    expect(backendExecutableName('darwin')).toBe('givenergy-local');
  });
});
