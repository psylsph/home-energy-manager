// @vitest-environment node

import { describe, expect, test } from 'vitest';
import { requireOkJson } from '../e2e/admin-responses.js';

describe('E2E admin response validation', () => {
  test('returns a successful JSON object', async () => {
    await expect(requireOkJson(
      new Response(JSON.stringify({ ok: true, writes: [] }), { status: 200 }),
      'POST /writes/drain',
    )).resolves.toEqual({ ok: true, writes: [] });
  });

  test('rejects an HTTP error even when the body says ok', async () => {
    await expect(requireOkJson(
      new Response(JSON.stringify({ ok: true }), { status: 503 }),
      'POST /reset',
    )).rejects.toThrow('POST /reset returned HTTP 503');
  });

  test('rejects an unsuccessful admin response', async () => {
    await expect(requireOkJson(
      new Response(JSON.stringify({ ok: false, error: 'not reset' }), { status: 200 }),
      'POST /reset',
    )).rejects.toThrow('POST /reset returned HTTP 200');
  });

  test('rejects a non-JSON admin response', async () => {
    await expect(requireOkJson(
      new Response('bad gateway', { status: 502 }),
      'POST /writes/drain',
    )).rejects.toThrow('POST /writes/drain returned HTTP 502');
  });
});
