import { describe, it, expect } from 'vitest';
import { cosyBlockedMessage } from '../../src/lib/cosyBlocked';

describe('cosyBlockedMessage', () => {
  it('explains each hard override in plain words', () => {
    expect(cosyBlockedMessage('pause', true)).toMatch(/battery pause/i);
    expect(cosyBlockedMessage('force', true)).toMatch(/Force Charge or Force Discharge/);
    expect(cosyBlockedMessage('safety', true)).toMatch(/safety limit/i);
    expect(cosyBlockedMessage('calibration', true)).toMatch(/battery calibration/i);
  });

  it('says what Cosy is not doing and that it will carry on by itself', () => {
    for (const code of ['pause', 'force', 'safety']) {
      const message = cosyBlockedMessage(code, true) ?? '';
      expect(message).toMatch(/charge times/i);
      expect(message).toMatch(/resume/i);
    }
  });

  it('stays quiet when nothing is blocking Cosy', () => {
    expect(cosyBlockedMessage('', true)).toBeNull();
    expect(cosyBlockedMessage(undefined, true)).toBeNull();
    expect(cosyBlockedMessage(null, true)).toBeNull();
  });

  it('stays quiet when Cosy is switched off, whatever the code says', () => {
    expect(cosyBlockedMessage('pause', false)).toBeNull();
    expect(cosyBlockedMessage('pause', undefined)).toBeNull();
  });

  it('still warns for a code from a newer backend', () => {
    expect(cosyBlockedMessage('something_new', true)).toMatch(/on hold/i);
  });
});
