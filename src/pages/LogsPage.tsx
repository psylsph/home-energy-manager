import { useState, useEffect, useRef, useCallback } from 'react';
import { apiGet } from '../lib/api';

interface LogEntry {
  timestamp: string;
  level: string;
  message: string;
  raw: string;
}

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
  const [levelFilter, setLevelFilter] = useState<string>('all');
  const [autoScroll, setAutoScroll] = useState(true);
  const containerRef = useRef<HTMLDivElement>(null);
  const prevCountRef = useRef(0);

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
    // Initial fetch
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

  const filteredLogs = logs.filter((log) => {
    if (levelFilter !== 'all' && log.level !== levelFilter) return false;
    if (filter && !log.raw.toLowerCase().includes(filter.toLowerCase())) return false;
    return true;
  });

  const handleScroll = () => {
    if (!containerRef.current) return;
    const { scrollTop, scrollHeight, clientHeight } = containerRef.current;
    const atBottom = scrollHeight - scrollTop - clientHeight < 50;
    setAutoScroll(atBottom);
  };

  const clearFilter = () => {
    setFilter('');
    setLevelFilter('all');
  };

  return (
    <div className="flex flex-col gap-4 max-w-4xl mx-auto px-4 py-6 h-[calc(100vh-10rem)]">
      {/* Header */}
      <div className="flex items-center justify-between">
        <h1 className="text-text-primary text-lg font-semibold font-sans">Developer Logs</h1>
        <div className="flex items-center gap-3">
          <span className="text-text-secondary text-xs font-sans">
            {filteredLogs.length}/{logs.length} lines
          </span>
          <button
            onClick={fetchLogs}
            className="bg-bg-elevated text-text-primary font-sans text-xs px-3 py-1.5 rounded-lg hover:bg-bg-base transition-colors"
          >
            Refresh
          </button>
        </div>
      </div>

      {/* Filters */}
      <div className="flex items-center gap-3">
        <input
          type="text"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder="Filter logs…"
          className="flex-1 bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-flow-active outline-none transition-colors"
        />
        <select
          value={levelFilter}
          onChange={(e) => setLevelFilter(e.target.value)}
          className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-sans border border-bg-elevated focus:border-flow-active outline-none transition-colors"
        >
          <option value="all">All Levels</option>
          <option value="ERROR">ERROR</option>
          <option value="WARN">WARN</option>
          <option value="INFO">INFO</option>
          <option value="DEBUG">DEBUG</option>
        </select>
        {(filter || levelFilter !== 'all') && (
          <button
            onClick={clearFilter}
            className="text-text-secondary text-xs font-sans hover:text-text-primary transition-colors"
          >
            Clear
          </button>
        )}
      </div>

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
              <div key={i} className="flex gap-3 hover:bg-white/[0.02] rounded">
                <span className="text-text-secondary/60 shrink-0 w-[4.5rem]">{log.timestamp}</span>
                <span className={`shrink-0 w-10 ${levelColor(log.level)}`}>{log.level || '    '}</span>
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
