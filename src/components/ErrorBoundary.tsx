import { Component } from 'react';
import type { ErrorInfo, ReactNode } from 'react';

interface Props {
  children: ReactNode;
}

interface State {
  error: Error | null;
  countdown: number;
}

export default class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null, countdown: 30 };
  private timer: ReturnType<typeof setInterval> | null = null;

  static getDerivedStateFromError(error: Error): State {
    return { error, countdown: 30 };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error('ErrorBoundary caught:', error, info.componentStack);
  }

  componentDidMount() {
    // When a child throws during the *initial* render, the boundary mounts
    // straight into its error state and `componentDidUpdate` never fires — so
    // the countdown must also be started here. Without this, a page that
    // throws on load would show "Will retry in 30s" but the timer would never
    // tick and the auto-retry would never happen (only the manual "Retry now"
    // button worked). `startCountdown` clears any prior timer first, so this
    // is safe even if React also runs `componentDidUpdate` for the same error
    // (CODE_REVIEW issue 3.4).
    if (this.state.error) {
      this.startCountdown();
    }
  }

  componentDidUpdate(_prevProps: Props, prevState: State) {
    if (prevState.error !== this.state.error && this.state.error) {
      this.startCountdown();
    }
  }

  componentWillUnmount() {
    this.clearTimer();
  }

  private startCountdown() {
    this.clearTimer();
    this.timer = setInterval(() => {
      this.setState((prev) => {
        if (prev.countdown <= 1) {
          this.clearTimer();
          return { error: null, countdown: 30 };
        }
        return { error: prev.error, countdown: prev.countdown - 1 };
      });
    }, 1000);
  }

  private clearTimer() {
    if (this.timer !== null) {
      clearInterval(this.timer);
      this.timer = null;
    }
  }

  private handleRetry = () => {
    this.clearTimer();
    this.setState({ error: null, countdown: 30 });
  };

  render() {
    if (this.state.error) {
      return (
        <div className="flex flex-col items-center justify-center min-h-[40vh] gap-4 px-6">
          <div className="w-12 h-12 rounded-full bg-red-900/30 flex items-center justify-center">
            <svg className="w-6 h-6 text-red-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
              <path strokeLinecap="round" strokeLinejoin="round" d="M12 9v3.75m9-.75a9 9 0 11-18 0 9 9 0 0118 0zm-9 3.75h.008v.008H12v-.008z" />
            </svg>
          </div>
          <p className="text-text-primary text-sm font-sans font-semibold">Something went wrong</p>
          <p className="text-text-secondary text-xs font-sans text-center max-w-sm">
            {this.state.error.message}
          </p>
          <p className="text-text-secondary/60 text-xs font-sans">
            Will retry in {this.state.countdown}s
          </p>
          <button
            onClick={this.handleRetry}
            className="mt-1 px-4 py-2 rounded-lg bg-flow-active text-bg-base text-sm font-sans font-medium hover:opacity-90 transition-opacity"
          >
            Retry now
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}
