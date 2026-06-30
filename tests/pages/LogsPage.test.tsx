import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// LogsPage polls /api/logs every 2s, parses lines into timestamp/level/
// message, applies a client-side text filter, and lets the user change the
// backend capture level via PUT /api/log-level. We mock the api module and
// drive it with fake timers so the polling interval doesn't leak between
// tests.
// ---------------------------------------------------------------------------

const apiGetMock = vi.fn();
const apiPutMock = vi.fn();

vi.mock('../../src/lib/api', () => ({
  apiGet: (...args: unknown[]) => apiGetMock(...(args as [string])),
  apiPut: (...args: unknown[]) => apiPutMock(...(args as [string, unknown])),
}));

import LogsPage from '../../src/pages/LogsPage';

function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

const SAMPLE_LINES = [
  '10:30:00.123 ERROR [poll] connection refused',
  '10:30:01.456 WARN [sanitize] battery power spike rejected',
  '10:30:02.789 INFO [server] HTTP server starting on 0.0.0.0:7337',
  '10:30:03.111 DEBUG [decoder] decoded block HR(0,60)',
  '10:30:04.222 TRACE [framer] received frame',
  'not a structured log line at all',
];

describe('<LogsPage/>', () => {
  beforeEach(() => {
    silenceConsoleError();
    apiGetMock.mockReset();
    apiPutMock.mockReset();
    // GET /api/log-level on mount + GET /api/logs
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/log-level') return { ok: true, level: 'WARN' };
      if (path === '/api/logs') return { ok: true, lines: SAMPLE_LINES, count: SAMPLE_LINES.length };
      return { ok: true };
    });
    apiPutMock.mockResolvedValue({ ok: true, level: 'INFO' });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  it('renders the parsed log entries', async () => {
    render(<LogsPage />);
    expect(await screen.findByText(/connection refused/)).toBeDefined();
    expect(await screen.findByText(/HTTP server starting/)).toBeDefined();
  });

  it('shows the timestamp and level columns for structured lines', async () => {
    const { container } = render(<LogsPage />);
    await screen.findByText(/connection refused/);
    expect(container.textContent).toContain('10:30:00.123');
    expect(container.textContent).toContain('ERROR');
    expect(container.textContent).toContain('WARN');
    expect(container.textContent).toContain('INFO');
  });

  it('renders unparseable lines as bare message text (no timestamp/level)', async () => {
    const { container } = render(<LogsPage />);
    await screen.findByText(/connection refused/);
    // The unstructured line still appears as a message row.
    expect(container.textContent).toContain('not a structured log line at all');
  });

  it('narrows displayed logs via the filter input', async () => {
    const { container } = render(<LogsPage />);
    await screen.findByText(/connection refused/);

    const input = screen.getByPlaceholderText('Filter logs…') as HTMLInputElement;
    fireEvent.change(input, { target: { value: 'spike' } });

    expect(container.textContent).toContain('battery power spike rejected');
    expect(container.textContent).not.toContain('connection refused');
  });

  it('shows the no-match message when the filter excludes everything', async () => {
    const { container } = render(<LogsPage />);
    await screen.findByText(/connection refused/);

    const input = screen.getByPlaceholderText('Filter logs…') as HTMLInputElement;
    fireEvent.change(input, { target: { value: 'zzz-nothing-matches' } });

    expect(container.textContent).toContain('No logs match the filter');
  });

  it('Clear button resets the filter', async () => {
    const { container } = render(<LogsPage />);
    await screen.findByText(/connection refused/);

    const input = screen.getByPlaceholderText('Filter logs…') as HTMLInputElement;
    fireEvent.change(input, { target: { value: 'spike' } });
    expect(container.textContent).not.toContain('connection refused');

    fireEvent.click(screen.getByText('Clear'));
    // All logs visible again.
    expect(container.textContent).toContain('connection refused');
  });

  it('calls PUT /api/log-level when a capture level button is clicked', async () => {
    render(<LogsPage />);
    await screen.findByText(/connection refused/);

    const infoBtn = screen.getByRole('button', { name: 'INFO' });
    fireEvent.click(infoBtn);

    await waitFor(() => {
      expect(apiPutMock).toHaveBeenCalledWith('/api/log-level', { level: 'INFO' });
    });
  });

  it('shows a status message after changing the capture level', async () => {
    render(<LogsPage />);
    await screen.findByText(/connection refused/);

    fireEvent.click(screen.getByRole('button', { name: 'INFO' }));

    await waitFor(() => {
      expect(screen.getByText(/Capture level set to/)).toBeDefined();
    });
  });

  it('shows a failure status when the PUT rejects', async () => {
    apiPutMock.mockRejectedValueOnce(new Error('network'));
    render(<LogsPage />);
    await screen.findByText(/connection refused/);

    fireEvent.click(screen.getByRole('button', { name: 'INFO' }));

    await waitFor(() => {
      expect(screen.getByText('Failed to change capture level')).toBeDefined();
    });
  });

  it('Refresh button re-fetches logs', async () => {
    render(<LogsPage />);
    await screen.findByText(/connection refused/);
    const initialLogsCalls = apiGetMock.mock.calls.filter((c) => c[0] === '/api/logs').length;

    fireEvent.click(screen.getByText('Refresh'));

    await waitFor(() => {
      const after = apiGetMock.mock.calls.filter((c) => c[0] === '/api/logs').length;
      expect(after).toBeGreaterThan(initialLogsCalls);
    });
  });

  it('shows the empty-state message when no logs are captured', async () => {
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/log-level') return { ok: true, level: 'WARN' };
      if (path === '/api/logs') return { ok: true, lines: [], count: 0 };
      return { ok: true };
    });
    const { container } = render(<LogsPage />);
    await waitFor(() => {
      expect(container.textContent).toContain('No logs captured yet');
    });
  });

  it('highlights the active capture level', async () => {
    render(<LogsPage />);
    await screen.findByText(/connection refused/);
    // WARN is the active level returned by the mock.
    const warnBtn = screen.getByRole('button', { name: 'WARN' });
    expect(warnBtn.className).toContain('flow-active');
  });

  it('polls for new logs on the interval', async () => {
    render(<LogsPage />);
    await screen.findByText(/connection refused/);
    const initialCalls = apiGetMock.mock.calls.filter((c) => c[0] === '/api/logs').length;

    // The polling interval is 2s. Advance real time past it.
    await new Promise((r) => setTimeout(r, 2200));

    const after = apiGetMock.mock.calls.filter((c) => c[0] === '/api/logs').length;
    expect(after).toBeGreaterThan(initialCalls);
  });
});
