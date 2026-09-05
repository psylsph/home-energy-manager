// @vitest-environment node

import { EventEmitter } from 'node:events';
import type { ChildProcess } from 'node:child_process';
import { describe, expect, test, vi } from 'vitest';
import { stopChildProcess } from '../e2e/process-lifecycle.js';

function fakeChild(exitCode: number | null, signalCode: NodeJS.Signals | null = null) {
  const events = new EventEmitter();
  const child = {
    exitCode,
    signalCode,
    kill: vi.fn(),
    once: events.once.bind(events),
  } as unknown as ChildProcess;
  return { child, events };
}

describe('E2E child-process teardown', () => {
  test('returns immediately for a child that already exited', async () => {
    const { child } = fakeChild(1);

    await stopChildProcess(child, 'backend', 5_000);

    expect(child.kill).not.toHaveBeenCalled();
  });

  test('waits for a live child after requesting SIGTERM', async () => {
    const { child, events } = fakeChild(null);
    vi.mocked(child.kill).mockImplementation(() => {
      events.emit('exit', 0, null);
      return true;
    });

    await stopChildProcess(child, 'backend', 5_000);

    expect(child.kill).toHaveBeenCalledWith('SIGTERM');
  });
});
