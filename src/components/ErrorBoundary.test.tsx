import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent, act, cleanup } from '@testing-library/react';
import type { ReactNode } from 'react';
import ErrorBoundary from './ErrorBoundary';

// A child that always throws during render.
function Boom({ message = 'kaboom' }: { message?: string }): ReactNode {
  throw new Error(message);
}

// A child that throws only while `shouldThrow` is true, so a test can recover
// the subtree by flipping the flag (then retry).
let shouldThrow = false;
function Flaky({ children }: { children?: ReactNode }) {
  if (shouldThrow) throw new Error('flaky render');
  return <div data-testid="child">{children ?? 'ok'}</div>;
}

// React logs caught errors to console.error in dev; the boundary's own
// `componentDidCatch` also console.errors. Silence both for the tests that
// deliberately throw so the output stays clean.
function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

describe('ErrorBoundary', () => {
  beforeEach(() => {
    shouldThrow = false;
  });

  afterEach(() => {
    // Tear down rendered DOM between tests so getByText can't match leftover
    // nodes from a previous case, and restore timers + spies.
    cleanup();
    vi.restoreAllMocks();
    vi.useRealTimers();
  });

  it('renders its children when nothing throws', () => {
    render(
      <ErrorBoundary>
        <div data-testid="healthy">healthy</div>
      </ErrorBoundary>,
    );
    expect(screen.getByTestId('healthy')).toBeDefined();
    expect(screen.queryByText('Something went wrong')).toBeNull();
  });

  it('catches a render error and shows the fallback UI with the message', () => {
    silenceConsoleError();
    render(
      <ErrorBoundary>
        <Boom message="the inverter exploded" />
      </ErrorBoundary>,
    );
    expect(screen.getByText('Something went wrong')).toBeDefined();
    expect(screen.getByText('the inverter exploded')).toBeDefined();
    expect(screen.getByRole('button', { name: 'Retry now' })).toBeDefined();
  });

  it('re-renders the children when "Retry now" is clicked after recovery', () => {
    silenceConsoleError();
    shouldThrow = true;
    render(
      <ErrorBoundary>
        <Flaky />
      </ErrorBoundary>,
    );
    expect(screen.queryByTestId('child')).toBeNull();
    expect(screen.getByText('flaky render')).toBeDefined();

    // Recover the subtree, then retry — the boundary clears its error state
    // and the child renders successfully.
    shouldThrow = false;
    act(() => {
      fireEvent.click(screen.getByRole('button', { name: 'Retry now' }));
    });
    expect(screen.getByTestId('child')).toBeDefined();
    expect(screen.queryByText('Something went wrong')).toBeNull();
  });

  it('auto-retries by clearing the error once the countdown elapses', () => {
    vi.useFakeTimers();
    silenceConsoleError();
    shouldThrow = true;
    render(
      <ErrorBoundary>
        <Flaky />
      </ErrorBoundary>,
    );
    expect(screen.queryByTestId('child')).toBeNull();

    // Recover before the countdown finishes so the auto-retry succeeds.
    shouldThrow = false;
    act(() => {
      // Boundary counts down from 30 at 1s ticks; the 30th tick clears the
      // error and re-renders the children.
      vi.advanceTimersByTime(30_000);
    });
    expect(screen.getByTestId('child')).toBeDefined();
  });

  it('still shows the fallback if the error persists across the auto-retry', () => {
    vi.useFakeTimers();
    silenceConsoleError();
    shouldThrow = true;
    render(
      <ErrorBoundary>
        <Flaky />
      </ErrorBoundary>,
    );
    expect(screen.getByText('flaky render')).toBeDefined();

    // Leave `shouldThrow` true so the retry re-throws immediately. The
    // boundary must catch it again and keep showing the fallback rather than
    // propagating to (and crashing) the test renderer.
    expect(() => {
      act(() => {
        vi.advanceTimersByTime(30_000);
      });
    }).not.toThrow();
    expect(screen.getByText('Something went wrong')).toBeDefined();
    expect(screen.getByText('flaky render')).toBeDefined();
  });

  it('decrements the visible countdown each second', () => {
    vi.useFakeTimers();
    silenceConsoleError();
    render(
      <ErrorBoundary>
        <Boom />
      </ErrorBoundary>,
    );
    expect(screen.getByText(/Will retry in 30s/)).toBeDefined();
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(screen.getByText(/Will retry in 29s/)).toBeDefined();
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(screen.getByText(/Will retry in 28s/)).toBeDefined();
  });

  it('clears the countdown interval on unmount (no setState after unmount)', () => {
    // Real timers (not fake) so vi.spyOn captures the actual interval handle
    // that componentWillUnmount must clear — this is the property we care
    // about, and fake timers make the handle opaque.
    const setIntervalSpy = vi.spyOn(globalThis, 'setInterval');
    const clearIntervalSpy = vi.spyOn(globalThis, 'clearInterval');
    silenceConsoleError();

    const { unmount } = render(
      <ErrorBoundary>
        <Boom />
      </ErrorBoundary>,
    );

    // Catching the error starts the 1s countdown interval.
    const handle = setIntervalSpy.mock.results.at(-1)?.value;
    expect(handle).toBeDefined();

    unmount();

    // The countdown interval must be torn down so no setState fires on the
    // unmounted boundary (mirrors the useAction unmount-cleanup invariant).
    expect(clearIntervalSpy).toHaveBeenCalledWith(handle);
  });

  it('only starts one countdown when the error is first caught', () => {
    const setIntervalSpy = vi.spyOn(globalThis, 'setInterval');
    silenceConsoleError();

    render(
      <ErrorBoundary>
        <Boom />
      </ErrorBoundary>,
    );

    // Exactly one interval is created on catch (not one per render/tick).
    const intervalCalls = setIntervalSpy.mock.calls.length;
    expect(intervalCalls).toBe(1);
  });

  it('isolates errors: a sibling ErrorBoundary keeps rendering normally', () => {
    // Mirrors the app architecture, where each route is its own boundary. A
    // throw inside one boundary must never affect a sibling — this is the
    // property that guarantees one page's error can't crash the rest of the
    // app (CODE_REVIEW issue 3.4).
    silenceConsoleError();
    render(
      <>
        <ErrorBoundary>
          <Boom message="left page broke" />
        </ErrorBoundary>
        <ErrorBoundary>
          <div data-testid="healthy-sibling">healthy page</div>
        </ErrorBoundary>
      </>,
    );
    // The throwing boundary shows its fallback…
    expect(screen.getByText('left page broke')).toBeDefined();
    expect(screen.getByText('Something went wrong')).toBeDefined();
    // …while the sibling boundary is completely unaffected.
    expect(screen.getByTestId('healthy-sibling')).toBeDefined();
  });
});
