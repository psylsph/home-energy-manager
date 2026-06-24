/**
 * E2E tests for the "Submit a Support Bundle" feature (issue #125).
 *
 * Covers the Settings page UI (section rendering, form interactions) and the
 * backend validation path. The full ntfy delivery is deliberately NOT exercised
 * here — it depends on live `ntfy.sh` availability and would make the suite
 * flaky. Instead the submission flow is verified against a mocked endpoint
 * response, and the real endpoint is hit only for the validation branch that
 * returns *before* any network call.
 */

import { test, expect } from './local-fixture.js';

test.describe('Settings Page - Support Bundle', () => {
  test('should show the Submit Support Bundle section', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(
      page.getByRole('heading', { name: 'Submit a Support Bundle' }),
    ).toBeVisible({ timeout: 10_000 });
  });

  test('should show the category dropdown and description field', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.getByText("What's the issue?")).toBeVisible({ timeout: 10_000 });
    await expect(page.getByText('Describe the problem')).toBeVisible();
    // Category dropdown with the expected options. Scope to the
    // Support Bundle section so we don't match tariff time <select>s.
    const supportSection = page.locator('section').filter({ hasText: 'Submit a Support Bundle' });
    const categorySelect = supportSection.locator('select').first();
    await expect(categorySelect).toBeVisible();
    for (const cat of ['Connection', 'Battery', 'Other']) {
      await expect(categorySelect.locator(`option`)).toContainText([cat]);
    }
    // Optional GitHub issue number box.
    await expect(page.getByPlaceholder(/e.g. 125/)).toBeVisible();
  });

  test('should disable the submit button until a description is entered', async ({ page }) => {
    await page.goto('/#/settings');
    const button = page.getByRole('button', { name: 'Submit Support Bundle' });
    await expect(button).toBeVisible({ timeout: 10_000 });
    await expect(button).toBeDisabled();

    const textarea = page.getByPlaceholder(/What were you trying to do/);
    await textarea.fill('Battery stops charging at 60% in slot 1.');
    await expect(button).toBeEnabled();
  });

  test('should submit the bundle and show a confirmation', async ({ page }) => {
    await page.goto('/#/settings');

    // Mock the submission endpoint so the test doesn't depend on live ntfy.sh.
    // Assert the request body carries the user's description, category, and
    // privacy toggles through to the backend.
    let capturedBody: Record<string, unknown> | null = null;
    await page.route('**/api/support/submit', async (route) => {
      const request = route.request();
      capturedBody = JSON.parse(request.postData() || '{}');
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          ok: true,
          bundle_id: 'hem-TEST123-20260623T1432Z',
          size_bytes: 4096,
          sent_to: [{ channel: 'ntfy', ok: true }],
          message: 'Bundle hem-TEST123-20260623T1432Z submitted.',
        }),
      });
    });

    const textarea = page.getByPlaceholder(/What were you trying to do/);
    await textarea.fill('Inverter reboots every hour.');

    // Enter a GitHub issue number so we can assert it travels through too.
    await page.getByPlaceholder(/e.g. 125/).fill('125');

    // History is opt-in (default off) — toggle it on to exercise the path.
    const historyCheckbox = page.getByLabel(/Include last 24 h of history/);
    await expect(historyCheckbox).not.toBeChecked();
    await historyCheckbox.check();

    await page.getByRole('button', { name: 'Submit Support Bundle' }).click();

    // The confirmation flash carries the bundle id.
    await expect(page.getByText(/hem-TEST123-20260623T1432Z/)).toBeVisible({
      timeout: 10_000,
    });

    // Verify the request payload reached the backend intact.
    expect(capturedBody).not.toBeNull();
    expect(capturedBody!.description).toBe('Inverter reboots every hour.');
    expect(capturedBody!.category).toBe('connection'); // default selection
    expect(capturedBody!.issue_number).toBe('125');
    expect(capturedBody!.include_history).toBe(true);
    // The network opt-in no longer exists (GDPR).
    expect(capturedBody!.include_network).toBeUndefined();
  });

  test('POST /api/support/submit rejects an empty description', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/support/submit`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ description: '   ', category: 'other' }),
    });
    // Validation fails before any network delivery, so this is deterministic.
    expect(resp.status).toBe(400);
    const data = await resp.json();
    expect(data.ok).toBe(false);
    expect(data.error).toContain('Description');
  });

  test('POST /api/support/submit rejects an invalid category', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/support/submit`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ description: 'something broke', category: 'bogus' }),
    });
    expect(resp.status).toBe(400);
    const data = await resp.json();
    expect(data.ok).toBe(false);
    expect(data.error).toContain('Invalid category');
  });

  test('POST /api/support/submit rejects an invalid issue number', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/support/submit`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        description: 'something broke',
        category: 'other',
        issue_number: 'not-a-number',
      }),
    });
    // Validation runs before any network delivery, so this is deterministic.
    expect(resp.status).toBe(400);
    const data = await resp.json();
    expect(data.ok).toBe(false);
    expect(data.error).toContain('Issue number');
  });
});
