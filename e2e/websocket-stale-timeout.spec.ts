/**
 * Regression coverage for browser-side stale snapshot handling.
 *
 * The backend continues to run, but the browser WebSocket stops delivering
 * snapshots. This simulates a half-open route such as a disconnected Tailscale
 * link and proves the dashboard stops presenting the old animated diagram as
 * live data.
 */

import { test, expect } from './fixture.js';
import { startBackend, stopBackend } from './backend.js';

test.beforeAll(async () => {
  await startBackend();
});

test.afterAll(async () => {
  await stopBackend();
});

test('clears the dashboard after the browser stops receiving snapshots', async ({ page, baseUrl }) => {
  const snapshotResponse = await page.request.get(`${baseUrl}/api/snapshot`);
  const snapshotBody = await snapshotResponse.json();
  expect(snapshotBody.ok).toBe(true);

  let connectionCount = 0;
  await page.routeWebSocket('/ws', (webSocket) => {
    connectionCount += 1;

    if (connectionCount === 1) {
      webSocket.send(JSON.stringify({
        type: 'connection',
        state: 'connected',
        host: '127.0.0.1:18899',
      }));
      webSocket.send(JSON.stringify({
        type: 'snapshot',
        ...snapshotBody.data,
      }));
    } else {
      // The first socket is closed by the stale-data watchdog. Do not send
      // another snapshot on its reconnect: this keeps the simulated network
      // outage in place while the real backend remains healthy.
      webSocket.send(JSON.stringify({
        type: 'connection',
        state: 'disconnected',
        host: '127.0.0.1:18899',
      }));
    }
  });

  await page.clock.install();
  await page.goto('/');

  await expect(page.locator('[data-testid="energy-orbit-ring"]')).toBeVisible({ timeout: 10_000 });
  await expect(page.getByText('Disconnected — will retry automatically')).toBeHidden();

  // useWebSocket checks every 15s and expires snapshots after 2 minutes.
  await page.clock.runFor('02:15');

  await expect(page.getByText('Disconnected — will retry automatically')).toBeVisible();
  await expect(page.locator('[data-testid="energy-orbit-ring"]')).toBeHidden();
});
