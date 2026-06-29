import { useState, useEffect, useRef, useCallback } from 'react';
import { apiGet, apiPut } from '../lib/api';

interface LogEntry {
  timestamp: string;
  level: string;
  message: string;
  raw: string;
}

const LEVELS = ['ERROR', 'WARN', 'INFO', 'DEBUG', 'TRACE'] as const;

function parseLogLine(line: string): LogEntry {
  // Format: "HH:MM:SS.mmm LEVEL [module] message"
  const timeMatch = line.match(/^(\d{2}:\d{2}:\d{2}\.\d+)\s+(ERROR|WARN|INFO|DEBUG|TRACE)\s+\[([^\]]*)\]\s*(.*)/);
  if (timeMatch) {
    return {
      timestamp: timeMatch[1],
      level: timeMatch[2].trim(),
      message: timeMatch[4],
      raw: line,
    };
  }
  return {
    timestamp: '',
    level: '',
    message: line,
    raw: line,
  };
}

function levelColor(level: string): string {
  switch (level) {
    case 'ERROR': return 'text-red-400';
    case 'WARN': return 'text-yellow-400';
    case 'INFO': return 'text-green-400';
    case 'DEBUG': return 'text-blue-400';
    case 'TRACE': return 'text-gray-500';
    default: return 'text-text-secondary';
  }
}

export default function LogsPage() {
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [filter, setFilter] = useState<string>('');
  const [captureLevel, setCaptureLevel] = useState<string>('WARN');
  const [statusMsg, setStatusMsg] = useState<string | null>(null);
  const [autoScroll, setAutoScroll] = useState(true);
  const containerRef = useRef<HTMLDivElement>(null);
  const prevCountRef = useRef(0);

  // Fetch the current backend capture level on mount
  useEffect(() => {
    (async () => {
      try {
        const res = await apiGet<{ ok: boolean; level: string }>('/api/log-level');
        if (res.ok) setCaptureLevel(res.level);
      } catch {
        // backend not running
      }
    })();
  }, []);

  const fetchLogs = useCallback(async () => {
    try {
      const res = await apiGet<{ ok: boolean; lines: string[]; count: number }>('/api/logs');
      const parsed = res.lines.map(parseLogLine);
      setLogs(parsed);
    } catch {
      // ignore — backend might not be running
    }
  }, []);

  // Poll for new logs every 2 seconds
  const logsFetchedRef = useRef(false);
  useEffect(() => {
    if (!logsFetchedRef.current) {
      logsFetchedRef.current = true;
      fetchLogs();
    }
    const interval = setInterval(fetchLogs, 2000);
    return () => clearInterval(interval);
  }, [fetchLogs]);

  // Auto-scroll to bottom when new logs arrive
  useEffect(() => {
    if (autoScroll && containerRef.current && logs.length > prevCountRef.current) {
      containerRef.current.scrollTop = containerRef.current.scrollHeight;
    }
    prevCountRef.current = logs.length;
  }, [logs, autoScroll]);

  // Change the backend capture level
  const changeCaptureLevel = async (level: string) => {
    try {
      const res = await apiPut<{ ok: boolean; level: string }>('/api/log-level', { level });
      if (res.ok) {
        setCaptureLevel(res.level);
        setStatusMsg(`Capture level set to ${res.level}`);
        setTimeout(() => setStatusMsg(null), 3000);
      }
    } catch {
      setStatusMsg('Failed to change capture level');
      setTimeout(() => setStatusMsg(null), 3000);
    }
  };

  // Text filter — client-side only, narrows what's displayed
  const filteredLogs = logs.filter((log) => {
    if (filter && !log.raw.toLowerCase().includes(filter.toLowerCase())) return false;
    return true;
  });

  const handleScroll = () => {
    if (!containerRef.current) return;
    const { scrollTop, scrollHeight, clientHeight } = containerRef.current;
    const atBottom = scrollHeight - scrollTop - clientHeight < 50;
    setAutoScroll(atBottom);
  };

  return (
    <div className="flex flex-col gap-4 max-w-4xl mx-auto px-4 py-6 h-[calc(100vh-10rem)]">
      {/* Header */}
      <div className="flex items-center justify-between gap-2">
        <h1 className="text-text-primary text-lg font-semibold font-sans truncate min-w-0">Developer Logs</h1>
        <div className="flex items-center gap-2 shrink-0">
          <span className="text-text-secondary text-xs font-sans hidden xs:inline">
            {filteredLogs.length}/{logs.length} lines
          </span>
          <button
            onClick={fetchLogs}
            className="bg-bg-elevated text-text-primary font-sans text-xs px-3 py-1.5 rounded-lg hover:bg-bg-base transition-colors shrink-0"
          >
            Refresh
          </button>
        </div>
      </div>

      {/* Controls */}
      <div className="flex flex-col gap-2">
        {/* Backend capture level selector — own row */}
        <div className="flex items-center gap-1.5">
          <span className="text-text-secondary text-xs font-sans shrink-0">Capture:</span>
          <div className="flex rounded-lg overflow-hidden border border-bg-elevated">
            {LEVELS.map((level) => (
              <button
                key={level}
                onClick={() => changeCaptureLevel(level)}
                className={`px-2 py-1.5 text-xs font-sans transition-colors ${
                  captureLevel === level
                    ? 'bg-flow-active text-bg-base font-semibold'
                    : 'bg-bg-elevated text-text-secondary hover:text-text-primary'
                }`}
              >
                {level}
              </button>
            ))}
          </div>
        </div>

        {/* Text filter input + Clear — own row */}
        <div className="flex items-center gap-2">
          <input
            type="text"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="Filter logs…"
            className="flex-1 bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
          />
          {filter && (
            <button
              onClick={() => setFilter('')}
              className="text-text-secondary text-xs font-sans hover:text-text-primary transition-colors shrink-0"
            >
              Clear
            </button>
          )}
        </div>
      </div>

      {/* Status message */}
      {statusMsg && (
        <div className="text-xs text-flow-active font-sans text-center">{statusMsg}</div>
      )}

      {/* Log output */}
      <div
        ref={containerRef}
        onScroll={handleScroll}
        className="flex-1 bg-bg-surface rounded-xl overflow-auto border border-white/5"
      >
        {filteredLogs.length === 0 ? (
          <div className="flex items-center justify-center h-full text-text-secondary text-sm font-sans">
            {logs.length === 0 ? 'No logs captured yet' : 'No logs match the filter'}
          </div>
        ) : (
          <div className="px-2 py-1 font-mono text-xs leading-5 whitespace-pre-wrap">
            {filteredLogs.map((log, i) => (
              <div key={i} className="flex gap-1 hover:bg-white/[0.02] rounded">
                <span className="text-text-secondary/60 shrink-0 w-[5.5rem]">{log.timestamp}</span>
                <span className={`shrink-0 w-12 ${levelColor(log.level)}`}>{log.level || '    '}</span>
                <span className="text-text-primary break-all">{log.message}</span>
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Auto-scroll indicator */}
      {!autoScroll && (
        <button
          onClick={() => {
            setAutoScroll(true);
            if (containerRef.current) {
              containerRef.current.scrollTop = containerRef.current.scrollHeight;
            }
          }}
          className="fixed bottom-20 right-8 bg-flow-active text-bg-base text-xs font-sans font-semibold px-3 py-2 rounded-full shadow-lg hover:opacity-90 transition-opacity z-40"
        >
          ↓ Scroll to bottom
        </button>
      )}
    </div>
  );
}
