// @vitest-environment node

import { EventEmitter } from 'node:events';
import { describe, expect, test, vi } from 'vitest';
import { attachErrorHandler } from '../e2e/process-errors.js';

describe('E2E infrastructure error handlers', () => {
  test('consume and report process-style error events', () => {
    const emitter = new EventEmitter();
    const error = new Error('spawn failed');
    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

    attachErrorHandler(emitter, 'backend');

    expect(() => emitter.emit('error', error)).not.toThrow();
    expect(errorSpy).toHaveBeenCalledWith('[backend] error: spawn failed');

    errorSpy.mockRestore();
  });
});
