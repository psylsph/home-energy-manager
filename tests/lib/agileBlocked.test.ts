import { describe, it, expect } from 'vitest';
import { agileBlockedMessage } from '../../src/lib/agileBlocked';

describe('agileBlockedMessage', () => {
  it('explains each owner that can hold Agile back', () => {
    expect(agileBlockedMessage('timed_export', true)).toMatch(/Timed Export schedule/);
    expect(agileBlockedMessage('manual_mode', true)).toMatch(/Timed Demand/);
    expect(agileBlockedMessage('pause', true)).toMatch(/battery pause/i);
    expect(agileBlockedMessage('force', true)).toMatch(/Force Charge or Force Discharge/);
    expect(agileBlockedMessage('safety', true)).toMatch(/safety limit/i);
  });

  it('says what Agile is not doing and that it will carry on by itself', () => {
    const message = agileBlockedMessage('timed_export', true) ?? '';
    expect(message).toMatch(/charge and discharge times/i);
    expect(message).toMatch(/resume/i);
  });

  it('stays quiet when nothing is blocking Agile', () => {
    expect(agileBlockedMessage('', true)).toBeNull();
    expect(agileBlockedMessage(undefined, true)).toBeNull();
    expect(agileBlockedMessage(null, true)).toBeNull();
  });

  it('stays quiet when Agile is switched off', () => {
    expect(agileBlockedMessage('pause', false)).toBeNull();
    expect(agileBlockedMessage('pause', undefined)).toBeNull();
  });

  it('still warns for a code from a newer backend', () => {
    expect(agileBlockedMessage('something_new', true)).toMatch(/on hold/i);
  });
});
