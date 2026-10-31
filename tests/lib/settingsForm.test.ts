import { describe, it, expect } from 'vitest';
import {
  clampInterval,
  cleanCtArrays,
  octopusConfigured,
  scannedHost,
  positiveOrZero,
  VALID_INTERVALS,
} from '../../src/lib/settingsForm';
import { errorText } from '../../src/components/settings/useSettingsSave';
import {
  DEFAULT_ALERTS_CONFIG,
  hasChannel,
  ntfyTopic,
} from '../../src/components/settings/notifications/alertsConfig';

describe('clampInterval', () => {
  it('keeps an offered interval', () => {
    for (const v of VALID_INTERVALS) expect(clampInterval(v)).toBe(v);
  });

  it('snaps to the nearest offered interval, the lower one on a tie', () => {
    expect(clampInterval(1)).toBe(5);
    expect(clampInterval(12)).toBe(10);
    expect(clampInterval(13)).toBe(15);
    expect(clampInterval(25)).toBe(20);
    expect(clampInterval(3600)).toBe(60);
  });
});

describe('octopusConfigured', () => {
  it('needs both a saved key and an account number', () => {
    expect(octopusConfigured({})).toBe(false);
    expect(octopusConfigured({ octopus_api_key_configured: true })).toBe(false);
    expect(octopusConfigured({ octopus_api_key_configured: true, octopus_account_number: '   ' })).toBe(false);
    expect(octopusConfigured({ octopus_api_key_configured: false, octopus_account_number: 'A-1' })).toBe(false);
    expect(octopusConfigured({ octopus_api_key_configured: true, octopus_account_number: 'A-1' })).toBe(true);
  });
});

describe('cleanCtArrays', () => {
  it('trims names, zeroes blank or junk kWp, and drops meters outside 1–8', () => {
    expect(
      cleanCtArrays([
        { meter_address: 1, name: '  East  ', rated_kw: '6' },
        { meter_address: 2, name: '', rated_kw: '' },
        { meter_address: 3, name: 'x', rated_kw: 'abc' },
        { meter_address: 4, name: 'y', rated_kw: '-2' },
        { meter_address: 0, name: 'grid', rated_kw: '5' },
        { meter_address: 9, name: 'nine', rated_kw: '5' },
        { meter_address: 1.5, name: 'half', rated_kw: '5' },
      ]),
    ).toEqual([
      { meter_address: 1, name: 'East', rated_kw: 6 },
      { meter_address: 2, name: '', rated_kw: 0 },
      { meter_address: 3, name: 'x', rated_kw: 0 },
      { meter_address: 4, name: 'y', rated_kw: 0 },
    ]);
  });
});

describe('positiveOrZero', () => {
  it('reads blank, junk, negative and infinite entries as 0 (e.g. no Standing Charge)', () => {
    expect(positiveOrZero('')).toBe(0);
    expect(positiveOrZero('abc')).toBe(0);
    expect(positiveOrZero('-3')).toBe(0);
    expect(positiveOrZero('Infinity')).toBe(0);
    expect(positiveOrZero('54.86')).toBe(54.86);
  });
});

describe('errorText', () => {
  it("uses the error's message, else the fallback", () => {
    expect(errorText(new Error('boom'), 'fallback')).toBe('boom');
    expect(errorText(new Error(''), 'fallback')).toBe('fallback');
    expect(errorText('boom', 'fallback')).toBe('fallback');
    expect(errorText(null, 'fallback')).toBe('fallback');
  });
});

describe('scannedHost', () => {
  it('reads `host`, or `ip` from older backends', () => {
    expect(scannedHost({ host: '10.0.0.2' })).toBe('10.0.0.2');
    expect(scannedHost({ ip: '10.0.0.3' } as unknown as { host: string })).toBe('10.0.0.3');
  });
});

describe('ntfyTopic', () => {
  it("uses the user's topic over the serial default", () => {
    expect(ntfyTopic({ ntfy_topic: 'mine' }, 'SA1')).toEqual({ generated: 'hem-SA1', effective: 'mine' });
  });

  it('falls back to hem-<serial>, or nothing without a serial', () => {
    expect(ntfyTopic({ ntfy_topic: '' }, 'SA1')).toEqual({ generated: 'hem-SA1', effective: 'hem-SA1' });
    expect(ntfyTopic({ ntfy_topic: '' }, '')).toEqual({ generated: '', effective: '' });
  });
});

describe('hasChannel', () => {
  const base = DEFAULT_ALERTS_CONFIG;

  it('is false with nothing configured', () => {
    expect(hasChannel(base, '')).toBe(false);
  });

  it('needs both Telegram fields', () => {
    expect(hasChannel({ ...base, telegram_bot_token: 't' }, '')).toBe(false);
    expect(hasChannel({ ...base, telegram_bot_token: 't', telegram_chat_id: '1' }, '')).toBe(true);
  });

  it('counts an ntfy topic on its own', () => {
    expect(hasChannel(base, 'hem-SA1')).toBe(true);
  });

  it('needs both Pushover fields', () => {
    expect(hasChannel({ ...base, pushover_app_token: 'a' }, '')).toBe(false);
    expect(hasChannel({ ...base, pushover_app_token: 'a', pushover_user_key: 'u' }, '')).toBe(true);
  });
});
