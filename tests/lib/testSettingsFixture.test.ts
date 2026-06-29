import { describe, expect, it } from 'vitest';
import { readFile } from 'node:fs/promises';
import { writeTestSettings } from '../../e2e/test-settings.ts';

describe('writeTestSettings', () => {
  it('writes weather backfill date as null so Rust can parse Option<NaiveDate>', async () => {
    const fixture = await writeTestSettings({ port: 18899, httpPort: 17337, tag: 'unit' });
    try {
      const settings = JSON.parse(await readFile(fixture.settingsPath, 'utf8'));
      expect(settings.weather_config.last_backfill_completed).toBeNull();
    } finally {
      await fixture.cleanup();
    }
  });
});
