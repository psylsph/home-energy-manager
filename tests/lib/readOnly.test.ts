import { describe, it, expect } from 'vitest';
import { addressIsReadOnly } from '../../src/lib/readOnly';

describe('addressIsReadOnly (issues #114, #471)', () => {
  it('finds ?RO before the hash or in the hash route', () => {
    expect(addressIsReadOnly('?RO', '')).toBe(true);
    expect(addressIsReadOnly('?x=1&RO', '#/')).toBe(true);
    expect(addressIsReadOnly('', '#/forecast?RO')).toBe(true);
    expect(addressIsReadOnly('', '#/?RO=1')).toBe(true);
  });

  it('is false otherwise', () => {
    expect(addressIsReadOnly('', '')).toBe(false);
    expect(addressIsReadOnly('?ROX', '#/forecast')).toBe(false);
    expect(addressIsReadOnly('', '#/forecast?mode=RO')).toBe(false);
  });
});
