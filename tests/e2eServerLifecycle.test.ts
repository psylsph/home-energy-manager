// @vitest-environment node

import { describe, expect, test, vi } from 'vitest';
import { closeServerWithDeadline, type ClosableServer } from '../e2e/mock-modbus.js';

describe('E2E server teardown', () => {
  test('force-closes open connections before waiting for the close callback', async () => {
    const close = vi.fn((callback?: (error?: Error) => void) => queueMicrotask(() => callback?.()));
    const closeAllConnections = vi.fn();
    const closeIdleConnections = vi.fn();
    const server: ClosableServer = { close, closeAllConnections, closeIdleConnections };

    await closeServerWithDeadline(server, 'test server', 100);

    expect(closeAllConnections).toHaveBeenCalledOnce();
    expect(closeIdleConnections).toHaveBeenCalledOnce();
    expect(close).toHaveBeenCalledOnce();
  });

  test('resolves on the deadline if a server close callback remains blocked', async () => {
    const close = vi.fn((callback?: (error?: Error) => void) => {
      void callback;
    });
    const closeAllConnections = vi.fn();
    const closeIdleConnections = vi.fn();

    await closeServerWithDeadline(
      { close, closeAllConnections, closeIdleConnections } satisfies ClosableServer,
      'blocked test server',
      10,
    );

    expect(closeAllConnections).toHaveBeenCalledOnce();
    expect(closeIdleConnections).toHaveBeenCalledOnce();
  });
});
