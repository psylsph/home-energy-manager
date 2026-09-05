import type { ChildProcess } from 'child_process';

function hasExited(process: ChildProcess): boolean {
  return process.exitCode !== null || process.signalCode !== null;
}

/** Stop a child without waiting for a timeout when it has already exited. */
export async function stopChildProcess(
  process: ChildProcess | null,
  label: string,
  timeoutMs: number,
): Promise<void> {
  if (!process) return;

  await new Promise<void>((resolve) => {
    let settled = false;
    const timer: { id?: ReturnType<typeof setTimeout> } = {};
    const finish = (): void => {
      if (settled) return;
      settled = true;
      if (timer.id !== undefined) clearTimeout(timer.id);
      resolve();
    };

    // Attach before checking exitCode so a concurrent exit cannot fall
    // between the state check and listener registration.
    process.once('exit', finish);
    if (hasExited(process)) {
      finish();
      return;
    }

    process.kill('SIGTERM');
    timer.id = setTimeout(() => {
      if (!hasExited(process)) {
        console.warn(`[${label}] did not exit after SIGTERM; sending SIGKILL`);
        process.kill('SIGKILL');
      }
      finish();
    }, timeoutMs);
  });
}
