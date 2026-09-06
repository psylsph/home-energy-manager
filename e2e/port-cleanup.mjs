import { execFileSync } from 'child_process';
import { readdirSync, readFileSync, readlinkSync } from 'fs';

/**
 * Kill processes listening on a test port without requiring a system utility.
 *
 * lsof is used when available; Linux's /proc tables provide a dependency-free
 * fallback for minimal test containers. The current runner is always excluded
 * because the mock global setup owns listeners in that process.
 *
 * @param {number} port
 * @returns {void}
 */
export function killPort(port) {
  if (!Number.isInteger(port) || port < 1 || port > 65535) return;

  const pids = new Set();
  if (process.platform === 'win32') {
    try {
      const output = execFileSync('netstat', ['-ano', '-p', 'tcp'], {
        encoding: 'utf8',
        stdio: ['ignore', 'pipe', 'ignore'],
      });
      for (const pid of listeningPidsFromWindowsNetstat(output, port)) pids.add(pid);
    } catch {
      // Nothing to clean when Windows cannot enumerate TCP listeners.
    }
  } else {
    const lsofArgs = [['-sTCP:LISTEN', '-ti', `tcp:${port}`], ['-ti', `tcp:${port}`]];
    for (const args of lsofArgs) {
      try {
        const output = execFileSync('lsof', args, {
          encoding: 'utf8',
          stdio: ['ignore', 'pipe', 'ignore'],
        });
        for (const value of output.trim().split(/\s+/)) {
          const pid = Number(value);
          if (Number.isSafeInteger(pid) && pid > 0) pids.add(pid);
        }
        break;
      } catch {
        // Retry without the filter for older lsof builds, then use /proc.
      }
    }
    for (const pid of listeningPidsFromProc(port)) pids.add(pid);
  }

  pids.delete(process.pid);
  for (const pid of pids) {
    try {
      process.kill(pid, 'SIGKILL');
    } catch {
      // The process may have exited between discovery and cleanup.
    }
  }
}

/**
 * Parse Windows `netstat -ano -p tcp` output for listeners on `port`.
 *
 * @param {string} output
 * @param {number} port
 * @returns {number[]}
 */
export function listeningPidsFromWindowsNetstat(output, port) {
  const pids = new Set();
  for (const line of output.split(/\r?\n/)) {
    const fields = line.trim().split(/\s+/);
    if (fields.length < 5 || fields[0]?.toUpperCase() !== 'TCP') continue;
    if (fields[3]?.toUpperCase() !== 'LISTENING') continue;

    const localAddress = fields[1] ?? '';
    const separator = localAddress.lastIndexOf(':');
    if (separator < 0 || Number(localAddress.slice(separator + 1)) !== port) continue;

    const pid = Number(fields[4]);
    if (Number.isSafeInteger(pid) && pid > 0) pids.add(pid);
  }
  return [...pids];
}

/**
 * Find listening PIDs from Linux's TCP tables and file descriptors.
 *
 * @param {number} port
 * @returns {number[]}
 */
function listeningPidsFromProc(port) {
  const inodes = new Set();
  const hexPort = port.toString(16).toUpperCase().padStart(4, '0');
  for (const netFile of ['/proc/net/tcp', '/proc/net/tcp6']) {
    let content;
    try {
      content = readFileSync(netFile, 'utf8');
    } catch {
      continue;
    }
    for (const line of content.split('\n').slice(1)) {
      const fields = line.trim().split(/\s+/);
      if (fields.length < 10 || fields[3] !== '0A') continue;
      if (fields[1]?.endsWith(`:${hexPort}`)) inodes.add(fields[9]);
    }
  }
  if (inodes.size === 0) return [];

  const pids = [];
  let entries;
  try {
    entries = readdirSync('/proc');
  } catch {
    return [];
  }
  for (const entry of entries) {
    if (!/^\d+$/.test(entry)) continue;
    const fdDir = `/proc/${entry}/fd`;
    let fds;
    try {
      fds = readdirSync(fdDir);
    } catch {
      continue;
    }
    for (const fd of fds) {
      let link;
      try {
        link = readlinkSync(`${fdDir}/${fd}`);
      } catch {
        continue;
      }
      const match = /^socket:\[(\d+)\]$/.exec(link);
      if (match && inodes.has(match[1])) {
        pids.push(Number(entry));
        break;
      }
    }
  }
  return pids;
}
