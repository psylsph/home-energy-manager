/** Attach a listener so an infrastructure error cannot become an unhandled event. */
export function attachErrorHandler(
  emitter: { on: (event: 'error', listener: (error: Error) => void) => unknown },
  label: string,
): void {
  emitter.on('error', (error) => {
    console.error(`[${label}] error: ${error.message}`);
  });
}
